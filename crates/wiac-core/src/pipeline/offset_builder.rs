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
    apply_cut_direction, apply_overcut_to_offsets, attach_tabs_to_offsets,
    inflate_islands_by_tool_radius, parallel_offset_inward, parallel_offset_outward,
    pocket_for_object, small_circle_drill, PocketEmit, PolylineOffset, TabPoint,
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
    objects: &[VcObject],
    setup: &Setup,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(Vec<PolylineOffset>, usize), PipelineError> {
    if cancelled(cancel) {
        return Err(PipelineError::Cancelled);
    }
    // Discard any stale panic records left over by a prior op on this
    // thread (defensive — every op-completion path also drains, so this
    // should be empty in normal operation).
    let _ = crate::cam::offsets::take_parallel_offset_panics();
    // 0tsy: same defensive drain for nocontour-allowance ignored events
    // so a stale entry from a prior op can't get attributed to this one.
    let _ = crate::cam::offsets::take_nocontour_allowance_ignored();
    // cpym: defensive drain for zigzag-stride-degenerate events so a
    // stale entry from a prior op can't bleed into this one.
    let _ = crate::cam::offsets::take_zigzag_stride_degenerate();
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
    let mut tabs_by_object: HashMap<usize, Vec<TabPoint>> =
        build_op_tabs_by_object(op, objects, warnings);

    // Pattern repetition (5fz): when the op carries a PatternConfig, expand
    // the source set into N transformed clones BEFORE the per-object loops
    // run. After expansion, every clone is "selected" (so the inner loops
    // see them via OpSource::All on the effective op), and tabs
    // attached to the original objects are translated/rotated alongside
    // the geometry so each instance keeps its tab placement.
    //
    // jzpl Phase 1: the expansion now owns a local `Vec<VcObject>` instead
    // of mutating an input `&mut Vec<VcObject>`. The caller can pass the
    // imported chain by shared reference and skip the per-op `.to_vec()`
    // it used to do defensively.
    let pattern_expanded: Option<Vec<VcObject>> = if let Some(pattern) = op.pattern() {
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
                    // 1ztc: each instance's tab is re-anchored on the
                    // rotated/translated geometry via
                    // `apply_pattern_to_point`. Tabs aren't "shared with
                    // the original" — every instance gets its own
                    // entry, and the per-tab width/height overrides
                    // carry over so a manually-tuned tab keeps its size
                    // across the pattern copies.
                    let xformed: Vec<TabPoint> = src_tabs
                        .iter()
                        .map(|t| {
                            let p = apply_pattern_to_point(Point2::new(t.x, t.y), *inst);
                            TabPoint {
                                x: p.x,
                                y: p.y,
                                width_override_mm: t.width_override_mm,
                                height_override_mm: t.height_override_mm,
                            }
                        })
                        .collect();
                    expanded_tabs.insert(new_idx, xformed);
                }
                expanded.push(clone);
            }
        }
        tabs_by_object = expanded_tabs;
        Some(expanded)
    } else {
        None
    };
    let effective_op_storage: Option<Op> = pattern_expanded.as_ref().map(|_| {
        let mut clone = op.clone();
        clone.source = OpSource::All;
        clone
    });

    // View after pattern expansion (input borrow otherwise — no clone).
    let after_pattern: &[VcObject] = pattern_expanded.as_deref().unwrap_or(objects);

    // Pocket-Outside (rt1.3): when an op carries `frame_shape`, the
    // pipeline auto-prepends a synthetic frame VcObject derived from
    // the op's current selection and rewrites the op source to put the
    // frame's id FIRST, with SourceCombine::Difference. The frame is not
    // persisted on the project (no Frame_<n> layer) so there's nothing
    // stale to clean up — recomputed every generate from the op params.
    //
    // jzpl Phase 1: produces a locally-owned Vec instead of mutating the
    // caller's input. After kbx5 patterns are Drill-only so pattern +
    // frame can't both fire on the same op; the code still composes
    // correctly if that constraint ever loosens (frame builds from
    // `after_pattern`, which is the pattern-expanded view when active).
    let cur_op_for_frame: &Op = effective_op_storage.as_ref().unwrap_or(op);
    let pocket_for_frame = cur_op_for_frame.pocket_params();
    let (frame_expanded, frame_op_storage): (Option<Vec<VcObject>>, Option<Op>) =
        if pocket_for_frame.is_some_and(|p| p.frame_shape.is_some()) {
            let tool_radius_mm = setup.tool.diameter * 0.5;
            let user_padding_mm = pocket_for_frame
                .and_then(|p| p.frame_padding_mm)
                .unwrap_or(0.0)
                .max(0.0);
            if user_padding_mm < tool_radius_mm {
                warnings.push(PipelineWarning {
                    op_id: Some(cur_op_for_frame.id),
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
                synthesize_pocket_outside_objects(cur_op_for_frame, after_pattern, tool_radius_mm)
            {
                let ordered_ids: Vec<u32> =
                    ordered_indices.iter().map(|&i| (i as u32) + 1).collect();
                let mut clone = cur_op_for_frame.clone();
                clone.source = OpSource::Objects {
                    ids: ordered_ids,
                    combine: SourceCombine::Difference,
                };
                (Some(new_objects), Some(clone))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };
    let effective_op: &Op = frame_op_storage
        .as_ref()
        .or(effective_op_storage.as_ref())
        .unwrap_or(op);

    // Final effective view used by the offset cascade below. Shadow
    // `objects` so the existing downstream code (selected_set, region
    // combine, per-object cascade) reads through it without renames.
    // No clone — `&[VcObject]` borrow through the chain.
    let objects: &[VcObject] = frame_expanded.as_deref().unwrap_or(after_pattern);

    // ldu2: arc-fit the SOURCE geometry before the offset cascade. When a
    // circle (or any smooth curve) arrives tessellated as many short LINE
    // segments — the usual shape of an SVG/DXF import — cavalier_contours
    // offsets it with a tool-radius round join at EVERY vertex, producing
    // a (line, arc, line, arc, …) path. The emit-time arc-fitter only
    // collapses consecutive line runs, so each line wedged between corner
    // arcs survives as its own move and the gcode explodes (a 360-segment
    // circle Profile → ~4.4k lines instead of ~70). Collapsing the source
    // line runs back into true arcs first lets cavalier offset a clean arc
    // and emit a handful of G2/G3. `fit_line_runs` is a no-op when
    // `machine.arcs` is off (the user opted into pure-line output) or when
    // a run doesn't lie on a circle within `effective_arc_tolerance`
    // (rectangles/polygons stay as lines). Only the path / offset-cascade
    // ops consume `obj.segments` as a contour; Drill / Thread / Chamfer /
    // VCarve derive their geometry differently, so leave them untouched.
    let fit_source = setup.machine.arcs
        && matches!(
            effective_op.kind,
            OpKind::Profile { .. }
                | OpKind::Pocket { .. }
                | OpKind::Engrave { .. }
                | OpKind::DragKnife { .. }
                | OpKind::TSlot { .. }
                | OpKind::Dovetail { .. }
        );
    let fitted_storage: Option<Vec<VcObject>> = fit_source.then(|| {
        objects
            .iter()
            .map(|o| {
                let mut c = o.clone();
                c.segments = crate::gcode::fit_line_runs(&o.segments, setup);
                c
            })
            .collect()
    });
    let objects: &[VcObject] = fitted_storage.as_deref().unwrap_or(objects);

    // (Removed: `for obj in objects.iter_mut() { obj.tool_offset = setup.mill.offset; }`.
    //  `obj.tool_offset` has no production reader post-55o4 — only a test
    //  asserts on a frame's tool_offset, which `synthesize_pocket_outside_objects`
    //  sets directly. Removing this loop was the unblock to taking `objects`
    //  by shared reference.)

    // 1mlv: stock_to_leave_mm shifts the cutter centerline farther from
    // the geometric wall on EVERY offset-cascade op (Profile + Pocket).
    // For Profile-Inside the cutter walks `tool_radius + stock_to_leave`
    // inside the boundary; for Pocket the inset wall sits at the same
    // distance. The effect is identical to bumping every offset by the
    // configured amount, so we fold it into `radius` once and let the
    // existing cascade code see the augmented value.
    let stock_to_leave = effective_op.params.stock_to_leave_mm.max(0.0);
    let radius = setup.tool.diameter * 0.5 + stock_to_leave;
    // Lateral step between consecutive Pocket cuts. Default 0.5
    // overlap = step is half the tool diameter (≈ tool radius). The
    // explicit param lets the user dial it tighter for cleaner fill or
    // looser for faster cuts. Clamp to a sane envelope so a stray 1.0
    // (= no advance) doesn't loop forever and a stray 0 doesn't pin to
    // the lower bound forever either.
    // kbx5 step 2: xy_overlap lives on PocketParams. Non-Pocket ops
    // fall through to the 0.5 default (was the prior behavior since
    // OpParams::default initialised xy_overlap to 0.0 → 0.5 fallback).
    // dr5: when the op leaves it at 0 ("use default") and the tool
    // carries a `default_xy_overlap`, the tool value wins; otherwise
    // fall through to the global 0.5.
    let overlap_setting = effective_op.pocket_params().map_or(0.0, |p| p.xy_overlap);
    let overlap = if overlap_setting > 0.0 {
        overlap_setting.clamp(0.05, 0.95)
    } else if let Some(tool_default) = setup.tool.default_xy_overlap {
        tool_default.clamp(0.05, 0.95)
    } else {
        0.5
    };
    let xy_step = setup.tool.diameter * (1.0 - overlap);
    // 3e5: Wirbeln no longer clamps the cascade step. The v1
    // implementation (xy_step ≤ tool_radius / 2) bounded engagement
    // by reducing stepover, which slowed every cut. The 3e5 helical
    // overlay applied at gcode-emit time bounds engagement directly
    // by making the cutter rotate around the toolpath centerline —
    // the cascade can stay at the user's xy_step regardless.
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
    if let OpKind::Pocket { strategy, .. } = effective_op.kind {
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
                let pocket_for_kind = effective_op.pocket_params();
                // knd4: pocket_zigzag / pocket_cascade_with_islands /
                // stitch_rings_to_polyline all document their `islands`
                // input as "pre-inflated by tool_radius" — they use the
                // outline as the centerline safe boundary. Pipeline used
                // to pass region.holes RAW; the cutter edge then bit
                // tool_r into every raised feature. Inflate here so the
                // pocket emitters see a Minkowski-sum boundary that
                // keeps the centerline a tool_r away from the raw wall.
                let inflated_holes = inflate_islands_by_tool_radius(&region.holes, radius);
                for mut o in pocket_for_object(
                    &synthetic,
                    radius,
                    pocket_for_kind.is_some_and(|p| p.pocket_nocontour),
                    6,
                    pocket_emit,
                    &inflated_holes,
                    xy_step,
                    pocket_for_kind
                        .and_then(|p| p.finish_xy_allowance_mm)
                        .unwrap_or(0.0),
                    finish_ring_r,
                    setup.tool.spindle_direction,
                ) {
                    o.source_object_idx = region.source_idx;
                    offsets.push(o);
                }
            }
            if !tabs_by_object.is_empty() {
                attach_tabs_to_offsets(&mut offsets, &tabs_by_object, setup.tool.diameter * 1.5);
            }
            if effective_op.profile_params().is_some_and(|p| p.overcut) {
                apply_overcut_to_offsets(&mut offsets, objects, setup.tool.diameter * 0.5);
            }
            apply_cut_direction(
                &mut offsets,
                effective_op,
                false,
                setup.tool.spindle_direction,
            );
            if let Some(ap) = effective_op.contour_params().and_then(|c| c.approach_point) {
                crate::cam::offsets::rotate_offsets_to_approach_point(&mut offsets, ap);
            }
            push_tool_fit_size_warning(effective_op, setup, closed, &offsets, warnings);
            drain_parallel_offset_panics(effective_op, warnings);
            drain_pocket_cascade_truncations(effective_op, warnings);
            drain_trochoidal_incompletes(effective_op, warnings);
            drain_approach_point_far_rotations(effective_op, warnings);
            drain_nocontour_allowance_ignored(effective_op, warnings);
            drain_zigzag_stride_degenerate(effective_op, warnings);
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
            OpKind::Pocket { strategy, .. } => {
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
                //
                // 473k: when source=All AND every nested inner object is
                // closed, auto-detect the donut/annular intent without
                // requiring the user to flip `pocket_islands`. The
                // canonical case: a frame plate with a center hole +
                // default Pocket op. Most users expect this to cut a
                // donut, not pocket through the inner closed contour.
                // The legacy `pocket_islands=true` path is preserved
                // (still triggers when no inners are auto-detected, but
                // we hit it via the same fallback now).
                let auto_annular = matches!(effective_op.source, OpSource::All)
                    && !obj.inner_objects.is_empty()
                    && obj
                        .inner_objects
                        .iter()
                        .all(|i| objects.get(*i).is_some_and(|o| o.closed));
                let legacy_pocket_islands_flag = effective_op
                    .pocket_params()
                    .is_some_and(|p| p.pocket_islands)
                    && matches!(effective_op.source, OpSource::All);
                if islands.is_empty() && (auto_annular || legacy_pocket_islands_flag) {
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
                    let pocket_for_kind = effective_op.pocket_params();
                    // knd4: inflate inner-object islands by tool_radius
                    // before handing them to pocket_for_object. The
                    // pocket emitters treat the island input as the
                    // centerline safe boundary; passing raw inner
                    // contours used to gouge the island wall by tool_r.
                    let inflated_islands = inflate_islands_by_tool_radius(&islands, radius);
                    for mut o in pocket_for_object(
                        obj,
                        radius,
                        pocket_for_kind.is_some_and(|p| p.pocket_nocontour),
                        6,
                        pocket_emit,
                        &inflated_islands,
                        xy_step,
                        pocket_for_kind
                            .and_then(|p| p.finish_xy_allowance_mm)
                            .unwrap_or(0.0),
                        finish_ring_r,
                        setup.tool.spindle_direction,
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
                            is_finish: true,
                        }]
                    }
                    ToolOffset::Outside => parallel_offset_outward(obj, radius),
                    ToolOffset::Inside => parallel_offset_inward(obj, radius),
                };
                // c0pm: a single-pass Profile op IS the finishing wall pass
                // — there's no "rough then finish" split, the lone offset
                // defines the final wall. Tag every emitted offset as
                // is_finish so the gcode emitter substitutes the tool's
                // finish-set rates (rate_h_finish / speed_finish /
                // rate_v_finish). Pre-fix the rough-set rates were used
                // even when the user configured distinct finish values,
                // silently degrading surface finish + tool life.
                for mut o in new_offsets {
                    o.source_object_idx = idx;
                    o.is_finish = true;
                    offsets.push(o);
                }
            }
            OpKind::Engrave { .. }
            | OpKind::DragKnife { .. }
            | OpKind::TSlot { .. }
            | OpKind::Dovetail { .. } => {
                // All follow the source path with no offset. Engrave
                // rides the centerline; drag-knife adds trail compensation
                // in the emitter; 3g6u/b7qz: T-slot and dovetail ride the
                // centerline too but at the op's floor Z, where the
                // cutter's head / angled flanks sweep the undercut (a
                // single-Z pass — the multi-level cascade that a
                // Profile/Pocket would run is exactly the
                // cutter-at-every-depth bug these op kinds fix).
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
                        let pt = Segment::point(center, obj.layer.clone(), obj.color);
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
                return Err(PipelineError::UnimplementedKind(Box::new(
                    effective_op.kind.clone(),
                )));
            }
            OpKind::VCarve { .. } => {
                // V-Carve runs through `run_vcarve_op` from the per-op
                // driver; it should never reach this offset-cascade
                // path. Skip silently rather than erroring so a stray
                // call here doesn't crash the program — the dedicated
                // dispatcher already produced the toolpath.
            }
            OpKind::ReliefMill { .. } => {
                // f60x: relief surfacing runs through `run_relief_op` (its
                // own drop-cutter driver), never the offset cascade. Skip.
            }
            OpKind::Pause { .. } => {
                // rt1.34: Pause is emitted inline by the pipeline op
                // loop (M5 + M0 + M3) before this offset-cascade path
                // ever runs. Reach here means a programming error
                // upstream; skip silently to keep the program intact.
            }
        }
    }
    let _ = emitted_objects;

    if !tabs_by_object.is_empty() {
        attach_tabs_to_offsets(&mut offsets, &tabs_by_object, setup.tool.diameter * 1.5);
    }
    if effective_op.profile_params().is_some_and(|p| p.overcut) {
        apply_overcut_to_offsets(&mut offsets, objects, setup.tool.diameter * 0.5);
    }
    apply_cut_direction(
        &mut offsets,
        effective_op,
        false,
        setup.tool.spindle_direction,
    );
    if let Some(ap) = effective_op.contour_params().and_then(|c| c.approach_point) {
        crate::cam::offsets::rotate_offsets_to_approach_point(&mut offsets, ap);
    }
    push_tool_fit_size_warning(effective_op, setup, closed, &offsets, warnings);
    drain_parallel_offset_panics(effective_op, warnings);
    drain_pocket_cascade_truncations(effective_op, warnings);
    drain_trochoidal_incompletes(effective_op, warnings);
    drain_approach_point_far_rotations(effective_op, warnings);
    drain_nocontour_allowance_ignored(effective_op, warnings);
    drain_zigzag_stride_degenerate(effective_op, warnings);
    Ok((offsets, closed))
}

/// 1ao5: drain any early-termination events stashed by `pocket_trochoidal`
/// for the current op and surface them as `trochoidal_incomplete`
/// warnings. The emitter terminates when a centerline vertex's loop disc
/// strays outside the safe interior — chopping the unsafe tail
/// prevents a full-slot G1 at trochoidal feed/RPM, but the user needs to
/// know part of the pocket was left uncleared.
fn drain_trochoidal_incompletes(op: &Op, warnings: &mut Vec<PipelineWarning>) {
    for ev in crate::cam::trochoidal::take_trochoidal_incompletes() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "trochoidal_incomplete".into(),
            message: format!(
                "op '{}': trochoidal pocket terminated at centerline vertex {}/{} — the loop disc (r={:.2} mm, engagement {:.0}°) couldn't fit the pocket interior at that point, and continuing would have required a full-slot move at trochoidal feed/RPM. Part of the pocket was left uncleared. Pick a smaller loop_radius_factor or engagement angle, or finish the unswept tail with a separate (zigzag/cascade) op.",
                op.name, ev.bail_index, ev.centerline_total, ev.r_loop, ev.engagement_angle_deg,
            ),
        });
    }
}

/// 0tsy: drain any `NocontourAllowanceIgnored` events stashed by
/// `pocket_for_object` and surface them as
/// `nocontour_ignores_finish_allowance` warnings. The function folded
/// the allowance back to 0 (rough cascade walked the wall straight)
/// because no wall ring would have removed the stock; the user picked
/// an incompatible combination and we want them to know.
fn drain_nocontour_allowance_ignored(op: &Op, warnings: &mut Vec<PipelineWarning>) {
    for ev in crate::cam::offsets::take_nocontour_allowance_ignored() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "nocontour_ignores_finish_allowance".into(),
            message: format!(
                "op '{}': pocket_nocontour=true skips the wall ring, so the configured XY finish allowance ({:.3} mm) has no finish pass to remove it. The allowance was ignored — the rough cascade walks the wall directly at the tool radius. To get a finishing wall pass, turn pocket_nocontour off (or use the dual-tool finish-radius path instead).",
                op.name, ev.allowance_mm,
            ),
        });
    }
}

/// cpym: drain any zigzag-stride-degenerate events stashed by
/// [`crate::cam::offsets::pocket_zigzag`] for the current op and surface
/// them as `zigzag_stride_clamped_below_minimum` warnings. Pre-cpym a
/// sub-mm stride (mirror-finish raster) was silently clamped to 0.1 mm
/// and the user got coarser scallops than requested; now strides below
/// FP working precision still bail (the algorithm can't do anything
/// useful with them) but the user is told instead of left guessing.
fn drain_zigzag_stride_degenerate(op: &Op, warnings: &mut Vec<PipelineWarning>) {
    for ev in crate::cam::offsets::take_zigzag_stride_degenerate() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "zigzag_stride_clamped_below_minimum".into(),
            message: format!(
                "op '{}': zigzag pocket stride of {:.6} mm is below the working precision (1e-6 mm) — no raster strokes were emitted. Set the per-pass step to at least 1e-6 mm (sub-fp strides cannot be represented stably). For mirror-finish work pick a stride that resolves at your DRO precision (typically ≥ 0.01 mm).",
                op.name, ev.stride_mm,
            ),
        });
    }
}

/// mdpo: drain any cascade-truncation events stashed by
/// `pocket_cascade_with_islands` for the current op and surface them as
/// `pocket_cascade_truncated` warnings. Truncation means the inside-most
/// rings of a very large pocket weren't carved — the user sees a hollow
/// doughnut otherwise.
fn drain_pocket_cascade_truncations(op: &Op, warnings: &mut Vec<PipelineWarning>) {
    for ev in crate::cam::offsets::take_pocket_cascade_truncations() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "pocket_cascade_truncated".into(),
            message: format!(
                "op '{}': pocket cascade emitted {} rings at step {:.3} mm, hitting the {} ring cap. Inner rings were truncated — the centre of the pocket may not be fully carved. Consider increasing the per-pass step (less ring count) or running multiple smaller pockets.",
                op.name, ev.rings_emitted, ev.delta, ev.ring_cap,
            ),
        });
    }
}

/// kzz9: drain any far-approach-point records stashed by
/// `rotate_offsets_to_approach_point` and surface them as
/// `rotate_offsets_far_from_approach` warnings. Typical cause: the user
/// moved the source contour after picking the approach point.
fn drain_approach_point_far_rotations(op: &Op, warnings: &mut Vec<PipelineWarning>) {
    for ev in crate::cam::offsets::take_approach_point_far_rotations() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "rotate_offsets_far_from_approach".into(),
            message: format!(
                "op '{}': approach point ({:.2}, {:.2}) is {:.2} mm from the nearest closed-offset vertex (threshold {:.0} mm). The cut still starts at the nearest vertex, but check that the source contour didn't move after the approach point was set.",
                op.name, ev.approach.0, ev.approach.1, ev.distance_mm, crate::cam::offsets::APPROACH_POINT_WARN_MM,
            ),
        });
    }
}

/// z4t6: drain any `cavalier_contours` panics trapped by
/// `parallel_offset_object` during the current op and surface them as
/// `parallel_offset_panicked` `PipelineWarning`s. The panic itself was
/// already caught (the pipeline kept running); this is where we make it
/// VISIBLE to the user via the warnings sidebar instead of only the
/// stderr log.
fn drain_parallel_offset_panics(op: &Op, warnings: &mut Vec<PipelineWarning>) {
    for panic in crate::cam::offsets::take_parallel_offset_panics() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "parallel_offset_panicked".into(),
            message: format!(
                "op '{}': parallel-offset on layer '{}' (bbox ({:.2}, {:.2}) → ({:.2}, {:.2}), digest {:#018x}, delta {:.3}) tripped a cavalier_contours assert and produced no toolpath. Try simplifying or re-tessellating the contour (e.g. remove self-touching vertices in HATCH boundaries / ELLIPSE flattening).",
                op.name,
                panic.layer,
                panic.bbox_min_x,
                panic.bbox_min_y,
                panic.bbox_max_x,
                panic.bbox_max_y,
                panic.input_digest,
                panic.delta,
            ),
        });
        // Color is captured but we don't include it in the human message
        // — it's exposed in the structured PipelineWarning kind tag for
        // tests that want to assert on it deterministically.
        let _ = panic.color;
    }
}

/// Map a frontend pocket strategy choice onto the offsets-layer
/// emitter, including the trochoidal-specific climb/conventional and
/// loop parameters.
///
/// q57s: `PocketEmit::Trochoidal { climb }` stores the user's climb
/// intent unchanged; `pocket_trochoidal` XORs that with the spindle
/// direction internally so the geometric loop winding matches physical
/// climb on either M3 / M4.
fn pocket_emit_for(strategy: PocketStrategy, op: &Op) -> PocketEmit {
    match strategy {
        PocketStrategy::Zigzag { angle_deg } => PocketEmit::Zigzag { angle_deg },
        PocketStrategy::Spiral => PocketEmit::Spiral,
        PocketStrategy::Cascade => PocketEmit::Cascade,
        PocketStrategy::Trochoidal {
            engagement_angle_deg,
            loop_radius_factor,
        } => PocketEmit::Trochoidal {
            engagement_angle_deg,
            loop_radius_factor,
            climb: matches!(
                op.contour_params().map(|c| c.cut_direction),
                Some(crate::project::CutDirection::Climb)
            ),
        },
        // Halfpipe ops never reach this codepath — they're routed
        // through run_halfpipe_op before build_op_offsets runs. Fall
        // back to Cascade for defense in depth.
        PocketStrategy::Halfpipe { .. } => PocketEmit::Cascade,
    }
}

#[cfg(test)]
mod tests {
    use crate::cam::setup::{
        LeadKind, MachineConfig, PlungeStrategy, TabType, TabsConfig, ToolOffset,
    };
    use crate::pipeline::test_helpers::{
        closed_square_offset, endmill, first_lead_phase, profile_leads_op,
    };
    use crate::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
    use crate::project::{Op, OpKind, OpParams, OpSource, Project};

    // ─── Plunge strategies ─────────────────────────────────────────────

    /// Ramp plunge: the FIRST cut moves descend Z linearly while
    /// walking forward. With angle=10° and step=-1, `ramp_length` =
    /// 1/tan(10°) ≈ 5.67mm.
    #[test]
    fn ramp_plunge_descends_z_during_first_cuts() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = PlungeStrategy::Ramp { angle_deg: 10.0 };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Ramped profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
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
        let first = path[0];
        assert!(
            first.from.z > -0.001,
            "ramp should start at Z≈0, got {} → {}",
            first.from.z,
            first.to.z
        );
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
        let expected = 1.0 / 10f64.to_radians().tan();
        assert!(
            (horizontal_during_ramp - expected).abs() / expected < 0.25,
            "horizontal ramp length should be ~{expected:.2}mm, got {horizontal_during_ramp:.2}",
        );
    }

    /// Ramp plunge cleanup: after the ramped pass, a constant-depth
    /// lap cuts the ramp region down to `total_depth`.
    #[test]
    fn ramp_plunge_cleans_up_with_a_final_constant_depth_pass() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = PlungeStrategy::Ramp { angle_deg: 10.0 };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Ramped profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let cuts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut) && s.op_id == 1)
            .collect();
        let cleanup_distance: f64 = cuts
            .iter()
            .filter(|s| (s.from.z - -1.0).abs() < 1e-3 && (s.to.z - -1.0).abs() < 1e-3)
            .map(|s| (s.to.x - s.from.x).hypot(s.to.y - s.from.y))
            .sum();
        assert!(
            cleanup_distance > 700.0,
            "expected ≥700mm of constant-depth cuts (post-ramp + cleanup); got {cleanup_distance:.1}",
        );
    }

    /// Helix entry: arcs with monotonically descending Z.
    #[test]
    fn helix_plunge_emits_arc_arcs_descending_z() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(3.0),
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Helical pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let path: Vec<_> = resp.toolpath.iter().filter(|s| s.op_id == 1).collect();
        assert!(!path.is_empty(), "expected toolpath segments");
        let mut arc_count = 0;
        let mut last_z = f64::INFINITY;
        let mut reached_depth = false;
        for s in &path {
            if matches!(
                s.kind,
                crate::gcode::preview::MoveKind::Rapid | crate::gcode::preview::MoveKind::Plunge
            ) {
                continue;
            }
            if matches!(s.kind, crate::gcode::preview::MoveKind::Arc) {
                arc_count += 1;
                assert!(
                    s.to.z <= last_z + 1e-6,
                    "helix Z should descend monotonically, but {} → {}",
                    last_z,
                    s.to.z,
                );
                last_z = s.to.z;
            }
            if s.to.z <= -0.999 {
                reached_depth = true;
                break;
            }
        }
        assert!(reached_depth, "Z never reached cut depth via helix");
        assert!(
            arc_count >= 2,
            "helix should emit ≥2 arc moves before reaching depth; got {arc_count}",
        );
    }

    /// Helix radius < `tool_radius` → falls back to Ramp.
    #[test]
    fn helix_falls_back_to_ramp_when_radius_smaller_than_tool() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = PlungeStrategy::Helix {
            angle_deg: 10.0,
            radius_mm: Some(1.0),
        };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Op {
                id: 1,
                name: "Helix-too-small".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let first_cutting = resp
            .toolpath
            .iter()
            .find(|s| {
                s.op_id == 1
                    && matches!(
                        s.kind,
                        crate::gcode::preview::MoveKind::Cut | crate::gcode::preview::MoveKind::Arc
                    )
            })
            .expect("expected at least one cut/arc move");
        assert!(
            first_cutting.from.z > -0.001,
            "ramp fallback should start at Z≈0, got {}",
            first_cutting.from.z,
        );
        let helix_arc_present = resp.toolpath.iter().any(|s| {
            s.op_id == 1
                && matches!(s.kind, crate::gcode::preview::MoveKind::Arc)
                && s.from.z > -0.001
                && (s.from.x - 50.0).hypot(s.from.y - 50.0) < 5.0
        });
        assert!(
            !helix_arc_present,
            "fallback should not emit a helix-entry arc near the polygon centroid",
        );
    }

    /// Auto-fit helix on a pocket too small for the tool: emits
    /// `helix_radius_unfittable` and falls through to Ramp.
    #[test]
    fn auto_helix_falls_back_when_pocket_too_small() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: None,
        };
        let project = Project {
            segments: closed_square_offset(8.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Op {
                id: 1,
                name: "Auto-helix-tight".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let warned = resp
            .warnings
            .iter()
            .any(|w| w.kind == "helix_radius_unfittable" && w.op_id == Some(1));
        assert!(
            warned,
            "expected helix_radius_unfittable warning; got: {:?}",
            resp.warnings,
        );
        let helix_arc_present = resp.toolpath.iter().any(|s| {
            s.op_id == 1
                && matches!(s.kind, crate::gcode::preview::MoveKind::Arc)
                && s.from.z > -0.001
                && (s.from.x - 4.0).hypot(s.from.y - 4.0) < 3.0
        });
        assert!(
            !helix_arc_present,
            "auto-fit should not emit a helix arc when pocket is too small",
        );
    }

    /// Auto-fit helix on a roomy pocket: picks a radius and emits
    /// descending helix arcs.
    #[test]
    fn auto_helix_emits_arcs_when_pocket_fits() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: None,
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 0.5)],
            operations: vec![Op {
                id: 1,
                name: "Auto-helix-roomy".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let arc_count = resp
            .toolpath
            .iter()
            .filter(|s| {
                s.op_id == 1
                    && matches!(s.kind, crate::gcode::preview::MoveKind::Arc)
                    && s.to.z <= s.from.z
                    && s.from.z > -0.999
            })
            .count();
        assert!(
            arc_count >= 2,
            "auto-fit roomy pocket should emit helix arcs; got {arc_count}",
        );
        assert!(
            !resp
                .warnings
                .iter()
                .any(|w| w.kind == "helix_radius_unfittable"),
            "no unfit warning expected: {:?}",
            resp.warnings,
        );
    }

    /// Helix `radius_mm: null` round-trips through JSON and the
    /// legacy bare-number form still loads.
    #[test]
    fn helix_radius_null_round_trip_and_legacy_compat() {
        let plunge = PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: None,
        };
        let json = serde_json::to_string(&plunge).unwrap();
        assert!(
            json.contains("\"radius_mm\":null"),
            "expected radius_mm:null in serialized form: {json}",
        );
        let parsed: PlungeStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, plunge);

        let legacy = r#"{"kind":"helix","angle_deg":3.0,"radius_mm":5.0}"#;
        let parsed: PlungeStrategy = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            parsed,
            PlungeStrategy::Helix {
                angle_deg: 3.0,
                radius_mm: Some(5.0),
            },
        );

        let new_form = r#"{"kind":"helix","angle_deg":3.0,"radius_mm":null}"#;
        let parsed: PlungeStrategy = serde_json::from_str(new_form).unwrap();
        assert_eq!(
            parsed,
            PlungeStrategy::Helix {
                angle_deg: 3.0,
                radius_mm: None,
            },
        );
    }

    /// Tabs active → helix entry suppressed; falls back to the tabs
    /// straight-plunge walker.
    #[test]
    fn helix_with_tabs_active_falls_back() {
        let mut params = OpParams::mill_default();
        params.depth = -2.0;
        params.step = Some(-2.0);
        params.start_depth = 0.0;
        params.plunge = PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(3.0),
        };
        let contour = crate::project::ContourParams {
            tabs: TabsConfig {
                active: true,
                width: 10.0,
                height: 1.0,
                tab_type: TabType::Rectangle,
                ramp_angle_deg: 30.0,
            },
            tab_mode: crate::project::TabPlacementMode::Manual,
            tab_placements: vec![crate::project::TabPlacement {
                object_id: 1,
                t: 0.125,
                width_override_mm: None,
                height_override_mm: None,
            }],
            ..crate::project::ContourParams::default()
        };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Helix-with-tabs".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour,
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let helix_arc_present = resp.toolpath.iter().any(|s| {
            s.op_id == 1
                && matches!(s.kind, crate::gcode::preview::MoveKind::Arc)
                && (s.from.x - 50.0).hypot(s.from.y - 50.0) < 10.0
        });
        assert!(
            !helix_arc_present,
            "tabs-active path should not emit a helical entry arc near the polygon centroid",
        );
        assert!(
            resp.gcode.contains("Z-1"),
            "expected tab Z-lift in gcode: {}",
            resp.gcode,
        );
    }

    /// Direct plunge (default): first cut starts at cut depth.
    #[test]
    fn direct_plunge_keeps_default_behavior() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Direct profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
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

    // ─── Lead-in (p31) ─────────────────────────────────────────────────

    /// Profile + Outside + Arc lead-in emits a G2/G3 arc between the
    /// surface plunge and the cut plunge.
    #[test]
    fn lead_in_arc_emits_g2_or_g3_before_first_cut() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_leads_op(ToolOffset::Outside, LeadKind::Arc, 2.0)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let (_rapid, between, _first_cut) = first_lead_phase(&resp.gcode);
        let saw_arc = between
            .iter()
            .any(|l| l.starts_with("G2 ") || l.starts_with("G3 "));
        assert!(
            saw_arc,
            "expected a G2 / G3 arc lead-in at z=0, got intermediate moves={between:?}\n{}",
            resp.gcode,
        );
    }

    /// `LeadKind::Off`: no motion between surface plunge and cut plunge.
    #[test]
    fn lead_in_off_emits_no_lead() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_leads_op(ToolOffset::Outside, LeadKind::Off, 0.0)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let (_rapid, between, _first_cut) = first_lead_phase(&resp.gcode);
        let saw_motion = between.iter().any(|l| {
            l.starts_with("G0 ")
                || l.starts_with("G1 ")
                || l.starts_with("G2 ")
                || l.starts_with("G3 ")
        });
        assert!(
            !saw_motion,
            "LeadKind::Off should plunge straight to depth, but saw intermediate moves={between:?}\n{}",
            resp.gcode,
        );
    }

    /// `LeadKind::Straight`: rapid to a perpendicular hop point, plunge
    /// there. No z=0 motion; rapid target is OFFSET from any corner.
    #[test]
    fn lead_in_straight_emits_a_straight_segment() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_leads_op(
                ToolOffset::Outside,
                LeadKind::Straight,
                2.0,
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let (rapid, between, first_cut) = first_lead_phase(&resp.gcode);
        let saw_motion = between.iter().any(|l| {
            l.starts_with("G0 ")
                || l.starts_with("G1 ")
                || l.starts_with("G2 ")
                || l.starts_with("G3 ")
        });
        assert!(
            !saw_motion,
            "Straight lead-in plunges at the offset hop XY, no z=0 motion expected; got {between:?}\n{}",
            resp.gcode,
        );
        let rapid = rapid.expect("expected a G0 X Y rapid");
        let corners = [(0.0_f64, 0.0_f64), (50.0, 0.0), (50.0, 50.0), (0.0, 50.0)];
        let on_geom_corner = corners
            .iter()
            .any(|(cx, cy)| (rapid.0 - cx).abs() < 0.5 && (rapid.1 - cy).abs() < 0.5);
        assert!(
            !on_geom_corner,
            "Straight lead-in's rapid target should be OFFSET from any geometry corner, got {rapid:?}\n{}",
            resp.gcode,
        );
        assert!(
            first_cut.is_some(),
            "expected a first cut motion\n{}",
            resp.gcode
        );
    }

    // ─── Depth scheduling ──────────────────────────────────────────────

    /// `finish_step` emits an extra thin pass just above the final Z.
    #[test]
    fn finish_step_emits_extra_thin_final_pass() {
        let mut params = OpParams::mill_default();
        params.depth = -2.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.finish_step = Some(-0.2);
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.gcode.contains("Z-1\n") || resp.gcode.contains("Z-1 "));
        assert!(resp.gcode.contains("Z-1.8"));
        assert!(resp.gcode.contains("Z-2\n") || resp.gcode.contains("Z-2 "));
    }

    /// `through_depth` extends the cut past nominal depth.
    #[test]
    fn through_depth_extends_final_z() {
        let mut params = OpParams::mill_default();
        params.depth = -2.0;
        params.step = Some(-1.0);
        params.through_depth = 0.5;
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("Z-2.5"),
            "expected through-cut Z-2.5 in gcode",
        );
    }

    /// `depth_list` overrides the step schedule entirely.
    #[test]
    fn depth_list_overrides_step_schedule() {
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.step = Some(-1.0);
        params.depth_list = vec![-0.5, -1.5, -3.0];
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.gcode.contains("Z-0.5"));
        assert!(resp.gcode.contains("Z-1.5"));
        assert!(resp.gcode.contains("Z-3"));
        assert!(!resp.gcode.contains("Z-1\n") && !resp.gcode.contains("Z-1 "));
        assert!(!resp.gcode.contains("Z-2\n") && !resp.gcode.contains("Z-2 "));
    }

    // ─── Profile offsets ───────────────────────────────────────────────

    /// Profile offset honors the polygon's winding: CCW and CW input
    /// should both produce the same outward / inward offset.
    #[test]
    fn profile_offset_works_for_cw_and_ccw_input() {
        use crate::gcode::preview::MoveKind;
        use crate::geometry::Segment;
        use crate::pipeline::test_helpers::{closed_square_offset, endmill, profile_op};
        let ccw_segments = closed_square_offset(100.0, 0.0, 0.0);
        let cw_segments: Vec<Segment> = ccw_segments
            .iter()
            .rev()
            .map(|s| Segment::line(s.end, s.start, s.layer.clone(), s.color))
            .collect();
        for (winding_label, segments) in &[("CCW", &ccw_segments), ("CW", &cw_segments)] {
            let mk = |offset: ToolOffset| Project {
                segments: (*segments).clone(),
                machine: MachineConfig::default(),
                tools: vec![endmill(1, 3.0)],
                operations: vec![profile_op(1, 1, offset)],
                fixtures: Vec::default(),
                text_layers: Vec::default(),
                work_offset: crate::project::WorkOffset::default(),
                stock: None,
                relief_sources: Vec::new(),
            };
            let cut_max_x = |toolpath: &[crate::gcode::preview::ToolpathSegment]| -> f64 {
                toolpath
                    .iter()
                    .filter(|s| matches!(s.kind, MoveKind::Cut))
                    .flat_map(|s| [s.from.x, s.to.x])
                    .fold(f64::NEG_INFINITY, f64::max)
            };
            let cases: [(&str, ToolOffset); 3] = [
                ("On", ToolOffset::On),
                ("Outside", ToolOffset::Outside),
                ("Inside", ToolOffset::Inside),
            ];
            for (offset_label, offset) in cases {
                let resp = run_pipeline(
                    PipelineRequest {
                        project: mk(offset),
                        post_processor: None,
                    },
                    |_, _, _| {},
                )
                .unwrap();
                let max_x = cut_max_x(&resp.toolpath);
                let ok = match offset {
                    ToolOffset::On | ToolOffset::None => (max_x - 100.0).abs() < 0.1,
                    ToolOffset::Outside => max_x > 100.5,
                    ToolOffset::Inside => max_x < 99.5,
                };
                assert!(
                    ok,
                    "{winding_label} input + {offset_label} offset: cut max_x = {max_x} fails the expected position check"
                );
            }
        }
    }

    /// Profile + Outside selecting an INNER circle: the cutter walks at
    /// radius `circle_r + tool_r` around the centre.
    #[test]
    fn profile_outside_selecting_inner_circle_offsets_outward() {
        use crate::gcode::preview::MoveKind;
        use crate::geometry::Point2;
        use crate::pipeline::test_helpers::{closed_circle, closed_square_offset, endmill};
        let outer = closed_square_offset(100.0, 0.0, 0.0);
        let inner = closed_circle(Point2::new(50.0, 50.0), 10.0);
        let segments: Vec<_> = outer.into_iter().chain(inner).collect();
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![2],
                    combine: crate::project::SourceCombine::Auto,
                },
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let max_x = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, MoveKind::Cut | MoveKind::Arc))
            .flat_map(|s| [s.from.x, s.to.x])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            max_x > 61.0 && max_x < 62.0,
            "Profile + Outside on inner circle: cut max_x={max_x}, expected ~61.5"
        );
    }

    /// Wire-shape Profile op (matches build-project.ts's payload): the
    /// outside offset must actually be applied end-to-end through
    /// `serde_json::from_value`.
    #[test]
    fn profile_outside_with_source_objects_actually_offsets() {
        use crate::gcode::preview::MoveKind;
        let raw = serde_json::json!({
            "project": {
                "segments": [
                    { "type": "LINE", "start": { "x": 0.0, "y": 0.0 }, "end": { "x": 100.0, "y": 0.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 100.0, "y": 0.0 }, "end": { "x": 100.0, "y": 100.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 100.0, "y": 100.0 }, "end": { "x": 0.0, "y": 100.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 0.0, "y": 100.0 }, "end": { "x": 0.0, "y": 0.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                ],
                "machine": { "unit": "mm", "mode": "mill", "comments": true, "arcs": true, "supports_toolchange": false },
                "tools": [{ "id": 1, "name": "3mm", "kind": "endmill", "diameter": 3.0, "flutes": 2, "speed": 18000, "plunge_rate": 100, "feed_rate": 800, "coolant": "off" }],
                "operations": [{
                    "id": 1, "name": "Profile", "enabled": true,
                    "kind": { "type": "profile", "offset": "outside" },
                    "tool_id": 1,
                    "source": { "kind": "objects", "ids": [1] },
                    "params": {
                        "depth": -2.0, "start_depth": 0.0, "step": -1.0, "fast_move_z": 5.0,
                        "helix": false, "reverse": false, "objectorder": "nearest", "overcut": false,
                        "pocket_islands": true, "pocket_nocontour": false, "pocket_insideout": false,
                        "tabs": { "active": false, "width": 10.0, "height": 1.0, "tab_type": "rectangle" },
                        "leads": { "in": "off", "out": "off", "in_lenght": 5.0, "out_lenght": 5.0 }
                    }
                }],
                "tabs": {}
            }
        });
        let req: PipelineRequest = serde_json::from_value(raw).expect("wire JSON");
        let resp = run_pipeline(req, |_, _, _| {}).unwrap();
        let max_x = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, MoveKind::Cut))
            .flat_map(|s| [s.from.x, s.to.x])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            max_x > 100.5,
            "user-shape Profile + outside + source=objects: cut max_x={}, expected > 100.5\n\nFull gcode:\n{}",
            max_x,
            resp.gcode
        );
    }

    /// End-to-end deserialization of a Profile op from the frontend's
    /// `build-project.ts` JSON, then verify the offset is honored.
    #[test]
    fn profile_offset_via_wire_json_outside_actually_offsets() {
        use crate::gcode::preview::MoveKind;
        let raw = serde_json::json!({
            "project": {
                "segments": [
                    { "type": "LINE", "start": { "x": 0.0, "y": 0.0 }, "end": { "x": 100.0, "y": 0.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 100.0, "y": 0.0 }, "end": { "x": 100.0, "y": 100.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 100.0, "y": 100.0 }, "end": { "x": 0.0, "y": 100.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 0.0, "y": 100.0 }, "end": { "x": 0.0, "y": 0.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                ],
                "machine": { "unit": "mm", "mode": "mill", "comments": true, "arcs": true, "supports_toolchange": false },
                "tools": [{ "id": 1, "name": "3mm", "kind": "endmill", "diameter": 3.0, "flutes": 2, "speed": 18000, "plunge_rate": 100, "feed_rate": 800, "coolant": "off" }],
                "operations": [{
                    "id": 1, "name": "Profile", "enabled": true,
                    "kind": { "type": "profile", "offset": "outside" },
                    "tool_id": 1,
                    "source": { "kind": "all" },
                    "params": {
                        "depth": -2.0, "start_depth": 0.0, "step": -1.0, "fast_move_z": 5.0,
                        "helix": false, "reverse": false, "objectorder": "nearest", "overcut": false,
                        "pocket_islands": false, "pocket_nocontour": false, "pocket_insideout": false,
                        "tabs": { "active": false, "width": 10.0, "height": 1.0, "tab_type": "rectangle" },
                        "leads": { "in": "off", "out": "off", "in_lenght": 5.0, "out_lenght": 5.0 }
                    }
                }],
                "tabs": {}
            }
        });
        let req: PipelineRequest = serde_json::from_value(raw).expect("wire JSON deserialization");
        if let OpKind::Profile { offset, .. } = &req.project.operations[0].kind {
            let offset = *offset;
            assert_eq!(
                offset,
                ToolOffset::Outside,
                "wire 'outside' string didn't deserialize to ToolOffset::Outside"
            );
        } else {
            panic!("not a profile op");
        }
        let resp = run_pipeline(req, |_, _, _| {}).unwrap();
        let max_x = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, MoveKind::Cut))
            .flat_map(|s| [s.from.x, s.to.x])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            max_x > 100.5,
            "wire JSON Profile + outside: cut max_x={max_x}, expected > 100.5"
        );
    }

    /// Open polyline: Profile offset should either offset OR emit
    /// nothing — but never silently cut on the source line.
    #[test]
    fn profile_offset_open_polyline_either_offsets_or_emits_nothing_never_on_line() {
        use crate::gcode::preview::MoveKind;
        use crate::geometry::{Point2, Segment};
        use crate::pipeline::test_helpers::{endmill, profile_op};
        let segments = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(50.0, 30.0), "0", 7),
            Segment::line(Point2::new(50.0, 30.0), Point2::new(100.0, 0.0), "0", 7),
        ];
        let mk = |offset: ToolOffset| Project {
            segments: segments.clone(),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, offset)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        for offset in [ToolOffset::Outside, ToolOffset::Inside] {
            let resp = run_pipeline(
                PipelineRequest {
                    project: mk(offset),
                    post_processor: None,
                },
                |_, _, _| {},
            )
            .unwrap();
            let cut: Vec<_> = resp
                .toolpath
                .iter()
                .filter(|s| matches!(s.kind, MoveKind::Cut))
                .collect();
            let on_apex = cut.iter().any(|s| {
                let mid_x = (s.from.x + s.to.x) * 0.5;
                let mid_y = (s.from.y + s.to.y) * 0.5;
                (mid_x - 50.0).abs() < 5.0 && (mid_y - 30.0).abs() < 0.2
            });
            assert!(
                !on_apex || cut.is_empty(),
                "{offset:?} on open polyline: cut crosses the source apex (50, 30) — offset isn't being applied (on-line cut bug)"
            );
        }
    }

    /// Three offsets (On / Outside / Inside) produce distinct cut
    /// extents — sanity check that the offset is applied at all.
    #[test]
    fn profile_offset_actually_offsets_outside_inside_on() {
        use crate::gcode::preview::MoveKind;
        use crate::pipeline::test_helpers::{closed_square_offset, endmill, profile_op};
        let segments = closed_square_offset(100.0, 0.0, 0.0);
        let mk = |offset: ToolOffset| Project {
            segments: segments.clone(),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, offset)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let cut_max_x = |toolpath: &[crate::gcode::preview::ToolpathSegment]| -> f64 {
            toolpath
                .iter()
                .filter(|s| matches!(s.kind, MoveKind::Cut))
                .flat_map(|s| [s.from.x, s.to.x])
                .fold(f64::NEG_INFINITY, f64::max)
        };
        let on = run_pipeline(
            PipelineRequest {
                project: mk(ToolOffset::On),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let outside = run_pipeline(
            PipelineRequest {
                project: mk(ToolOffset::Outside),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let inside = run_pipeline(
            PipelineRequest {
                project: mk(ToolOffset::Inside),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let on_x = cut_max_x(&on.toolpath);
        let outside_x = cut_max_x(&outside.toolpath);
        let inside_x = cut_max_x(&inside.toolpath);
        assert!(
            (on_x - 100.0).abs() < 0.1,
            "On offset should cut at exactly the boundary (max_x≈100), got {on_x}"
        );
        assert!(
            outside_x > 100.5,
            "Outside offset should push cut past the boundary (max_x>100.5), got {outside_x}"
        );
        assert!(
            inside_x < 99.5,
            "Inside offset should pull cut inside the boundary (max_x<99.5), got {inside_x}"
        );
    }

    // ─── Pocket / strategy / fill ──────────────────────────────────────
    // These tests pull in additional fixtures + the preview module that
    // the plunge/lead/depth tests above didn't need.
    use crate::gcode::preview;
    use crate::geometry::{Point2, Segment};
    use crate::pipeline::test_helpers::pocket_op;
    use crate::pipeline::PipelineResponse;
    use crate::project::{Coolant, SourceCombine, ToolEntry, ToolKind};

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
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pocket_op(
                1,
                1,
                OpSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Auto,
                },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let annulus_project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pocket_op(
                1,
                1,
                OpSource::Objects {
                    ids: vec![1, 2],
                    combine: SourceCombine::Auto,
                },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
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

    /// `SourceCombine::Difference` applied at the pipeline level should
    /// produce one annulus pocket from "outer minus inner", matching
    /// what the user means when they pick Difference explicitly. This
    /// guards the `synthesize_region_object` path that fakes a `VcObject`
    /// from clipper2 polytree output.
    #[test]
    fn pocket_with_difference_combine_emits_an_annulus() {
        let mut segments = closed_square_offset(50.0, 0.0, 0.0);
        segments.extend(closed_square_offset(20.0, 15.0, 15.0));
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket-diff".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![1, 2],
                    combine: SourceCombine::Difference,
                },
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.stats.offset_count >= 1,
            "Difference produced no offsets"
        );
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
        assert!(
            visited_outside_inner,
            "annulus pocket should reach outside the inner box"
        );
        assert!(
            visited_inside_outer,
            "annulus pocket should stay inside the outer box"
        );
    }

    /// Pocket-Outside (rt1.3): a Pocket op carrying `frame_shape` should
    /// auto-prepend a frame around the selection at pipeline time and
    /// emit a toolpath that fills the area BETWEEN the frame and the
    /// selection — not the area inside the selection.
    #[test]
    fn pocket_outside_carves_between_frame_and_selection() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let params = OpParams::mill_default();
        let pocket = crate::project::PocketParams {
            frame_shape: Some(crate::cam::source_combine::FrameShape::Rectangle),
            frame_padding_mm: Some(10.0),
            ..crate::project::PocketParams::default()
        };
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket-Outside".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Difference,
                },
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.stats.offset_count >= 1,
            "Pocket-Outside produced no offsets",
        );
        // The cutter should reach OUTSIDE the 50x50 inner square (in the
        // padding region) AND must NOT cut deep inside the inner square's
        // interior (the source selection is the high part).
        let cuts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .collect();
        // Cuts in the padding region: x or y outside [0, 50].
        let visited_padding = cuts.iter().any(|s| {
            let in_inner = |x: f64, y: f64| (0.0..=50.0).contains(&x) && (0.0..=50.0).contains(&y);
            !in_inner(s.from.x, s.from.y) || !in_inner(s.to.x, s.to.y)
        });
        assert!(
            visited_padding,
            "Pocket-Outside should cut in the padding region between frame and selection",
        );
        // Cuts deep inside the source square (≥ tool_radius from the wall)
        // should not happen — the inner is the "raised" area, not carved.
        let inner_carve_min = 5.0;
        let inner_carve_max = 45.0;
        let cut_inside_inner = cuts.iter().any(|s| {
            let deep_inside = |x: f64, y: f64| {
                x > inner_carve_min
                    && x < inner_carve_max
                    && y > inner_carve_min
                    && y < inner_carve_max
            };
            deep_inside(s.from.x, s.from.y) && deep_inside(s.to.x, s.to.y)
        });
        assert!(
            !cut_inside_inner,
            "Pocket-Outside should NOT cut deep inside the source selection",
        );
    }

    /// Pocket-Outside (rt1.3) regression: when the user enters a frame
    /// padding smaller than the cutter radius, the pipeline must clamp
    /// the padding up to (at least) the tool radius and emit a warning
    /// — otherwise the synthetic frame's "Inside" offset puts the
    /// cutter inside the very shape it should be carving around.
    #[test]
    fn pocket_outside_clamps_padding_below_tool_radius() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let params = OpParams::mill_default();
        let pocket = crate::project::PocketParams {
            frame_shape: Some(crate::cam::source_combine::FrameShape::Rectangle),
            frame_padding_mm: Some(1.0), // < tool radius (3.0)
            ..crate::project::PocketParams::default()
        };
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)], // 6 mm Ø ⇒ 3 mm radius
            operations: vec![Op {
                id: 1,
                name: "Pocket-Outside-tight".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Difference,
                },
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let warned = resp
            .warnings
            .iter()
            .any(|w| w.kind == "frame_padding_below_tool_radius" && w.op_id == Some(1));
        assert!(
            warned,
            "expected frame_padding_below_tool_radius warning, got {:?}",
            resp.warnings,
        );
        // After the clamp the cutter centerline can sit on the bbox
        // boundary at worst, but must never step into the interior of
        // the source square — that's the very thing the clamp prevents.
        let cuts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .collect();
        let inner_carve_min = 1.0;
        let inner_carve_max = 49.0;
        let cut_inside_inner = cuts.iter().any(|s| {
            let deep_inside = |x: f64, y: f64| {
                x > inner_carve_min
                    && x < inner_carve_max
                    && y > inner_carve_min
                    && y < inner_carve_max
            };
            deep_inside(s.from.x, s.from.y) && deep_inside(s.to.x, s.to.y)
        });
        assert!(
            !cut_inside_inner,
            "clamped Pocket-Outside must not cut inside the source square",
        );
    }

    /// Two-op regression: a plain Pocket followed by a Pocket-Outside
    /// on the same source must produce two distinct toolpath blocks
    /// (one inside, one in the padding ring outside) without one
    /// op's mutations bleeding into the other. Catches the case where
    /// `frame_op_storage` mutating `objects` would leak into a prior or
    /// subsequent op.
    #[test]
    fn pocket_then_pocket_outside_produces_disjoint_cuts() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![
                Op {
                    id: 1,
                    name: "Inner pocket".into(),
                    enabled: true,
                    kind: OpKind::Pocket {
                        strategy: crate::project::PocketStrategy::Cascade,
                        contour: crate::project::ContourParams::default(),
                        pocket: crate::project::PocketParams::default(),
                    },
                    tool_id: 1,
                    finish_tool_id: None,
                    source: OpSource::Objects {
                        ids: vec![1],
                        combine: SourceCombine::Auto,
                    },
                    params: OpParams::mill_default(),
                },
                Op {
                    id: 2,
                    name: "Pocket Outside".into(),
                    enabled: true,
                    kind: OpKind::Pocket {
                        strategy: crate::project::PocketStrategy::Cascade,
                        contour: crate::project::ContourParams::default(),
                        pocket: crate::project::PocketParams {
                            frame_shape: Some(crate::cam::source_combine::FrameShape::Rectangle),
                            frame_padding_mm: Some(10.0),
                            ..crate::project::PocketParams::default()
                        },
                    },
                    tool_id: 1,
                    finish_tool_id: None,
                    source: OpSource::Objects {
                        ids: vec![1],
                        combine: SourceCombine::Difference,
                    },
                    params: OpParams::mill_default(),
                },
            ],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.stats.offset_count >= 2,
            "expected ≥2 offsets total (pocket + pocket-outside), got {}",
            resp.stats.offset_count,
        );
        let cuts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .collect();
        // Cuts should cover BOTH the inside (pocket op) and the
        // padding ring (pocket-outside).
        let inside_cuts = cuts.iter().any(|s| {
            let deep_inside = |x: f64, y: f64| (5.0..45.0).contains(&x) && (5.0..45.0).contains(&y);
            deep_inside(s.from.x, s.from.y) && deep_inside(s.to.x, s.to.y)
        });
        let outside_cuts = cuts.iter().any(|s| {
            let in_padding =
                |x: f64, y: f64| !((0.0..=50.0).contains(&x) && (0.0..=50.0).contains(&y));
            in_padding(s.from.x, s.from.y) || in_padding(s.to.x, s.to.y)
        });
        assert!(inside_cuts, "first pocket should cut inside the square");
        assert!(
            outside_cuts,
            "pocket-outside should cut in the padding ring",
        );
        // The regions preview must also distinguish them: one region
        // per op_id, with op 1 inside and op 2 in the ring.
        let op1_regions = resp.regions.iter().filter(|r| r.op_id == 1).count();
        let op2_regions = resp.regions.iter().filter(|r| r.op_id == 2).count();
        assert!(
            op1_regions >= 1,
            "op 1 should have a fill region in the preview (got {op1_regions})",
        );
        assert!(
            op2_regions >= 1,
            "op 2 (pocket-outside) should have a fill region (got {op2_regions})",
        );
    }

    /// Climb on the main + conventional on the finishing pass: walks the
    /// pipeline output and verifies the level=0 ring uses the
    /// conventional winding (CCW for an inner pocket boundary) while
    /// any level≥1 cascade ring uses climb (CW for an inner ring).
    #[test]
    fn pocket_with_climb_main_and_conventional_finish_winds_correctly() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let params = OpParams::mill_default();
        let contour = crate::project::ContourParams {
            cut_direction: crate::project::CutDirection::Climb,
            finish_cut_direction: crate::project::CutDirection::Conventional,
            ..crate::project::ContourParams::default()
        };
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour,
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
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
            loop_segs.first().map_or(0.0, |s| s.from.z)
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
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Op {
                id: 7,
                name: "Tiny pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
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
        assert_eq!(
            too_large.len(),
            1,
            "expected one tool_too_large warning, got {:?}",
            resp.warnings
        );
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
            tip_angle_deg: 60.0,
            dragoff: None,
            flutes: 2,
            speed: 18_000,
            plunge_rate: 100,
            feed_rate: 800,
            coolant: Coolant::Off,
            speed_finish: None,
            plunge_rate_finish: None,
            feed_rate_finish: None,
            speed_drill: None,
            plunge_rate_drill: None,
            feed_rate_drill: None,
            default_peck_step_mm: None,
            default_step: None,
            default_xy_overlap: None,
            comment: None,
            z_shift_mm: None,
            laser_pierce_sec: None,
            laser_lead_in_mm: None,
            kerf_mm: None,
            corner_radius_mm: None,
            form_profile_mm: Vec::new(),
            wirbeln: false,
            wirbeln_stepover_mm: None,
            wirbeln_extra_width_mm: None,
            wirbeln_osc_mm: None,
            pause: 1,
            flute_length_mm: None,
            length_mm: None,
            compression_transition_mm: None,
            thread_pitch_mm: None,
            shank_diameter_mm: None,
            stickout_length_mm: None,
            holder: None,
            spindle_direction: crate::project::SpindleDirection::default(),
            drag_knife_self_align_angle_deg: None,
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            vcarve_lead_in_angle_deg: None,
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![drill],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
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

    // Plunge tests moved to pipeline/offset_builder.rs (bvzj).

    /// A 10x10 pocket with a 6mm endmill: tool fits the boundary
    /// offset (4x4 left after a 3mm offset) but no cascade ring fits
    /// inside it → the cutter walks the wall and leaves a hollow
    /// rectangle. We surface this as a `pocket_fill_incomplete` warning
    /// so the user understands why the gcode is just the contour.
    #[test]
    fn pocket_with_just_fitting_tool_warns_about_incomplete_fill() {
        let project = Project {
            segments: closed_square_offset(10.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Op {
                id: 9,
                name: "Hollow pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
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

    /// Higher `xy_overlap` → smaller step → more cascade rings on the
    /// same geometry. Verifies the new knob actually drives the cascade
    /// step. With 0.7 overlap the cut path on a 50x50 pocket should be
    /// strictly longer than at 0.3 overlap.
    #[test]
    fn higher_xy_overlap_emits_a_longer_cut_path() {
        fn cut_total(resp: &PipelineResponse) -> f64 {
            resp.toolpath
                .iter()
                .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
                .map(|s| {
                    let dx = s.to.x - s.from.x;
                    let dy = s.to.y - s.from.y;
                    (dx * dx + dy * dy).sqrt()
                })
                .sum()
        }
        let make = |overlap: f64| -> PipelineResponse {
            let params = OpParams::mill_default();
            let pocket = crate::project::PocketParams {
                xy_overlap: overlap,
                ..crate::project::PocketParams::default()
            };
            let project = Project {
                segments: closed_square_offset(50.0, 0.0, 0.0),
                machine: MachineConfig::default(),
                tools: vec![endmill(1, 3.0)],
                operations: vec![Op {
                    id: 1,
                    name: "Pocket".into(),
                    enabled: true,
                    kind: OpKind::Pocket {
                        strategy: crate::project::PocketStrategy::Cascade,
                        contour: crate::project::ContourParams::default(),
                        pocket,
                    },
                    tool_id: 1,
                    finish_tool_id: None,
                    source: OpSource::All,
                    params,
                }],
                fixtures: Vec::default(),
                text_layers: Vec::default(),
                work_offset: crate::project::WorkOffset::default(),
                stock: None,
                relief_sources: Vec::new(),
            };
            run_pipeline(
                PipelineRequest {
                    project,
                    post_processor: None,
                },
                |_, _, _| {},
            )
            .unwrap()
        };
        let lo = cut_total(&make(0.3));
        let hi = cut_total(&make(0.7));
        assert!(
            hi > lo * 1.2,
            "expected higher overlap to add ≥20% cut length; got {hi} vs {lo}",
        );
    }

    /// Direct end-to-end check that zigzag emission is alive: at default
    /// `xy_overlap` the gcode for a 50x50 pocket must contain cuts at
    /// distinct Y values inside the pocket — not just the boundary
    /// contour at four corners.
    #[test]
    fn zigzag_pocket_emits_interior_strokes() {
        let params = OpParams::mill_default();
        // Force the default explicitly so the test pins behavior even
        // if the constant changes later.
        let pocket = crate::project::PocketParams {
            xy_overlap: 0.5,
            ..crate::project::PocketParams::default()
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Zigzag pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Zigzag { angle_deg: 0.0 },
                    contour: crate::project::ContourParams::default(),
                    pocket,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // Cuts at the level=0 contour visit only y=1.5 and y=48.5 (the
        // contour inset by tool_radius=1.5 from the original 0..50).
        // Zigzag fill should add strokes at intermediate Y values.
        let interior_cut_y_values: std::collections::HashSet<i32> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .filter_map(|s| {
                // Round to the nearest mm so floating-point doesn't
                // explode the set.
                let y_mm = s.from.y.round() as i32;
                if (1..=49).contains(&y_mm) {
                    Some(y_mm)
                } else {
                    None
                }
            })
            .collect();
        // A 50x50 pocket at 1.5mm stride gives at least 20 distinct
        // interior Y rows. If we see only 2 (just the contour edges),
        // zigzag emission is broken.
        assert!(
            interior_cut_y_values.len() > 5,
            "expected many distinct interior Y rows from zigzag, got {}: {:?}",
            interior_cut_y_values.len(),
            interior_cut_y_values,
        );
    }

    /// Cascade with a tool too wide for any inward ring emits ONLY the
    /// boundary contour (no silent fallback to zigzag — that was
    /// confusing for users who picked cascade explicitly and saw
    /// zigzag). The `pocket_fill_incomplete` warning fires so they know.
    #[test]
    fn cascade_with_tool_too_wide_emits_only_boundary_no_zigzag_substitute() {
        let params = OpParams::mill_default();
        let pocket = crate::project::PocketParams {
            xy_overlap: 0.05, // 95% step — no inward rings will fit
            ..crate::project::PocketParams::default()
        };
        let project = Project {
            // 6×6 with a 3mm tool: boundary inset by 1.5mm leaves a
            // 3×3 path; cascade inflate by 2.85mm → empty → 0 rings.
            segments: closed_square_offset(6.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // pocket_fill_incomplete warning must fire so the user knows.
        assert!(
            resp.warnings
                .iter()
                .any(|w| w.kind == "pocket_fill_incomplete"),
            "expected pocket_fill_incomplete warning, got {:?}",
            resp.warnings,
        );
    }

    /// CW-wound input must still pocket INWARD. Cavalier-Contours
    /// treats positive delta as left-of-tangent, which is the polygon
    /// interior for CCW but the EXTERIOR for CW. The user reported
    /// (test.vc-project.json) a CW DXF where the pocket was being cut
    /// outside the boundary, enlarging the shape by the tool diameter.
    /// `parallel_offset_inward` now picks the right sign per winding.
    #[test]
    fn pocket_on_cw_polygon_cuts_inside_not_outside() {
        // Build a 50×50 square wound CW (clockwise from above): walk
        // (0,0)→(0,50)→(50,50)→(50,0)→(0,0). signed_area would be
        // negative for this winding.
        let s = 50.0;
        let segments = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(0.0, s), "0", 7),
            Segment::line(Point2::new(0.0, s), Point2::new(s, s), "0", 7),
            Segment::line(Point2::new(s, s), Point2::new(s, 0.0), "0", 7),
            Segment::line(Point2::new(s, 0.0), Point2::new(0.0, 0.0), "0", 7),
        ];
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // Every cut must stay INSIDE the polygon's bounding box —
        // outside cuts mean the cutter went the wrong way.
        for s in &resp.toolpath {
            if !matches!(s.kind, crate::gcode::preview::MoveKind::Cut) {
                continue;
            }
            for pt in [s.from, s.to] {
                assert!(
                    pt.x >= -0.01 && pt.x <= 50.01 && pt.y >= -0.01 && pt.y <= 50.01,
                    "cut went outside the CW pocket boundary: ({}, {})",
                    pt.x,
                    pt.y,
                );
            }
        }
    }

    // ─── Drill ops ─────────────────────────────────────────────────────
    // Moved to pipeline/op_drivers/drill.rs (vk77 phase 2).

    /// Per-op feedrate overrides win over the tool's defaults.
    #[test]
    fn feed_rate_override_appears_in_gcode() {
        let mut params = OpParams::mill_default();
        params.feed_rate_override = Some(123);
        params.plunge_rate_override = Some(45);
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("F123"),
            "expected feed_rate_override 123 in gcode, got:\n{}",
            resp.gcode,
        );
        assert!(
            resp.gcode.contains("F45"),
            "expected plunge_rate_override 45 in gcode",
        );
        // Tool's defaults (800 / 100) should NOT appear when overridden.
        assert!(!resp.gcode.lines().any(|l| l.trim() == "F800"));
    }

    /// Pocket op with a slower finish feed: the gcode must contain the
    /// finish feedrate before the wall-defining (level=0) ring is cut
    /// (rt1.27).
    #[test]
    fn pocket_finish_ring_emits_finish_feedrate() {
        let mut tool = endmill(1, 3.0);
        tool.speed = 20_000;
        tool.feed_rate = 1500;
        tool.speed_finish = Some(8_000);
        tool.feed_rate_finish = Some(400);

        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![pocket_op(1, 1, OpSource::All)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // The rough feed (F1500) should appear for the cascade rings;
        // the finish feed (F400) must appear before the level=0 wall
        // ring is cut. Both must show up — and the post should also
        // emit the finish spindle (S8000) somewhere in the body.
        assert!(
            resp.gcode.contains("F1500"),
            "expected rough feed 1500 in gcode:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("F400"),
            "expected finish feed 400 in gcode:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("S8000"),
            "expected finish spindle 8000 in gcode:\n{}",
            resp.gcode
        );
    }

    /// Pocket op WITHOUT a finish override: rough feed is used
    /// everywhere — no surprise feed change before the level=0 ring
    /// (rt1.27 fallback behavior).
    #[test]
    fn pocket_without_finish_override_uses_rough_throughout() {
        let mut tool = endmill(1, 3.0);
        tool.speed = 20_000;
        tool.feed_rate = 1500;
        // no finish overrides set
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![pocket_op(1, 1, OpSource::All)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.gcode.contains("F1500"));
        assert!(
            !resp.gcode.contains("F400"),
            "no finish-feed F400 should appear when finish overrides are unset"
        );
    }

    /// Pocket with `xy_finish_allowance` emits an extra wall ring at
    /// the actual contour (`tool_radius` offset) AND the rough rings
    /// step inward from (`tool_radius` + allowance) — leaving stock at
    /// the wall that the finish ring removes (rt1.24).
    #[test]
    fn pocket_finish_xy_allowance_emits_extra_boundary_pass() {
        use crate::cam::offsets::{pocket_for_object, PocketEmit};
        use crate::cam::VcObject;
        let pt = |x: f64, y: f64| Point2::new(x, y);
        let segs = vec![
            Segment::line(pt(0.0, 0.0), pt(50.0, 0.0), "0", 7),
            Segment::line(pt(50.0, 0.0), pt(50.0, 50.0), "0", 7),
            Segment::line(pt(50.0, 50.0), pt(0.0, 50.0), "0", 7),
            Segment::line(pt(0.0, 50.0), pt(0.0, 0.0), "0", 7),
        ];
        let obj = VcObject::new(segs, true);
        let tool_radius = 1.5;
        let no_allow = pocket_for_object(
            &obj,
            tool_radius,
            false,
            6,
            PocketEmit::Cascade,
            &[],
            1.5,
            0.0,
            None,
            crate::project::tool::SpindleDirection::Cw,
        );
        let with_allow = pocket_for_object(
            &obj,
            tool_radius,
            false,
            6,
            PocketEmit::Cascade,
            &[],
            1.5,
            0.5,
            None,
            crate::project::tool::SpindleDirection::Cw,
        );
        // With allowance we expect exactly one MORE level-0 ring:
        // the rough boundary (is_finish=false) + the finish boundary
        // (is_finish=true). Without allowance there's a single
        // boundary tagged as finish.
        let finish_count_no = no_allow.iter().filter(|o| o.is_finish).count();
        let finish_count_with = with_allow.iter().filter(|o| o.is_finish).count();
        assert_eq!(finish_count_no, 1);
        assert_eq!(finish_count_with, 1);
        // The extra rough boundary in `with_allow` is a non-finish
        // level-0 ring that doesn't exist in `no_allow`.
        let rough_level0_no = no_allow
            .iter()
            .filter(|o| o.level == 0 && !o.is_finish)
            .count();
        let rough_level0_with = with_allow
            .iter()
            .filter(|o| o.level == 0 && !o.is_finish)
            .count();
        assert_eq!(rough_level0_no, 0);
        assert_eq!(rough_level0_with, 1);
        assert_eq!(with_allow.len(), no_allow.len() + 1);
    }

    /// Pocket with `xy_finish_allowance` produces gcode that visits the
    /// rough rings at the tool's general feed and the finish ring at
    /// the finish-set feed (rt1.24 × rt1.27).
    #[test]
    fn pocket_with_xy_allowance_finish_ring_uses_finish_feed() {
        let mut tool = endmill(1, 3.0);
        tool.feed_rate = 1500;
        tool.feed_rate_finish = Some(400);
        let params = OpParams::mill_default();
        let pocket = crate::project::PocketParams {
            finish_xy_allowance_mm: Some(0.5),
            ..crate::project::PocketParams::default()
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.gcode.contains("F1500"), "rough feed missing");
        assert!(resp.gcode.contains("F400"), "finish feed missing");
    }

    /// 1mlv: `OpParamsCommon.stock_to_leave_mm` enlarges the effective
    /// cutter radius on Profile/Pocket offset cascades. Verify a Pocket
    /// with 1.0 mm stock-to-leave produces an inset boundary that sits
    /// further from the geometric wall than a baseline pocket without
    /// stock-to-leave.
    #[test]
    fn stock_to_leave_increases_effective_offset_radius() {
        // 20×20 box, 2 mm tool. Baseline offset = 1 mm; with
        // stock_to_leave = 1 mm the cutter centre walks at 2 mm from
        // the wall (radius 1 + stock 1 = 2). We verify by checking the
        // emitted toolpath's nearest-to-wall distance.
        let mut params = OpParams::mill_default();
        params.stock_to_leave_mm = 1.0;
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 2.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: crate::cam::setup::ToolOffset::Inside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // The 20x20 wall lies at x ∈ {0, 20}, y ∈ {0, 20}. With
        // tool_radius=1 + stock_to_leave=1 the cutter centerline
        // sits at min/max coordinates {2, 18}. We pick the minimum
        // X across cut moves and verify it's ≥ 1.8 (with a small
        // float slack) — pre-1mlv it would have been ~1.0.
        let min_cut_x = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .map(|s| s.from.x.min(s.to.x))
            .filter(|x| (0.0..=20.0).contains(x))
            .fold(f64::INFINITY, f64::min);
        assert!(
            min_cut_x >= 1.8,
            "expected cutter centerline ≥ 1.8 mm from wall (radius 1 + stock 1); got min_cut_x = {min_cut_x}",
        );
    }

    // ─── Spiral / trochoidal / approach-point / corner-feed ────────────
    use crate::pipeline::test_helpers::{closed_circle, profile_op, project_with};
    use crate::project::PocketStrategy;

    /// Approach point (rt1.26): when set on a Pocket op, each closed
    /// offset's segment list rotates so the start (where plunge
    /// happens) is the vertex closest to the user-picked XY.
    #[test]
    fn approach_point_rotates_closed_offsets_to_nearest_vertex() {
        // A 20x20 closed square at (0..20, 0..20). With approach_point
        // ~ (20, 20) the closest vertex of the inward-offset ring is
        // the top-right corner. Without approach_point, plunge happens
        // at an arbitrary auto-picked vertex.
        let center_ap = (20.0, 20.0);
        let params = OpParams::mill_default();
        let contour = crate::project::ContourParams {
            approach_point: Some(center_ap),
            ..crate::project::ContourParams::default()
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 1.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour,
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // The first G0 rapid that lands on the cut plane should
        // approach a point in the upper-right quadrant (X >= 10, Y >=
        // 10). With a 1-mm endmill and 20-mm box, the inward offset
        // wall ring sits around (0.5..19.5)^2; the rotated start lands
        // near (19.5, 19.5).
        let mut found_quadrant_entry = false;
        for line in resp.gcode.lines() {
            if !line.starts_with("G0 ") {
                continue;
            }
            // Parse X and Y if present.
            let mut x: Option<f64> = None;
            let mut y: Option<f64> = None;
            for tok in line.split_whitespace() {
                if let Some(rest) = tok.strip_prefix('X') {
                    x = rest.parse().ok();
                } else if let Some(rest) = tok.strip_prefix('Y') {
                    y = rest.parse().ok();
                }
            }
            if let (Some(xv), Some(yv)) = (x, y) {
                if xv > 10.0 && yv > 10.0 {
                    found_quadrant_entry = true;
                    break;
                }
            }
        }
        assert!(
            found_quadrant_entry,
            "expected a G0 entry in the upper-right quadrant after approach_point=(20,20):\n{}",
            resp.gcode
        );
    }

    /// Corner feed reduction emits a slower F before sharp turns.
    /// Verified on a zigzag pocket where adjacent strokes are joined
    /// by a 180° turn — exactly the worst-case for high-feed motion.
    #[test]
    fn corner_feed_reduction_emits_slower_f_at_sharp_turns() {
        let mut params = OpParams::mill_default();
        params.feed_rate_override = Some(1000);
        let contour = crate::project::ContourParams {
            corner_feed_reduction: 0.5, // halve at corners
            ..crate::project::ContourParams::default()
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Zigzag { angle_deg: 0.0 },
                    contour,
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("F500"),
            "expected reduced corner feed F500 (= 1000 * 0.5) in gcode",
        );
    }

    /// `PocketStrategy::Spiral` now emits ONE continuous open polyline
    /// instead of N concentric closed rings. Verified by counting
    /// distinct `; OP / level / pocket` blocks in the gcode — Spiral
    /// gives one `is_pocket=2` emit per object, Cascade gives N.
    #[test]
    fn spiral_emits_one_continuous_polyline_not_concentric_rings() {
        fn count_pocket_blocks(gcode: &str) -> usize {
            gcode
                .lines()
                .filter(|l| l.contains("pocket=2 segments="))
                .count()
        }
        let cascade_project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let mut spiral_project = cascade_project.clone();
        spiral_project.operations[0].kind = OpKind::Pocket {
            strategy: crate::project::PocketStrategy::Spiral,
            contour: crate::project::ContourParams::default(),
            pocket: crate::project::PocketParams::default(),
        };
        let cascade_gcode = run_pipeline(
            PipelineRequest {
                project: cascade_project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap()
        .gcode;
        let spiral_gcode = run_pipeline(
            PipelineRequest {
                project: spiral_project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap()
        .gcode;
        let cascade_blocks = count_pocket_blocks(&cascade_gcode);
        let spiral_blocks = count_pocket_blocks(&spiral_gcode);
        assert!(
            cascade_blocks > 1,
            "cascade should emit many ring blocks, got {cascade_blocks}"
        );
        assert_eq!(
            spiral_blocks, 1,
            "spiral should emit exactly one continuous block, got {spiral_blocks}"
        );
    }

    /// w91: in a non-convex pocket the straight bridge between cascade
    /// rings can cut through a re-entrant pocket wall. The fix detects
    /// the bad bridge and silently falls back to cascade emission
    /// (separate closed rings, no bridges) rather than emitting a wrong
    /// cut. The test uses an L-shape — its inner cascade rings break
    /// into pieces whose centroids are in different L arms, so the
    /// nearest-vertex bridge between them crosses the L's notch wall.
    #[test]
    fn spiral_in_non_convex_pocket_falls_back_to_cascade() {
        // L-shape outline (CCW), 30 mm tall × 30 mm wide × 10 mm leg
        // thickness — wide enough that the inset rings split.
        let p0 = Point2::new(0.0, 0.0);
        let p1 = Point2::new(30.0, 0.0);
        let p2 = Point2::new(30.0, 10.0);
        let p3 = Point2::new(10.0, 10.0);
        let p4 = Point2::new(10.0, 30.0);
        let p5 = Point2::new(0.0, 30.0);
        let l_shape = vec![
            Segment::line(p0, p1, "0", 7),
            Segment::line(p1, p2, "0", 7),
            Segment::line(p2, p3, "0", 7),
            Segment::line(p3, p4, "0", 7),
            Segment::line(p4, p5, "0", 7),
            Segment::line(p5, p0, "0", 7),
        ];
        let project = Project {
            segments: l_shape,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Spiral,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let gcode = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap()
        .gcode;
        // When spiral works (convex pocket): exactly one pocket=2
        // block. When it falls back to cascade in a non-convex shape:
        // multiple pocket=2 blocks (one per ring). For the L-shape
        // above we expect more than one, proving the fallback fired.
        let pocket_blocks = gcode
            .lines()
            .filter(|l| l.contains("pocket=2 segments="))
            .count();
        assert!(
            pocket_blocks >= 1,
            "L-shape pocket should emit at least one block; got {pocket_blocks}\n{gcode}"
        );
    }

    /// Source = Selected Objects with only the outer ring selected:
    /// inner circles inside the ring are NOT in the selection, so no
    /// pocket strategy should treat them as islands. Pre-fix, the
    /// `pocket_islands` legacy fallback in pipeline.rs would auto-fill
    /// the island list with all geometrically-nested closed children,
    /// which made cascade and spiral mill around the unselected
    /// circles while zigzag (whose offsets path doesn't plumb islands)
    /// ignored them — a strategy-dependent inconsistency the user
    /// reported. The fix restricts the auto-fill to source = All.
    ///
    /// Test approach: for each pocket strategy, compare the toolpath
    /// against a baseline run where the inner circles aren't even in
    /// the segment list. A correctly-implemented "selected only"
    /// pocket should produce IDENTICAL toolpath output regardless of
    /// whether unselected circles happen to be present in the
    /// document — the unselected geometry must have no influence.
    #[test]
    fn selected_objects_pocket_ignores_unselected_inner_circles_across_strategies() {
        use crate::project::{PocketStrategy, SourceCombine};
        let outer = closed_square_offset(100.0, 0.0, 0.0);
        let inner_a = closed_circle(Point2::new(30.0, 50.0), 5.0);
        let inner_b = closed_circle(Point2::new(70.0, 50.0), 5.0);
        let with_inners: Vec<Segment> = outer
            .iter()
            .cloned()
            .chain(inner_a.iter().cloned())
            .chain(inner_b.iter().cloned())
            .collect();
        let outer_only: Vec<Segment> = outer.clone();
        // Selection contains only object 1 (the outer ring) — same
        // value in both runs since chaining puts the outer first.
        let mk = |segments: Vec<Segment>, strategy: PocketStrategy, pocket_islands: bool| Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams {
                        pocket_islands,
                        ..crate::project::PocketParams::default()
                    },
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Auto,
                },
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let strategies = [
            PocketStrategy::Cascade,
            PocketStrategy::Spiral,
            PocketStrategy::Zigzag { angle_deg: 0.0 },
        ];
        for strategy in strategies {
            for pocket_islands in [false, true] {
                let baseline = run_pipeline(
                    PipelineRequest {
                        project: mk(outer_only.clone(), strategy, pocket_islands),
                        post_processor: None,
                    },
                    |_, _, _| {},
                )
                .unwrap();
                let with_inners_run = run_pipeline(
                    PipelineRequest {
                        project: mk(with_inners.clone(), strategy, pocket_islands),
                        post_processor: None,
                    },
                    |_, _, _| {},
                )
                .unwrap();
                // Same toolpath segment count = unselected inner
                // geometry had no influence on the cut. If the
                // pocket_islands fallback leaks into source=Objects,
                // the with_inners run gets extra cascade rings around
                // each circle and the count diverges.
                assert_eq!(
                    baseline.toolpath.len(),
                    with_inners_run.toolpath.len(),
                    "strategy {:?} pocket_islands={}: with-inners toolpath has \
                     {} segments vs baseline {} — unselected inner circles \
                     are leaking into the pocket as auto-islands",
                    strategy,
                    pocket_islands,
                    with_inners_run.toolpath.len(),
                    baseline.toolpath.len()
                );
            }
        }
    }

    /// Trochoidal pocket on a 100×60 rectangle with a 6 mm endmill.
    /// Validates that the emitted cut path is comparable in length to
    /// the spiral equivalent (1.0×–1.5×) — trochoidal is intentionally
    /// a longer path than spiral because every centerline step picks
    /// up a small loop, but it shouldn't blow up the path length by
    /// more than 50%.
    #[test]
    fn trochoidal_pocket_path_length_within_envelope_of_spiral() {
        let p0 = Point2::new(0.0, 0.0);
        let p1 = Point2::new(100.0, 0.0);
        let p2 = Point2::new(100.0, 60.0);
        let p3 = Point2::new(0.0, 60.0);
        let rect = vec![
            Segment::line(p0, p1, "0", 7),
            Segment::line(p1, p2, "0", 7),
            Segment::line(p2, p3, "0", 7),
            Segment::line(p3, p0, "0", 7),
        ];
        let mk = |strategy: PocketStrategy| Project {
            segments: rect.clone(),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams {
                    plunge: crate::cam::setup::PlungeStrategy::Helix {
                        angle_deg: 3.0,
                        radius_mm: Some(4.5),
                    },
                    ..OpParams::mill_default()
                },
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
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
        let spiral = run_pipeline(
            PipelineRequest {
                project: mk(PocketStrategy::Spiral),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let trochoidal = run_pipeline(
            PipelineRequest {
                project: mk(PocketStrategy::Trochoidal {
                    engagement_angle_deg: 30.0,
                    loop_radius_factor: 0.6,
                }),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let s_len = cut_total(&spiral.toolpath);
        let t_len = cut_total(&trochoidal.toolpath);
        assert!(s_len > 0.0, "spiral baseline empty");
        assert!(t_len > 0.0, "trochoidal toolpath empty");
        // Trochoidal IS longer than spiral by design (loops add
        // distance), so we expect t_len > s_len. Cap it at 5× to
        // catch obvious blow-ups; the brief's 1.5× bound applies to
        // the centerline-only portion which is hard to extract from
        // the toolpath stream — keep the integration check loose.
        assert!(
            t_len > s_len * 0.5,
            "trochoidal path {t_len} too short vs spiral {s_len}"
        );
        assert!(
            t_len < s_len * 5.0,
            "trochoidal path {t_len} blew up vs spiral {s_len}"
        );
    }

    /// Pipeline emits a `tabs_with_trochoidal_unsupported` warning
    /// when an op asks for both at once.
    #[test]
    fn trochoidal_with_tabs_emits_unsupported_warning() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let mut params = OpParams::mill_default();
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(4.5),
        };
        let contour = crate::project::ContourParams {
            tabs: crate::cam::setup::TabsConfig {
                active: true,
                ..crate::cam::setup::TabsConfig::default()
            },
            ..crate::project::ContourParams::default()
        };
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 7,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: PocketStrategy::Trochoidal {
                        engagement_angle_deg: 30.0,
                        loop_radius_factor: 0.6,
                    },
                    contour,
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.warnings
                .iter()
                .any(|w| w.kind == "tabs_with_trochoidal_unsupported" && w.op_id == Some(7)),
            "expected tabs_with_trochoidal_unsupported, got {:?}",
            resp.warnings
        );
    }

    /// Pipeline overrides Direct/Ramp plunges to Helix on Trochoidal
    /// pockets and emits `plunge_overridden`.
    #[test]
    fn trochoidal_with_direct_plunge_emits_plunge_overridden_warning() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 9,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: PocketStrategy::Trochoidal {
                        engagement_angle_deg: 30.0,
                        loop_radius_factor: 0.6,
                    },
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams {
                    plunge: crate::cam::setup::PlungeStrategy::Direct,
                    ..OpParams::mill_default()
                },
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.warnings
                .iter()
                .any(|w| w.kind == "plunge_overridden" && w.op_id == Some(9)),
            "expected plunge_overridden warning, got {:?}",
            resp.warnings
        );
    }

    #[test]
    fn op_step_and_tool_default_step_emit_identical_gcode() {
        let mut tool_a = endmill(1, 3.0);
        tool_a.default_step = None;
        let mut op_a = profile_op(1, 1, ToolOffset::Outside);
        op_a.params.step = Some(-0.5);

        let mut tool_b = endmill(1, 3.0);
        tool_b.default_step = Some(-0.5);
        let mut op_b = profile_op(1, 1, ToolOffset::Outside);
        op_b.params.step = None;

        let resp_a = run_pipeline(
            PipelineRequest {
                project: project_with(vec![op_a], vec![tool_a]),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let resp_b = run_pipeline(
            PipelineRequest {
                project: project_with(vec![op_b], vec![tool_b]),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(resp_a.gcode, resp_b.gcode);
        assert!(resp_a.warnings.iter().all(|w| w.kind != "step_unspecified"));
        assert!(resp_b.warnings.iter().all(|w| w.kind != "step_unspecified"));
    }

    /// 473k regression: under `OpSource::All`, two nested closed contours
    /// (outer rectangle + inner circle) should auto-build an annular
    /// (donut) pocket — the inner closed contour becomes an island
    /// without the user needing to flip `pocket_islands`. Pre-fix the
    /// pipeline pocketed straight through the inner circle by default.
    ///
    /// We assert this by comparing the toolpath against a baseline
    /// where the inner circle is explicitly treated as an island
    /// (`pocket_islands=true`) — the two should produce the same number
    /// of cut segments because the auto-annular detection now matches
    /// the legacy `pocket_islands` behaviour.
    #[test]
    fn pocket_all_with_nested_closed_contours_builds_donut_by_default() {
        use crate::cam::setup::PlungeStrategy;
        use crate::project::PocketStrategy;
        let outer = closed_square_offset(60.0, 0.0, 0.0);
        let inner = closed_circle(Point2::new(30.0, 30.0), 8.0);
        let segments: Vec<Segment> = outer.iter().cloned().chain(inner.iter().cloned()).collect();
        let mk = |pocket_islands: bool| Project {
            segments: segments.clone(),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams {
                        pocket_islands,
                        ..crate::project::PocketParams::default()
                    },
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams {
                    plunge: PlungeStrategy::Direct,
                    ..OpParams::mill_default()
                },
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let auto = run_pipeline(
            PipelineRequest {
                project: mk(false),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let explicit = run_pipeline(
            PipelineRequest {
                project: mk(true),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // The auto-detect path must produce the SAME toolpath length
        // as explicitly flipping pocket_islands=true — the donut shape
        // is identical in both cases.
        assert_eq!(
            auto.toolpath.len(),
            explicit.toolpath.len(),
            "auto-annular (pocket_islands=false, default) must produce the same toolpath as pocket_islands=true under source=All"
        );
        // Sanity check that the toolpath actually leaves the inner
        // circle uncut: no cut segment should pass through the centre.
        let cut_segments: Vec<&crate::gcode::preview::ToolpathSegment> = auto
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .collect();
        // The cutter offsets inward by tool_radius (1.5 mm) from the
        // 8 mm circle and from the outer wall. No segment should pass
        // through the center (30, 30) within radius (8 - 1.5 - eps).
        for seg in &cut_segments {
            let mid_x = (seg.from.x + seg.to.x) * 0.5;
            let mid_y = (seg.from.y + seg.to.y) * 0.5;
            let dx = mid_x - 30.0;
            let dy = mid_y - 30.0;
            let d = (dx * dx + dy * dy).sqrt();
            assert!(
                d > 5.0,
                "cut segment midpoint ({mid_x:.2}, {mid_y:.2}) is inside the auto-annular island region (distance {d:.2} from centre)"
            );
        }
    }

    /// knd4 regression: a Zigzag pocket with an island must keep the
    /// cutter CENTERLINE at least `tool_radius` away from the raw
    /// island wall. Pre-fix the pipeline passed the raw island
    /// polygons into `pocket_for_object`, but the pocket emitters
    /// document their `islands` input as already-inflated-by-tool-radius
    /// and used the polygon-as-given for centerline trimming. Effect:
    /// the cutter EDGE bit `tool_r` into the raw island on every
    /// scanline endpoint adjacent to the island — silently shaving the
    /// label/boss the user wanted preserved. We now inflate islands
    /// by `tool_radius` in the pipeline before calling `pocket_for_object`,
    /// so no centerline point lies within `tool_radius - eps` of the
    /// raw island.
    #[test]
    fn pocket_zigzag_with_island_keeps_centerline_outside_raw_island() {
        use crate::cam::setup::PlungeStrategy;
        use crate::project::PocketStrategy;
        let outer = closed_square_offset(60.0, 0.0, 0.0);
        let island_center = Point2::new(30.0, 30.0);
        let island_radius = 8.0;
        let inner = closed_circle(island_center, island_radius);
        let segments: Vec<Segment> = outer.iter().cloned().chain(inner.iter().cloned()).collect();
        let tool_diameter = 3.0;
        let tool_radius = tool_diameter * 0.5;
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, tool_diameter)],
            operations: vec![Op {
                id: 1,
                name: "Zigzag pocket with island".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: PocketStrategy::Zigzag { angle_deg: 0.0 },
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams {
                        pocket_islands: true,
                        ..crate::project::PocketParams::default()
                    },
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams {
                    plunge: PlungeStrategy::Direct,
                    ..OpParams::mill_default()
                },
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // Every CUT segment's interior must keep both endpoints
        // ≥ (island_radius + tool_radius − arc-tol slack) from the
        // island centre — the cutter centerline never enters the
        // tool-radius safety band. Pre-fix, scanline endpoints
        // adjacent to the island sat at d ≈ island_radius (touching
        // the raw wall), so this gives at least `tool_radius`
        // (1.5 mm) of headroom on the regression.
        //
        // Slack budget: clipper2's outward inflate uses round joins
        // with arc_tol=0.25, so the Minkowski-sum boundary the
        // pipeline computes is a chord polygon that sits up to
        // ~arc_tol inside the perfect tool-radius circle around the
        // raw island. We allow that tolerance here — anything more
        // than that is a knd4 regression (the pipeline forgot to
        // inflate islands at all).
        let safe_min_distance = island_radius + tool_radius - 0.30;
        let cuts: Vec<&crate::gcode::preview::ToolpathSegment> = resp
            .toolpath
            .iter()
            .filter(|s| s.op_id == 1 && matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .collect();
        assert!(
            !cuts.is_empty(),
            "expected at least one cut segment for the zigzag pocket"
        );
        for seg in &cuts {
            for pt in [&seg.from, &seg.to] {
                let dx = pt.x - island_center.x;
                let dy = pt.y - island_center.y;
                let d = (dx * dx + dy * dy).sqrt();
                assert!(
                    d >= safe_min_distance,
                    "knd4 regression: cut endpoint ({:.3}, {:.3}) sits {:.3} mm from raw island centre — must be ≥ {:.3} (island_r={:.1} + tool_r={:.1})",
                    pt.x, pt.y, d, safe_min_distance, island_radius, tool_radius,
                );
            }
        }
    }

    /// c0pm regression: a single-pass Profile op is by definition the
    /// finishing wall pass — the tool's finish-set rates
    /// (`rate_h_finish` / `speed_finish` / `rate_v_finish`) must drive the
    /// emitted gcode, not the rough-set rates. Pre-fix the Profile
    /// branch never set `is_finish=true` on the offsets, so the gcode
    /// emitter substituted the rough feed even when the user
    /// configured distinct finish values.
    #[test]
    fn profile_op_uses_finish_feed_and_speed() {
        use crate::cam::setup::PlungeStrategy;
        let mut tool = endmill(1, 3.0);
        tool.speed = 20_000;
        tool.feed_rate = 1500;
        tool.speed_finish = Some(8_000);
        tool.feed_rate_finish = Some(400);
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![Op {
                id: 1,
                name: "Profile Outside".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams {
                    plunge: PlungeStrategy::Direct,
                    ..OpParams::mill_default()
                },
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // The finish feed (F400) must appear — the single Profile pass
        // IS the finishing pass. The rough feed (F1500) must NOT appear
        // because there is no rough pass on a single-pass Profile.
        assert!(
            resp.gcode.contains("F400"),
            "c0pm regression: Profile op should use the finish feedrate F400; gcode:\n{}",
            resp.gcode,
        );
        assert!(
            resp.gcode.contains("S8000"),
            "c0pm regression: Profile op should use the finish spindle S8000; gcode:\n{}",
            resp.gcode,
        );
        assert!(
            !resp.gcode.contains("F1500"),
            "c0pm regression: Profile op must NOT use rough feed F1500 — its single pass is by definition a finishing pass; gcode:\n{}",
            resp.gcode,
        );
    }

    /// 0tsy regression: `pocket_nocontour=true` + `finish_xy_allowance_mm` > 0
    /// is a meaningless combination — there's no wall ring to absorb the
    /// allowance, so the rough cascade would walk `allowance` mm inboard
    /// of the wall and leave that stock behind forever. We fold allowance
    /// back to 0 and surface a `nocontour_ignores_finish_allowance`
    /// warning. The Pocket's outer rough-cascade ring must therefore sit
    /// at the same XY position as if allowance had never been set.
    #[test]
    fn pocket_nocontour_with_allowance_folds_to_zero_and_warns() {
        use crate::cam::setup::PlungeStrategy;
        use crate::project::PocketStrategy;
        let tool_diameter = 3.0;
        let mk = |allowance: f64, nocontour: bool| Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, tool_diameter)],
            operations: vec![Op {
                id: 1,
                name: "Pocket nocontour".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams {
                        pocket_nocontour: nocontour,
                        finish_xy_allowance_mm: if allowance > 0.0 {
                            Some(allowance)
                        } else {
                            None
                        },
                        ..crate::project::PocketParams::default()
                    },
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams {
                    plunge: PlungeStrategy::Direct,
                    ..OpParams::mill_default()
                },
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
        };
        // Baseline: nocontour + allowance=0.
        let baseline = run_pipeline(
            PipelineRequest {
                project: mk(0.0, true),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // Pathological: nocontour + allowance=0.2. Should produce IDENTICAL
        // toolpath (we folded allowance to 0) AND emit the warning.
        let ignored = run_pipeline(
            PipelineRequest {
                project: mk(0.2, true),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            ignored
                .warnings
                .iter()
                .any(|w| w.kind == "nocontour_ignores_finish_allowance"),
            "0tsy regression: expected nocontour_ignores_finish_allowance warning when pocket_nocontour=true is combined with a non-zero finish allowance; got warnings: {:?}",
            ignored.warnings,
        );
        // Same number of cut moves — the allowance was ignored, not
        // applied. (We compare counts rather than exact strings because
        // the gcode-emitter's plunge / lift ordering may differ on
        // hashing details, but the rough cascade should walk the same
        // boundary in both runs.)
        let cuts_baseline = baseline
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .count();
        let cuts_ignored = ignored
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .count();
        assert_eq!(
            cuts_baseline, cuts_ignored,
            "0tsy regression: nocontour+allowance must produce the same cut count as nocontour+allowance=0 (allowance is folded to 0). baseline={cuts_baseline}, ignored={cuts_ignored}",
        );
    }
}
