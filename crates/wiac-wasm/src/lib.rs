//! wiaConstructor WASM bindings — exposes the same JSON contract the HTTP
//! and Tauri transports speak so the frontend can run the entire CAM
//! pipeline in-browser without any server.
//!
//! Built with `wasm-pack build crates/wiac-wasm --target web --release`.
//! The resulting `pkg/` ships JS glue + a wiac_wasm_bg.wasm blob that the
//! Vite frontend can import directly.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use wiac_core::cam::chaining::{classify_containment, segments_to_objects};
use wiac_core::cam::offsets::{
    apply_overcut_to_offsets, attach_tabs_to_offsets, parallel_offset_object, pocket_for_object,
    PolylineOffset, TabPoint,
};
use wiac_core::cam::setup::{Setup, ToolOffset};
use wiac_core::gcode::{emit_polylines, grbl, hpgl, linuxcnc, preview};
use wiac_core::ImportOptions;

#[wasm_bindgen(start)]
pub fn start() {
    // Surface Rust panics in the JS console with stack traces. Silently
    // becomes a no-op if the hook is already installed.
    console_error_panic_hook::set_once();
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Serialize)]
struct VersionResponse<'a> {
    version: &'a str,
    transport: &'a str,
    git_sha: Option<&'a str>,
}

#[wasm_bindgen]
pub fn healthz() -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&HealthResponse { ok: true }).map_err(into_js_error)
}

#[wasm_bindgen]
pub fn version() -> Result<JsValue, JsValue> {
    let v = VersionResponse {
        version: env!("CARGO_PKG_VERSION"),
        transport: "wasm",
        git_sha: option_env!("GIT_SHA"),
    };
    serde_wasm_bindgen::to_value(&v).map_err(into_js_error)
}

/// Import a DXF/SVG/HPGL byte buffer. The web client sends `File`
/// contents as a Uint8Array and provides the filename so the Rust core
/// can match the format detector.
#[wasm_bindgen(js_name = importBytes)]
pub fn import_bytes(filename: &str, bytes: &[u8]) -> Result<JsValue, JsValue> {
    let opts = ImportOptions::default();
    let out = wiac_core::input::import_bytes(filename, bytes, &opts)
        .map_err(|e| into_js_error(format!("{e}")))?;
    serde_wasm_bindgen::to_value(&out).map_err(into_js_error)
}

#[derive(Deserialize)]
struct GenerateRequest {
    segments: Vec<wiac_core::Segment>,
    #[serde(default)]
    setup: Option<Setup>,
    #[serde(default)]
    post_processor: Option<String>,
    #[serde(default)]
    tabs: HashMap<u32, Vec<TabPoint>>,
}

#[derive(Serialize)]
struct GenerateResponse {
    gcode: String,
    toolpath: Vec<preview::ToolpathSegment>,
    stats: GenerateStats,
}

#[derive(Serialize, Default)]
struct GenerateStats {
    object_count: usize,
    closed_object_count: usize,
    offset_count: usize,
}

#[wasm_bindgen]
pub fn generate(request: JsValue) -> Result<JsValue, JsValue> {
    let req: GenerateRequest =
        serde_wasm_bindgen::from_value(request).map_err(into_js_error)?;
    let resp = run_generate(req).map_err(into_js_error)?;
    serde_wasm_bindgen::to_value(&resp).map_err(into_js_error)
}

#[derive(Serialize)]
struct DefaultsResponse {
    setup: Setup,
    schema: serde_json::Value,
    definitions: serde_json::Value,
}

#[wasm_bindgen]
pub fn defaults() -> Result<JsValue, JsValue> {
    let components = wiac_core::schema::components_schemas();
    let setup_schema = components
        .get("Setup")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let resp = DefaultsResponse {
        setup: Setup::default(),
        schema: setup_schema,
        definitions: components,
    };
    serde_wasm_bindgen::to_value(&resp).map_err(into_js_error)
}

fn run_generate(req: GenerateRequest) -> Result<GenerateResponse, String> {
    let setup = req.setup.unwrap_or_default();
    let mut objects = segments_to_objects(&req.segments);
    classify_containment(&mut objects);
    for obj in &mut objects {
        obj.tool_offset = setup.mill.offset;
    }

    let mut tabs_by_object: HashMap<usize, Vec<TabPoint>> = HashMap::new();
    if !req.tabs.is_empty() {
        let segment_to_object = build_segment_to_object_map(&req.segments, &objects);
        for (seg_idx, tabs) in &req.tabs {
            if let Some(&obj_idx) = segment_to_object.get(&(*seg_idx as usize)) {
                tabs_by_object
                    .entry(obj_idx)
                    .or_default()
                    .extend_from_slice(tabs);
            }
        }
    }

    let radius = setup.tool.diameter * 0.5;
    let mut offsets = Vec::new();
    let mut closed = 0usize;
    for (idx, obj) in objects.iter().enumerate() {
        if obj.closed {
            closed += 1;
        }
        if obj.closed && setup.pockets.active {
            let islands: Vec<Vec<wiac_core::Point2>> = if setup.pockets.islands {
                obj.inner_objects
                    .iter()
                    .filter_map(|i| objects.get(*i))
                    .filter(|inner| inner.closed)
                    .map(|inner| wiac_core::cam::segments_to_points(&inner.segments, 6))
                    .collect()
            } else {
                Vec::new()
            };
            for mut o in pocket_for_object(
                obj,
                radius,
                setup.pockets.nocontour,
                6,
                setup.pockets.zigzag,
                &islands,
            ) {
                o.source_object_idx = idx;
                offsets.push(o);
            }
            continue;
        }
        let delta = match setup.mill.offset {
            ToolOffset::None | ToolOffset::On => 0.0,
            ToolOffset::Outside => -radius,
            ToolOffset::Inside => radius,
        };
        if delta.abs() < 1e-9 {
            offsets.push(PolylineOffset {
                segments: obj.segments.clone(),
                closed: obj.closed,
                level: 0,
                is_pocket: 0,
                layer: obj.layer.clone(),
                color: obj.color,
                source_object_idx: idx,
                tabs: Vec::new(),
            });
        } else {
            for mut o in parallel_offset_object(obj, delta) {
                o.source_object_idx = idx;
                offsets.push(o);
            }
        }
    }

    if !tabs_by_object.is_empty() {
        attach_tabs_to_offsets(&mut offsets, &tabs_by_object, setup.tool.diameter * 1.5);
    }
    if setup.mill.overcut {
        apply_overcut_to_offsets(&mut offsets, &objects, setup.tool.diameter * 0.5);
    }

    let post_kind = req.post_processor.as_deref().unwrap_or("linuxcnc");
    let gcode = match post_kind {
        "linuxcnc" => emit_polylines(&setup, &offsets, &mut linuxcnc::Post::new()),
        "grbl" => emit_polylines(&setup, &offsets, &mut grbl::Post::new()),
        "hpgl" => emit_polylines(&setup, &offsets, &mut hpgl::Post::new()),
        other => return Err(format!("unknown post_processor: {other}")),
    };
    let toolpath = preview::interpret(&gcode);
    Ok(GenerateResponse {
        stats: GenerateStats {
            object_count: objects.len(),
            closed_object_count: closed,
            offset_count: offsets.len(),
        },
        gcode,
        toolpath,
    })
}

fn build_segment_to_object_map(
    segments: &[wiac_core::Segment],
    objects: &[wiac_core::cam::VcObject],
) -> HashMap<usize, usize> {
    let mut map = HashMap::new();
    for (obj_idx, obj) in objects.iter().enumerate() {
        for chain_seg in &obj.segments {
            for (seg_idx, src) in segments.iter().enumerate() {
                let same =
                    approx_pt(src.start, chain_seg.start) && approx_pt(src.end, chain_seg.end);
                let reverse =
                    approx_pt(src.start, chain_seg.end) && approx_pt(src.end, chain_seg.start);
                if same || reverse {
                    map.entry(seg_idx).or_insert(obj_idx);
                }
            }
        }
    }
    map
}

fn approx_pt(a: wiac_core::Point2, b: wiac_core::Point2) -> bool {
    (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
}

fn into_js_error<E: std::fmt::Display>(err: E) -> JsValue {
    JsValue::from_str(&err.to_string())
}
