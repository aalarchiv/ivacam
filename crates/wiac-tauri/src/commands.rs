//! Tauri command handlers — the in-process equivalent of the HTTP endpoints
//! exposed by `wiac-server`. Frontend calls these via `invoke('name', args)`
//! when running inside the desktop app; the same `WiacClient` interface
//! abstracts over HTTP vs Tauri so component code is transport-agnostic.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use wiac_core::cam::chaining::{classify_containment, segments_to_objects};
use wiac_core::cam::offsets::{
    apply_overcut_to_offsets, attach_tabs_to_offsets, parallel_offset_object, pocket_for_object,
    PolylineOffset, TabPoint,
};
use wiac_core::cam::setup::{Setup, ToolOffset};
use wiac_core::gcode::{emit_polylines, grbl, hpgl, linuxcnc, preview};
use wiac_core::{ImportOptions, ImportOutput};

#[derive(Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Serialize)]
pub struct VersionResponse {
    pub version: &'static str,
    pub transport: &'static str,
    pub git_sha: Option<&'static str>,
}

#[tauri::command]
pub fn healthz() -> HealthResponse {
    HealthResponse { ok: true }
}

#[tauri::command]
pub fn version() -> VersionResponse {
    VersionResponse {
        version: env!("CARGO_PKG_VERSION"),
        transport: "tauri",
        git_sha: option_env!("GIT_SHA"),
    }
}

/// Import a DXF/SVG/HPGL file by path. Counterpart of the HTTP `/import`
/// multipart endpoint; the desktop shell can hand a real OS path so we
/// avoid an upload round-trip.
#[tauri::command]
pub async fn import_path(path: String) -> Result<ImportOutput, String> {
    let path = PathBuf::from(path);
    let opts = ImportOptions::default();
    tokio::task::spawn_blocking(move || wiac_core::input::import_path(&path, &opts))
        .await
        .map_err(|e| format!("join error: {e}"))?
        .map_err(|e| format!("{e}"))
}

#[derive(Deserialize)]
pub struct GenerateRequest {
    pub segments: Vec<wiac_core::Segment>,
    #[serde(default)]
    pub setup: Option<Setup>,
    #[serde(default)]
    pub post_processor: Option<String>,
    #[serde(default)]
    pub tabs: HashMap<u32, Vec<TabPoint>>,
}

#[derive(Serialize)]
pub struct GenerateResponse {
    pub gcode: String,
    pub toolpath: Vec<preview::ToolpathSegment>,
    pub stats: GenerateStats,
}

#[derive(Serialize, Default)]
pub struct GenerateStats {
    pub object_count: usize,
    pub closed_object_count: usize,
    pub offset_count: usize,
}

#[tauri::command]
pub async fn generate(request: GenerateRequest) -> Result<GenerateResponse, String> {
    tokio::task::spawn_blocking(move || run_generate(request))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

#[derive(Serialize)]
pub struct DefaultsResponse {
    pub setup: Setup,
    pub schema: serde_json::Value,
    pub definitions: serde_json::Value,
}

#[tauri::command]
pub fn defaults() -> Result<DefaultsResponse, String> {
    let components = wiac_core::schema::components_schemas();
    let setup_schema = components
        .get("Setup")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok(DefaultsResponse {
        setup: Setup::default(),
        schema: setup_schema,
        definitions: components,
    })
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

/// Same heuristic as `wiac-server`'s handler — match imported segments to
/// the chain object that consumed them by endpoint coincidence. Tabs are
/// keyed by imported-segment index, but the offsets they get attached to
/// are object-keyed; this bridges the two.
fn build_segment_to_object_map(
    segments: &[wiac_core::Segment],
    objects: &[wiac_core::cam::VcObject],
) -> HashMap<usize, usize> {
    let mut map = HashMap::new();
    for (obj_idx, obj) in objects.iter().enumerate() {
        for chain_seg in &obj.segments {
            for (seg_idx, src) in segments.iter().enumerate() {
                let same = approx_pt(src.start, chain_seg.start) && approx_pt(src.end, chain_seg.end);
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
