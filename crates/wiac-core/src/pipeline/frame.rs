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
    // kbx5 step 2: read frame fields from PocketParams; non-Pocket ops
    // never carry a frame so they short-circuit to None.
    let pocket = op.pocket_params()?;
    let frame_shape = pocket.frame_shape?;
    let selected_indices: Vec<usize> = (0..objects.len())
        .filter(|i| op_includes_object(op, &objects[*i], *i))
        .collect();
    if selected_indices.is_empty() {
        return None;
    }
    let frame = {
        let frame_selection: Vec<&VcObject> =
            selected_indices.iter().map(|&i| &objects[i]).collect();
        let user_padding = pocket.frame_padding_mm.unwrap_or(0.0).max(0.0);
        // l8fz: the real outward extent of the source contour grows
        // beyond the raw selection bbox when the op carries lead-in
        // arcs (radius), tabs (width along the cut chord), or a
        // finish-pass stock allowance (`finish_xy_allowance_mm`). The
        // frame walls have to clear ALL of those — otherwise the cutter
        // chews into the user-set frame instead of the planned air gap.
        // Compute the worst single contributor and stack it on top of
        // the user's padding so a 5 mm lead-arc + 2 mm frame_padding
        // yields a 7 mm air gap as the docstring promises.
        let contour = op.contour_params();
        let lead_in = contour.map_or(0.0, |c| {
            if matches!(c.leads.r#in, crate::cam::setup::LeadKind::Off) {
                0.0
            } else {
                c.leads.in_lenght.max(0.0)
            }
        });
        let lead_out = contour.map_or(0.0, |c| {
            if matches!(c.leads.out, crate::cam::setup::LeadKind::Off) {
                0.0
            } else {
                c.leads.out_lenght.max(0.0)
            }
        });
        // Tabs grow the cutter envelope by their chord width — the
        // cutter traces the tab profile sideways before rejoining the
        // contour. Honor either the legacy `tabs.active` flag or a
        // non-Off tab placement mode.
        let tabs_outward = contour.map_or(0.0, |c| {
            let placed = !matches!(c.tab_mode, crate::project::TabPlacementMode::Off);
            if c.tabs.active || placed {
                c.tabs.width.max(0.0)
            } else {
                0.0
            }
        });
        let stock_allowance = pocket.finish_xy_allowance_mm.unwrap_or(0.0).max(0.0);
        let outward = lead_in.max(lead_out).max(tabs_outward).max(stock_allowance);
        let padding = user_padding.max(tool_radius_mm.max(0.0)) + outward;
        build_frame(
            &frame_selection,
            frame_shape,
            padding,
            pocket.frame_corner_radius_mm,
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

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::cam::setup::{LeadKind, LeadsConfig, TabsConfig};
    use crate::cam::source_combine::FrameShape;
    use crate::cam::VcObject;
    use crate::geometry::{Point2, Segment};
    use crate::project::{
        ContourParams, Op, OpKind, OpParams, OpSource, PocketParams, PocketStrategy,
    };

    fn closed_square_obj(side: f64) -> VcObject {
        VcObject::new(
            vec![
                Segment::line(Point2::new(0.0, 0.0), Point2::new(side, 0.0), "0", 7),
                Segment::line(Point2::new(side, 0.0), Point2::new(side, side), "0", 7),
                Segment::line(Point2::new(side, side), Point2::new(0.0, side), "0", 7),
                Segment::line(Point2::new(0.0, side), Point2::new(0.0, 0.0), "0", 7),
            ],
            true,
        )
    }

    fn pocket_frame_op(
        contour: ContourParams,
        pocket_padding_mm: Option<f64>,
        finish_xy_allowance_mm: Option<f64>,
    ) -> Op {
        Op {
            id: 1,
            name: "pocket-outside".into(),
            enabled: true,
            kind: OpKind::Pocket {
                strategy: PocketStrategy::Cascade,
                contour,
                pocket: PocketParams {
                    frame_shape: Some(FrameShape::Rectangle),
                    frame_padding_mm: pocket_padding_mm,
                    finish_xy_allowance_mm,
                    ..PocketParams::default()
                },
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }
    }

    fn frame_bbox_min_x(objects: &[VcObject], frame_idx: usize) -> f64 {
        objects[frame_idx]
            .segments
            .iter()
            .flat_map(|s| [s.start.x, s.end.x])
            .fold(f64::INFINITY, f64::min)
    }

    /// l8fz: lead-arc length 5 mm + `frame_padding` 2 mm should yield a
    /// frame that sits 7 mm outside the source contour (lead-arc adds to
    /// padding, not max with).
    #[test]
    fn frame_envelope_includes_lead_in_arc() {
        let mut contour = ContourParams::default();
        contour.leads = LeadsConfig {
            r#in: LeadKind::Arc,
            out: LeadKind::Off,
            in_lenght: 5.0,
            out_lenght: 0.0,
        };
        let op = pocket_frame_op(contour, Some(2.0), None);
        let objects = vec![closed_square_obj(20.0)];
        // tool_radius = 0 to isolate the lead/tabs/allowance contribution.
        let (new_objects, ordered) =
            synthesize_pocket_outside_objects(&op, &objects, 0.0).expect("frame synthesized");
        let frame_idx = ordered[0];
        let min_x = frame_bbox_min_x(&new_objects, frame_idx);
        // Square at x ∈ [0, 20]. Frame should sit at x = -(2 + 5) = -7.
        assert!(
            (min_x - (-7.0)).abs() < 1e-6,
            "expected frame min_x = -7.0 (padding 2 + lead 5), got {min_x}"
        );
    }

    /// l8fz: `finish_xy_allowance` pushes the frame outward too.
    #[test]
    fn frame_envelope_includes_finish_xy_allowance() {
        let op = pocket_frame_op(ContourParams::default(), Some(1.0), Some(3.0));
        let objects = vec![closed_square_obj(20.0)];
        let (new_objects, ordered) =
            synthesize_pocket_outside_objects(&op, &objects, 0.0).expect("frame synthesized");
        let frame_idx = ordered[0];
        let min_x = frame_bbox_min_x(&new_objects, frame_idx);
        // padding 1 + allowance 3 = 4 mm outward
        assert!(
            (min_x - (-4.0)).abs() < 1e-6,
            "expected frame min_x = -4.0 (padding 1 + allowance 3), got {min_x}"
        );
    }

    /// l8fz: tabs widen the outward extent by tabs.width when the
    /// op carries an active tabs config.
    #[test]
    fn frame_envelope_includes_tab_width() {
        let mut contour = ContourParams::default();
        contour.tabs = TabsConfig {
            active: true,
            width: 6.0,
            height: 1.0,
            ..TabsConfig::default()
        };
        let op = pocket_frame_op(contour, Some(2.0), None);
        let objects = vec![closed_square_obj(20.0)];
        let (new_objects, ordered) =
            synthesize_pocket_outside_objects(&op, &objects, 0.0).expect("frame synthesized");
        let frame_idx = ordered[0];
        let min_x = frame_bbox_min_x(&new_objects, frame_idx);
        // padding 2 + tab width 6 = 8 mm outward.
        assert!(
            (min_x - (-8.0)).abs() < 1e-6,
            "expected frame min_x = -8.0 (padding 2 + tab width 6), got {min_x}"
        );
    }
}
