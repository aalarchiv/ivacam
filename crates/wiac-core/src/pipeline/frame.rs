//! Pocket-Outside frame synthesis (rt1.3 / audit-57li). When a Pocket
//! op carries `frame_shape`, the pipeline auto-prepends a synthetic
//! frame [`VcObject`] derived from the op's current selection so the
//! downstream `SourceCombine::Difference` carves the area between the
//! frame and the original geometry. The frame is not persisted on the
//! project — recomputed every Generate from the op params.

use crate::cam::source_combine::build_frame;
use crate::cam::VcObject;
use crate::project::Op;

use super::op_includes_object;

/// Pocket-Outside (rt1.3) helper. When the op carries `frame_shape`,
/// builds the synthetic frame around the op's current selection and
/// returns `(new_objects, ordered_indices)` where:
///   * `new_objects` is `objects` with the frame appended at the end.
///   * `ordered_indices` lists `[frame_idx, ...selection_idxs]` so
///     downstream `SourceCombine::Difference` carves between the
///     frame and the original selection.
///
/// Returns `None` when the op has no `frame_shape` or the selection is
/// empty. Single source of truth used by both the preview pass
/// (`build_region_previews`) and the toolpath driver (`build_op_offsets`)
/// so they cannot drift.
///
/// `tool_radius_mm` clamps the lower bound of `frame_padding_mm`. With
/// frame `tool_offset = Inside`, the cutter centerline walks at
/// `bbox + padding - tool_radius`; if `padding < tool_radius` the
/// centerline ends up INSIDE the selection's bbox, so the cutter cuts
/// into the very shape it should be carving around. Clamping ensures
/// the geometry is well-formed regardless of user input.
pub(super) fn synthesize_pocket_outside_objects(
    op: &Op,
    objects: &[VcObject],
    tool_radius_mm: f64,
) -> Option<(Vec<VcObject>, Vec<usize>)> {
    let frame_shape = op.params.frame_shape?;
    let selected_indices: Vec<usize> = (0..objects.len())
        .filter(|i| op_includes_object(op, &objects[*i], *i))
        .collect();
    if selected_indices.is_empty() {
        return None;
    }
    let frame = {
        let frame_selection: Vec<&VcObject> =
            selected_indices.iter().map(|&i| &objects[i]).collect();
        let user_padding = op.params.frame_padding_mm.unwrap_or(0.0).max(0.0);
        let padding = user_padding.max(tool_radius_mm.max(0.0));
        build_frame(
            &frame_selection,
            frame_shape,
            padding,
            op.params.frame_corner_radius_mm,
        )
    };
    let mut new_objects = objects.to_vec();
    let frame_idx = new_objects.len();
    new_objects.push(frame);
    let mut ordered = Vec::with_capacity(selected_indices.len() + 1);
    ordered.push(frame_idx);
    ordered.extend(selected_indices);
    Some((new_objects, ordered))
}
