//! Largest inscribed circle inside a closed pocket region.
//!
//! Used by the auto-fit helix-entry plunge: pick the medial-axis vertex
//! with the largest `r_inscribed`, then shrink by the tool radius plus a
//! 0.5 mm wall clearance so the helix cuts a clean access hole without
//! grazing the pocket walls.

use crate::cam::vcarve::{medial_axis, VcRegion};
use crate::geometry::Point2;

const WALL_CLEARANCE_MM: f64 = 0.5;
const MIN_HELIX_RADIUS_FACTOR: f64 = 1.2;

/// Auto-fit the helix-entry circle to the largest inscribed disc inside
/// `region`. Returns `Some((cx, cy, helix_radius))` when a helix circle
/// of radius ≥ `1.2 * tool_radius` fits with `WALL_CLEARANCE_MM` of slack
/// to the boundary; otherwise `None` so the caller can fall through to
/// Ramp / Direct.
///
/// Tie-break for multiple equal-radius vertices (long pill shapes etc.):
/// first hit wins by medial-axis traversal order.
///
/// 3fvj: the candidate medial-axis vertex is additionally validated
/// against EVERY island boundary — `best.r` is the distance to the
/// nearest densified-boundary SAMPLE (so islands contribute), but the
/// 0.1 mm sampling can miss long flat island walls whose nearest sample
/// sits further than the true edge distance. After picking the best
/// vertex we measure the true line-segment distance to each island edge;
/// if any island wall is closer than the helix circle + tool radius, the
/// candidate is rejected so the caller falls back to Ramp.
#[must_use]
pub fn inscribed_circle(region: &VcRegion, tool_radius: f64) -> Option<(f64, f64, f64)> {
    // First-hit tie-break (matters for long-pill shapes where the spine
    // has many vertices at equal max r): walk explicitly and only update
    // on strict improvement.
    let mut best: Option<crate::cam::vcarve::VPoint> = None;
    for v in medial_axis(region).into_iter().flatten() {
        let take = match best {
            Some(b) => v.r > b.r,
            None => true,
        };
        if take {
            best = Some(v);
        }
    }
    let best = best?;
    let helix_radius = best.r - tool_radius - WALL_CLEARANCE_MM;
    if helix_radius < tool_radius * MIN_HELIX_RADIUS_FACTOR {
        return None;
    }
    // True-distance check against every island wall. medial_axis already
    // densifies and samples the island boundaries (so `best.r` reflects
    // them), but a long flat island edge can sit between two sample
    // points whose distance to the vertex is greater than the true
    // line-segment distance. Re-test segment-to-point to guard the
    // helix-fit decision.
    let candidate = Point2::new(best.x, best.y);
    let required = helix_radius + tool_radius;
    for hole in &region.holes {
        if min_distance_to_polyline(&candidate, hole) <= required {
            return None;
        }
    }
    Some((best.x, best.y, helix_radius))
}

/// Minimum line-segment distance from `p` to the closed polyline
/// `verts`. Used by the island-clearance check above; segment distance
/// (not vertex distance) is essential for long flat island walls.
fn min_distance_to_polyline(p: &Point2, verts: &[Point2]) -> f64 {
    let n = verts.len();
    if n < 2 {
        return f64::INFINITY;
    }
    let mut best = f64::INFINITY;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        let ex = b.x - a.x;
        let ey = b.y - a.y;
        let len_sq = ex * ex + ey * ey;
        let d = if len_sq < 1e-18 {
            ((p.x - a.x).powi(2) + (p.y - a.y).powi(2)).sqrt()
        } else {
            let t = ((p.x - a.x) * ex + (p.y - a.y) * ey) / len_sq;
            let t = t.clamp(0.0, 1.0);
            let px = a.x + t * ex;
            let py = a.y + t * ey;
            ((p.x - px).powi(2) + (p.y - py).powi(2)).sqrt()
        };
        if d < best {
            best = d;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point2;

    fn rect(w: f64, h: f64) -> VcRegion {
        VcRegion {
            outer: vec![
                Point2::new(0.0, 0.0),
                Point2::new(w, 0.0),
                Point2::new(w, h),
                Point2::new(0.0, h),
            ],
            holes: Vec::new(),
        }
    }

    #[test]
    fn rect_50x30_with_6mm_endmill() {
        let region = rect(50.0, 30.0);
        let tool_radius = 3.0;
        let (cx, cy, r) = inscribed_circle(&region, tool_radius).expect("should fit");
        // Spine of a 50x30 rect runs (15,15)-(35,15); tie-break is
        // first hit in traversal order so cx lands somewhere on it.
        assert!((15.0 - 0.1..=35.0 + 0.1).contains(&cx), "cx = {cx}");
        assert!((cy - 15.0).abs() < 0.5, "cy = {cy}");
        assert!((r - 11.5).abs() < 0.1, "r = {r}");
    }

    #[test]
    fn l_shape_picks_wide_arm() {
        // L-shape with a wide arm (16 mm wide → incircle r=8) on the
        // bottom and a narrow arm (8 mm wide → incircle r=4) on top-right.
        let outer = vec![
            Point2::new(0.0, 0.0),
            Point2::new(40.0, 0.0),
            Point2::new(40.0, 16.0),
            Point2::new(16.0, 16.0),
            Point2::new(16.0, 40.0),
            Point2::new(8.0, 40.0),
            Point2::new(8.0, 16.0),
            Point2::new(0.0, 16.0),
        ];
        let region = VcRegion {
            outer,
            holes: Vec::new(),
        };
        let tool_radius = 2.0;
        let (cx, cy, _r) = inscribed_circle(&region, tool_radius).expect("should fit");
        assert!(
            cy < 16.0,
            "auto pick should land in the wide arm: cy = {cy}"
        );
        assert!((0.0..=40.0).contains(&cx));
    }

    #[test]
    fn pocket_smaller_than_tool_returns_none() {
        let region = rect(5.0, 5.0);
        let tool_radius = 3.0;
        assert!(inscribed_circle(&region, tool_radius).is_none());
    }

    /// 3fvj regression: a region with an island close to the best
    /// medial-axis vertex rejects the candidate via the line-segment
    /// distance check against EVERY island wall. The medial axis is
    /// densified at 0.1 mm sampling — a long flat island wall between
    /// two samples can sit closer to the candidate than the nearest
    /// sample, which the prior code missed.
    #[test]
    fn inscribed_circle_rejects_when_island_wall_too_close() {
        // 50 mm wide pocket with an island just 4 mm from the spine.
        // Tool radius 2 mm → helix circle would need ≥ 4 + 2 = 6 mm
        // clearance from the island, so any island within 6 mm causes
        // rejection.
        let outer = vec![
            Point2::new(0.0, 0.0),
            Point2::new(50.0, 0.0),
            Point2::new(50.0, 30.0),
            Point2::new(0.0, 30.0),
        ];
        // Island: a tall narrow box close to the spine (centre y = 15).
        let hole = vec![
            Point2::new(20.0, 13.0),
            Point2::new(30.0, 13.0),
            Point2::new(30.0, 17.0),
            Point2::new(20.0, 17.0),
        ];
        let region = VcRegion {
            outer,
            holes: vec![hole],
        };
        let tool_radius = 2.0;
        // With the island present the helix circle (radius >= 2.4) must
        // clear it — but every spine point is < 6 mm from one of the
        // island walls, so no helix fits.
        let result = inscribed_circle(&region, tool_radius);
        assert!(
            result.is_none(),
            "expected None (island too close); got {result:?}"
        );
        // Sanity check: without the island the SAME outer should fit
        // a helix.
        let region_no_hole = VcRegion {
            outer: region.outer.clone(),
            holes: Vec::new(),
        };
        let result = inscribed_circle(&region_no_hole, tool_radius);
        assert!(
            result.is_some(),
            "without island the same outer fits a helix"
        );
    }
}
