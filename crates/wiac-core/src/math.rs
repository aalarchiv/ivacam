//! Geometry math primitives shared across importers and the CAM core.
//!
//! Ports `ezdxf.math.bulge_to_arc` / `arc_to_bulge` plus the small
//! distance/angle helpers from viaConstructor's `calc.py`.

use crate::geometry::Point2;

pub const TWO_PI: f64 = std::f64::consts::TAU;

/// Convert a polyline bulge between `start` and `end` to arc parameters
/// (center, start_angle, end_angle, radius).
///
/// Bulge convention (matches AutoCAD / ezdxf): `bulge = tan(included_angle/4)`.
/// Positive bulge means counter-clockwise.
pub fn bulge_to_arc(start: Point2, end: Point2, bulge: f64) -> (Point2, f64, f64, f64) {
    if bulge.abs() < 1e-12 {
        // Degenerate: callers shouldn't pass a zero bulge, but be safe.
        let cx = (start.x + end.x) * 0.5;
        let cy = (start.y + end.y) * 0.5;
        return (Point2::new(cx, cy), 0.0, 0.0, 0.0);
    }
    let chord = start.distance(end);
    if chord < 1e-12 {
        return (start, 0.0, 0.0, 0.0);
    }
    let sagitta = bulge * chord * 0.5;
    let half_chord = chord * 0.5;
    let radius = (half_chord * half_chord) / (2.0 * sagitta.abs()) + sagitta.abs() * 0.5;

    let mx = (start.x + end.x) * 0.5;
    let my = (start.y + end.y) * 0.5;
    // Unit perpendicular pointing "into" the arc.
    let ux = -(end.y - start.y) / chord;
    let uy = (end.x - start.x) / chord;
    let h = radius - sagitta.abs();
    let sign = if bulge > 0.0 { 1.0 } else { -1.0 };
    let cx = mx + ux * h * sign;
    let cy = my + uy * h * sign;
    let center = Point2::new(cx, cy);

    let start_angle = (start.y - cy).atan2(start.x - cx);
    let end_angle = (end.y - cy).atan2(end.x - cx);
    (center, start_angle, end_angle, radius)
}

/// Inverse of `bulge_to_arc`: from a center, two angles and radius, derive the
/// arc's start/end points and the bulge between them.
pub fn arc_to_bulge(
    center: Point2,
    start_angle: f64,
    end_angle: f64,
    radius: f64,
) -> (Point2, Point2, f64) {
    let start = Point2::new(
        center.x + radius * start_angle.cos(),
        center.y + radius * start_angle.sin(),
    );
    let end = Point2::new(
        center.x + radius * end_angle.cos(),
        center.y + radius * end_angle.sin(),
    );
    let mut sweep = end_angle - start_angle;
    while sweep > std::f64::consts::PI {
        sweep -= TWO_PI;
    }
    while sweep < -std::f64::consts::PI {
        sweep += TWO_PI;
    }
    let bulge = (sweep * 0.25).tan();
    (start, end, bulge)
}

/// 2D cross product of vectors AB and AC. Positive => C is left of AB.
pub fn cross_2d(a: Point2, b: Point2, c: Point2) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// Tessellate an arc described by start/end/bulge into polyline points
/// (inclusive of both endpoints), with each step at most `max_angle_rad`
/// of sweep. Returns at least two points.
pub fn tessellate_arc(start: Point2, end: Point2, bulge: f64, max_angle_rad: f64) -> Vec<Point2> {
    if bulge.abs() < 1e-12 {
        return vec![start, end];
    }
    let (center, a0, a1, radius) = bulge_to_arc(start, end, bulge);
    if radius < 1e-12 {
        return vec![start, end];
    }
    let mut sweep = a1 - a0;
    if bulge > 0.0 && sweep < 0.0 {
        sweep += TWO_PI;
    }
    if bulge < 0.0 && sweep > 0.0 {
        sweep -= TWO_PI;
    }
    let max_angle_rad = max_angle_rad.max(0.001);
    let steps = (sweep.abs() / max_angle_rad).ceil() as usize;
    let steps = steps.max(2);
    let mut out = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = a0 + sweep * (i as f64) / (steps as f64);
        out.push(Point2::new(
            center.x + radius * t.cos(),
            center.y + radius * t.sin(),
        ));
    }
    // Snap endpoints exactly.
    if let Some(p) = out.first_mut() {
        *p = start;
    }
    if let Some(p) = out.last_mut() {
        *p = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn semicircle_bulge_round_trip() {
        // 180° CCW arc (bulge=1) from (1,0) to (-1,0) about origin.
        let (center, a0, a1, r) = bulge_to_arc(Point2::new(1.0, 0.0), Point2::new(-1.0, 0.0), 1.0);
        assert!(approx(center.x, 0.0));
        assert!(approx(center.y, 0.0));
        assert!(approx(r, 1.0));
        // a0 = 0, a1 = π.
        assert!(approx(a0, 0.0));
        assert!(approx(a1.abs(), std::f64::consts::PI));
    }

    #[test]
    fn arc_to_bulge_inverse() {
        let center = Point2::new(2.5, -1.0);
        let r = 3.0;
        let (s, e, b) = arc_to_bulge(center, 0.2, 0.2 + std::f64::consts::FRAC_PI_2, r);
        let (c2, _, _, r2) = bulge_to_arc(s, e, b);
        assert!(approx(c2.x, center.x));
        assert!(approx(c2.y, center.y));
        assert!(approx(r2, r));
    }

    #[test]
    fn tessellate_step_count() {
        let pts = tessellate_arc(
            Point2::new(1.0, 0.0),
            Point2::new(-1.0, 0.0),
            1.0,
            std::f64::consts::FRAC_PI_8, // 22.5°
        );
        // 180° / 22.5° = 8 steps -> 9 points.
        assert_eq!(pts.len(), 9);
        // First and last snap exactly.
        assert!(approx(pts[0].x, 1.0));
        assert!(approx(pts.last().unwrap().x, -1.0));
    }
}
