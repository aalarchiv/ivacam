//! Lead-in / lead-out geometry: where the cutter rolls onto and off the contour. Straight or quarter-arc, on the FREE side of the tangent (away from stock).

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

use crate::cam::setup::{LeadKind, Setup, ToolOffset};
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::math;

/// Geometry of a lead-in or lead-out move.
///
/// `Straight` keeps the legacy "perpendicular hop" lead — the approach
/// (lead-in) or exit (lead-out) point sits `in_lenght` mm to the LEFT of
/// the contour tangent, and the cutter travels in a straight line.
///
/// `Arc` is a tangent roll-on / roll-off: a quarter-circle of `in_lenght`
/// mm radius whose center is `radius` perpendicular to the tangent on the
/// LEFT (same convention as Straight). The arc lands tangent to the
/// contour at the entry/exit point, so the cutter eases into / out of the
/// cut without dwelling at the start. `entry_or_exit` is the off-contour
/// endpoint of the arc (lead-in: WHERE we G0 to before arcing onto the
/// contour; lead-out: WHERE we end up after arcing off the contour).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LeadGeometry {
    None,
    Straight {
        from: Point2,
    },
    Arc {
        entry_or_exit: Point2,
        center: Point2,
        ccw: bool,
    },
}

/// Compute the unit tangent at the START of the first segment in a cut
/// path. For a Line, this is just the direction from start→end; for an
/// Arc / Circle, it's the radius vector rotated 90° in the arc's
/// orientation (CCW for positive bulge, CW for negative).
fn first_segment_start_tangent(seg: &Segment) -> Option<(f64, f64)> {
    match seg.kind {
        SegmentKind::Line | SegmentKind::Point => {
            let dx = seg.end.x - seg.start.x;
            let dy = seg.end.y - seg.start.y;
            let n = (dx * dx + dy * dy).sqrt();
            if n < 1e-12 {
                None
            } else {
                Some((dx / n, dy / n))
            }
        }
        SegmentKind::Arc | SegmentKind::Circle => {
            let center = seg
                .center
                .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
            let rx = seg.start.x - center.x;
            let ry = seg.start.y - center.y;
            let n = (rx * rx + ry * ry).sqrt();
            if n < 1e-12 {
                return None;
            }
            // CCW (bulge > 0): tangent at start = rotate radius 90° CCW.
            // CW: rotate 90° CW.
            let (tx, ty) = if seg.bulge >= 0.0 {
                (-ry / n, rx / n)
            } else {
                (ry / n, -rx / n)
            };
            Some((tx, ty))
        }
    }
}

/// Tangent at the END of the last segment.
fn last_segment_end_tangent(seg: &Segment) -> Option<(f64, f64)> {
    match seg.kind {
        SegmentKind::Line | SegmentKind::Point => {
            let dx = seg.end.x - seg.start.x;
            let dy = seg.end.y - seg.start.y;
            let n = (dx * dx + dy * dy).sqrt();
            if n < 1e-12 {
                None
            } else {
                Some((dx / n, dy / n))
            }
        }
        SegmentKind::Arc | SegmentKind::Circle => {
            let center = seg
                .center
                .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
            let rx = seg.end.x - center.x;
            let ry = seg.end.y - center.y;
            let n = (rx * rx + ry * ry).sqrt();
            if n < 1e-12 {
                return None;
            }
            let (tx, ty) = if seg.bulge >= 0.0 {
                (-ry / n, rx / n)
            } else {
                (ry / n, -rx / n)
            };
            Some((tx, ty))
        }
    }
}

/// Chord-polygon signed area of a closed-ish offset polyline. Positive
/// = CCW, negative = CW. Bulge is ignored — the sign is dominated by
/// the chord winding except in pathological >180° arcs that the offset
/// pass would not produce.
fn polyline_signed_area(segments: &[Segment]) -> f64 {
    let mut a = 0.0;
    for s in segments {
        a += s.start.x * s.end.y - s.end.x * s.start.y;
    }
    a * 0.5
}

/// Decide which side of the tangent is FREE SPACE (no stock), so the
/// lead-in rapids in through air rather than carving into the part.
///
/// Rule:
///   * Outer profile (offset expanded outside the part) — the part
///     sits in the INTERIOR of the offset polygon. Free space is on
///     the side opposite the interior.
///   * Inner profile (pocket boundary, offset contracted) — free
///     space IS the interior (pocket center).
///
/// Winding tells us where the interior is: CCW (positive signed area)
/// ⇒ interior on the LEFT of tangent; CW ⇒ on the RIGHT.
///
/// Returns true when free space is on the LEFT of the tangent
/// (perpendicular CCW = `(-ty, tx)`); false ⇒ RIGHT (`(ty, -tx)`).
fn lead_free_side_left(setup: &Setup, segments: &[Segment]) -> bool {
    let ccw = polyline_signed_area(segments) > 0.0;
    let is_outer = matches!(setup.mill.offset, ToolOffset::Outside);
    // outer + ccw → interior left → free right;  outer + cw → free left
    // inner + ccw → interior left = free left;   inner + cw → free right
    // ⇒ free_left = is_outer XOR ccw == !is_outer && ccw || is_outer && !ccw
    is_outer != ccw
}

pub(crate) fn lead_in_geometry(setup: &Setup, segments: &[Segment]) -> LeadGeometry {
    if setup.leads.r#in == LeadKind::Off || segments.is_empty() {
        return LeadGeometry::None;
    }
    let len = setup.leads.in_lenght.max(0.0);
    if len < 1e-9 {
        return LeadGeometry::None;
    }
    let first = &segments[0];
    let Some((tx, ty)) = first_segment_start_tangent(first) else {
        return LeadGeometry::None;
    };
    let free_left = lead_free_side_left(setup, segments);
    let (px, py) = if free_left { (-ty, tx) } else { (ty, -tx) };
    match setup.leads.r#in {
        LeadKind::Straight => LeadGeometry::Straight {
            from: Point2::new(first.start.x + len * px, first.start.y + len * py),
        },
        LeadKind::Arc => {
            // Quarter-arc roll-on:
            //   center    = P0 + perp_free * radius
            //   arc_start = P0 + radius * (perp_free - tangent)
            // Sweep direction follows the perpendicular hand: free-on-
            // left ⇒ CCW (G3); free-on-right ⇒ CW (G2). Either way the
            // cutter lands at P0 tangent to (+tx, +ty).
            let radius = len;
            let center = Point2::new(first.start.x + radius * px, first.start.y + radius * py);
            let arc_start = Point2::new(
                first.start.x + radius * (px - tx),
                first.start.y + radius * (py - ty),
            );
            LeadGeometry::Arc {
                entry_or_exit: arc_start,
                center,
                ccw: free_left,
            }
        }
        LeadKind::Off => LeadGeometry::None,
    }
}

pub(crate) fn lead_out_geometry(setup: &Setup, segments: &[Segment]) -> LeadGeometry {
    if setup.leads.out == LeadKind::Off || segments.is_empty() {
        return LeadGeometry::None;
    }
    let len = setup.leads.out_lenght.max(0.0);
    if len < 1e-9 {
        return LeadGeometry::None;
    }
    let last = segments.last().unwrap();
    let Some((tx, ty)) = last_segment_end_tangent(last) else {
        return LeadGeometry::None;
    };
    let free_left = lead_free_side_left(setup, segments);
    let (px, py) = if free_left { (-ty, tx) } else { (ty, -tx) };
    match setup.leads.out {
        LeadKind::Straight => LeadGeometry::Straight {
            from: Point2::new(last.end.x + len * px, last.end.y + len * py),
        },
        LeadKind::Arc => {
            // Mirror of lead-in: cutter is at Pn moving along +t.
            //   center  = Pn + perp_free * radius
            //   arc_end = Pn + radius * (perp_free + tangent)
            // Sweep direction = free_left (CCW iff free is on the left).
            let radius = len;
            let center = Point2::new(last.end.x + radius * px, last.end.y + radius * py);
            let arc_end = Point2::new(
                last.end.x + radius * (px + tx),
                last.end.y + radius * (py + ty),
            );
            LeadGeometry::Arc {
                entry_or_exit: arc_end,
                center,
                ccw: free_left,
            }
        }
        LeadKind::Off => LeadGeometry::None,
    }
}
