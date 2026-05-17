//! Drill op driver: canned drill cycle + optional Stufenfase rim
//! chamfer.
//!
//! Run from [`super::run_standard_op`] when the op kind matches
//! `OpKind::Drill { cycle }`. Emits the canned cycle via
//! `emit_drill_block`, then — when `chamfer_after_width_mm` is set
//! (rt1.20 Stufenfase) — walks a single revolution at each hole's
//! rim at the V-bit chamfer depth.

// CAM/sim pedantic-lint exemption: STEPS is a tiny constant, cast
// to f64 for trig is fine.
#![allow(clippy::cast_precision_loss)]

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::Setup;
use crate::cam::VcObject;
use crate::gcode::{emit_drill_block, emit_vcarve_block, PostProcessor};
use crate::geometry::{Point2, SegmentKind};
use crate::pipeline::setup_resolver::resolve_peck_step;
use crate::pipeline::{
    op_includes_object, synthesize_finish_setup, PipelineError, PipelineWarning,
};
use crate::project::{DrillCycle, Op, Project};

#[allow(clippy::too_many_arguments)]
pub(super) fn run_drill<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    offsets: &[PolylineOffset],
    cycle: DrillCycle,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<(), PipelineError> {
    // Peck cycles fall back to the tool's `default_peck_step_mm`
    // when the op's own peck_step_mm is unset (== 0).
    let resolved_cycle = resolve_peck_step(cycle, project, op);
    emit_drill_block(setup, offsets, resolved_cycle, post, last_pos);
    // rt1.20 (Stufenfase): when the drill op carries a
    // chamfer-after width, walk a single revolution at each hole's
    // rim at the V-bit chamfer depth.
    if let Some(w) = op.params.chamfer_after_width_mm {
        if w > 0.0 {
            emit_stufenfase(op, project, objects, setup, w, post, last_pos, warnings)?;
        }
    }
    Ok(())
}

/// Single full-revolution rim chamfer emitted after the drill block.
/// V-bit depth comes from the cutter's tip angle and the user-set
/// chamfer width. Honors `op.finish_tool_id` for dual-tool
/// drill+chamfer setups.
#[allow(clippy::too_many_arguments)]
fn emit_stufenfase<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    drill_setup: &Setup,
    width_mm: f64,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<(), PipelineError> {
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
    emit_vcarve_block(&chamfer_setup, &polylines, post, last_pos);
    Ok(())
}
