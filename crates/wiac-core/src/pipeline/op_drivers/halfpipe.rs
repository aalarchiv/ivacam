//! Halfpipe pocket driver. Reuses V-Carve's medial-axis sweep but
//! derives the per-axis Z from the configured half-pipe profile
//! (`CircularArc { R }` ⇒ `z = -(R - sqrt(R² - r²))` capped at `-R`;
//! `VBottom { θ }` ⇒ `z = -r / tan(θ/2)`). Both clip to the op's
//! nominal `depth`.

#![allow(clippy::cast_precision_loss)]

use crate::cam::setup::Setup;
use crate::cam::source_combine::combine_source_regions;
use crate::cam::VcObject;
use crate::gcode::{emit_vcarve_block, PostProcessor};
use crate::geometry::Point2;
use crate::pipeline::warnings::push_tool_fit_kind_warnings;
use crate::pipeline::{
    cancelled, effective_step, ordered_selection, source_combine_mode, CancelToken, PipelineError,
    PipelineWarning,
};
use crate::project::{Op, OpKind, PocketStrategy, Project};

// Halfpipe driver (Pocket strategy with cross-section profile) walks
// densified pocket regions per pass — see 55o4 for the planned split.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(in crate::pipeline) fn run_halfpipe_op<P: PostProcessor>(
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

    emit_vcarve_block(setup, &polylines, post, last_pos);
    Ok(())
}
