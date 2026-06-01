//! Geometry math primitives shared across importers and the CAM core.
//!
//! Ports `ezdxf.math.bulge_to_arc` / `arc_to_bulge` plus the small
//! distance/angle helpers from viaConstructor's `calc.py`.

// # CAM/sim pedantic-lint exemptions
// Math primitives (bulge/arc/chord/sagitta) use textbook names; the few casts
// go through small integer ranges (`< 2^32`).
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use crate::geometry::Point2;

/// Convert a polyline bulge between `start` and `end` to arc parameters
/// (center, `start_angle`, `end_angle`, radius).
///
/// Bulge convention (matches `AutoCAD` / ezdxf): `bulge = tan(included_angle/4)`.
/// Positive bulge means counter-clockwise.
#[must_use]
pub fn bulge_to_arc(start: Point2, end: Point2, bulge: f64) -> (Point2, f64, f64, f64) {
    if bulge.abs() < 1e-12 {
        // Degenerate: callers shouldn't pass a zero bulge, but be safe.
        let cx = (start.x + end.x) * 0.5;
        let cy = (start.y + end.y) * 0.5;
        return (Point2::new(cx, cy), 0.0, 0.0, 0.0);
    }
    let chord = start.distance(end);
    if chord < 1e-12 {
        // Zero-length chord with a finite bulge is indeterminate — a full
        // circle would need an infinite bulge. Report radius 0 so callers
        // (e.g. `tessellate_arc`) fall back to a degenerate result instead
        // of fabricating a circle. See `tessellate_arc`'s contract (v0ih).
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
#[must_use]
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
        sweep -= std::f64::consts::TAU;
    }
    while sweep < -std::f64::consts::PI {
        sweep += std::f64::consts::TAU;
    }
    let bulge = (sweep * 0.25).tan();
    (start, end, bulge)
}

/// 2D cross product of vectors AB and AC. Positive => C is left of AB.
#[must_use]
pub fn cross_2d(a: Point2, b: Point2, c: Point2) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// Tessellate an arc described by start/end/bulge into polyline points
/// (inclusive of both endpoints), with each step at most `max_angle_rad`
/// of sweep. Returns at least two points.
///
/// **Contract (v0ih):** the bulge convention is `tan(included_angle / 4)`,
/// so a single segment can only represent sweeps in `(-2π, 2π)` — a *full*
/// 360° circle would require an infinite bulge and cannot be encoded here.
/// A full circle must therefore be supplied as two (or more) sub-arc
/// segments with distinct endpoints; that is exactly what the DXF importer
/// does (`emit_circle` splits every CIRCLE into two semicircles, each
/// `bulge = 1.0`). If a caller passes a degenerate segment with coincident
/// endpoints (`start == end`) and a finite bulge, the radius is
/// indeterminate and this function returns the two-point degenerate
/// `[start, end]` rather than guessing a circle. Callers that hold a true
/// circle (center + radius) should pre-split it, not route it through a
/// zero-chord bulge.
#[must_use]
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
        sweep += std::f64::consts::TAU;
    }
    if bulge < 0.0 && sweep > 0.0 {
        sweep -= std::f64::consts::TAU;
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

/// 7iej.10: positive arc sweep angle (radians, normalized to `[0, 2π)`)
/// from `start` to `end` about `center`, traversed CCW or CW per `ccw`.
/// Single owner for the atan2-and-normalize the arc-fitter (`gcode::arc_fit`)
/// and the cut walker (`gcode::walk`) each derived independently.
#[must_use]
pub fn arc_sweep(center: Point2, start: Point2, end: Point2, ccw: bool) -> f64 {
    use std::f64::consts::TAU;
    let a0 = (start.y - center.y).atan2(start.x - center.x);
    let a1 = (end.y - center.y).atan2(end.x - center.x);
    let mut sweep = if ccw { a1 - a0 } else { a0 - a1 };
    while sweep < 0.0 {
        sweep += TAU;
    }
    while sweep > TAU {
        sweep -= TAU;
    }
    sweep
}

/// 7iej.10: does the directed arc `[theta_start, theta_start + sweep]`
/// contain the angle `theta`? `sweep > 0` is CCW, `sweep < 0` is CW; a
/// `|sweep| >= 2π` arc contains every angle. Normalizes `theta -
/// theta_start` into the sweep's direction so the test reduces to
/// `0 <= delta <= |sweep|` (with a small tolerance). Single owner for what
/// `gcode::leads` and `gcode::tabs` kept as byte-identical copies.
#[must_use]
pub fn arc_contains_angle(theta_start: f64, sweep: f64, theta: f64) -> bool {
    let two_pi = std::f64::consts::TAU;
    if sweep.abs() >= two_pi - 1e-9 {
        return true;
    }
    // Walk forward by sweep direction; normalize (theta - theta_start) into
    // the sweep direction's sign so the comparison reduces to
    // 0 ≤ delta ≤ |sweep|.
    let mut delta = theta - theta_start;
    if sweep >= 0.0 {
        // CCW: normalize delta into [0, 2π).
        while delta < -1e-12 {
            delta += two_pi;
        }
        while delta >= two_pi - 1e-12 {
            delta -= two_pi;
        }
        delta <= sweep + 1e-9
    } else {
        // CW: normalize delta into (-2π, 0].
        while delta > 1e-12 {
            delta -= two_pi;
        }
        while delta <= -two_pi + 1e-12 {
            delta += two_pi;
        }
        delta >= sweep - 1e-9
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn arc_sweep_normalizes_both_directions() {
        use std::f64::consts::{FRAC_PI_2, PI, TAU};
        let c = Point2::new(0.0, 0.0);
        let e = Point2::new(1.0, 0.0); // angle 0
        let n = Point2::new(0.0, 1.0); // angle π/2
        // CCW from +x to +y is a quarter turn; CW is three-quarters.
        assert!(approx(arc_sweep(c, e, n, true), FRAC_PI_2));
        assert!(approx(arc_sweep(c, e, n, false), 3.0 * FRAC_PI_2));
        // Reverse endpoints: CCW from +y to +x is three-quarters.
        assert!(approx(arc_sweep(c, n, e, true), 3.0 * FRAC_PI_2));
        // A half-turn is π either way.
        let w = Point2::new(-1.0, 0.0);
        assert!(approx(arc_sweep(c, e, w, true), PI));
        // Result always in [0, 2π).
        for &ccw in &[true, false] {
            let s = arc_sweep(c, e, n, ccw);
            assert!((0.0..TAU).contains(&s));
        }
    }

    #[test]
    fn arc_contains_angle_ccw_and_cw() {
        use std::f64::consts::{FRAC_PI_2, PI, TAU};
        // CCW quarter arc [0, π/2]: contains π/4, excludes π.
        assert!(arc_contains_angle(0.0, FRAC_PI_2, FRAC_PI_2 / 2.0));
        assert!(!arc_contains_angle(0.0, FRAC_PI_2, PI));
        // CW quarter arc [0, -π/2]: contains -π/4, excludes +π/4.
        assert!(arc_contains_angle(0.0, -FRAC_PI_2, -FRAC_PI_2 / 2.0));
        assert!(!arc_contains_angle(0.0, -FRAC_PI_2, FRAC_PI_2 / 2.0));
        // Wraps across the ±π discontinuity: arc from 3π/4 sweeping +π/2
        // crosses π and contains -3π/4 (≡ 5π/4).
        assert!(arc_contains_angle(3.0 * FRAC_PI_2 / 2.0, FRAC_PI_2, -3.0 * FRAC_PI_2 / 2.0));
        // Full revolution contains everything.
        assert!(arc_contains_angle(1.0, TAU, 42.0));
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

    /// v0ih: a coincident-endpoint segment with a finite bulge is
    /// geometrically indeterminate (a full circle needs infinite bulge),
    /// so the documented contract is a two-point degenerate — never a
    /// silently-wrong arc. Pin that so a future "helpfully reconstruct a
    /// circle" change has to update the contract deliberately.
    #[test]
    fn tessellate_full_circle_bulge_is_degenerate_two_points() {
        let p = Point2::new(3.0, 4.0);
        let pts = tessellate_arc(p, p, 1.0, std::f64::consts::FRAC_PI_8);
        assert_eq!(pts.len(), 2);
        assert!(approx(pts[0].x, 3.0) && approx(pts[0].y, 4.0));
        assert!(approx(pts[1].x, 3.0) && approx(pts[1].y, 4.0));
        // bulge_to_arc reports radius 0 for the zero chord (no circle guess).
        let (_c, _a0, _a1, r) = bulge_to_arc(p, p, 1.0);
        assert!(approx(r, 0.0));
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
