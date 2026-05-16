//! Region-preview construction + the small helper for synthesising a
//! [`VcObject`] from a [`CombinedRegion`]. The preview pass mirrors
//! the per-op driver (`build_op_offsets`) so the UI and the emitted
//! G-code agree on which boundaries pertain to which op.

use crate::cam::source_combine::{combine_source_regions, CombinedRegion};
use crate::cam::VcObject;
use crate::geometry::Segment;
use crate::project::{OperationKind, Project, SourceCombine};

use super::frame::synthesize_pocket_outside_objects;
use super::{ordered_selection, source_combine_mode, RegionPreview};

/// Compute the filled-region preview for every enabled Pocket op. Auto
/// mode runs through the same containment-aware logic as the per-op
/// driver; explicit modes route through the clipper2 boolean ops in
/// cam::source_combine. Non-Pocket ops contribute nothing.
pub(super) fn build_region_previews(project: &Project, objects: &[VcObject]) -> Vec<RegionPreview> {
    let mut out = Vec::new();
    for op in project.operations.iter().filter(|o| o.enabled) {
        if !matches!(op.kind, OperationKind::Pocket { .. }) {
            continue;
        }
        // Pocket-Outside (rt1.3) preview: when the op declares a frame,
        // synthesize the frame + ordered-ids the same way the toolpath
        // driver does (`synthesize_pocket_outside_objects`) so preview
        // and emit stay in lockstep.
        if op.params.frame_shape.is_some() {
            let tool_radius = project
                .tools
                .iter()
                .find(|t| t.id == op.tool_id)
                .map_or(0.0, |t| t.diameter * 0.5);
            if let Some((local_objects, ordered)) =
                synthesize_pocket_outside_objects(op, objects, tool_radius)
            {
                let regions =
                    combine_source_regions(&local_objects, &ordered, SourceCombine::Difference);
                for r in regions {
                    out.push(RegionPreview {
                        op_id: op.id,
                        outer: r.boundary,
                        holes: r.holes,
                    });
                }
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

/// Build a synthetic [`VcObject`] from a [`CombinedRegion`]'s boundary
/// so it can be fed into `pocket_for_object` (which is shaped around
/// VcObjects). The region's holes are passed alongside as islands;
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
    obj.layer = region.layer.clone();
    obj.color = region.color;
    obj
}
