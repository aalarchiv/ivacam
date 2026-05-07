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
use crate::cam::source_combine::{combine_source_regions, CombinedRegion};
use crate::cam::offsets::{
    apply_cut_direction, apply_overcut_to_offsets, attach_tabs_to_offsets,
    parallel_offset_object, pocket_for_object,
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
    Operation, OperationKind, OperationSource, PocketStrategy, Project, SourceCombine,
};

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
    /// Filled-area preview for Pocket ops: the actual region the cutter
    /// will machine, computed via the per-op SourceCombine mode (Auto by
    /// default — outer + inner = annulus). The frontend paints these as
    /// translucent fills so the user sees what they're cutting before
    /// reading the toolpath. Empty for non-Pocket ops.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<RegionPreview>,
    /// Non-fatal warnings raised during planning — mostly tool-fit
    /// problems (cutter doesn't fit the geometry, kind mismatch, …).
    /// The frontend surfaces these in the operations list status badge
    /// and a sidebar list; the gcode is still emitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PipelineWarning>,
}

/// One non-fatal warning attached to (optionally) a specific op.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PipelineWarning {
    /// Op the warning applies to. `None` means project-wide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<u32>,
    /// Stable identifier — frontend can branch on this to render an
    /// icon, link to docs, etc.
    pub kind: String,
    /// Human-readable description.
    pub message: String,
}

/// One filled region attached to a specific operation. `outer` is the
/// outer boundary; `holes` are the islands the cutter must avoid. Both
/// in project units (typically mm).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RegionPreview {
    pub op_id: u32,
    pub outer: Vec<Point2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<Vec<Point2>>,
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
    let mut warnings: Vec<PipelineWarning> = Vec::new();

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
                &mut warnings,
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
            &mut warnings,
        )?,
        PostProcessorKind::Hpgl => run_per_op(
            &project,
            &mut objects.clone(),
            &header_setup,
            &mut hpgl::Post::new(),
            &stats_collector,
            progress_ref,
            n_ops,
            &mut warnings,
        )?,
    };
    let (total_closed, total_offsets, _) = *stats_collector.borrow();

    progress("preview", 0.92, "interpreting toolpath");
    let (toolpath, gcode_index) = preview::interpret_with_index(&gcode);
    let regions = build_region_previews(&project, &objects);
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
        regions,
        warnings,
    })
}

/// Compute the filled-region preview for every enabled Pocket op. Auto
/// mode runs through the same containment-aware logic as the per-op
/// driver; explicit modes route through the clipper2 boolean ops in
/// cam::source_combine. Non-Pocket ops contribute nothing.
fn build_region_previews(project: &Project, objects: &[VcObject]) -> Vec<RegionPreview> {
    let mut out = Vec::new();
    for op in project.operations.iter().filter(|o| o.enabled) {
        if !matches!(op.kind, OperationKind::Pocket { .. }) {
            continue;
        }
        let selected = ordered_selection(op, objects);
        let mode = source_combine_mode(op);
        let regions = combine_source_regions(objects, &selected, mode);
        for r in regions {
            out.push(RegionPreview {
                op_id: op.id,
                outer: r.boundary,
                holes: r.holes,
            });
        }
    }
    out
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
    warnings: &mut Vec<PipelineWarning>,
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
            build_op_offsets(op, project, &mut objects.clone(), &setup, warnings)?;
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
    warnings: &mut Vec<PipelineWarning>,
) -> Result<(Vec<PolylineOffset>, usize), PipelineError> {
    // Up-front sanity checks that don't depend on whether the cascade
    // succeeds. push_tool_fit_kind_warnings populates `warnings` for
    // tool-kind / op-kind mismatches and impossible tool geometry.
    push_tool_fit_kind_warnings(op, project, setup, warnings);
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

    // Non-Auto combine modes (Union/Difference/Intersection/Xor/None) for
    // Pocket short-circuit the per-object loop: we materialize the
    // combined regions once via clipper2 and emit a pocket per region.
    // Other op kinds (Profile, Engrave, DragKnife) keep their per-object
    // semantics — they cut paths, not regions.
    if let OperationKind::Pocket { strategy } = op.kind {
        let combine = source_combine_mode(op);
        if !matches!(combine, SourceCombine::Auto) {
            // Preserve the user-specified selection order — Difference is
            // order-sensitive ("first minus the rest"), so we cannot iterate
            // a HashSet here. ordered_selection() walks op.source as the
            // user wrote it and returns the corresponding object indices.
            let selected = ordered_selection(op, objects);
            let regions = combine_source_regions(objects, &selected, combine);
            let zigzag = matches!(strategy, PocketStrategy::Zigzag);
            for region in &regions {
                if region.boundary.len() < 3 {
                    continue;
                }
                closed += 1;
                emitted_objects += 1;
                let synthetic = synthesize_region_object(region);
                for mut o in pocket_for_object(
                    &synthetic,
                    radius,
                    op.params.pocket_nocontour,
                    6,
                    zigzag,
                    &region.holes,
                ) {
                    o.source_object_idx = region.source_idx;
                    offsets.push(o);
                }
            }
            if !tabs_by_object.is_empty() {
                attach_tabs_to_offsets(&mut offsets, &tabs_by_object, setup.tool.diameter * 1.5);
            }
            if op.params.overcut {
                apply_overcut_to_offsets(&mut offsets, objects, setup.tool.diameter * 0.5);
            }
            apply_cut_direction(&mut offsets, op, false);
            push_tool_fit_size_warning(op, setup, closed, &offsets, warnings);
            return Ok((offsets, closed));
        }
    }

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
    apply_cut_direction(&mut offsets, op, false);
    push_tool_fit_size_warning(op, setup, closed, &offsets, warnings);
    Ok((offsets, closed))
}

/// Sanity warnings that don't depend on whether the offset cascade
/// succeeded. Run before the heavy work.
fn push_tool_fit_kind_warnings(
    op: &Operation,
    project: &Project,
    setup: &Setup,
    warnings: &mut Vec<PipelineWarning>,
) {
    use crate::project::ToolKind;
    let Some(tool) = project.tools.iter().find(|t| t.id == op.tool_id) else {
        return;
    };
    // Impossible tool geometry: tip diameter ≥ shank diameter.
    if let Some(tip) = tool.tip_diameter {
        if tip >= tool.diameter {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "tool_geometry_impossible".into(),
                message: format!(
                    "tool '{}': tip diameter {tip} ≥ shank diameter {}",
                    tool.name, tool.diameter
                ),
            });
        }
    }
    // Tool kind mismatched with op kind. We warn rather than error
    // because the gcode emitter still produces something usable in many
    // cases (a drag knife on a Profile is fine, for instance), but a
    // drill on a Pocket really doesn't make sense.
    let mismatch = match (&op.kind, tool.kind) {
        (OperationKind::Pocket { .. }, ToolKind::Drill) => Some("pocket op assigned a drill bit"),
        (OperationKind::Pocket { .. }, ToolKind::DragKnife) => {
            Some("pocket op assigned a drag knife (cut path won't carve area)")
        }
        (OperationKind::Profile { .. }, ToolKind::Drill) => Some("profile op assigned a drill bit"),
        _ => None,
    };
    if let Some(msg) = mismatch {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tool_kind_mismatch".into(),
            message: format!(
                "{msg} — '{}' on op '{}'. Pick a different tool kind.",
                tool.name, op.name
            ),
        });
    }
    let _ = setup; // reserved for future feed/speed sanity checks
}

/// Post-build warning: a closed boundary was supplied but the offset
/// cascade produced nothing — the tool diameter doesn't fit the
/// geometry (slot too narrow, pocket smaller than the tool, etc.).
fn push_tool_fit_size_warning(
    op: &Operation,
    setup: &Setup,
    closed_count: usize,
    offsets: &[PolylineOffset],
    warnings: &mut Vec<PipelineWarning>,
) {
    if closed_count == 0 {
        return; // nothing closed → not a tool-fit problem, just no work
    }
    // Profile-on / Engrave / DragKnife emit straight contour walks even
    // when offsets is empty in the cascade sense, so don't flag them.
    let needs_offset = match op.kind {
        OperationKind::Pocket { .. } => true,
        OperationKind::Profile {
            offset: crate::cam::setup::ToolOffset::Outside,
        }
        | OperationKind::Profile {
            offset: crate::cam::setup::ToolOffset::Inside,
        } => true,
        _ => false,
    };
    if !needs_offset {
        return;
    }
    if offsets.is_empty() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tool_too_large".into(),
            message: format!(
                "tool diameter {:.2} mm doesn't fit op '{}' — offset/cascade produced no toolpath. Try a smaller tool.",
                setup.tool.diameter, op.name,
            ),
        });
        return;
    }
    // Pocket-specific second pass: the boundary contour fits but the
    // cascade carved no inward rings → the cutter is wide enough to
    // reach the wall but not to chew out the interior. The user gets
    // a hollow pocket (just the wall trace), which can look like
    // "pocketing isn't working". Surface this so they can pick a
    // smaller tool. PolylineOffset.is_pocket == 0 is the boundary,
    // is_pocket >= 1 is a cascade ring or zigzag fill.
    if matches!(op.kind, OperationKind::Pocket { .. })
        && offsets.iter().any(|o| o.is_pocket == 0)
        && !offsets.iter().any(|o| o.is_pocket >= 1)
    {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "pocket_fill_incomplete".into(),
            message: format!(
                "tool diameter {:.2} mm fits the pocket boundary in op '{}' but not the interior — only the wall is cut, not the fill. Use a smaller tool to pocket the inside.",
                setup.tool.diameter, op.name,
            ),
        });
    }
}

/// Walk the op's source in user-specified order and return the matching
/// object indices. Used by non-Auto combine modes — Difference in
/// particular is order-sensitive ("first selected minus the rest"), so
/// we cannot iterate the unordered selected_set there.
fn ordered_selection(op: &Operation, objects: &[VcObject]) -> Vec<usize> {
    match &op.source {
        OperationSource::All => (0..objects.len()).collect(),
        OperationSource::Layers { layers, .. } => objects
            .iter()
            .enumerate()
            .filter(|(_, obj)| layers.iter().any(|l| l == &obj.layer))
            .map(|(i, _)| i)
            .collect(),
        OperationSource::Objects { ids, .. } => ids
            .iter()
            .filter_map(|id| {
                let idx = (*id as usize).checked_sub(1)?;
                objects.get(idx).map(|_| idx)
            })
            .collect(),
    }
}

/// Pull the SourceCombine mode out of an op's source. Defaults to Auto
/// when the source is `All` (no combine choice applies) or when no
/// combine field is set (back-compat for pre-p5o projects).
fn source_combine_mode(op: &Operation) -> SourceCombine {
    match &op.source {
        OperationSource::All => SourceCombine::Auto,
        OperationSource::Layers { combine, .. } | OperationSource::Objects { combine, .. } => {
            *combine
        }
    }
}

/// Build a synthetic VcObject from a CombinedRegion's boundary so it can
/// be fed into pocket_for_object (which is shaped around VcObjects). The
/// region's holes are passed alongside as islands; only the outer
/// boundary lives in this object.
fn synthesize_region_object(region: &CombinedRegion) -> VcObject {
    let pts = &region.boundary;
    let mut segments = Vec::with_capacity(pts.len());
    for win in pts.windows(2) {
        segments.push(Segment::line(win[0], win[1], region.layer.clone(), region.color));
    }
    if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
        if first.distance(*last) > 1e-6 {
            segments.push(Segment::line(*last, *first, region.layer.clone(), region.color));
        }
    }
    let mut obj = VcObject::new(segments, true);
    obj.layer = region.layer.clone();
    obj.color = region.color;
    obj
}

fn op_includes_object(op: &Operation, obj: &VcObject, idx: usize) -> bool {
    match &op.source {
        OperationSource::All => true,
        OperationSource::Layers { layers, .. } => layers.iter().any(|l| l == &obj.layer),
        // OperationSource::Objects ids are 1-based, matching the
        // ImportOutput.objects[i] mapping the frontend uses for
        // selection.
        OperationSource::Objects { ids, .. } => {
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
        plunge: op.params.plunge,
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
        Coolant, Operation, OperationKind, OperationParams, OperationSource, SourceCombine,
        ToolEntry, ToolKind,
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
                OperationSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Auto,
                },
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
                OperationSource::Objects {
                    ids: vec![1, 2],
                    combine: SourceCombine::Auto,
                },
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

    /// SourceCombine::Difference applied at the pipeline level should
    /// produce one annulus pocket from "outer minus inner", matching
    /// what the user means when they pick Difference explicitly. This
    /// guards the synthesize_region_object path that fakes a VcObject
    /// from clipper2 polytree output.
    #[test]
    fn pocket_with_difference_combine_emits_an_annulus() {
        let mut segments = closed_square_offset(50.0, 0.0, 0.0);
        segments.extend(closed_square_offset(20.0, 15.0, 15.0));
        let project = Project {
            segments,
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket-diff".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::Objects {
                    ids: vec![1, 2],
                    combine: SourceCombine::Difference,
                },
                params: OperationParams::mill_default(),
            }],
            tabs: Default::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.stats.offset_count >= 1, "Difference produced no offsets");
        // The cut path must include moves that are NOT in the inner box —
        // i.e., the cutter does visit points outside the inner 20x20.
        // A trivially-wrong implementation that pocketed only the inner
        // box (or only the outer) would fail one of these area checks.
        let visited_outside_inner = resp.toolpath.iter().any(|s| {
            let in_inner = |x: f64, y: f64| x > 15.0 && x < 35.0 && y > 15.0 && y < 35.0;
            !in_inner(s.from.x, s.from.y) || !in_inner(s.to.x, s.to.y)
        });
        let visited_inside_outer = resp.toolpath.iter().any(|s| {
            let in_outer = |x: f64, y: f64| x > 0.0 && x < 50.0 && y > 0.0 && y < 50.0;
            in_outer(s.from.x, s.from.y) && in_outer(s.to.x, s.to.y)
        });
        assert!(visited_outside_inner, "annulus pocket should reach outside the inner box");
        assert!(visited_inside_outer, "annulus pocket should stay inside the outer box");
    }

    /// Climb on the main + conventional on the finishing pass: walks the
    /// pipeline output and verifies the level=0 ring uses the
    /// conventional winding (CCW for an inner pocket boundary) while
    /// any level≥1 cascade ring uses climb (CW for an inner ring).
    #[test]
    fn pocket_with_climb_main_and_conventional_finish_winds_correctly() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let mut params = OperationParams::mill_default();
        params.cut_direction = crate::project::CutDirection::Climb;
        params.finish_cut_direction = crate::project::CutDirection::Conventional;
        let project = Project {
            segments,
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
            }],
            tabs: Default::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // We can't read PolylineOffset directly here (it isn't on the
        // PipelineResponse), but the toolpath order encodes the cut.
        // Walk the cut moves at op_id=1 and group them by Z-plane to
        // recover individual passes; then check the winding of each.
        let cuts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut) && s.op_id == 1)
            .collect();
        assert!(!cuts.is_empty(), "expected cut segments");
        // Group consecutive cuts that form a closed loop (same Z and the
        // final point is near the first). The first such loop is the
        // boundary (level=0) — we look at its signed area.
        let mut loops: Vec<Vec<&preview::ToolpathSegment>> = Vec::new();
        let mut cur: Vec<&preview::ToolpathSegment> = Vec::new();
        for s in &cuts {
            if cur.is_empty() {
                cur.push(s);
                continue;
            }
            let prev = cur.last().unwrap();
            // New loop when there's a Z jump or a position discontinuity.
            let z_jump = (s.from.z - prev.to.z).abs() > 1e-3;
            let xy_jump = (s.from.x - prev.to.x).hypot(s.from.y - prev.to.y) > 0.01;
            if z_jump || xy_jump {
                loops.push(std::mem::take(&mut cur));
            }
            cur.push(s);
        }
        if !cur.is_empty() {
            loops.push(cur);
        }
        let area_of_loop = |loop_segs: &[&preview::ToolpathSegment]| -> f64 {
            let mut s = 0.0;
            for seg in loop_segs {
                s += seg.from.x * seg.to.y - seg.to.x * seg.from.y;
            }
            s * 0.5
        };
        // The boundary pass = the loop with the largest |area| (it's the
        // outermost ring in the cascade). With Conventional + Pocket
        // (inner context) we expect CCW = positive signed area.
        // Group loops by Z so we look at one cut-pass plane only —
        // multiple Z passes would each repeat the same XY rings.
        let z_of = |loop_segs: &[&preview::ToolpathSegment]| -> f64 {
            loop_segs.first().map(|s| s.from.z).unwrap_or(0.0)
        };
        let first_z = z_of(&loops[0]);
        let same_z: Vec<_> = loops
            .iter()
            .filter(|l| (z_of(l) - first_z).abs() < 1e-3)
            .collect();
        let mut areas: Vec<f64> = same_z.iter().map(|l| area_of_loop(l)).collect();
        areas.sort_by(|a, b| b.abs().partial_cmp(&a.abs()).unwrap());
        let boundary_area = areas[0];
        assert!(
            boundary_area > 0.0,
            "finishing pass should be CCW (conventional) for an inner pocket; got area {boundary_area}"
        );
        // For a square boundary the cascade produces ≥ 1 inner ring on
        // a 50×50 pocket with a 3 mm tool; that ring should be CW =
        // negative signed area under climb.
        if areas.len() >= 2 {
            assert!(
                areas[1] < 0.0,
                "cascade ring should be CW (climb) for an inner pocket; got area {}",
                areas[1]
            );
        }
    }

    /// Pocket a 4mm box with a 6mm endmill — the cutter doesn't fit.
    /// Expect a `tool_too_large` warning attached to the op id, and the
    /// pipeline still completes (no error).
    #[test]
    fn pocket_with_oversized_tool_emits_tool_too_large_warning() {
        let project = Project {
            segments: closed_square_offset(4.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Operation {
                id: 7,
                name: "Tiny pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
            }],
            tabs: Default::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let too_large: Vec<_> = resp
            .warnings
            .iter()
            .filter(|w| w.kind == "tool_too_large")
            .collect();
        assert_eq!(too_large.len(), 1, "expected one tool_too_large warning, got {:?}", resp.warnings);
        assert_eq!(too_large[0].op_id, Some(7));
    }

    /// Drill bit on a Pocket op — emits a `tool_kind_mismatch` warning
    /// regardless of whether the cascade actually produced anything.
    #[test]
    fn pocket_with_drill_bit_warns_about_tool_kind() {
        let drill = ToolEntry {
            id: 1,
            name: "drill".into(),
            kind: ToolKind::Drill,
            diameter: 1.0,
            tip_diameter: None,
            dragoff: None,
            flutes: 2,
            speed: 18_000,
            plunge_rate: 100,
            feed_rate: 800,
            coolant: Coolant::Off,
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![drill],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
            }],
            tabs: Default::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.warnings.iter().any(|w| w.kind == "tool_kind_mismatch"));
    }

    /// Ramp plunge: the FIRST cut moves descend Z linearly while
    /// walking forward along the path. With angle=10° and step=-1,
    /// ramp_length = 1/tan(10°) ≈ 5.67mm. After ~5.67mm of XY travel
    /// the cutter should be at Z=-1; subsequent cut moves stay at -1.
    #[test]
    fn ramp_plunge_descends_z_during_first_cuts() {
        let mut params = OperationParams::mill_default();
        params.depth = -1.0;
        params.step = -1.0;
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Ramp { angle_deg: 10.0 };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Ramped profile".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
            }],
            tabs: Default::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // Walk the cut+arc moves at op_id=1. Either kind can carry the
        // descending Z during the ramp depending on whether the offset
        // polyline starts with a corner arc or a straight edge.
        let path: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| {
                s.op_id == 1
                    && matches!(
                        s.kind,
                        crate::gcode::preview::MoveKind::Cut | crate::gcode::preview::MoveKind::Arc
                    )
            })
            .collect();
        assert!(!path.is_empty(), "expected cut/arc moves");
        // The very first move's `from` is wherever the plunge left the
        // cutter — for ramp plunge that's start_depth (=0), not the
        // final cut depth.
        let first = path[0];
        assert!(
            first.from.z > -0.001,
            "ramp should start at Z≈0, got {} → {}",
            first.from.z,
            first.to.z
        );
        // Find where Z first reaches the cut depth.
        let mut horizontal_during_ramp = 0.0;
        let mut reached_depth = false;
        for s in &path {
            if !reached_depth {
                horizontal_during_ramp += (s.to.x - s.from.x).hypot(s.to.y - s.from.y);
            }
            if s.to.z <= -0.999 {
                reached_depth = true;
                break;
            }
        }
        assert!(reached_depth, "Z never reached cut depth during ramp");
        // Expected ramp length is 1 / tan(10°) ≈ 5.67mm. Allow ±25%:
        // the offset polyline may begin with a small corner arc that
        // can't be split mid-arc, which slightly extends the
        // descending portion.
        let expected = 1.0 / 10f64.to_radians().tan();
        assert!(
            (horizontal_during_ramp - expected).abs() / expected < 0.25,
            "horizontal ramp length should be ~{expected:.2}mm, got {horizontal_during_ramp:.2}",
        );
    }

    #[test]
    fn direct_plunge_keeps_default_behavior() {
        // Sanity-check that the new plunge field doesn't affect the
        // default Direct path: the first cut move must already be at
        // the cut depth (the plunge happens before XY travel starts).
        let mut params = OperationParams::mill_default();
        params.depth = -1.0;
        params.step = -1.0;
        // params.plunge defaults to Direct.
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Direct profile".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
            }],
            tabs: Default::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let first_cut = resp
            .toolpath
            .iter()
            .find(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut) && s.op_id == 1)
            .expect("expected at least one cut");
        assert!(
            first_cut.from.z <= -0.999,
            "direct plunge should reach cut depth before XY travel; first cut from.z = {}",
            first_cut.from.z
        );
    }

    /// A 10x10 pocket with a 6mm endmill: tool fits the boundary
    /// offset (4x4 left after a 3mm offset) but no cascade ring fits
    /// inside it → the cutter walks the wall and leaves a hollow
    /// rectangle. We surface this as a pocket_fill_incomplete warning
    /// so the user understands why the gcode is just the contour.
    #[test]
    fn pocket_with_just_fitting_tool_warns_about_incomplete_fill() {
        let project = Project {
            segments: closed_square_offset(10.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Operation {
                id: 9,
                name: "Hollow pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
            }],
            tabs: Default::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let incomplete: Vec<_> = resp
            .warnings
            .iter()
            .filter(|w| w.kind == "pocket_fill_incomplete")
            .collect();
        assert_eq!(
            incomplete.len(),
            1,
            "expected pocket_fill_incomplete warning, got {:?}",
            resp.warnings,
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
