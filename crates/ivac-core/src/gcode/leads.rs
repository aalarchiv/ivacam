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

/// xmwy: open-contour test — the lead-in / lead-out helpers below
/// downgrade Arc → Straight when the path is open (start != end). On
/// an open slot the cutter enters and exits in free space already; a
/// tangent roll-on arc adds tool-path length and a 90° sweep that
/// the operator has no reason to want, and on small parts the arc's
/// swept disk often collides with stock left of the entry point
/// (which the `arc_lead_fits` check can't see — it only inspects the
/// contour itself, not unmilled stock around it).
fn is_closed_contour(segments: &[Segment]) -> bool {
    if segments.len() < 2 {
        return false;
    }
    let first = segments.first().unwrap().start;
    let last = segments.last().unwrap().end;
    (first.x - last.x).hypot(first.y - last.y) < 1e-3
}

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
    // xmwy: arc lead-in only makes geometric sense on a closed
    // contour where the cutter has to ease tangent to a continuous
    // wall. On an open slot the first segment ends in free space; the
    // arc adds path length + a 90° sweep with no quality benefit, and
    // on small slots the swept disk often grazes unmilled stock
    // beyond the contour (which arc_lead_fits can't detect). Demote
    // to a straight lead so the operator still gets a non-vertical
    // entry but without the redundant sweep.
    let kind = if matches!(setup.leads.r#in, LeadKind::Arc) && !is_closed_contour(segments) {
        LeadKind::Straight
    } else {
        setup.leads.r#in
    };
    match kind {
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
            // 62pd: validate that the arc envelope fits — the swept
            // quarter-disk of radius `radius` around `center` must not
            // overlap any non-adjacent contour wall, otherwise the
            // roll-on cuts into the part. If it doesn't fit, fall back
            // to a straight lead-in of the same length (still safer
            // than no lead-in, and shorter than carving the part).
            if arc_lead_fits(segments, center, radius, true) {
                LeadGeometry::Arc {
                    entry_or_exit: arc_start,
                    center,
                    ccw: free_left,
                }
            } else {
                LeadGeometry::Straight {
                    from: Point2::new(first.start.x + len * px, first.start.y + len * py),
                }
            }
        }
        LeadKind::Off => LeadGeometry::None,
    }
}

/// 62pd: does the arc-lead's swept disk overlap any contour wall that
/// isn't immediately adjacent to the entry / exit point?
///
/// Returns true when the disk of radius `radius` around `center`
/// touches NONE of the contour's chord segments past the adjacency
/// window — i.e. the arc has room. Adjacency window is the first /
/// last segment depending on `is_lead_in`: the lead naturally lands
/// tangent to that segment so a small overlap there is expected and
/// harmless.
fn arc_lead_fits(segments: &[Segment], center: Point2, radius: f64, is_lead_in: bool) -> bool {
    if segments.is_empty() || radius <= 0.0 {
        return true;
    }
    // Allow 1 % radius slack so a chord that brushes the envelope
    // doesn't trigger a fallback. We're guarding against real
    // collisions, not infinitesimal contact at the tangent point.
    let r_clear = radius * 0.99;
    // For a closed contour, the first AND last segments both share the
    // entry/exit vertex. Skip BOTH so a 1 mm arc-lead at the corner of
    // a closed square doesn't fail on the segment running back to the
    // start (which is geometrically adjacent and naturally close to
    // the arc envelope).
    let closed = is_closed_contour(segments);
    let last_idx = segments.len() - 1;
    let skip_anchor = if is_lead_in { 0 } else { last_idx };
    let skip_companion = if closed {
        Some(if is_lead_in { last_idx } else { 0 })
    } else {
        None
    };
    for (i, seg) in segments.iter().enumerate() {
        if i == skip_anchor {
            continue;
        }
        if Some(i) == skip_companion {
            continue;
        }
        let d = segment_distance_to_point(seg, center);
        if d < r_clear {
            return false;
        }
    }
    true
}

/// Shortest distance from `center` to a segment.
///
/// Lines: standard point-to-chord projection.
///
/// Arcs / Circles: u2u1 — the prior chord-based distance over-estimated
/// safety by the sagitta. A bulgy contour wall could carve into the
/// lead arc's swept disk while chord distance still reported "fits in
/// available room"; the lead-in then arc'd straight into the just-cut
/// surface. Use the true point-to-arc distance:
///   * If `center` projects onto the arc's angular sweep, the closest
///     point on the arc is `|distance(arc_center, center) - radius|`.
///   * Otherwise the nearest point is one of the arc endpoints — fall
///     back to the smaller endpoint distance.
fn segment_distance_to_point(seg: &Segment, center: Point2) -> f64 {
    use crate::geometry::SegmentKind;
    let line_chord_distance = |sx: f64, sy: f64, ex: f64, ey: f64| {
        let dx = ex - sx;
        let dy = ey - sy;
        let len_sq = dx * dx + dy * dy;
        if len_sq < 1e-18 {
            return (center.x - sx).hypot(center.y - sy);
        }
        let t = (((center.x - sx) * dx + (center.y - sy) * dy) / len_sq).clamp(0.0, 1.0);
        let px = sx + t * dx;
        let py = sy + t * dy;
        (center.x - px).hypot(center.y - py)
    };
    match seg.kind {
        SegmentKind::Line | SegmentKind::Point => {
            line_chord_distance(seg.start.x, seg.start.y, seg.end.x, seg.end.y)
        }
        SegmentKind::Arc | SegmentKind::Circle => {
            // Fall back to deriving the center from the bulge when
            // the segment data didn't carry it — same convention as
            // the surrounding arc-center derivations in this module.
            let arc_center = seg
                .center
                .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
            let arc_radius = (seg.start.x - arc_center.x).hypot(seg.start.y - arc_center.y);
            if arc_radius < 1e-9 {
                return line_chord_distance(seg.start.x, seg.start.y, seg.end.x, seg.end.y);
            }
            // Full circle (no angular gating): the closest point lies
            // along the radial line from `arc_center` toward `center`.
            let dx = center.x - arc_center.x;
            let dy = center.y - arc_center.y;
            let d_to_arc_center = dx.hypot(dy);
            let radial_dist = (d_to_arc_center - arc_radius).abs();
            if matches!(seg.kind, SegmentKind::Circle) {
                return radial_dist;
            }
            // Arc: check whether the radial foot falls inside the
            // angular span. Same machinery as `arc_intersects_tab` in
            // tabs.rs — sweep from theta_start through `4*atan(bulge)`.
            let theta_start = (seg.start.y - arc_center.y).atan2(seg.start.x - arc_center.x);
            let sweep = 4.0 * seg.bulge.atan();
            // Theta of the candidate foot on the circle (only well-
            // defined when `center` isn't at the arc center; fall back
            // to the endpoint distance if it is).
            if d_to_arc_center < 1e-12 {
                let de_s = (center.x - seg.start.x).hypot(center.y - seg.start.y);
                let de_e = (center.x - seg.end.x).hypot(center.y - seg.end.y);
                return de_s.min(de_e);
            }
            let theta_foot = dy.atan2(dx);
            if math::arc_contains_angle(theta_start, sweep, theta_foot) {
                radial_dist
            } else {
                // Foot lies outside the sweep — nearest point on the
                // arc is one of the endpoints.
                let de_s = (center.x - seg.start.x).hypot(center.y - seg.start.y);
                let de_e = (center.x - seg.end.x).hypot(center.y - seg.end.y);
                de_s.min(de_e)
            }
        }
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
    // xmwy: same closed-contour gate as lead_in_geometry — open
    // contours roll off into free space; the arc sweep adds nothing.
    let kind = if matches!(setup.leads.out, LeadKind::Arc) && !is_closed_contour(segments) {
        LeadKind::Straight
    } else {
        setup.leads.out
    };
    match kind {
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
            // 62pd: same fit check as lead-in — if the swept disk
            // overlaps a non-adjacent contour wall, fall back to a
            // straight lead-out so the cutter doesn't carve into the
            // already-cut profile while rolling off.
            if arc_lead_fits(segments, center, radius, false) {
                LeadGeometry::Arc {
                    entry_or_exit: arc_end,
                    center,
                    ccw: free_left,
                }
            } else {
                LeadGeometry::Straight {
                    from: Point2::new(last.end.x + len * px, last.end.y + len * py),
                }
            }
        }
        LeadKind::Off => LeadGeometry::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::setup::{LeadKind, ToolOffset};

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    fn segline(a: Point2, b: Point2) -> Segment {
        Segment::line(a, b, "0", 7)
    }

    #[test]
    fn p62d_arc_lead_fits_returns_true_with_room() {
        // Direct test of the fit helper — single isolated chord, no
        // walls anywhere near the swept disk.
        let segments = vec![segline(p(0.0, 0.0), p(50.0, 0.0))];
        let center = Point2::new(0.0, 50.0); // far above
        let radius = 1.0;
        assert!(arc_lead_fits(&segments, center, radius, true));
    }

    #[test]
    fn p62d_arc_lead_fits_returns_false_on_collision() {
        // Lead-in (skip first seg). Second segment is a wall close to
        // the arc envelope — should detect collision.
        let segments = vec![
            segline(p(0.0, 0.0), p(20.0, 0.0)),   // first / adjacent — skipped
            segline(p(20.0, 0.0), p(20.0, 10.0)), // far wall
            segline(p(20.0, 10.0), p(0.0, 10.0)), // ceiling at y=10
        ];
        // Arc center at (0, 9) with radius 5 → ceiling at y=10 is only
        // 1 mm away — well inside the swept disk.
        let center = Point2::new(0.0, 9.0);
        let radius = 5.0;
        assert!(!arc_lead_fits(&segments, center, radius, true));
    }

    #[test]
    fn p62d_arc_lead_fits_skips_adjacent_segment() {
        // The adjacent (first for lead-in) chord must NOT trip the
        // collision check — the lead lands tangent to it on purpose.
        let segments = vec![segline(p(0.0, 0.0), p(20.0, 0.0))];
        // Arc center sitting RIGHT on the chord (radius 1).
        let center = Point2::new(10.0, 0.0);
        let radius = 1.0;
        // Lead-in mode (skip index 0) → no collision.
        assert!(arc_lead_fits(&segments, center, radius, true));
    }

    #[test]
    fn xmwy_open_contour_arc_lead_demotes_to_straight() {
        // An open slot (start != end) configured with `LeadKind::Arc`
        // must yield a Straight lead — the tangent roll-on serves no
        // geometric purpose when the cutter enters in free space, and
        // can graze stock around the open contour that arc_lead_fits
        // can't see.
        let mut setup = Setup::default();
        setup.mill.offset = ToolOffset::Outside;
        setup.leads.r#in = LeadKind::Arc;
        setup.leads.in_lenght = 5.0;
        setup.leads.out = LeadKind::Arc;
        setup.leads.out_lenght = 5.0;
        // 20 mm open slot along +X.
        let segments = vec![segline(p(0.0, 0.0), p(20.0, 0.0))];
        let g_in = lead_in_geometry(&setup, &segments);
        assert!(
            matches!(g_in, LeadGeometry::Straight { .. }),
            "open contour with Arc lead-in must demote to Straight, got {g_in:?}",
        );
        let g_out = lead_out_geometry(&setup, &segments);
        assert!(
            matches!(g_out, LeadGeometry::Straight { .. }),
            "open contour with Arc lead-out must demote to Straight, got {g_out:?}",
        );
    }

    #[test]
    fn xmwy_closed_contour_arc_lead_still_emits_arc() {
        // Sanity: the demotion only fires for OPEN contours. A closed
        // square with room for the arc still gets an Arc lead.
        let mut setup = Setup::default();
        setup.mill.offset = ToolOffset::Inside;
        setup.leads.r#in = LeadKind::Arc;
        setup.leads.in_lenght = 1.0;
        // 50 × 50 closed square, CCW — large enough that the 1 mm
        // arc lead envelope clears the adjacent walls (62pd fit-check
        // passes alongside the xmwy closed-contour gate).
        let segments = vec![
            segline(p(0.0, 0.0), p(50.0, 0.0)),
            segline(p(50.0, 0.0), p(50.0, 50.0)),
            segline(p(50.0, 50.0), p(0.0, 50.0)),
            segline(p(0.0, 50.0), p(0.0, 0.0)),
        ];
        let g = lead_in_geometry(&setup, &segments);
        assert!(
            matches!(g, LeadGeometry::Arc { .. }),
            "closed contour should still get Arc lead, got {g:?}",
        );
    }

    /// u2u1 regression: arc segments are measured against the true
    /// arc-to-point distance, not chord distance. A bulgy contour wall
    /// whose chord sits comfortably outside the lead arc's swept disk
    /// while the bulge itself reaches INTO the disk must be detected
    /// as a collision — the lead arc would otherwise carve into the
    /// arc's sagitta.
    #[test]
    fn u2u1_arc_segment_distance_accounts_for_sagitta() {
        // Half-circle on the UPPER side from (10, 0) CCW to (-10, 0)
        // — start angle 0, sweep +π, passing through (0, 10). bulge
        // = tan(π/4) = +1. Center (0, 0), radius 10. The chord is
        // the X-axis y=0. A probe at (0, -0.5) sits 0.5 mm BELOW
        // the chord, so chord distance is 0.5 — but the arc itself
        // is on the +Y side, 10.5 mm away from the probe at its
        // closest point (the endpoint (10, 0) or (-10, 0), the
        // radial foot falls outside the sweep).
        let arc = Segment::arc(
            Point2::new(10.0, 0.0),
            Point2::new(-10.0, 0.0),
            1.0,
            Some(Point2::new(0.0, 0.0)),
            "0",
            7,
        );
        // Probe directly BELOW the arc center, distance 5 below the
        // X-axis. Chord distance = 5; arc-to-point distance: the
        // radial foot points at angle -π/2 which is OUTSIDE the
        // sweep [0, π], so the nearest arc point is one of the
        // endpoints (-10, 0) or (10, 0), distance √(100+25) ≈ 11.18.
        let probe = Point2::new(0.0, -5.0);
        let d = segment_distance_to_point(&arc, probe);
        let expected = (100.0f64 + 25.0).sqrt();
        assert!(
            (d - expected).abs() < 1e-9,
            "expected true arc-to-point distance {expected}, got {d}",
        );
    }

    /// u2u1: a point on the arc itself must report ~zero distance.
    /// The chord distance would over-report by the local sagitta.
    #[test]
    fn u2u1_arc_segment_distance_zero_on_arc() {
        // Upper-half-circle from (10, 0) CCW to (-10, 0) (bulge +1).
        // A probe at (0, 10) sits on the arc apex (angle π/2 which
        // is inside the sweep [0, π]); distance must be ~0. Chord
        // distance to y=0 would be 10.
        let arc = Segment::arc(
            Point2::new(10.0, 0.0),
            Point2::new(-10.0, 0.0),
            1.0,
            Some(Point2::new(0.0, 0.0)),
            "0",
            7,
        );
        let probe = Point2::new(0.0, 10.0);
        let d = segment_distance_to_point(&arc, probe);
        assert!(d < 1e-9, "probe on arc should have ~zero distance, got {d}");
    }

    /// u2u1: when the probe's radial foot falls outside the arc's
    /// angular sweep, the nearest point on the arc is one of the
    /// endpoints — not a phantom radial projection.
    #[test]
    fn u2u1_arc_segment_distance_outside_sweep_uses_endpoint() {
        // Quarter arc from (10, 0) CCW to (0, 10) (start angle 0,
        // end angle π/2, sweep π/2). bulge = tan(π/8). Center
        // origin, radius 10.
        let bulge = (std::f64::consts::PI / 8.0).tan();
        let arc = Segment::arc(
            Point2::new(10.0, 0.0),
            Point2::new(0.0, 10.0),
            bulge,
            Some(Point2::new(0.0, 0.0)),
            "0",
            7,
        );
        // Probe at (5, -5) — radial foot direction (5, -5) is at
        // angle -π/4, OUTSIDE the sweep [0, π/2]. Nearest endpoint
        // is (10, 0), distance √(25+25) = 5√2.
        let probe = Point2::new(5.0, -5.0);
        let d = segment_distance_to_point(&arc, probe);
        assert!(
            (d - (5.0f64.hypot(5.0))).abs() < 1e-9,
            "expected endpoint distance, got {d}",
        );
    }

    /// u2u1: chord-distance would report ~0 here (the probe sits
    /// right on the chord midpoint); the true arc distance is the
    /// radius minus zero (probe is at arc center) → radius. Verifies
    /// the function doesn't accidentally pretend chord-distance is
    /// arc-distance when the probe is INSIDE the swept circle.
    #[test]
    fn u2u1_arc_segment_distance_at_arc_center_returns_radius() {
        // Upper-half-circle from (10, 0) CCW to (-10, 0), center
        // (0,0), radius 10.
        let arc = Segment::arc(
            Point2::new(10.0, 0.0),
            Point2::new(-10.0, 0.0),
            1.0,
            Some(Point2::new(0.0, 0.0)),
            "0",
            7,
        );
        let probe = Point2::new(0.0, 0.0);
        let d = segment_distance_to_point(&arc, probe);
        // At the center, distance to any point on the circle is the
        // radius. With radial-foot ill-defined (d_to_arc_center = 0),
        // we fall back to endpoint distance — both endpoints are
        // (±10, 0) which sit 10 from the center. Either way the
        // result is 10.
        assert!(
            (d - 10.0).abs() < 1e-9,
            "expected radius-distance at arc center, got {d}",
        );
    }

    #[test]
    fn p62d_arc_lead_falls_back_to_straight_when_no_room() {
        // Integration test through lead_in_geometry: a constrained
        // outside-profile contour where the arc lead lands inside the
        // workpiece. Single segment + adversarial neighbor wall.
        let mut setup = Setup::default();
        setup.mill.offset = ToolOffset::Outside;
        setup.leads.r#in = LeadKind::Arc;
        setup.leads.in_lenght = 5.0;
        // Single CCW polyline that boxes the lead-in into a corner —
        // arc swept disk inevitably grazes one of the non-adjacent
        // walls regardless of free-side orientation.
        let segments = vec![
            segline(p(0.0, 0.0), p(2.0, 0.0)),    // tiny floor
            segline(p(2.0, 0.0), p(2.0, 3.0)),    // right wall
            segline(p(2.0, 3.0), p(-3.0, 3.0)),   // ceiling reaching across
            segline(p(-3.0, 3.0), p(-3.0, -3.0)), // far-left wall
            segline(p(-3.0, -3.0), p(0.0, -3.0)), // bottom-left segment
            segline(p(0.0, -3.0), p(0.0, 0.0)),   // back to start
        ];
        // With a 5 mm arc radius and walls within 3 mm in every
        // direction, fit_lead must produce a Straight fallback.
        let g = lead_in_geometry(&setup, &segments);
        assert!(
            matches!(g, LeadGeometry::Straight { .. }),
            "expected Straight fallback when no arc fits, got {g:?}",
        );
    }
}
