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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
        params.tabs = TabsConfig {
            active: true,
            width: 10.0,
            height: 1.0,
            tab_type: TabType::Rectangle,
            ramp_angle_deg: 30.0,
        };
        params.plunge = PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(3.0),
        };
        params.tab_mode = crate::project::TabPlacementMode::Manual;
        params.tab_placements = vec![crate::project::TabPlacement {
            object_id: 1,
            t: 0.125,
            width_override_mm: None,
            height_override_mm: None,
        }];
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
            operations: vec![profile_leads_op(ToolOffset::Outside, LeadKind::Straight, 2.0)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
        use crate::geometry::Segment;
        use crate::pipeline::test_helpers::{closed_square_offset, endmill, profile_op};
        use crate::gcode::preview::MoveKind;
        let ccw_segments = closed_square_offset(100.0, 0.0, 0.0);
        let cw_segments: Vec<Segment> = ccw_segments
            .iter()
            .rev()
            .map(|s| Segment::line(s.end, s.start, &s.layer, s.color))
            .collect();
        for (winding_label, segments) in &[("CCW", &ccw_segments), ("CW", &cw_segments)] {
            let mk = |offset: ToolOffset| Project {
                segments: (*segments).clone(),
                machine: MachineConfig::default(),
                tools: vec![endmill(1, 3.0)],
                operations: vec![profile_op(1, 1, offset)],
                fixtures: Vec::default(),
                text_layers: Vec::default(),
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
        use crate::geometry::Point2;
        use crate::pipeline::test_helpers::{closed_circle, closed_square_offset, endmill};
        use crate::gcode::preview::MoveKind;
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
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![2],
                    combine: crate::project::SourceCombine::Auto,
                },
                params: OpParams::mill_default(),
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
        if let OpKind::Profile { offset } = req.project.operations[0].kind {
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
        use crate::geometry::{Point2, Segment};
        use crate::pipeline::test_helpers::{endmill, profile_op};
        use crate::gcode::preview::MoveKind;
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
        use crate::pipeline::test_helpers::{closed_square_offset, endmill, profile_op};
        use crate::gcode::preview::MoveKind;
        let segments = closed_square_offset(100.0, 0.0, 0.0);
        let mk = |offset: ToolOffset| Project {
            segments: segments.clone(),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, offset)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
}
