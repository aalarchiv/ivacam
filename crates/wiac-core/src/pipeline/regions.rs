//! Region-preview construction + the small helper for synthesising a
//! [`VcObject`] from a [`CombinedRegion`]. The preview pass mirrors
//! the per-op driver (`build_op_offsets`) so the UI and the emitted
//! G-code agree on which boundaries pertain to which op.

use crate::cam::source_combine::{combine_source_regions, CombinedRegion};
use crate::cam::VcObject;
use crate::geometry::Segment;
use crate::project::{OpKind, Project, SourceCombine};

use super::frame::synthesize_pocket_outside_objects;
use super::{ordered_selection, source_combine_mode, RegionPreview};

/// Compute the filled-region preview for every enabled Pocket op. Auto
/// mode runs through the same containment-aware logic as the per-op
/// driver; explicit modes route through the clipper2 boolean ops in
/// `cam::source_combine`. Non-Pocket ops contribute nothing.
pub(super) fn build_region_previews(project: &Project, objects: &[VcObject]) -> Vec<RegionPreview> {
    let mut out = Vec::new();
    for op in project.operations.iter().filter(|o| o.enabled) {
        if !matches!(op.kind, OpKind::Pocket { .. }) {
            continue;
        }
        // Pocket-Outside (rt1.3) preview: when the op declares a frame,
        // synthesize the frame + ordered-ids the same way the toolpath
        // driver does (`synthesize_pocket_outside_objects`) so preview
        // and emit stay in lockstep.
        //
        // nahx: route through `frame_preview_regions` so the difference
        // pass that emits the frame's outer + selection holes is
        // shape-agnostic — Rectangle, RoundedRectangle, and any future
        // frame shape (circle / polygon / hull) all surface the same
        // (outer, holes) pair to the frontend. Previously this site
        // inlined the difference call; keeping it in one helper makes
        // adding a new FrameShape variant a single-site change.
        if op.pocket_params().is_some_and(|p| p.frame_shape.is_some()) {
            let tool_radius = project
                .tools
                .iter()
                .find(|t| t.id == op.tool_id)
                .map_or(0.0, |t| t.diameter * 0.5);
            if let Some((local_objects, ordered)) =
                synthesize_pocket_outside_objects(op, objects, tool_radius)
            {
                out.extend(frame_preview_regions(&local_objects, &ordered, op.id));
            }
            continue;
        }
        let selected = ordered_selection(op, objects);
        let mode = source_combine_mode(op);
        let regions = combine_source_regions(objects, &selected, mode);
        for r in regions {
            out.push(RegionPreview {
                op_id: op.id,
                outer: r.boundary,
                holes: r.holes,
            });
        }
    }
    out
}

/// nahx: build a hole-preserving region preview for a frame op given a
/// pre-built ordered selection `[frame_idx, ...selection_idxs]`. The
/// frame op's preview is `frame - selection`, which always produces an
/// outer (the frame's footprint) plus N holes (each selection object
/// the cutter must avoid). Today's FrameShape variants — Rectangle and
/// RoundedRectangle — both flow through this path; future variants
/// (circle, polygon, hull) will too because the difference + hole-
/// preserving conversion is shape-agnostic.
fn frame_preview_regions(
    local_objects: &[VcObject],
    ordered: &[usize],
    op_id: u32,
) -> Vec<RegionPreview> {
    combine_source_regions(local_objects, ordered, SourceCombine::Difference)
        .into_iter()
        .map(|r| RegionPreview {
            op_id,
            outer: r.boundary,
            holes: r.holes,
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::cam::source_combine::FrameShape;
    use crate::geometry::Point2;
    use crate::pipeline::test_helpers::{closed_circle, endmill, project_with_segments};
    use crate::project::{
        ContourParams, Op, OpKind, OpParams, OpSource, PocketParams, PocketStrategy,
    };

    fn pocket_outside_op(frame_shape: FrameShape) -> Op {
        Op {
            id: 1,
            name: "pocket-outside".into(),
            enabled: true,
            kind: OpKind::Pocket {
                strategy: PocketStrategy::Cascade,
                contour: ContourParams::default(),
                pocket: PocketParams {
                    frame_shape: Some(frame_shape),
                    frame_padding_mm: Some(2.0),
                    ..PocketParams::default()
                },
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
        }
    }

    /// nahx: a Rectangle-frame Pocket-Outside op produces a preview
    /// whose outer is the (padded) bbox and whose holes contain the
    /// selection's geometry. Without the hole, the frontend's filled-
    /// area paint covers the selection — wrong.
    #[test]
    fn rectangle_frame_preview_carries_selection_as_hole() {
        let project = project_with_segments(
            closed_circle(Point2::new(0.0, 0.0), 5.0),
            vec![pocket_outside_op(FrameShape::Rectangle)],
            vec![endmill(1, 3.0)],
        );
        let objects = crate::cam::chaining::segments_to_objects(&project.segments);
        let regions = build_region_previews(&project, &objects);
        assert_eq!(regions.len(), 1, "expected exactly one region");
        let r = &regions[0];
        assert!(
            !r.holes.is_empty(),
            "rectangle frame must carry the selection as a hole"
        );
    }

    /// nahx: same guarantee under a RoundedRectangle frame. The audit
    /// flagged this case because the difference call previously could
    /// have lost the hole boundary if the rounded outer collapsed or
    /// the polytree-to-region conversion ignored children.
    #[test]
    fn rounded_rectangle_frame_preview_carries_selection_as_hole() {
        let project = project_with_segments(
            closed_circle(Point2::new(0.0, 0.0), 5.0),
            vec![pocket_outside_op(FrameShape::RoundedRectangle)],
            vec![endmill(1, 3.0)],
        );
        let objects = crate::cam::chaining::segments_to_objects(&project.segments);
        let regions = build_region_previews(&project, &objects);
        assert_eq!(regions.len(), 1, "expected exactly one region");
        let r = &regions[0];
        assert!(
            !r.holes.is_empty(),
            "rounded rectangle frame must carry the selection as a hole"
        );
        // The outer should be a curved polyline (more than 4 points)
        // because the rounded corners tessellate; this is a coarse
        // sanity check that the rounded variant didn't silently collapse
        // to a flat rectangle.
        assert!(
            r.outer.len() > 4,
            "rounded outer should tessellate to >4 points, got {}",
            r.outer.len()
        );
    }
}

/// Build a synthetic [`VcObject`] from a [`CombinedRegion`]'s boundary
/// so it can be fed into `pocket_for_object` (which is shaped around
/// `VcObjects`). The region's holes are passed alongside as islands;
/// only the outer boundary lives in this object.
pub(super) fn synthesize_region_object(region: &CombinedRegion) -> VcObject {
    let pts = &region.boundary;
    let mut segments = Vec::with_capacity(pts.len());
    for win in pts.windows(2) {
        segments.push(Segment::line(
            win[0],
            win[1],
            region.layer.clone(),
            region.color,
        ));
    }
    if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
        if first.distance(*last) > 1e-6 {
            segments.push(Segment::line(
                *last,
                *first,
                region.layer.clone(),
                region.color,
            ));
        }
    }
    let mut obj = VcObject::new(segments, true);
    obj.layer.clone_from(&region.layer);
    obj.color = region.color;
    obj
}
