//! Per-op offset cascade — the workhorse of the standard CAM pipeline.
//!
//! Takes a single [`Op`] plus its working [`VcObject`] set and
//! produces the list of [`PolylineOffset`]s the gcode emitter will walk.
//! Each op kind (Profile / Pocket / Drill / `DualTool` / Engrave /
//! `DragKnife` / Chamfer) carries its own branch inside
//! [`build_op_offsets`]; V-Carve / Halfpipe / Thread route through
//! dedicated drivers in [`super::op_drivers`].
//!
//! Extracted from `pipeline.rs` (audit 55o4) so the orchestrator
//! ([`super::run_pipeline_impl`]) and per-op driver
//! ([`super::run_per_op`]) read straight-through and the offset cascade
//! can grow new branches without bloating the top-level file.
//!
//! Side effects this pass owns:
//!   * Pattern expansion (`apply_pattern_to_*`)
//!   * Synthetic Pocket-Outside frame insertion
//!   * Containment-aware Pocket island selection
//!   * Tab attachment, overcut, cut-direction, approach-point rotation
//!   * Tool-fit warning emission

use std::collections::{HashMap, HashSet};

use crate::cam::offsets::{
    apply_cut_direction, apply_overcut_to_offsets, attach_tabs_to_offsets, parallel_offset_inward,
    parallel_offset_outward, pocket_for_object, small_circle_drill, PocketEmit, PolylineOffset,
    TabPoint,
};
use crate::cam::setup::{Setup, ToolOffset};
use crate::cam::source_combine::combine_source_regions;
use crate::cam::{segments_to_points, VcObject};
use crate::geometry::{Point2, Segment};
use crate::project::{Op, OpKind, OpSource, PocketStrategy, Project, SourceCombine};

use super::frame::synthesize_pocket_outside_objects;
use super::patterns::{apply_pattern_to_point, apply_pattern_to_segments, pattern_offsets};
use super::regions::synthesize_region_object;
use super::setup_resolver::dual_tool_finish_radius;
use super::tabs::build_op_tabs_by_object;
use super::warnings::{
    push_ramp_with_arcs_warning, push_tool_fit_kind_warnings, push_tool_fit_size_warning,
    push_trochoidal_warnings,
};
use super::{
    cancelled, op_includes_object, ordered_selection, source_combine_mode, CancelToken,
    PipelineError, PipelineWarning,
};

/// XY bbox-center helper used by the Drill branch when an object isn't
/// a point and isn't a small circle (i.e. an arbitrary closed shape).
/// Kept private to this module — the drill emitter is the only caller.
fn object_bbox_center(obj: &VcObject) -> Option<Point2> {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for s in &obj.segments {
        min_x = min_x.min(s.start.x).min(s.end.x);
        max_x = max_x.max(s.start.x).max(s.end.x);
        min_y = min_y.min(s.start.y).min(s.end.y);
        max_y = max_y.max(s.start.y).max(s.end.y);
    }
    if !min_x.is_finite() {
        return None;
    }
    Some(Point2::new((min_x + max_x) * 0.5, (min_y + max_y) * 0.5))
}

// The offset-cascade pass per op covers Profile / Pocket / Drill /
// DualTool / Engrave / DragKnife — splitting it would scatter the
// "compute pocket regions → apply offsets → attach tabs → cut order"
// pipeline across multiple files. 55o4 tracks future per-kind splits.
#[allow(clippy::too_many_lines)]
pub(super) fn build_op_offsets(
    op: &Op,
    project: &Project,
    objects: &mut Vec<VcObject>,
    setup: &Setup,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(Vec<PolylineOffset>, usize), PipelineError> {
    if cancelled(cancel) {
        return Err(PipelineError::Cancelled);
    }
    // Up-front sanity checks that don't depend on whether the cascade
    // succeeds. push_tool_fit_kind_warnings populates `warnings` for
    // tool-kind / op-kind mismatches and impossible tool geometry.
    push_tool_fit_kind_warnings(op, project, setup, warnings);
    push_trochoidal_warnings(op, warnings);
    push_ramp_with_arcs_warning(op, objects, warnings);
    // Per-op tab positions (rt1.10): the op's `tab_mode` +
    // `tab_placements` drive Manual / Auto / Mixed; Off ⇒ no tabs.
    // Resolves to a (object_idx → Vec<TabPoint>) map the existing
    // attach_tabs_to_offsets consumes verbatim.
    let mut tabs_by_object: HashMap<usize, Vec<TabPoint>> = build_op_tabs_by_object(op, objects);

    // Pattern repetition (5fz): when the op carries a PatternConfig, expand
    // the source set into N transformed clones BEFORE the per-object loops
    // run. After expansion, every clone is "selected" (so the inner loops
    // see them via OpSource::All on the effective op), and tabs
    // attached to the original objects are translated/rotated alongside
    // the geometry so each instance keeps its tab placement.
    let effective_op_storage: Option<Op> = if let Some(pattern) = op.pattern {
        let instances = pattern_offsets(pattern);
        let mut expanded: Vec<VcObject> = Vec::with_capacity(instances.len() * objects.len());
        let mut expanded_tabs: HashMap<usize, Vec<TabPoint>> = HashMap::new();
        for inst in &instances {
            for (idx, obj) in objects.iter().enumerate() {
                if !op_includes_object(op, obj, idx) {
                    continue;
                }
                let mut clone = obj.clone();
                apply_pattern_to_segments(&mut clone.segments, *inst);
                // Containment relationships index into the OLD object list,
                // which doesn't match the expanded set. Drop them; the
                // pocket-skipping logic relies on selected_set membership
                // which is recomputed below for the expanded set.
                clone.outer_objects.clear();
                clone.inner_objects.clear();
                let new_idx = expanded.len();
                if let Some(src_tabs) = tabs_by_object.get(&idx) {
                    let xformed: Vec<TabPoint> = src_tabs
                        .iter()
                        .map(|t| {
                            let p = apply_pattern_to_point(Point2::new(t.x, t.y), *inst);
                            TabPoint {
                                x: p.x,
                                y: p.y,
                                width_override_mm: None,
                                height_override_mm: None,
                            }
                        })
                        .collect();
                    expanded_tabs.insert(new_idx, xformed);
                }
                expanded.push(clone);
            }
        }
        *objects = expanded;
        tabs_by_object = expanded_tabs;
        let mut clone = op.clone();
        clone.source = OpSource::All;
        Some(clone)
    } else {
        None
    };
    // Pocket-Outside (rt1.3): when an op carries `frame_shape`, the
    // pipeline auto-prepends a synthetic frame VcObject derived from
    // the op's current selection and rewrites the op source to put the
    // frame's id FIRST, with SourceCombine::Difference. The frame is not
    // persisted on the project (no Frame_<n> layer) so there's nothing
    // stale to clean up — recomputed every generate from the op params.
    let frame_op_storage: Option<Op> = {
        let cur_op: &Op = effective_op_storage.as_ref().unwrap_or(op);
        if cur_op.params.frame_shape.is_some() {
            let tool_radius_mm = setup.tool.diameter * 0.5;
            let user_padding_mm = cur_op.params.frame_padding_mm.unwrap_or(0.0).max(0.0);
            if user_padding_mm < tool_radius_mm {
                warnings.push(PipelineWarning {
                    op_id: Some(cur_op.id),
                    kind: "frame_padding_below_tool_radius".into(),
                    message: format!(
                        "Frame padding {user:.3} mm is below the cutter radius {radius:.3} mm \
                         and was bumped to {radius:.3} mm so the cutter stays outside the \
                         selection. Set padding above the tool diameter ({diam:.3} mm) to \
                         actually carve material outside the shape.",
                        user = user_padding_mm,
                        radius = tool_radius_mm,
                        diam = setup.tool.diameter,
                    ),
                });
            }
            if let Some((new_objects, ordered_indices)) =
                synthesize_pocket_outside_objects(cur_op, objects, tool_radius_mm)
            {
                // Replace the working vec with the frame-augmented set.
                *objects = new_objects;
                let ordered_ids: Vec<u32> =
                    ordered_indices.iter().map(|&i| (i as u32) + 1).collect();
                let mut clone = cur_op.clone();
                clone.source = OpSource::Objects {
                    ids: ordered_ids,
                    combine: SourceCombine::Difference,
                };
                Some(clone)
            } else {
                None
            }
        } else {
            None
        }
    };
    let effective_op: &Op = frame_op_storage
        .as_ref()
        .or(effective_op_storage.as_ref())
        .unwrap_or(op);

    // Apply per-op tool-offset to the chain so order_offsets / lead-in see it.
    for obj in objects.iter_mut() {
        obj.tool_offset = setup.mill.offset;
    }

    let radius = setup.tool.diameter * 0.5;
    // Lateral step between consecutive Pocket cuts. Default 0.5
    // overlap = step is half the tool diameter (≈ tool radius). The
    // explicit param lets the user dial it tighter for cleaner fill or
    // looser for faster cuts. Clamp to a sane envelope so a stray 1.0
    // (= no advance) doesn't loop forever and a stray 0 doesn't pin to
    // the lower bound forever either.
    let overlap = if effective_op.params.xy_overlap > 0.0 {
        effective_op.params.xy_overlap.clamp(0.05, 0.95)
    } else {
        0.5
    };
    let mut xy_step = setup.tool.diameter * (1.0 - overlap);
    // Wirbeln (rt1.25): when the tool is flagged for automatic
    // chip-thinning, clamp the cascade step so radial engagement
    // stays bounded — half the tool radius (= tool_radius / 2) is the
    // classic chip-thinning rule. Pocket ops only; other op kinds
    // already control their own stepover. The user can override via
    // `ToolEntry.wirbeln_stepover_mm`.
    if matches!(effective_op.kind, OpKind::Pocket { .. }) {
        if let Some(tool) = project.tools.iter().find(|t| t.id == effective_op.tool_id) {
            if tool.wirbeln {
                let half_r = (tool.diameter * 0.5) * 0.5;
                let cap = tool
                    .wirbeln_stepover_mm
                    .filter(|v| *v > 0.0)
                    .unwrap_or(half_r);
                if cap > 0.0 && cap < xy_step {
                    xy_step = cap;
                }
            }
        }
    }
    let xy_step = xy_step;
    let mut offsets: Vec<PolylineOffset> = Vec::new();
    let mut closed = 0usize;
    let mut emitted_objects = 0usize;

    // Containment-aware Pocket: when the user selects an outer ring and
    // an inner ring, the inner one should become a hole in the outer
    // pocket — not a top-level pocket boundary on its own. Compute the
    // selected-object set up front so the Pocket branch can consult it
    // while iterating.
    let selected_set: HashSet<usize> = (0..objects.len())
        .filter(|i| op_includes_object(effective_op, &objects[*i], *i))
        .collect();

    // Non-Auto combine modes (Union/Difference/Intersection/Xor/None) for
    // Pocket short-circuit the per-object loop: we materialize the
    // combined regions once via clipper2 and emit a pocket per region.
    // Other op kinds (Profile, Engrave, DragKnife) keep their per-object
    // semantics — they cut paths, not regions.
    if let OpKind::Pocket { strategy } = effective_op.kind {
        let combine = source_combine_mode(effective_op);
        if !matches!(combine, SourceCombine::Auto) {
            // Preserve the user-specified selection order — Difference is
            // order-sensitive ("first minus the rest"), so we cannot iterate
            // a HashSet here. ordered_selection() walks op.source as the
            // user wrote it and returns the corresponding object indices.
            let selected = ordered_selection(effective_op, objects);
            let regions = combine_source_regions(objects, &selected, combine);
            let pocket_emit = pocket_emit_for(strategy, effective_op);
            for region in &regions {
                if cancelled(cancel) {
                    return Err(PipelineError::Cancelled);
                }
                if region.boundary.len() < 3 {
                    continue;
                }
                closed += 1;
                emitted_objects += 1;
                let synthetic = synthesize_region_object(region);
                let finish_ring_r = dual_tool_finish_radius(effective_op, project);
                for mut o in pocket_for_object(
                    &synthetic,
                    radius,
                    effective_op.params.pocket_nocontour,
                    6,
                    pocket_emit,
                    &region.holes,
                    xy_step,
                    effective_op.params.finish_xy_allowance_mm.unwrap_or(0.0),
                    finish_ring_r,
                ) {
                    o.source_object_idx = region.source_idx;
                    offsets.push(o);
                }
            }
            if !tabs_by_object.is_empty() {
                attach_tabs_to_offsets(&mut offsets, &tabs_by_object, setup.tool.diameter * 1.5);
            }
            if effective_op.params.overcut {
                apply_overcut_to_offsets(&mut offsets, objects, setup.tool.diameter * 0.5);
            }
            apply_cut_direction(&mut offsets, effective_op, false);
            if let Some(ap) = effective_op.params.approach_point {
                crate::cam::offsets::rotate_offsets_to_approach_point(&mut offsets, ap);
            }
            push_tool_fit_size_warning(effective_op, setup, closed, &offsets, warnings);
            return Ok((offsets, closed));
        }
    }

    for (idx, obj) in objects.iter().enumerate() {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        if !op_includes_object(effective_op, obj, idx) {
            continue;
        }
        emitted_objects += 1;
        if obj.closed {
            closed += 1;
        }

        match effective_op.kind {
            OpKind::Pocket { strategy } => {
                // Skip objects that are geometrically inside another
                // selected object — they belong to that pocket as islands.
                let contained_by_selected =
                    obj.outer_objects.iter().any(|o| selected_set.contains(o));
                if contained_by_selected {
                    continue;
                }
                let pocket_emit = pocket_emit_for(strategy, effective_op);
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
                // Legacy auto-island fallback: when `pocket_islands` is on
                // and the explicit selection didn't pick any inners, fall
                // back to the pre-selection behavior of treating ALL
                // geometrically-nested closed children as islands. ONLY
                // valid for source = All — under source = Layers /
                // Objects the user has explicitly stated which geometry
                // is in scope, so silently auto-including unselected
                // inners contradicts the selection. Pre-fix this caused
                // a strategy split: cascade/spiral milled around the
                // unselected circles (they ran with the auto-filled
                // island list) while zigzag ignored islands entirely
                // (its code path didn't take an islands argument). The
                // user expectation under "Selected" mode is that ONLY
                // what's selected matters — match that here for every
                // pocket strategy.
                if islands.is_empty()
                    && effective_op.params.pocket_islands
                    && matches!(effective_op.source, OpSource::All)
                {
                    islands = obj
                        .inner_objects
                        .iter()
                        .filter_map(|i| objects.get(*i))
                        .filter(|inner| inner.closed)
                        .map(|inner| segments_to_points(&inner.segments, 6))
                        .collect();
                }
                if obj.closed {
                    let finish_ring_r = dual_tool_finish_radius(effective_op, project);
                    for mut o in pocket_for_object(
                        obj,
                        radius,
                        effective_op.params.pocket_nocontour,
                        6,
                        pocket_emit,
                        &islands,
                        xy_step,
                        effective_op.params.finish_xy_allowance_mm.unwrap_or(0.0),
                        finish_ring_r,
                    ) {
                        o.source_object_idx = idx;
                        offsets.push(o);
                    }
                }
            }
            OpKind::Profile { .. } => {
                // Sign-correct offsets: parallel_offset_inward / outward
                // pick the cavalier delta sign based on the polygon's
                // signed area, so a CW input doesn't put the cutter on
                // the wrong side.
                let new_offsets = match setup.mill.offset {
                    ToolOffset::None | ToolOffset::On => {
                        vec![PolylineOffset {
                            segments: obj.segments.clone(),
                            closed: obj.closed,
                            level: 0,
                            is_pocket: 0,
                            layer: obj.layer.clone(),
                            color: obj.color,
                            source_object_idx: idx,
                            tabs: Vec::new(),
                            is_finish: false,
                        }]
                    }
                    ToolOffset::Outside => parallel_offset_outward(obj, radius),
                    ToolOffset::Inside => parallel_offset_inward(obj, radius),
                };
                for mut o in new_offsets {
                    o.source_object_idx = idx;
                    offsets.push(o);
                }
            }
            OpKind::Engrave | OpKind::DragKnife => {
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
                    is_finish: false,
                });
            }
            OpKind::Drill { .. } => {
                // Drill picks a single XY for each selected object:
                //   - A single POINT segment → the point itself.
                //   - A closed CIRCLE smaller than tool_radius → center
                //     of the circle (small_circle_drill, fast path that
                //     skips offset cascade).
                //   - Any other closed object → bbox center of its
                //     segments. This is what users expect for "drill
                //     this object" on a rectangle or arbitrary closed
                //     shape — the tool moves to the object's midpoint,
                //     plunges, retracts.
                // Open polylines are skipped — drilling along a stroke
                // has no sensible interpretation; the
                // tool_kind_mismatch warning surfaces the misuse.
                use crate::geometry::SegmentKind;
                if obj.segments.len() == 1 && matches!(obj.segments[0].kind, SegmentKind::Point) {
                    let seg = obj.segments[0].clone();
                    offsets.push(PolylineOffset {
                        segments: vec![seg],
                        closed: false,
                        level: 0,
                        is_pocket: 0,
                        layer: obj.layer.clone(),
                        color: obj.color,
                        source_object_idx: idx,
                        tabs: Vec::new(),
                        is_finish: false,
                    });
                } else if let Some(mut drill) = small_circle_drill(obj, radius) {
                    drill.source_object_idx = idx;
                    offsets.push(drill);
                } else if obj.closed {
                    if let Some(center) = object_bbox_center(obj) {
                        let pt = Segment::point(center, &obj.layer, obj.color);
                        offsets.push(PolylineOffset {
                            segments: vec![pt],
                            closed: false,
                            level: 0,
                            is_pocket: 0,
                            layer: obj.layer.clone(),
                            color: obj.color,
                            source_object_idx: idx,
                            tabs: Vec::new(),
                            is_finish: false,
                        });
                    }
                }
            }
            OpKind::Chamfer { finish_pass, .. } => {
                // Chamfer (rt1.18): the V-bit walks the source path
                // verbatim — no XY offset — and the depth comes from
                // the bit's cone math computed at synth time. The
                // first offset is the rough cut; if finish_pass is
                // on, emit a second offset tagged is_finish so the
                // tool's finish-set rates kick in.
                offsets.push(PolylineOffset {
                    segments: obj.segments.clone(),
                    closed: obj.closed,
                    level: 0,
                    is_pocket: 0,
                    layer: obj.layer.clone(),
                    color: obj.color,
                    source_object_idx: idx,
                    tabs: Vec::new(),
                    is_finish: false,
                });
                if finish_pass {
                    offsets.push(PolylineOffset {
                        segments: obj.segments.clone(),
                        closed: obj.closed,
                        level: 0,
                        is_pocket: 0,
                        layer: obj.layer.clone(),
                        color: obj.color,
                        source_object_idx: idx,
                        tabs: Vec::new(),
                        is_finish: true,
                    });
                }
            }
            OpKind::Thread { .. } => {
                // Thread runs through `run_thread_op` from the per-op
                // driver, not the offset-cascade emitter — skip
                // silently here so a stray dispatch doesn't crash.
            }
            OpKind::Helix => {
                return Err(PipelineError::UnimplementedKind(effective_op.kind));
            }
            OpKind::VCarve => {
                // V-Carve runs through `run_vcarve_op` from the per-op
                // driver; it should never reach this offset-cascade
                // path. Skip silently rather than erroring so a stray
                // call here doesn't crash the program — the dedicated
                // dispatcher already produced the toolpath.
            }
        }
    }
    let _ = emitted_objects;

    if !tabs_by_object.is_empty() {
        attach_tabs_to_offsets(&mut offsets, &tabs_by_object, setup.tool.diameter * 1.5);
    }
    if effective_op.params.overcut {
        apply_overcut_to_offsets(&mut offsets, objects, setup.tool.diameter * 0.5);
    }
    apply_cut_direction(&mut offsets, effective_op, false);
    if let Some(ap) = effective_op.params.approach_point {
        crate::cam::offsets::rotate_offsets_to_approach_point(&mut offsets, ap);
    }
    push_tool_fit_size_warning(effective_op, setup, closed, &offsets, warnings);
    Ok((offsets, closed))
}

/// Map a frontend pocket strategy choice onto the offsets-layer
/// emitter, including the trochoidal-specific climb/conventional and
/// loop parameters.
fn pocket_emit_for(strategy: PocketStrategy, op: &Op) -> PocketEmit {
    match strategy {
        PocketStrategy::Zigzag => PocketEmit::Zigzag,
        PocketStrategy::Spiral => PocketEmit::Spiral,
        PocketStrategy::Cascade => PocketEmit::Cascade,
        PocketStrategy::Trochoidal {
            engagement_angle_deg,
            loop_radius_factor,
        } => PocketEmit::Trochoidal {
            engagement_angle_deg,
            loop_radius_factor,
            climb: matches!(op.params.cut_direction, crate::project::CutDirection::Climb),
        },
        // Halfpipe ops never reach this codepath — they're routed
        // through run_halfpipe_op before build_op_offsets runs. Fall
        // back to Cascade for defense in depth.
        PocketStrategy::Halfpipe { .. } => PocketEmit::Cascade,
    }
}
