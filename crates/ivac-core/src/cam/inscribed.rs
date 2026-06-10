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
/// The candidate medial-axis vertex is additionally validated
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
// Math convention — `p`/`a`/`b` are point bindings, `n` /
// `t` are scalar accumulators in the standard projection identity.
// Renaming would obscure the formula vs the textbook.
#[allow(clippy::many_single_char_names)]
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

    /// Regression: the inscribed-circle picker's island-clearance
    /// check rejects a candidate whose true line-segment distance to
    /// any island edge is less than the helix circle + tool radius. The
    /// medial-axis vertex's `r` value reflects nearest-sample distance,
    /// which can OVER-estimate the true edge distance when a long flat
    /// island wall sits between two boundary samples.
    ///
    /// Test setup: a SMALL 16x16 pocket whose medial axis hits its peak
    /// at the centre (8, 8) with r ≈ 8 (the inscribed disc of an 8 mm
    /// half-pocket). Tool radius 2 → `helix_radius` = 8 - 2 - 0.5 = 5.5
    /// > 1.2 * 2 = 2.4, so the helix fits. Add an island whose nearest
    /// > wall sits 4 mm from the centre — required clearance = 5.5 + 2 =
    /// > 7.5, so 4 mm is too close and the candidate must reject.
    #[test]
    fn inscribed_circle_rejects_when_island_wall_too_close() {
        let outer = vec![
            Point2::new(0.0, 0.0),
            Point2::new(16.0, 0.0),
            Point2::new(16.0, 16.0),
            Point2::new(0.0, 16.0),
        ];
        let tool_radius = 2.0;
        // Sanity: without the island, the helix fits.
        let region_no_hole = VcRegion {
            outer: outer.clone(),
            holes: Vec::new(),
        };
        let baseline = inscribed_circle(&region_no_hole, tool_radius);
        assert!(
            baseline.is_some(),
            "without island the pocket fits a helix; got {baseline:?}"
        );
        let (cx0, cy0, r0) = baseline.unwrap();
        // Now place an island wall within `(r0 + tool_radius)` of the
        // picked candidate so the line-segment distance check rejects.
        // An island just 0.5 mm from the centre will sit well inside
        // the required clearance.
        let required = r0 + tool_radius;
        let close = required * 0.5; // half the required clearance — guaranteed too close
        let hole = vec![
            Point2::new(cx0 + close, cy0 - 0.5),
            Point2::new(cx0 + close + 1.0, cy0 - 0.5),
            Point2::new(cx0 + close + 1.0, cy0 + 0.5),
            Point2::new(cx0 + close, cy0 + 0.5),
        ];
        let region = VcRegion {
            outer,
            holes: vec![hole],
        };
        let result = inscribed_circle(&region, tool_radius);
        assert!(
            result.is_none() || result.is_some_and(|(_, _, r)| r < r0 - 1.0),
            "expected None or much smaller r (island too close); got {result:?} (baseline r={r0})"
        );
    }
}
