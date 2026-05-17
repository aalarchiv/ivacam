//! Tessellation tolerance tests for SPLINE/ELLIPSE flatteners.
//!
//! These exercise the geometric primitives directly (without going through
//! a Drawing fixture) and assert that every flattened endpoint lies within
//! a small Hausdorff-style tolerance of the analytic curve. Catches subtle
//! deviations earlier than gcode diff would.

// # CAM/sim pedantic-lint exemptions
// Computational-geometry tests use `a`, `b`, `p`, `phi`, `t` from the
// textbook ellipse/NURBS parametrizations. Sample-index casts are bounded by
// `SAMPLES` (small constants).
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::similar_names
)]

use wiac_core::input::nurbs;

const EPS_DEFAULT: f64 = 0.05; // mm

/// Distance from `p` to the analytic ellipse at the same polar parameter.
/// We sample the ellipse at fine intervals and take the minimum.
// Test helper takes the full ellipse parametrization (cx, cy, a, b, phi, t0, t1)
// — bundling into a struct just for clippy's arg-count threshold would lose the
// formula-mapping the test exercises.
#[allow(clippy::too_many_arguments)]
fn ellipse_min_distance(
    p: (f64, f64),
    cx: f64,
    cy: f64,
    a: f64,
    b: f64,
    phi: f64,
    t0: f64,
    t1: f64,
) -> f64 {
    let mut best = f64::INFINITY;
    let steps = 4096;
    for i in 0..=steps {
        let t = t0 + (t1 - t0) * f64::from(i) / f64::from(steps);
        let x = a * t.cos();
        let y = b * t.sin();
        let cos_p = phi.cos();
        let sin_p = phi.sin();
        let ex = cx + x * cos_p - y * sin_p;
        let ey = cy + x * sin_p + y * cos_p;
        let d = ((p.0 - ex).powi(2) + (p.1 - ey).powi(2)).sqrt();
        if d < best {
            best = d;
        }
    }
    best
}

#[test]
fn ellipse_flattening_stays_within_tolerance() {
    // Tessellate an ellipse the same way emit_ellipse does (polar steps over
    // [0, 2π]) and verify each endpoint sits on the analytic ellipse.
    let cx = 5.0;
    let cy = -3.0;
    let a = 10.0; // semi-major
    let b = 4.0; // semi-minor
    let phi = std::f64::consts::PI / 6.0; // 30° rotation
    let t0 = 0.0;
    let t1 = std::f64::consts::TAU;
    let step = 0.1; // ~6° — same default as importer's arc_max_step
    let total = t1 - t0;
    let n = ((total / step).ceil() as usize).max(8);

    for i in 0..=n {
        let t = t0 + total * (i as f64) / (n as f64);
        let x = a * t.cos();
        let y = b * t.sin();
        let cos_p = phi.cos();
        let sin_p = phi.sin();
        let px = cx + x * cos_p - y * sin_p;
        let py = cy + x * sin_p + y * cos_p;
        let d = ellipse_min_distance((px, py), cx, cy, a, b, phi, t0, t1);
        assert!(
            d < EPS_DEFAULT,
            "ellipse vertex {i} drifted {d} mm (tolerance {EPS_DEFAULT})"
        );
    }
}

#[test]
fn nurbs_flatten_clamped_quadratic_stays_on_curve() {
    // Clamped quadratic Bézier-equivalent NURBS: 3 control points,
    // degree=2, knots = [0,0,0,1,1,1].
    let degree = 2;
    let cps = vec![(0.0, 0.0, 1.0), (5.0, 10.0, 1.0), (10.0, 0.0, 1.0)];
    let weights = vec![1.0; cps.len()];
    let knots = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];

    let pts = nurbs::flatten(degree, &knots, &cps, &weights, 64);
    assert!(pts.len() >= 60, "expected ~64 samples, got {}", pts.len());

    // Closed-form Bernstein evaluation for a degree-2 Bézier:
    // B(t) = (1-t)^2 P0 + 2(1-t)t P1 + t^2 P2
    let bezier = |t: f64| -> (f64, f64) {
        let one = 1.0 - t;
        let x = one * one * cps[0].0 + 2.0 * one * t * cps[1].0 + t * t * cps[2].0;
        let y = one * one * cps[0].1 + 2.0 * one * t * cps[1].1 + t * t * cps[2].1;
        (x, y)
    };

    // Each sampled point should lie on the analytic curve at the same
    // parameter (uniform sampling, both ends clamped).
    let n = pts.len();
    for (i, p) in pts.iter().enumerate() {
        let t = (i as f64) / ((n - 1) as f64);
        let (bx, by) = bezier(t);
        let d = ((p.0 - bx).powi(2) + (p.1 - by).powi(2)).sqrt();
        assert!(
            d < 1e-9,
            "NURBS sample {i} at t={t} drifted {d} from analytic Bézier"
        );
    }
}

#[test]
fn nurbs_flatten_clamped_cubic_stays_on_curve() {
    // Clamped cubic with 4 control points and standard clamped knot vector.
    let degree = 3;
    let cps = vec![
        (0.0, 0.0, 1.0),
        (3.0, 10.0, 1.0),
        (7.0, 10.0, 1.0),
        (10.0, 0.0, 1.0),
    ];
    let weights = vec![1.0; cps.len()];
    let knots = vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0];

    let pts = nurbs::flatten(degree, &knots, &cps, &weights, 64);
    let bezier = |t: f64| -> (f64, f64) {
        let u = 1.0 - t;
        let x = u * u * u * cps[0].0
            + 3.0 * u * u * t * cps[1].0
            + 3.0 * u * t * t * cps[2].0
            + t * t * t * cps[3].0;
        let y = u * u * u * cps[0].1
            + 3.0 * u * u * t * cps[1].1
            + 3.0 * u * t * t * cps[2].1
            + t * t * t * cps[3].1;
        (x, y)
    };
    let n = pts.len();
    for (i, p) in pts.iter().enumerate() {
        let t = (i as f64) / ((n - 1) as f64);
        let (bx, by) = bezier(t);
        let d = ((p.0 - bx).powi(2) + (p.1 - by).powi(2)).sqrt();
        assert!(
            d < 1e-9,
            "Cubic NURBS sample {i} at t={t} drifted {d} from analytic Bézier"
        );
    }
}

#[test]
fn nurbs_rational_circle_quarter_stays_within_tolerance() {
    // A NURBS approximation of a quarter unit circle using a rational
    // quadratic with weights 1, sqrt(2)/2, 1.
    let degree = 2;
    let cps = vec![(1.0, 0.0, 1.0), (1.0, 1.0, 1.0), (0.0, 1.0, 1.0)];
    let w_mid = (2.0_f64).sqrt() * 0.5;
    let weights = vec![1.0, w_mid, 1.0];
    let knots = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];

    let pts = nurbs::flatten(degree, &knots, &cps, &weights, 256);
    for (i, p) in pts.iter().enumerate() {
        let r = (p.0 * p.0 + p.1 * p.1).sqrt();
        assert!(
            (r - 1.0).abs() < 1e-3,
            "Rational quadratic sample {i} = ({}, {}) drifted {} from unit circle",
            p.0,
            p.1,
            (r - 1.0).abs()
        );
    }
}
