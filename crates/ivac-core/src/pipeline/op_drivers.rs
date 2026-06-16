//! Per-op-kind drivers that don't fit the standard offset-cascade path,
//! plus the dispatcher `run_standard_op` for the kinds that do.
//!
//! Drivers in this module:
//!
//! * [`run_vcarve_op`] — V-Carve medial-axis sweep.
//! * [`run_halfpipe_op`] — Halfpipe pocket (circular-arc / V-bottom).
//! * [`run_thread_op`] — single-point helical thread.
//! * [`run_standard_op`] — Profile / Pocket / Engrave / Drill /
//!   `DragKnife` / Chamfer. Calls [`offset_builder::build_op_offsets`]
//!   to produce the offset cascade, then dispatches to either
//!   [`drill::run_drill`] (`OpKind::Drill`) or
//!   [`dual_tool::run_dual_tool_or_single`] (everything else).
//!
//! All sub-drivers reuse `gcode::emit_vcarve_block` /
//! `emit_polylines_block` / `emit_drill_block` for the actual G-code
//! emission since each produces XYZ polylines the emitter walks with
//! G1 cuts and safe-Z rapids.

mod drill;
mod dual_tool;
mod halfpipe;
mod raster;
mod surface_mill;
mod thread;
mod vcarve;

pub(in crate::pipeline) use halfpipe::{halfpipe_would_emit, run_halfpipe_op};
pub(in crate::pipeline) use raster::{raster_would_emit, run_raster_op};
pub(in crate::pipeline) use surface_mill::{relief_would_emit, run_relief_op};
pub(in crate::pipeline) use thread::{run_thread_op, thread_would_emit};
pub(in crate::pipeline) use vcarve::{run_vcarve_op, vcarve_would_emit};

use super::offset_builder::build_op_offsets;
use crate::cam::setup::Setup;
use crate::cam::VcObject;
use crate::gcode::PostProcessor;
use crate::geometry::Point2;
use crate::pipeline::{CancelToken, PipelineError, PipelineWarning};
use crate::project::{Op, OpKind, Project};

/// Standard offset-cascade dispatcher. Runs [`build_op_offsets`] for
/// the op, emits the `; OP <id>` marker, then hands the offsets to
/// the kind-specific sub-driver:
///
/// * **Drill** → [`drill::run_drill`] — canned drill cycle, optional
///   Stufenfase rim chamfer.
/// * **Profile / Pocket / Engrave / `DragKnife` / Chamfer** →
///   [`dual_tool::run_dual_tool_or_single`] — single
///   `emit_polylines_block` for the rough offsets, with an optional
///   dual-tool finish ring.
///
/// Returns `(closed_count, offset_count, swapped)` so the caller can
/// fold the numbers into [`super::PipelineStats`] without re-walking
/// the returned offsets. `swapped` is `true` when the kind-specific
/// sub-driver actually emitted an internal dual-tool toolchange
/// (rough→finish, drill→chamfer); used by `run_per_op` to bias
/// `prev_tool_id` only when a real swap happened.
///
/// `build_op_offsets` takes `&[VcObject]` and produces pattern / frame
/// expansions in locally-owned `Vec<VcObject>`s. The caller does not
/// need a defensive `.to_vec()` clone per op — that would make a 50-op
/// project on a 5000-segment DXF clone the full vec 50 times every Generate.
#[allow(clippy::too_many_arguments)]
pub(in crate::pipeline) fn run_standard_op<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(usize, usize, bool), PipelineError> {
    let (offsets, closed_count) = build_op_offsets(op, project, objects, setup, warnings, cancel)?;
    let offset_count = offsets.len();
    post.raw(&format!("; OP {}", op.id));
    let mut swapped = false;
    if !offsets.is_empty() {
        swapped = if let OpKind::Drill { cycle, .. } = op.kind {
            drill::run_drill(
                op, project, objects, setup, &offsets, cycle, post, last_pos, warnings,
            )?
        } else {
            dual_tool::run_dual_tool_or_single(
                op, project, setup, &offsets, post, last_pos, warnings,
            )?
        };
    }
    Ok((closed_count, offset_count, swapped))
}
