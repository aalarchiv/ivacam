//! Dual-tool finish dispatch (rt1.33).
//!
//! Run from [`super::run_standard_op`] for any non-Drill op kind:
//! if the op declares a finish tool AND the offsets cascade produced
//! at least one ring tagged `is_finish`, split at that boundary and
//! emit `rough → M6 toolchange → finish`. Otherwise fall through to
//! a single [`emit_polylines_block`] call with the op's primary
//! setup.
//!
//! All Profile / Pocket / Engrave / Chamfer / `DragKnife` ops share
//! this code path — the only per-kind behaviour is whether the
//! offsets cascade decided to emit a finish ring. The driver itself
//! is kind-agnostic.

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::Setup;
use crate::gcode::{emit_polylines_block, PostProcessor};
use crate::geometry::Point2;
use crate::pipeline::{synthesize_finish_setup, PipelineError, PipelineWarning};
use crate::project::{Op, Project};

#[allow(clippy::too_many_arguments)]
pub(super) fn run_dual_tool_or_single<P: PostProcessor>(
    op: &Op,
    project: &Project,
    setup: &Setup,
    offsets: &[PolylineOffset],
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<(), PipelineError> {
    let dual = synthesize_finish_setup(op, project, warnings)?;
    let has_finish_offsets = offsets.iter().any(|o| o.is_finish);
    let Some(finish_setup) = dual.filter(|_| has_finish_offsets) else {
        // Plain single-tool single-emit path — the common case for
        // Profile / Pocket / Engrave / etc. without a finish ring.
        emit_polylines_block(setup, offsets, post, last_pos);
        return Ok(());
    };

    let (rough_offsets, finish_offsets): (Vec<_>, Vec<_>) =
        offsets.iter().cloned().partition(|o| !o.is_finish);
    if !rough_offsets.is_empty() {
        emit_polylines_block(setup, &rough_offsets, post, last_pos);
    }
    // Toolchange + comment. post.tool() emits T<n> M6 for posts that
    // support it; no-op posts (GRBL) skip silently. Surface a
    // pipeline warning when the machine isn't toolchange-capable so
    // the user spots the manual-intervention requirement.
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
    // rt1.30: re-apply Z shift for the finish tool after the M6.
    // Each tool's shift is absolute (set such that the work-Z=0 line
    // matches the reference tool); we just emit the new value.
    if let Some(ft_id) = op.finish_tool_id {
        if let Some(ft) = project.tools.iter().find(|t| t.id == ft_id) {
            if let Some(shift) = ft.z_shift_mm {
                post.tool_z_shift(shift);
            }
        }
    }
    if !finish_offsets.is_empty() {
        emit_polylines_block(&finish_setup, &finish_offsets, post, last_pos);
    }
    Ok(())
}
