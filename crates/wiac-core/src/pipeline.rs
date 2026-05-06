//! Shared CAM pipeline driver — per-operation gcode emission.
//!
//! All three transports (HTTP, Tauri, WASM) funnel through `run_pipeline`.
//! Internally the driver always operates on a [`crate::project::Project`]:
//! legacy callers that hand us segments + Setup + tabs get migrated via
//! [`crate::project::migrate_legacy`] before the per-op loop runs.
//!
//! Each enabled operation produces a gcode block prefixed with a
//! `; OP <id>` marker so the preview interpreter (UX-2) can stamp the
//! right `op_id` on every resulting [`preview::ToolpathSegment`]. The
//! whole program shares a single header/footer; cut blocks concatenate
//! between them.

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
use crate::gcode::{
    emit_polylines_block, emit_program_begin, emit_program_end, grbl, hpgl, linuxcnc, preview,
    PostProcessor,
};
use crate::geometry::{Point2, Segment};
use crate::project::{
    collapse_to_setup, migrate_legacy, Operation, OperationKind, OperationSource, PocketStrategy,
    Project,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PipelineRequest {
    /// Legacy flat-shape input — segments + Setup + tabs. Still the path
    /// every existing client takes; UX-3 migrates them internally to
    /// Project + a single Profile/Pocket op.
    pub segments: Vec<Segment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<Setup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_processor: Option<PostProcessorKind>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tabs: HashMap<u32, Vec<TabPoint>>,

    /// New op-driven input. When present this takes precedence over
    /// segments/setup/tabs; the `Project` carries its own copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Project>,
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
    pub gcode_index: preview::GcodeIndex,
    pub stats: PipelineStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PipelineStats {
    pub object_count: usize,
    pub closed_object_count: usize,
    pub offset_count: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("unknown post_processor: {0}")]
    UnknownPostProcessor(String),
    #[error("operation #{0} references unknown tool id {1}")]
    UnknownTool(u32, u32),
    #[error("operation kind {0:?} is not implemented yet")]
    UnimplementedKind(OperationKind),
}

/// Run the full CAM pipeline. `progress(phase, fraction, message)` is
/// called at each phase boundary; pass a no-op closure for non-streaming
/// callers.
pub fn run_pipeline<F: Fn(&str, f64, &str)>(
    req: PipelineRequest,
    progress: F,
) -> Result<PipelineResponse, PipelineError> {
    progress("import", 0.05, "preparing project");

    // Settle on a Project regardless of which input shape the caller used.
    let project = match req.project {
        Some(p) => p,
        None => migrate_legacy(
            req.segments,
            req.setup.unwrap_or_default(),
            req.tabs,
        ),
    };

    let mut objects = segments_to_objects(&project.segments);
    classify_containment(&mut objects);
    progress("objects", 0.20, "chained segments into objects");

    let post_kind = req.post_processor.unwrap_or_default();
    // Use the first enabled op's setup as the program-level header / footer
    // basis — that way Setup.machine.unit / mill.fast_move_z / tool.rate_h
    // come from a real op, not a synthetic default.
    let header_setup = collapse_to_setup(&project).0;
    let stats_collector = std::cell::RefCell::new((0usize, 0usize, 0usize)); // (closed, offsets, _)
    let progress_ref = &progress;
    let n_ops = project.operations.iter().filter(|o| o.enabled).count().max(1);

    let gcode = match post_kind {
        PostProcessorKind::Linuxcnc => {
            run_per_op(
                &project,
                &mut objects.clone(),
                &header_setup,
                &mut linuxcnc::Post::new(),
                &stats_collector,
                progress_ref,
                n_ops,
            )?
        }
        PostProcessorKind::Grbl => run_per_op(
            &project,
            &mut objects.clone(),
            &header_setup,
            &mut grbl::Post::new(),
            &stats_collector,
            progress_ref,
            n_ops,
        )?,
        PostProcessorKind::Hpgl => run_per_op(
            &project,
            &mut objects.clone(),
            &header_setup,
            &mut hpgl::Post::new(),
            &stats_collector,
            progress_ref,
            n_ops,
        )?,
    };
    let (total_closed, total_offsets, _) = *stats_collector.borrow();

    progress("preview", 0.92, "interpreting toolpath");
    let (toolpath, gcode_index) = preview::interpret_with_index(&gcode);
    progress("done", 1.0, "complete");
    Ok(PipelineResponse {
        stats: PipelineStats {
            object_count: objects.len(),
            closed_object_count: total_closed,
            offset_count: total_offsets,
        },
        gcode,
        toolpath,
        gcode_index,
    })
}

/// Per-post-processor monomorphisation of the per-op driver. Pulled out
/// so we don't need to type-erase PostProcessor (its methods take Sized
/// `&mut self` so the trait object dance was painful).
fn run_per_op<P, F>(
    project: &Project,
    objects: &mut Vec<VcObject>,
    header_setup: &Setup,
    post: &mut P,
    stats: &std::cell::RefCell<(usize, usize, usize)>,
    progress: &F,
    n_ops: usize,
) -> Result<String, PipelineError>
where
    P: PostProcessor,
    F: Fn(&str, f64, &str),
{
    emit_program_begin(header_setup, post);
    let mut last_pos = Point2::new(0.0, 0.0);
    let mut emitted_ops = 0usize;
    for op in project.operations.iter().filter(|o| o.enabled) {
        let setup = synthesize_op_setup(op, project)?;
        let (offsets, closed_count) =
            build_op_offsets(op, project, &mut objects.clone(), &setup)?;
        {
            let mut s = stats.borrow_mut();
            s.0 += closed_count;
            s.1 += offsets.len();
        }
        post.raw(&format!("; OP {}", op.id));
        if !offsets.is_empty() {
            emit_polylines_block(&setup, &offsets, post, &mut last_pos);
        }
        emitted_ops += 1;
        progress(
            "gcode",
            0.30 + 0.55 * (emitted_ops as f64 / n_ops as f64),
            &format!("emitted op {}", op.id),
        );
    }
    emit_program_end(header_setup, post);
    Ok(post.finish())
}

// ─── per-op offset building ───────────────────────────────────────────────

/// Build the offset list a single op consumes. Currently supports
/// Profile / Pocket / Engrave / DragKnife — others raise UnimplementedKind.
fn build_op_offsets(
    op: &Operation,
    project: &Project,
    objects: &mut Vec<VcObject>,
    setup: &Setup,
) -> Result<(Vec<PolylineOffset>, usize), PipelineError> {
    // Map imported-segment-keyed tabs → owning chain object.
    let mut tabs_by_object: HashMap<usize, Vec<TabPoint>> = HashMap::new();
    if !project.tabs.is_empty() {
        let segment_to_object = build_segment_to_object_map(&project.segments, objects);
        for (seg_idx, tabs) in &project.tabs {
            if let Some(&obj_idx) = segment_to_object.get(&(*seg_idx as usize)) {
                tabs_by_object
                    .entry(obj_idx)
                    .or_default()
                    .extend_from_slice(tabs);
            }
        }
    }

    // Apply per-op tool-offset to the chain so order_offsets / lead-in see it.
    for obj in objects.iter_mut() {
        obj.tool_offset = setup.mill.offset;
    }

    let radius = setup.tool.diameter * 0.5;
    let mut offsets: Vec<PolylineOffset> = Vec::new();
    let mut closed = 0usize;
    let mut emitted_objects = 0usize;
    for (idx, obj) in objects.iter().enumerate() {
        if !op_includes_object(op, obj) {
            continue;
        }
        emitted_objects += 1;
        if obj.closed {
            closed += 1;
        }

        match op.kind {
            OperationKind::Pocket { strategy } => {
                let zigzag = matches!(strategy, PocketStrategy::Zigzag);
                let islands: Vec<Vec<Point2>> = if op.params.pocket_islands {
                    obj.inner_objects
                        .iter()
                        .filter_map(|i| objects.get(*i))
                        .filter(|inner| inner.closed)
                        .map(|inner| segments_to_points(&inner.segments, 6))
                        .collect()
                } else {
                    Vec::new()
                };
                if obj.closed {
                    for mut o in pocket_for_object(
                        obj,
                        radius,
                        op.params.pocket_nocontour,
                        6,
                        zigzag,
                        &islands,
                    ) {
                        o.source_object_idx = idx;
                        offsets.push(o);
                    }
                }
            }
            OperationKind::Profile { .. } => {
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
            OperationKind::Engrave | OperationKind::DragKnife => {
                // Both follow the source path with no offset; the gcode
                // emitter handles drag-knife trail compensation per-op.
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
            }
            OperationKind::Drill
            | OperationKind::Thread
            | OperationKind::Chamfer
            | OperationKind::Helix => {
                return Err(PipelineError::UnimplementedKind(op.kind));
            }
        }
    }
    let _ = emitted_objects;

    if !tabs_by_object.is_empty() {
        attach_tabs_to_offsets(&mut offsets, &tabs_by_object, setup.tool.diameter * 1.5);
    }
    if op.params.overcut {
        apply_overcut_to_offsets(&mut offsets, objects, setup.tool.diameter * 0.5);
    }
    Ok((offsets, closed))
}

fn op_includes_object(op: &Operation, obj: &VcObject) -> bool {
    match &op.source {
        OperationSource::All => true,
        OperationSource::Layers { layers } => layers.iter().any(|l| l == &obj.layer),
        // Object ids aren't yet stamped onto VcObjects — UX-8 (selection
        // model) lands them. Until then a non-empty Objects source falls
        // back to All so the op still runs.
        OperationSource::Objects { ids } => ids.is_empty() || ids.iter().any(|_| true),
    }
}

/// Build a Setup that represents this single op — copy in its tool from
/// `project.tools` and its params.kind-driven mill/pockets/tabs/leads.
fn synthesize_op_setup(op: &Operation, project: &Project) -> Result<Setup, PipelineError> {
    use crate::cam::setup::{
        MachineMode, MillConfig, PocketConfig, ToolConfig, ToolOffset,
    };

    let tool = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;

    let mut setup = Setup {
        machine: project.machine.clone(),
        ..Setup::default()
    };
    setup.tool = ToolConfig {
        number: tool.id,
        diameter: tool.diameter,
        speed: tool.speed,
        pause: 1,
        mist: matches!(tool.coolant, crate::project::Coolant::Mist),
        flood: matches!(tool.coolant, crate::project::Coolant::Flood),
        dragoff: tool.dragoff,
        rate_v: tool.plunge_rate,
        rate_h: tool.feed_rate,
    };
    let offset = match op.kind {
        OperationKind::Profile { offset } => offset,
        OperationKind::Pocket { .. } => ToolOffset::None,
        OperationKind::Engrave | OperationKind::DragKnife => ToolOffset::On,
        _ => ToolOffset::None,
    };
    setup.mill = MillConfig {
        active: true,
        depth: op.params.depth,
        start_depth: op.params.start_depth,
        step: op.params.step,
        fast_move_z: op.params.fast_move_z,
        helix_mode: op.params.helix,
        reverse: op.params.reverse,
        objectorder: op.params.objectorder,
        offset,
        overcut: op.params.overcut,
    };
    setup.pockets = match op.kind {
        OperationKind::Pocket { strategy } => PocketConfig {
            active: true,
            islands: op.params.pocket_islands,
            zigzag: matches!(strategy, PocketStrategy::Zigzag),
            insideout: op.params.pocket_insideout,
            nocontour: op.params.pocket_nocontour,
        },
        _ => PocketConfig::default(),
    };
    setup.tabs = op.params.tabs.clone();
    setup.leads = op.params.leads.clone();
    if matches!(op.kind, OperationKind::DragKnife) {
        setup.machine.mode = MachineMode::Drag;
    }
    Ok(setup)
}

// ─── helpers ──────────────────────────────────────────────────────────────

fn build_segment_to_object_map(
    segments: &[Segment],
    objects: &[VcObject],
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

fn approx_pt(a: Point2, b: Point2) -> bool {
    (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::setup::ToolOffset;
    use crate::geometry::Segment;
    use crate::project::{
        Coolant, Operation, OperationKind, OperationParams, OperationSource, ToolEntry,
        ToolKind,
    };

    fn closed_square(side: f64) -> Vec<Segment> {
        vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(side, 0.0), "0", 7),
            Segment::line(Point2::new(side, 0.0), Point2::new(side, side), "0", 7),
            Segment::line(Point2::new(side, side), Point2::new(0.0, side), "0", 7),
            Segment::line(Point2::new(0.0, side), Point2::new(0.0, 0.0), "0", 7),
        ]
    }

    #[test]
    fn run_pipeline_emits_a_recognizable_program() {
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.mill.depth = -2.0;
        setup.mill.offset = ToolOffset::Outside;
        setup.machine.comments = false;

        let resp = run_pipeline(
            PipelineRequest {
                segments: closed_square(20.0),
                setup: Some(setup),
                post_processor: Some(PostProcessorKind::Linuxcnc),
                tabs: HashMap::new(),
                project: None,
            },
            |_, _, _| {},
        )
        .unwrap();

        assert!(resp.gcode.contains("G21"));
        assert!(resp.gcode.contains("G90"));
        assert!(!resp.toolpath.is_empty());
        assert_eq!(resp.stats.object_count, 1);
        assert_eq!(resp.stats.closed_object_count, 1);
        assert!(resp.stats.offset_count >= 1);
    }

    #[test]
    fn run_pipeline_picks_grbl_when_requested() {
        let resp = run_pipeline(
            PipelineRequest {
                segments: closed_square(20.0),
                setup: None,
                post_processor: Some(PostProcessorKind::Grbl),
                tabs: HashMap::new(),
                project: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(!resp.gcode.is_empty());
    }

    #[test]
    fn legacy_request_emits_an_op_marker() {
        let resp = run_pipeline(
            PipelineRequest {
                segments: closed_square(10.0),
                setup: None,
                post_processor: Some(PostProcessorKind::Linuxcnc),
                tabs: HashMap::new(),
                project: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // migrate_legacy synthesises a single op with id=1. The driver
        // emits the marker before its cut block, and preview::interpret
        // stamps op_id=1 onto every produced segment after it. The
        // program-header rapid moves before the marker carry op_id=0,
        // which is the right "this isn't a cut" signal.
        assert!(resp.gcode.contains("; OP 1"));
        assert!(resp.toolpath.iter().any(|s| s.op_id == 1));
        assert!(
            resp.toolpath
                .iter()
                .filter(|s| s.op_id != 0)
                .all(|s| s.op_id == 1)
        );
    }

    #[test]
    fn two_op_project_emits_two_distinct_op_blocks() {
        let segments = closed_square(20.0);
        let project = Project {
            segments,
            machine: Default::default(),
            tools: vec![ToolEntry {
                id: 1,
                name: "test".into(),
                kind: ToolKind::Endmill,
                diameter: 3.0,
                tip_diameter: None,
                dragoff: None,
                flutes: 2,
                speed: 12_000,
                plunge_rate: 100,
                feed_rate: 800,
                coolant: Coolant::Off,
            }],
            operations: vec![
                Operation {
                    id: 1,
                    name: "rough".into(),
                    enabled: true,
                    kind: OperationKind::Profile {
                        offset: ToolOffset::Outside,
                    },
                    tool_id: 1,
                    source: OperationSource::All,
                    params: OperationParams::mill_default(),
                },
                Operation {
                    id: 2,
                    name: "finish".into(),
                    enabled: true,
                    kind: OperationKind::Profile {
                        offset: ToolOffset::Outside,
                    },
                    tool_id: 1,
                    source: OperationSource::All,
                    params: OperationParams::mill_default(),
                },
            ],
            tabs: Default::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                segments: vec![],
                setup: None,
                post_processor: Some(PostProcessorKind::Linuxcnc),
                tabs: HashMap::new(),
                project: Some(project),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.gcode.contains("; OP 1"));
        assert!(resp.gcode.contains("; OP 2"));
        // Toolpath segments split between op 1 and op 2.
        assert!(resp.toolpath.iter().any(|s| s.op_id == 1));
        assert!(resp.toolpath.iter().any(|s| s.op_id == 2));
    }

    #[test]
    fn progress_callback_fires_each_phase() {
        let phases = std::cell::RefCell::new(Vec::<String>::new());
        let _ = run_pipeline(
            PipelineRequest {
                segments: closed_square(10.0),
                setup: None,
                post_processor: None,
                tabs: HashMap::new(),
                project: None,
            },
            |phase, _f, _m| phases.borrow_mut().push(phase.to_string()),
        )
        .unwrap();
        let phases = phases.into_inner();
        for expected in ["import", "objects", "gcode", "preview", "done"] {
            assert!(phases.contains(&expected.to_string()), "missing {expected} in {phases:?}");
        }
    }

    #[test]
    fn unknown_post_processor_is_a_deserialization_failure() {
        let raw = serde_json::json!({
            "segments": [],
            "post_processor": "robotic_arm"
        });
        let res: Result<PipelineRequest, _> = serde_json::from_value(raw);
        assert!(res.is_err());
    }
}
