//! Per-op-kind drivers that don't fit the standard offset-cascade path.
//!
//! Profile / Pocket / Drill / Chamfer / `DragKnife` all flow through
//! `build_op_offsets` + the offsets-layer emitter. The four kinds in
//! this module need bespoke pipelines:
//!
//! * **V-Carve** — builds the medial axis of the region(s) and emits a
//!   per-axis ratchet sweep with depth = V-bit cone math at each point.
//! * **Halfpipe pocket** — same medial-axis machinery as V-Carve but the
//!   Z depth at each axis point comes from the configured half-pipe
//!   profile (circular-arc / V-bottom) instead of the V-bit cone.
//! * **Thread** — single-point helix inside / around a closed circular
//!   bore, tessellated waypoints fed through the V-Carve emit path.
//! * **Stufenfase** — single-revolution rim chamfer emitted after a
//!   drill block, optionally with a tool change to a dedicated chamfer
//!   bit. Reuses the V-Carve emit path.
//!
//! All four reuse `gcode::emit_vcarve_block` for the actual G-code
//! emission since each produces an XYZ polyline the emitter walks with
//! G1 cuts and safe-Z rapids.

// # CAM/sim pedantic-lint exemptions
// Per-op drivers cast sample counts (`STEPS`) and centroid indices to f64 for
// trig; values are tiny constants.
#![allow(clippy::cast_precision_loss)]

use super::offset_builder::build_op_offsets;
use super::setup_resolver::resolve_peck_step;
use super::warnings::push_tool_fit_kind_warnings;
use crate::cam::setup::Setup;
use crate::cam::source_combine::combine_source_regions;
use crate::cam::VcObject;
use crate::gcode::PostProcessor;
use crate::gcode::{emit_drill_block, emit_polylines_block};
use crate::geometry::Point2;
use crate::pipeline::{
    cancelled, effective_step, op_includes_object, ordered_selection, source_combine_mode,
    synthesize_finish_setup, CancelToken, PipelineError, PipelineWarning,
};
use crate::project::{Op, OpKind, PocketStrategy, Project};

/// V-Carve op driver. Builds the medial axis of the source region(s)
/// and emits a per-axis ratchet sweep with depth varying from
/// `start_depth` to the geometric V-bit depth at each point.
// V-Carve driver couples medial-axis sampling, multi-pass cascade, and
// optional finish-pass into a single state machine — see 55o4 for the
// planned per-stage extraction.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_vcarve_op<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(), PipelineError> {
    push_tool_fit_kind_warnings(op, project, setup, warnings);
    let tool = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;
    if !matches!(tool.kind, crate::project::ToolKind::VBit) {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tool_kind_mismatch".into(),
            message: format!(
                "V-Carve op '{}' uses tool '{}' which is not a V-bit. The carve depth is computed from the V-bit cone angle; engraver / endmill geometry won't produce a true V-groove.",
                op.name, tool.name
            ),
        });
    }
    let tip_angle_deg = tool.tip_angle_deg.clamp(1.0, 179.0);
    let tip_angle_rad = tip_angle_deg.to_radians();

    let selected = ordered_selection(op, objects);
    let combine = source_combine_mode(op);
    let regions = combine_source_regions(objects, &selected, combine);
    if regions.is_empty() {
        return Ok(());
    }

    let r_cap = op.params.carve_max_width_mm;
    let z_cap = if op.params.depth.abs() > 1e-9 {
        Some(op.params.depth)
    } else {
        None
    };
    let dpp = effective_step(op, tool)
        .map(f64::abs)
        .unwrap_or(1.0)
        .max(0.05);

    let mut polylines: Vec<Vec<(f64, f64, f64)>> = Vec::new();
    let mut any_depth_limited = false;

    for region in &regions {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        if region.boundary.len() < 3 {
            continue;
        }
        let vc_region = crate::cam::vcarve::VcRegion {
            outer: region.boundary.clone(),
            holes: region.holes.clone(),
        };
        let axes = crate::cam::vcarve::medial_axis_cancellable(&vc_region, cancel);
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        for axis in &axes {
            let (z_axis, depth_limited) =
                crate::cam::vcarve::polyline_to_z(axis, tip_angle_rad, r_cap, z_cap);
            if depth_limited {
                any_depth_limited = true;
            }
            let path = crate::cam::vcarve_emit::ratchet_emit(&z_axis, dpp);
            if path.len() >= 2 {
                polylines.push(path);
            }
        }
    }

    if any_depth_limited {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "vcarve_depth_limited".into(),
            message: format!(
                "V-Carve op '{}' was depth-limited: the V-bit can't reach the geometric corner because depth and/or carve_max_width caps clipped the inscribed-circle radius.",
                op.name
            ),
        });
    }

    if polylines.is_empty() {
        return Ok(());
    }

    crate::gcode::emit_vcarve_block(setup, &polylines, post, last_pos);
    Ok(())
}

/// Halfpipe pocket driver. Reuses V-Carve's medial-axis sweep but
/// derives the per-axis Z from the configured half-pipe profile
/// (`CircularArc { R }` ⇒ `z = -(R - sqrt(R² - r²))` capped at `-R`;
/// `VBottom { θ }` ⇒ `z = -r / tan(θ/2)`). Both clip to the op's
/// nominal `depth`.
// Halfpipe driver (Pocket strategy with cross-section profile) walks
// densified pocket regions per pass — see 55o4 for the planned split.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn run_halfpipe_op<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(), PipelineError> {
    let OpKind::Pocket {
        strategy: PocketStrategy::Halfpipe { profile: strategy },
    } = op.kind
    else {
        return Ok(());
    };
    push_tool_fit_kind_warnings(op, project, setup, warnings);
    let tool = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;
    // Profile-specific tool-kind hint. CircularArc wants a ball-nose
    // whose radius matches the configured R; VBottom wants a V-bit.
    match strategy {
        crate::project::HalfpipeProfile::CircularArc { radius_mm } => {
            if !matches!(tool.kind, crate::project::ToolKind::BallNose) {
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "tool_kind_mismatch".into(),
                    message: format!(
                        "Halfpipe (CircularArc) op '{}' uses tool '{}' which is not a ball-nose. The cut floor profile assumes a ball-bottom cutter; flat / V-bit will not produce a true half-pipe.",
                        op.name, tool.name
                    ),
                });
            }
            let tool_r = tool.diameter * 0.5;
            if (tool_r - radius_mm).abs() > 0.5 * radius_mm.max(1.0) {
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "halfpipe_radius_mismatch".into(),
                    message: format!(
                        "Halfpipe op '{}': tool radius {:.3} mm doesn't match the configured profile radius {:.3} mm — the cut won't trace the desired pipe.",
                        op.name, tool_r, radius_mm
                    ),
                });
            }
        }
        crate::project::HalfpipeProfile::VBottom { .. } => {
            if !matches!(tool.kind, crate::project::ToolKind::VBit) {
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "tool_kind_mismatch".into(),
                    message: format!(
                        "Halfpipe (VBottom) op '{}' uses tool '{}' which is not a V-bit; the depth math assumes a cone.",
                        op.name, tool.name
                    ),
                });
            }
        }
    }

    let selected = ordered_selection(op, objects);
    let combine = source_combine_mode(op);
    let regions = combine_source_regions(objects, &selected, combine);
    if regions.is_empty() {
        return Ok(());
    }

    let z_cap = if op.params.depth.abs() > 1e-9 {
        Some(op.params.depth)
    } else {
        None
    };
    let dpp = effective_step(op, tool)
        .map(f64::abs)
        .unwrap_or(1.0)
        .max(0.05);

    let mut polylines: Vec<Vec<(f64, f64, f64)>> = Vec::new();
    let mut any_depth_limited = false;

    for region in &regions {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        if region.boundary.len() < 3 {
            continue;
        }
        let vc_region = crate::cam::vcarve::VcRegion {
            outer: region.boundary.clone(),
            holes: region.holes.clone(),
        };
        let axes = crate::cam::vcarve::medial_axis_cancellable(&vc_region, cancel);
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        for axis in &axes {
            let (z_axis, depth_limited) =
                crate::cam::halfpipe::polyline_to_z(axis, strategy, z_cap);
            if depth_limited {
                any_depth_limited = true;
            }
            let path = crate::cam::vcarve_emit::ratchet_emit(&z_axis, dpp);
            if path.len() >= 2 {
                polylines.push(path);
            }
        }
    }

    if any_depth_limited {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "halfpipe_depth_limited".into(),
            message: format!(
                "Halfpipe op '{}' was depth-limited: the slot is wider than the configured profile cap (or the op's `depth` clipped it) at some medial-axis points.",
                op.name
            ),
        });
    }

    if polylines.is_empty() {
        return Ok(());
    }

    crate::gcode::emit_vcarve_block(setup, &polylines, post, last_pos);
    Ok(())
}

/// Helical thread emitter. For each selected closed circle in the
/// source set, compute the helix radius (bore − `tool_radius` for
/// internal, stud + `tool_radius` for external) and emit helix
/// waypoints between `start_depth` and `depth` at `pitch_mm` per
/// revolution. Reuses V-Carve's `emit_vcarve_block` since both walk a
/// pre-computed XYZ polyline at constant feed.
// Thread driver runs the per-circle helix walker; rather than threading
// state through five helpers, the per-revolution Z table lives inline.
// 55o4 tracks the broader pipeline split.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn run_thread_op<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(), PipelineError> {
    use crate::geometry::SegmentKind;
    let OpKind::Thread {
        pitch_mm,
        internal,
        climb,
    } = op.kind
    else {
        return Ok(());
    };
    let tool = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;
    let tool_radius = tool.diameter * 0.5;
    let top_z = op.params.start_depth;
    let bottom_z = op.params.depth;
    if (bottom_z - top_z).abs() < 1e-9 || pitch_mm <= 0.0 {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "thread_no_depth".into(),
            message: format!(
                "Thread op '{}' has zero Z range or non-positive pitch; nothing emitted.",
                op.name
            ),
        });
        return Ok(());
    }
    let mut polylines: Vec<Vec<(f64, f64, f64)>> = Vec::new();
    let mut emitted = 0usize;
    for (idx, obj) in objects.iter().enumerate() {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        if !op_includes_object(op, obj, idx) {
            continue;
        }
        if !obj.closed {
            continue;
        }
        // Accept any closed loop that is geometrically a circle:
        //   * A single Circle segment (the importer's preferred form).
        //   * A chain of Arc segments that all share the same center —
        //     what `chaining::segments_to_objects` produces for a
        //     DXF/SVG circle split into multiple arcs.
        let Some(first) = obj.segments.first() else {
            continue;
        };
        let (center, bore_radius) = match first.kind {
            SegmentKind::Circle => {
                let Some(c) = first.center else { continue };
                (c, first.start.distance(c))
            }
            SegmentKind::Arc => {
                let Some(c) = first.center else { continue };
                let radius = first.start.distance(c);
                let all_same_center = obj.segments.iter().all(|s| {
                    matches!(s.kind, SegmentKind::Arc | SegmentKind::Circle)
                        && s.center.is_some_and(|sc| {
                            (sc.x - c.x).abs() < 1e-4 && (sc.y - c.y).abs() < 1e-4
                        })
                });
                if !all_same_center {
                    continue;
                }
                (c, radius)
            }
            _ => continue,
        };
        let helix_radius = if internal {
            bore_radius - tool_radius
        } else {
            bore_radius + tool_radius
        };
        if helix_radius <= 0.05 {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "thread_tool_too_large".into(),
                message: format!(
                    "Thread op '{}': bore_radius {:.3} mm with tool_radius {:.3} mm leaves no room for an internal helix (needs bore > tool). Switch to external or pick a smaller cutter.",
                    op.name, bore_radius, tool_radius
                ),
            });
            continue;
        }
        let path = crate::cam::thread::helix_waypoints(
            center,
            helix_radius,
            top_z,
            bottom_z,
            pitch_mm,
            climb,
        );
        if path.len() >= 2 {
            polylines.push(path);
            emitted += 1;
        }
    }
    if emitted == 0 {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "thread_no_circles".into(),
            message: format!(
                "Thread op '{}' didn't find any closed circles in the selected source.",
                op.name
            ),
        });
        return Ok(());
    }
    crate::gcode::emit_vcarve_block(setup, &polylines, post, last_pos);
    Ok(())
}

/// Stufenfase (rt1.20): rim chamfer emitted after a drill block.
/// Walks one full revolution at each drilled hole's rim at z derived
/// from the cutter's tip angle and the user-set chamfer width.
/// Honors `op.finish_tool_id` for dual-tool drill+chamfer setups.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_stufenfase<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    drill_setup: &Setup,
    width_mm: f64,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<(), PipelineError> {
    use crate::geometry::SegmentKind;
    // Single full revolution at constant Z. 64 waypoints + closing
    // point so arc-fit produces clean one-or-two arcs.
    const STEPS: usize = 64;
    let cutter_id = op.finish_tool_id.unwrap_or(op.tool_id);
    let cutter = project
        .tools
        .iter()
        .find(|t| t.id == cutter_id)
        .ok_or(PipelineError::UnknownTool(op.id, cutter_id))?;
    let chamfer_z = crate::cam::chamfer::chamfer_depth(width_mm, cutter.tip_angle_deg);
    if chamfer_z.abs() < 1e-9 {
        return Ok(());
    }
    let mut polylines: Vec<Vec<(f64, f64, f64)>> = Vec::new();
    let mut found = 0usize;
    for (idx, obj) in objects.iter().enumerate() {
        if !op_includes_object(op, obj, idx) {
            continue;
        }
        if !obj.closed {
            continue;
        }
        let Some(first) = obj.segments.first() else {
            continue;
        };
        if !matches!(first.kind, SegmentKind::Circle) {
            continue;
        }
        let Some(center) = first.center else {
            continue;
        };
        let r = first.start.distance(center);
        if r < 0.05 {
            continue;
        }
        let mut flat: Vec<(f64, f64, f64)> = (0..=STEPS)
            .map(|i| {
                let a = (i as f64) * std::f64::consts::TAU / (STEPS as f64);
                (center.x + r * a.cos(), center.y + r * a.sin(), chamfer_z)
            })
            .collect();
        if let Some(&first) = flat.first() {
            if let Some(last) = flat.last_mut() {
                *last = first;
            }
        }
        polylines.push(flat);
        found += 1;
    }
    if found == 0 {
        return Ok(());
    }
    let mut chamfer_setup = drill_setup.clone();
    if op.finish_tool_id.is_some() && op.finish_tool_id != Some(op.tool_id) {
        if !project.machine.supports_toolchange {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "stufenfase_no_toolchange".into(),
                message: format!(
                    "drill op '{}' has chamfer_after_width_mm + a distinct finish_tool_id but the machine doesn't support toolchange; gcode will assume manual change.",
                    op.name
                ),
            });
        }
        if let Some(finish_setup) = synthesize_finish_setup(op, project, warnings)? {
            post.raw(&format!(
                "; stufenfase: toolchange to {} for hole-rim chamfer",
                finish_setup.tool.number
            ));
            post.tool(finish_setup.tool.number);
            if let Some(shift) = cutter.z_shift_mm {
                post.tool_z_shift(shift);
            }
            chamfer_setup = finish_setup;
        }
    }
    crate::gcode::emit_vcarve_block(&chamfer_setup, &polylines, post, last_pos);
    Ok(())
}

/// Standard offset-cascade emitter: runs [`build_op_offsets`] for the op,
/// emits a single `; OP <id>` marker, then dispatches by op kind:
///
/// * **Drill** — `emit_drill_block` with the canned cycle resolved
///   against the tool's default peck step. If the op carries a
///   `chamfer_after_width_mm` (Stufenfase / rt1.20), follow with a
///   single-revolution rim chamfer via [`emit_stufenfase`].
/// * **Profile / Pocket / Engrave / `DragKnife` / Chamfer** —
///   `emit_polylines_block` for the rough offsets, with an optional
///   dual-tool finish ring (rt1.33): the offsets cascade tags
///   `is_finish` on the wall-defining ring, this function splits at
///   that boundary and inserts a M6 toolchange + finish-tool setup.
///
/// Returns `(closed_count, offset_count)` so the caller can fold the
/// numbers into [`super::PipelineStats`] without re-walking the
/// returned offsets.
///
/// Cloned objects: [`build_op_offsets`] mutates its `&mut Vec<VcObject>`
/// in place for pattern expansion + Pocket-Outside frame insertion.
/// To keep the caller's `objects` set pristine for the next op in the
/// loop (and for sibling drivers like `run_vcarve_op`), we pass a fresh
/// `.to_vec()` clone here. Cheap: a `VcObject` is ~hundreds of bytes,
/// op count rarely exceeds a few dozen per project.
// Like build_op_offsets, this orchestrator interleaves Drill / dual-tool /
// single-tool dispatch with shared per-op state (offset sets, finish setup,
// toolchange comments). Splitting would scatter the state per-arm. 55o4
// epic tracks the planned per-kind extraction.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn run_standard_op<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(usize, usize), PipelineError> {
    let (offsets, closed_count) =
        build_op_offsets(op, project, &mut objects.to_vec(), setup, warnings, cancel)?;
    let offset_count = offsets.len();
    post.raw(&format!("; OP {}", op.id));
    if !offsets.is_empty() {
        if let OpKind::Drill { cycle } = op.kind {
            // Peck cycles fall back to the tool's `default_peck_step_mm`
            // when the op's own peck_step_mm is unset (== 0).
            let resolved_cycle = resolve_peck_step(cycle, project, op);
            emit_drill_block(setup, &offsets, resolved_cycle, post, last_pos);
            // rt1.20 (Stufenfase): when the drill op carries a
            // chamfer-after width, walk a single revolution at each
            // hole's rim at the V-bit chamfer depth.
            if let Some(w) = op.params.chamfer_after_width_mm {
                if w > 0.0 {
                    emit_stufenfase(op, project, objects, setup, w, post, last_pos, warnings)?;
                }
            }
        } else {
            // Dual-tool Pocket (rt1.33): split offsets at the
            // is_finish boundary, emit the rough block with the op's
            // tool setup, insert a M6 toolchange to the finish tool,
            // emit the finish block with the finish tool's setup.
            // Single-tool ops fall through to a single
            // emit_polylines_block call.
            let dual = synthesize_finish_setup(op, project, warnings)?;
            let has_finish_offsets = offsets.iter().any(|o| o.is_finish);
            if let (Some(finish_setup), true) = (&dual, has_finish_offsets) {
                let (rough_offsets, finish_offsets): (Vec<_>, Vec<_>) =
                    offsets.iter().cloned().partition(|o| !o.is_finish);
                if !rough_offsets.is_empty() {
                    emit_polylines_block(setup, &rough_offsets, post, last_pos);
                }
                // Toolchange + comment. post.tool() emits T<n> M6 for
                // posts that support it; no-op posts (GRBL) skip
                // silently, and we emit a pipeline warning when the
                // machine isn't toolchange-capable so the user can
                // spot it.
                if !project.machine.supports_toolchange {
                    warnings.push(PipelineWarning {
                        op_id: Some(op.id),
                        kind: "dual_tool_no_toolchange".into(),
                        message: format!(
                            "op '{}' uses a dual-tool setup (rough + finish) but the machine config has supports_toolchange=false; the gcode will assume a manual tool change.",
                            op.name
                        ),
                    });
                }
                post.raw(&format!(
                    "; toolchange: finish pass with tool {}",
                    finish_setup.tool.number
                ));
                post.tool(finish_setup.tool.number);
                // rt1.30: re-apply Z shift for the finish tool after
                // the M6. Each tool's shift is absolute (set such that
                // the work-Z=0 line matches the reference tool); we
                // just emit the new value.
                if let Some(ft_id) = op.finish_tool_id {
                    if let Some(ft) = project.tools.iter().find(|t| t.id == ft_id) {
                        if let Some(shift) = ft.z_shift_mm {
                            post.tool_z_shift(shift);
                        }
                    }
                }
                if !finish_offsets.is_empty() {
                    emit_polylines_block(finish_setup, &finish_offsets, post, last_pos);
                }
            } else {
                emit_polylines_block(setup, &offsets, post, last_pos);
            }
        }
    }
    Ok((closed_count, offset_count))
}
