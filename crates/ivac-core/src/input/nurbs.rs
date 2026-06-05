//! Minimal NURBS evaluator + adaptive flattening for DXF SPLINE entities.
//!
//! Implements de Boor evaluation of a non-uniform rational B-spline with
//! arbitrary degree, then samples uniformly across the parameter range.
//! Adaptive subdivision (chord error vs. line) lands as a follow-up; the
//! uniform sampler is good enough for typical CAM SPLINE entities at the
//! tessellation tolerances viaConstructor uses.

// # CAM/sim pedantic-lint exemptions
// NURBS evaluator uses `u`, `t`, `p`, `n`, `k` from the De Boor / Cox-de Boor
// recurrence textbook formulas; sample counts are bounded.
#![allow(clippy::cast_precision_loss, clippy::many_single_char_names)]

/// Flatten a NURBS curve into a polyline.
///
/// * `degree` — curve degree (DXF `degree_of_curve`).
/// * `knots` — knot vector, length `cps.len() + degree + 1`.
/// * `cps` — control points as `(x, y, _z=1.0)` (z used as a homogeneous
///   placeholder; ignore for 2D).
/// * `weights` — per-control-point weight; pass all-1 for non-rational.
/// * `samples` — number of evenly spaced parameter samples (>=2).
#[must_use]
pub fn flatten(
    degree: usize,
    knots: &[f64],
    cps: &[(f64, f64, f64)],
    weights: &[f64],
    samples: usize,
) -> Vec<(f64, f64)> {
    if cps.len() < 2 || degree == 0 {
        return cps.iter().map(|p| (p.0, p.1)).collect();
    }
    let n = cps.len();
    // `de_boor` indexes `weights[i]` for every control point, so a slice
    // shorter than `cps` (a non-rational caller that skipped the all-ones
    // vector, or a malformed DXF) would panic. Repair to all-ones rather
    // than panic, mirroring the knot-vector fallback below.
    let weights_owned;
    let weights: &[f64] = if weights.len() >= n {
        weights
    } else {
        weights_owned = vec![1.0; n];
        &weights_owned
    };
    let expected_knots = n + degree + 1;
    if knots.len() != expected_knots {
        // Fall back to a clamped uniform knot vector — better than panicking on
        // malformed inputs from older DXFs.
        let knots = uniform_clamped_knots(n, degree);
        return flatten_inner(degree, &knots, cps, weights, samples);
    }
    flatten_inner(degree, knots, cps, weights, samples)
}

fn flatten_inner(
    degree: usize,
    knots: &[f64],
    cps: &[(f64, f64, f64)],
    weights: &[f64],
    samples: usize,
) -> Vec<(f64, f64)> {
    let samples = samples.max(2);
    let u_min = knots[degree];
    let u_max = knots[knots.len() - degree - 1];
    if u_max <= u_min {
        return cps.iter().map(|p| (p.0, p.1)).collect();
    }
    let mut out = Vec::with_capacity(samples + 1);
    for i in 0..=samples {
        let u = u_min + (u_max - u_min) * (i as f64) / (samples as f64);
        out.push(de_boor(degree, knots, cps, weights, u));
    }
    out
}

fn de_boor(
    degree: usize,
    knots: &[f64],
    cps: &[(f64, f64, f64)],
    weights: &[f64],
    u: f64,
) -> (f64, f64) {
    // Find the knot span k such that knots[k] <= u < knots[k+1].
    // Clamp u to the valid range to keep span finding well-defined.
    let n = cps.len();
    let last_span = n - 1;
    let u = u.clamp(knots[degree], knots[knots.len() - degree - 1]);
    let mut k = degree;
    while k < last_span && u >= knots[k + 1] {
        k += 1;
    }

    // Working buffers in homogeneous coords (wx, wy, w).
    let mut d: Vec<(f64, f64, f64)> = (0..=degree)
        .map(|j| {
            let i = k + j - degree;
            let w = weights[i];
            (cps[i].0 * w, cps[i].1 * w, w)
        })
        .collect();

    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let i = k + j - degree;
            let denom = knots[i + degree - r + 1] - knots[i];
            if denom.abs() < 1e-12 {
                continue;
            }
            let alpha = (u - knots[i]) / denom;
            let prev = d[j - 1];
            let cur = d[j];
            d[j] = (
                (1.0 - alpha) * prev.0 + alpha * cur.0,
                (1.0 - alpha) * prev.1 + alpha * cur.1,
                (1.0 - alpha) * prev.2 + alpha * cur.2,
            );
        }
    }
    let (wx, wy, w) = d[degree];
    if w.abs() < 1e-12 {
        (wx, wy)
    } else {
        (wx / w, wy / w)
    }
}

fn uniform_clamped_knots(n: usize, degree: usize) -> Vec<f64> {
    let total = n + degree + 1;
    let mut out = Vec::with_capacity(total);
    out.resize(degree + 1, 0.0);
    let interior = total - 2 * (degree + 1);
    for i in 1..=interior {
        out.push(i as f64 / (interior + 1) as f64);
    }
    out.resize(out.len() + degree + 1, 1.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn straight_line_via_degree_one() {
        // Degree-1 NURBS over two points is just the segment.
        let cps = vec![(0.0, 0.0, 1.0), (10.0, 0.0, 1.0)];
        let weights = vec![1.0, 1.0];
        let knots = vec![0.0, 0.0, 1.0, 1.0];
        let pts = flatten(1, &knots, &cps, &weights, 4);
        assert_eq!(pts.len(), 5);
        // First & last match endpoints.
        assert!(approx(pts[0].0, 0.0));
        assert!(approx(pts[0].1, 0.0));
        assert!(approx(pts.last().unwrap().0, 10.0));
        // Linear interpolation in between.
        assert!(approx(pts[2].0, 5.0));
    }

    #[test]
    fn quadratic_bezier_passes_through_endpoints() {
        // Degree-2 with 3 control points — a quadratic Bezier.
        // Knot vector: clamped uniform [0,0,0,1,1,1].
        let cps = vec![(0.0, 0.0, 1.0), (5.0, 10.0, 1.0), (10.0, 0.0, 1.0)];
        let weights = vec![1.0, 1.0, 1.0];
        let knots = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let pts = flatten(2, &knots, &cps, &weights, 32);
        assert!(approx(pts[0].0, 0.0));
        assert!(approx(pts[0].1, 0.0));
        assert!(approx(pts.last().unwrap().0, 10.0));
        assert!(approx(pts.last().unwrap().1, 0.0));
        // Mid-point should be at the parabola's apex (2.5, 5).
        let mid = pts[pts.len() / 2];
        assert!((mid.0 - 5.0).abs() < 0.5);
        assert!(mid.1 > 0.0);
    }

    #[test]
    fn short_weights_slice_does_not_panic() {
        // A caller that passes too few (or no) weights must be repaired to
        // all-ones rather than panicking in de_boor's weights[i] indexing.
        let cps = vec![(0.0, 0.0, 1.0), (5.0, 10.0, 1.0), (10.0, 0.0, 1.0)];
        let knots = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        // Empty weights → treated as non-rational (all 1.0).
        let pts = flatten(2, &knots, &cps, &[], 16);
        assert!(approx(pts[0].0, 0.0));
        assert!(approx(pts.last().unwrap().0, 10.0));
        // Same result as an explicit all-ones weight vector.
        let with_ones = flatten(2, &knots, &cps, &[1.0, 1.0, 1.0], 16);
        assert_eq!(pts.len(), with_ones.len());
        assert!(approx(pts[pts.len() / 2].0, with_ones[with_ones.len() / 2].0));
    }
}
