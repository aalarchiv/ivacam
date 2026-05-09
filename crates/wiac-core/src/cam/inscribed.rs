//! Largest inscribed circle inside a closed pocket region.
//!
//! Used by the auto-fit helix-entry plunge: pick the medial-axis vertex
//! with the largest `r_inscribed`, then shrink by the tool radius plus a
//! 0.5 mm wall clearance so the helix cuts a clean access hole without
//! grazing the pocket walls.

use crate::cam::vcarve::{medial_axis, VcRegion};

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
    Some((best.x, best.y, helix_radius))
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
        assert!(cx >= 15.0 - 0.1 && cx <= 35.0 + 0.1, "cx = {cx}");
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
        assert!(cy < 16.0, "auto pick should land in the wide arm: cy = {cy}");
        assert!(cx >= 0.0 && cx <= 40.0);
    }

    #[test]
    fn pocket_smaller_than_tool_returns_none() {
        let region = rect(5.0, 5.0);
        let tool_radius = 3.0;
        assert!(inscribed_circle(&region, tool_radius).is_none());
    }
}
