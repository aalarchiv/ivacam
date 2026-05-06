//! Shared CAM pipeline driver — segments + setup + tabs → gcode + 3D preview.
//!
//! Three transports (HTTP, Tauri, WASM) used to host their own copy of this
//! glue. Now they all funnel through `run_pipeline`; the only thing each
//! transport owns is request/response (de)serialization at its own boundary.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::chaining::{classify_containment, segments_to_objects};
use crate::cam::offsets::{
    apply_overcut_to_offsets, attach_tabs_to_offsets, parallel_offset_object, pocket_for_object,
    PolylineOffset, TabPoint,
};
use crate::cam::setup::{Setup, ToolOffset};
use crate::cam::{segments_to_points, VcObject};
use crate::gcode::{emit_polylines, grbl, hpgl, linuxcnc, preview};
use crate::geometry::{Point2, Segment};

/// Pipeline input. Tabs are keyed by *imported-segment* index (the key the
/// frontend uses when tracking placed tabs); we resolve each tab to its
/// containing chain object internally.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PipelineRequest {
    pub segments: Vec<Segment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<Setup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_processor: Option<PostProcessorKind>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tabs: HashMap<u32, Vec<TabPoint>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PostProcessorKind {
    #[default]
    Linuxcnc,
    Grbl,
    Hpgl,
}


#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PipelineResponse {
    pub gcode: String,
    pub toolpath: Vec<preview::ToolpathSegment>,
    pub stats: PipelineStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PipelineStats {
    pub object_count: usize,
    pub closed_object_count: usize,
    pub offset_count: usize,
}

/// Bad-input or unknown-post-processor errors. Internal CAM failures (panics
/// in the offsetter, etc.) bubble up via panic / the underlying functions —
/// they aren't recoverable at this layer.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("unknown post_processor: {0}")]
    UnknownPostProcessor(String),
}

/// Run the full CAM pipeline. `progress(phase, fraction, message)` is called
/// at each phase boundary; pass a no-op closure for non-streaming callers.
pub fn run_pipeline<F: Fn(&str, f64, &str)>(
    req: PipelineRequest,
    progress: F,
) -> Result<PipelineResponse, PipelineError> {
    let setup = req.setup.unwrap_or_default();
    progress("import", 0.05, "preparing segments");

    let mut objects = segments_to_objects(&req.segments);
    classify_containment(&mut objects);
    for obj in &mut objects {
        obj.tool_offset = setup.mill.offset;
    }
    progress("objects", 0.20, "chained segments into objects");

    // Map imported-segment-keyed tabs to their owning chain object. Each
    // chain knows which imported-segment indices it consumed.
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
            let islands: Vec<Vec<Point2>> = if setup.pockets.islands {
                obj.inner_objects
                    .iter()
                    .filter_map(|i| objects.get(*i))
                    .filter(|inner| inner.closed)
                    .map(|inner| segments_to_points(&inner.segments, 6))
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
    progress("offsets", 0.55, "built parallel offsets");

    // Snap tabs onto their offsets. Tab radius = 1.5× tool radius so the
    // tab still gates the cut even after the parallel offset moves the
    // toolpath off the source contour.
    if !tabs_by_object.is_empty() {
        attach_tabs_to_offsets(&mut offsets, &tabs_by_object, setup.tool.diameter * 1.5);
    }
    if setup.mill.overcut {
        apply_overcut_to_offsets(&mut offsets, &objects, setup.tool.diameter * 0.5);
    }

    let post_kind = req.post_processor.unwrap_or_default();
    progress("gcode", 0.75, "emitting gcode");
    let gcode = match post_kind {
        PostProcessorKind::Linuxcnc => emit_polylines(&setup, &offsets, &mut linuxcnc::Post::new()),
        PostProcessorKind::Grbl => emit_polylines(&setup, &offsets, &mut grbl::Post::new()),
        PostProcessorKind::Hpgl => emit_polylines(&setup, &offsets, &mut hpgl::Post::new()),
    };
    progress("preview", 0.92, "interpreting toolpath");
    let toolpath = preview::interpret(&gcode);
    progress("done", 1.0, "complete");
    Ok(PipelineResponse {
        stats: PipelineStats {
            object_count: objects.len(),
            closed_object_count: closed,
            offset_count: offsets.len(),
        },
        gcode,
        toolpath,
    })
}

/// Match imported segments to the chain object that consumed them by endpoint
/// coincidence. Tabs are keyed by imported-segment index, but the offsets
/// they get attached to are object-keyed; this bridges the two.
fn build_segment_to_object_map(
    segments: &[Segment],
    objects: &[VcObject],
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

fn approx_pt(a: Point2, b: Point2) -> bool {
    (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
}
