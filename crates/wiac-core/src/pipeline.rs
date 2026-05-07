//! Shared CAM pipeline driver — per-operation gcode emission.
//!
//! All three transports (HTTP, Tauri, WASM) funnel through `run_pipeline`.
//! Each enabled operation produces a gcode block prefixed with a
//! `; OP <id>` marker so the preview interpreter (UX-2) can stamp the
//! right `op_id` on every resulting [`preview::ToolpathSegment`]. The
//! whole program shares a single header/footer; cut blocks concatenate
//! between them.

use std::collections::{HashMap, HashSet};

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
use crate::project::{Operation, OperationKind, OperationSource, PocketStrategy, Project};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PipelineRequest {
    /// The full project — geometry + machine + tools + operations + tabs.
    pub project: Project,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_processor: Option<PostProcessorKind>,
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
    let project = req.project;

    let mut objects = segments_to_objects(&project.segments);
    classify_containment(&mut objects);
    progress("objects", 0.20, "chained segments into objects");

    let post_kind = req.post_processor.unwrap_or_default();
    // Use the first enabled op's setup as the program-level header /
    // footer basis. This lets unit / fast_move_z / feed-rate come from
    // a real op rather than a synthetic default.
    let header_setup = header_setup_for(&project);
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

    // Containment-aware Pocket: when the user selects an outer ring and
    // an inner ring, the inner one should become a hole in the outer
    // pocket — not a top-level pocket boundary on its own. Compute the
    // selected-object set up front so the Pocket branch can consult it
    // while iterating.
    let selected_set: HashSet<usize> = (0..objects.len())
        .filter(|i| op_includes_object(op, &objects[*i], *i))
        .collect();
    for (idx, obj) in objects.iter().enumerate() {
        if !op_includes_object(op, obj, idx) {
            continue;
        }
        emitted_objects += 1;
        if obj.closed {
            closed += 1;
        }

        match op.kind {
            OperationKind::Pocket { strategy } => {
                // Skip objects that are geometrically inside another
                // selected object — they belong to that pocket as islands.
                let contained_by_selected = obj
                    .outer_objects
                    .iter()
                    .any(|o| selected_set.contains(o));
                if contained_by_selected {
                    continue;
                }
                let zigzag = matches!(strategy, PocketStrategy::Zigzag);
                // Islands = nested closed objects that are *also* in this
                // op's selection. Honored unconditionally so the user gets
                // an annulus pocket from "outer + inner" without having to
                // toggle pocket_islands. The legacy `pocket_islands` flag
                // still works as a fallback for pre-selection projects
                // (e.g. source = All) — there it pulls in *all* nested
                // closed children, matching the historical behavior.
                let mut islands: Vec<Vec<Point2>> = obj
                    .inner_objects
                    .iter()
                    .filter(|i| selected_set.contains(i))
                    .filter_map(|i| objects.get(*i))
                    .filter(|inner| inner.closed)
                    .map(|inner| segments_to_points(&inner.segments, 6))
                    .collect();
                if islands.is_empty() && op.params.pocket_islands {
                    islands = obj
                        .inner_objects
                        .iter()
                        .filter_map(|i| objects.get(*i))
                        .filter(|inner| inner.closed)
                        .map(|inner| segments_to_points(&inner.segments, 6))
                        .collect();
                }
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

fn op_includes_object(op: &Operation, obj: &VcObject, idx: usize) -> bool {
    match &op.source {
        OperationSource::All => true,
        OperationSource::Layers { layers } => layers.iter().any(|l| l == &obj.layer),
        // OperationSource::Objects ids are 1-based, matching the
        // ImportOutput.objects[i] mapping the frontend uses for
        // selection.
        OperationSource::Objects { ids } => {
            let chain_id = (idx as u32) + 1;
            ids.iter().any(|id| *id == chain_id)
        }
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

/// Header / footer Setup for the program. We synthesize it from the
/// first enabled op so machine.unit, mill.fast_move_z, tool.rate_h
/// pick up the user's actual values rather than struct defaults.
fn header_setup_for(project: &Project) -> Setup {
    let mut setup = Setup {
        machine: project.machine.clone(),
        ..Setup::default()
    };
    if let Some(op) = project.operations.iter().find(|o| o.enabled) {
        if let Some(tool) = project.tools.iter().find(|t| t.id == op.tool_id) {
            setup.tool = crate::cam::setup::ToolConfig {
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
        }
        setup.mill.fast_move_z = op.params.fast_move_z;
    } else if let Some(tool) = project.tools.first() {
        setup.tool = crate::cam::setup::ToolConfig {
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
    }
    setup
}

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
        Coolant, Operation, OperationKind, OperationParams, OperationSource, ToolEntry, ToolKind,
    };

    fn closed_square(side: f64) -> Vec<Segment> {
        vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(side, 0.0), "0", 7),
            Segment::line(Point2::new(side, 0.0), Point2::new(side, side), "0", 7),
            Segment::line(Point2::new(side, side), Point2::new(0.0, side), "0", 7),
            Segment::line(Point2::new(0.0, side), Point2::new(0.0, 0.0), "0", 7),
        ]
    }

    fn endmill(id: u32, diameter: f64) -> ToolEntry {
        ToolEntry {
            id,
            name: format!("{diameter:.1}mm endmill"),
            kind: ToolKind::Endmill,
            diameter,
            tip_diameter: None,
            dragoff: None,
            flutes: 2,
            speed: 18_000,
            plunge_rate: 100,
            feed_rate: 800,
            coolant: Coolant::Off,
        }
    }

    fn profile_op(id: u32, tool_id: u32, offset: ToolOffset) -> Operation {
        Operation {
            id,
            name: format!("Profile {id}"),
            enabled: true,
            kind: OperationKind::Profile { offset },
            tool_id,
            source: OperationSource::All,
            params: OperationParams::mill_default(),
        }
    }

    fn project_with(ops: Vec<Operation>, tools: Vec<ToolEntry>) -> Project {
        Project {
            segments: closed_square(20.0),
            machine: Default::default(),
            tools,
            operations: ops,
            tabs: Default::default(),
        }
    }

    #[test]
    fn run_pipeline_emits_a_recognizable_program() {
        let resp = run_pipeline(
            PipelineRequest {
                project: project_with(
                    vec![profile_op(1, 1, ToolOffset::Outside)],
                    vec![endmill(1, 3.0)],
                ),
                post_processor: Some(PostProcessorKind::Linuxcnc),
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
        assert!(resp.gcode.contains("; OP 1"));
        // Cut segments carry the op id; program-header rapids carry op_id=0.
        assert!(resp.toolpath.iter().any(|s| s.op_id == 1));
        assert!(
            resp.toolpath
                .iter()
                .filter(|s| s.op_id != 0)
                .all(|s| s.op_id == 1)
        );
    }

    #[test]
    fn run_pipeline_picks_grbl_when_requested() {
        let resp = run_pipeline(
            PipelineRequest {
                project: project_with(
                    vec![profile_op(1, 1, ToolOffset::Outside)],
                    vec![endmill(1, 3.0)],
                ),
                post_processor: Some(PostProcessorKind::Grbl),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(!resp.gcode.is_empty());
    }

    #[test]
    fn two_op_project_emits_two_distinct_op_blocks() {
        let project = project_with(
            vec![
                profile_op(1, 1, ToolOffset::Outside),
                profile_op(2, 1, ToolOffset::Outside),
            ],
            vec![endmill(1, 3.0)],
        );
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.gcode.contains("; OP 1"));
        assert!(resp.gcode.contains("; OP 2"));
        assert!(resp.toolpath.iter().any(|s| s.op_id == 1));
        assert!(resp.toolpath.iter().any(|s| s.op_id == 2));
    }

    #[test]
    fn progress_callback_fires_each_phase() {
        let phases = std::cell::RefCell::new(Vec::<String>::new());
        let _ = run_pipeline(
            PipelineRequest {
                project: project_with(
                    vec![profile_op(1, 1, ToolOffset::Outside)],
                    vec![endmill(1, 3.0)],
                ),
                post_processor: None,
            },
            |phase, _f, _m| phases.borrow_mut().push(phase.to_string()),
        )
        .unwrap();
        let phases = phases.into_inner();
        for expected in ["import", "objects", "gcode", "preview", "done"] {
            assert!(phases.contains(&expected.to_string()), "missing {expected} in {phases:?}");
        }
    }

    fn pocket_op(id: u32, tool_id: u32, source: OperationSource) -> Operation {
        Operation {
            id,
            name: format!("Pocket {id}"),
            enabled: true,
            kind: OperationKind::Pocket {
                strategy: crate::project::PocketStrategy::Cascade,
            },
            tool_id,
            source,
            params: OperationParams::mill_default(),
        }
    }

    fn closed_square_offset(side: f64, ox: f64, oy: f64) -> Vec<Segment> {
        vec![
            Segment::line(Point2::new(ox, oy), Point2::new(ox + side, oy), "0", 7),
            Segment::line(
                Point2::new(ox + side, oy),
                Point2::new(ox + side, oy + side),
                "0",
                7,
            ),
            Segment::line(
                Point2::new(ox + side, oy + side),
                Point2::new(ox, oy + side),
                "0",
                7,
            ),
            Segment::line(Point2::new(ox, oy + side), Point2::new(ox, oy), "0", 7),
        ]
    }

    /// Selecting an outer ring + inner ring as the source for a pocket op
    /// produces ONE annulus pocket (outer minus inner), not one pocket per
    /// ring. The bug was that the pipeline iterated each selected object
    /// independently, so the inner ring was getting machined as its own
    /// pocket boundary on top of the outer pocket.
    #[test]
    fn pocket_with_outer_plus_inner_selection_emits_a_single_annulus() {
        let mut segments = closed_square_offset(50.0, 0.0, 0.0);
        // Inner 20x20 box centered inside the outer 50x50.
        segments.extend(closed_square_offset(20.0, 15.0, 15.0));
        // Two distinct pocket projects, exact same geometry — one runs
        // pocket on JUST the outer (baseline), the other on outer+inner.
        // The annulus pocket should emit *fewer* offset segments than
        // pocketing the whole outer because the middle is left intact.
        let baseline_project = Project {
            segments: segments.clone(),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pocket_op(
                1,
                1,
                OperationSource::Objects { ids: vec![1] },
            )],
            tabs: Default::default(),
        };
        let annulus_project = Project {
            segments,
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pocket_op(
                1,
                1,
                OperationSource::Objects { ids: vec![1, 2] },
            )],
            tabs: Default::default(),
        };
        let baseline = run_pipeline(
            PipelineRequest {
                project: baseline_project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let annulus = run_pipeline(
            PipelineRequest {
                project: annulus_project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // Outer-only pocket fills the full 50x50; outer+inner leaves a
        // 20x20 hole, so its cut path must be strictly shorter.
        let cut_total = |toolpath: &[preview::ToolpathSegment]| -> f64 {
            toolpath
                .iter()
                .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
                .map(|s| {
                    let dx = s.to.x - s.from.x;
                    let dy = s.to.y - s.from.y;
                    (dx * dx + dy * dy).sqrt()
                })
                .sum()
        };
        let baseline_cut = cut_total(&baseline.toolpath);
        let annulus_cut = cut_total(&annulus.toolpath);
        assert!(
            annulus_cut < baseline_cut,
            "annulus cut length {annulus_cut} should be less than the full pocket {baseline_cut}",
        );
        // Also: the annulus should still emit at least one offset (the
        // outer pocket cascade with the inner ring as a hole). Zero would
        // mean we accidentally skipped both objects.
        assert!(
            annulus.stats.offset_count >= 1,
            "annulus pocket emitted no offsets",
        );
    }

    #[test]
    fn unknown_post_processor_is_a_deserialization_failure() {
        let raw = serde_json::json!({
            "project": {
                "segments": [],
                "machine": { "unit": "mm", "mode": "mill", "comments": true,
                             "arcs": true, "supports_toolchange": false },
                "tools": [],
                "operations": []
            },
            "post_processor": "robotic_arm"
        });
        let res: Result<PipelineRequest, _> = serde_json::from_value(raw);
        assert!(res.is_err());
    }
}
