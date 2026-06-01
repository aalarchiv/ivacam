//! Greedy polyline → arc fitter. Walks a sequence of `Point2`, attempting
//! to collapse consecutive line chords into single G2/G3 arcs whenever the
//! resulting fit stays within `tolerance_mm` of every input point.
//!
//! Used by `emit_polylines_block` when `MachineConfig.arcs == true`: line
//! runs are batched into points, run through `fit_arc_run`, and re-emitted
//! either as their original straight segments or as fitted arcs through
//! the post processor's `arc_cw` / `arc_ccw` paths.

// # CAM/sim pedantic-lint exemptions
// Arc-fit walks the polyline at bounded sample counts.
#![allow(clippy::cast_precision_loss)]

use crate::geometry::Point2;
use crate::math;

/// One fitted arc, ready to emit as a single G2/G3 move.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FittedArc {
    pub end: Point2,
    pub center: Point2,
    /// G3 if true, G2 if false.
    pub ccw: bool,
}

/// Result of running `fit_arc_run` over a chord chain.
#[derive(Debug, Clone, PartialEq)]
pub enum FitOutput {
    /// No fit attempted (or aborted) — emit the original chord chain.
    Lines(Vec<Point2>),
    /// Run replaced by one or more arcs. The arc chain starts at the
    /// run's first point and the n-th arc starts at the (n-1)-th arc's
    /// `end`. Each arc carries its own `center` / `ccw`.
    Arcs(Vec<FittedArc>),
}

/// Walk `points`, greedily collapsing chord runs into circular arcs while
/// every member point stays within `tolerance_mm` of the current circle.
///
/// Splits a new arc on any of:
/// - next point deviates from the circle by > `tolerance_mm`
/// - sweep would exceed 180° (some controllers reject `>180°` in a single
///   G2/G3, and a flipped chord would mis-sweep)
/// - included direction would flip (would imply a CW↔CCW switch)
///
/// Runs with fewer than 3 points fall through to `Lines` unchanged.
#[must_use]
pub fn fit_arc_run(points: &[Point2], tolerance_mm: f64) -> FitOutput {
    if points.len() < 3 {
        return FitOutput::Lines(points.to_vec());
    }
    let tol = tolerance_mm.max(0.0);
    let mut arcs: Vec<FittedArc> = Vec::new();
    let mut start = 0;
    while start + 2 < points.len() {
        let (consumed, fitted) = greedy_fit_from(&points[start..], tol);
        match fitted {
            Some(arc) => {
                arcs.push(arc);
                // The arc ends at points[start + consumed - 1]; the next
                // arc must START from that same point (shared endpoint) so
                // the run remains continuous.
                start += consumed - 1;
            }
            None => {
                // Couldn't fit even 3 points starting here → fall back to
                // a straight chord and try again from the next point.
                break;
            }
        }
    }
    if arcs.is_empty() {
        return FitOutput::Lines(points.to_vec());
    }
    // Last arc must end on the run's final point. nxmq: the snap
    // threshold here used to be `tolerance_mm` itself — i.e. each arc
    // chain could drift by up to one `tol` per emitted arc, and N
    // chained chains drift up to N·tol cumulatively. Use a quarter of
    // `tol` so the per-chain clamp is tight enough that several
    // chains in a row stay within the operator's expected envelope.
    // Still loose enough that a perfectly-fitted chain off by 1e-8
    // doesn't fall back to all-Lines (the historical regression that
    // motivated raising the threshold above 1e-9 in the first place).
    let snap_tol = (tolerance_mm * 0.25).max(1e-9);
    let last_arc_end = arcs.last().map(|a| a.end);
    let last_pt = points.last().copied();
    match (last_arc_end, last_pt) {
        (Some(a), Some(p)) if (a.x - p.x).hypot(a.y - p.y) <= snap_tol => FitOutput::Arcs(arcs),
        // 7iej.19: the arc chain must reach the run's final point — there
        // is no mixed "arcs + trailing line" output. A near-π run too short
        // to split (notably a 4-point run whose full sweep exceeds the
        // π·0.999 cap: the first arc covers 3 points, leaving 2 that can't
        // form a second arc) therefore falls back to all-Lines here. That
        // output is geometrically faithful (same vertices, straight chords)
        // — just not as smooth as an arc. A true fix would need a mixed
        // FitOutput variant + a matching emit path in `walk.rs`.
        _ => FitOutput::Lines(points.to_vec()),
    }
}

/// Try to grow an arc starting at points[0]. Returns the number of points
/// consumed (≥3) and the fitted arc, or None if no 3-point fit exists.
fn greedy_fit_from(points: &[Point2], tolerance_mm: f64) -> (usize, Option<FittedArc>) {
    if points.len() < 3 {
        return (0, None);
    }
    let p0 = points[0];
    let p1 = points[1];
    let p2 = points[2];
    let Some((center, radius)) = circumcircle(p0, p1, p2) else {
        return (0, None);
    };
    // Initial 3-point fit must itself stay within tolerance (cheap sanity
    // check — the 3 points are ON the circle by construction, but radius
    // ≈ 0 or huge would already fail above).
    // knm0: pick the CCW/CW direction from the SIGNED enclosed area of
    // the run-so-far, not just one chord. A noisy polyline can have a
    // single chord that crosses the chord direction without the overall
    // run reversing direction; majority-by-area is the robust signal.
    let ccw = run_direction_ccw(&points[..3], center);
    if max_deviation(&points[..3], center, radius) > tolerance_mm {
        return (0, None);
    }
    let mut best = FittedArc {
        end: p2,
        center,
        ccw,
    };
    let mut best_count = 3usize;
    let mut current_center = center;
    let mut current_radius = radius;
    for j in 3..points.len() {
        let next = points[j];
        // Refit through the first, middle, and new endpoint of the
        // growing run so the circle stays representative of the full
        // span. `points` here is already the run-local slice (the
        // caller rebases via `&points[start..]`) so `j / 2` indexes
        // the geometric middle of the current candidate run.
        let mid = points[j / 2];
        let new_circle = circumcircle(p0, mid, next).or(Some((current_center, current_radius)));
        let Some((nc, nr)) = new_circle else {
            break;
        };
        // Check that ALL points in the candidate range lie on the new
        // circle within tolerance.
        if max_deviation(&points[..=j], nc, nr) > tolerance_mm {
            break;
        }
        // Direction stability — the OVERALL signed area of the chord
        // run around the new center must preserve the CCW/CW
        // orientation of the arc. knm0: inspecting only the last chord
        // misclassifies noisy inputs where a single chord direction
        // briefly flips even though the run remains monotonic.
        let new_ccw = run_direction_ccw(&points[..=j], nc);
        if new_ccw != ccw {
            break;
        }
        // 85mj: cap total sweep STRICTLY below π. Single-arc emission
        // of an exact 180° sweep has ambiguous direction — both G2 and
        // G3 with the same I/J trace the same chord/center pair on a
        // 180° arc but opposite tool paths. Force such arcs to split
        // into two ~90° halves by lowering the cap to π·0.999 (about
        // 179.82°). The previous `> π + 1e-9` admitted exact π.
        if math::arc_sweep(nc, p0, next, ccw) > std::f64::consts::PI * 0.999 {
            // Retreat best so the SECOND arc has room: the caller
            // advances `start += consumed - 1` and then runs another
            // 3-point fit, which needs at least three remaining
            // points. Without this retreat, a near-π run breaks one
            // index short of the final point and the second arc
            // can't close — `fit_arc_run` then falls back to Lines.
            let remaining = points.len() - (j + 1);
            if remaining < 3 && j > 3 {
                // Back up best_count so points.len() - (best_count - 1)
                // >= 3, i.e. best_count <= points.len() - 2.
                let target = points.len().saturating_sub(2);
                if best_count > target {
                    best_count = target;
                    best = FittedArc {
                        end: points[best_count - 1],
                        center: current_center,
                        ccw,
                    };
                }
            }
            break;
        }
        current_center = nc;
        current_radius = nr;
        best = FittedArc {
            end: next,
            center: nc,
            ccw,
        };
        best_count = j + 1;
    }
    (best_count, Some(best))
}

/// Majority orientation over a chord chain: sum the signed area of
/// each chord (`p_k` → p_{k+1}) around `center`. Positive ⇒ CCW.
///
/// Falls back to the single-chord (`direction_ccw(center, p0, p1)`)
/// signal when the chain is degenerate (≤ 1 chord) or the accumulated
/// signed area is exactly 0 (alternating chords cancel).
fn run_direction_ccw(points: &[Point2], center: Point2) -> bool {
    if points.len() < 2 {
        return false;
    }
    let mut accum = 0.0_f64;
    for w in points.windows(2) {
        accum += math::cross_2d(center, w[0], w[1]);
    }
    if accum.abs() < 1e-12 {
        // Tie-break with the first chord — the legacy heuristic.
        return direction_ccw(center, points[0], points[1]);
    }
    accum > 0.0
}

/// Standard 3-point circumcircle. Returns None when the three points are
/// near-collinear (denominator vanishes) — those runs should fall back to
/// lines.
fn circumcircle(a: Point2, b: Point2, c: Point2) -> Option<(Point2, f64)> {
    let ax = a.x;
    let ay = a.y;
    let bx = b.x;
    let by = b.y;
    let cx = c.x;
    let cy = c.y;
    let d = 2.0 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
    if d.abs() < 1e-12 {
        return None;
    }
    let ux = ((ax * ax + ay * ay) * (by - cy)
        + (bx * bx + by * by) * (cy - ay)
        + (cx * cx + cy * cy) * (ay - by))
        / d;
    let uy = ((ax * ax + ay * ay) * (cx - bx)
        + (bx * bx + by * by) * (ax - cx)
        + (cx * cx + cy * cy) * (bx - ax))
        / d;
    let r = ((ax - ux).hypot(ay - uy)).abs();
    if !r.is_finite() || r < 1e-9 {
        return None;
    }
    Some((Point2::new(ux, uy), r))
}

/// Worst chord-vs-arc deviation across the run. The polyline samples lie
/// ON the fitted circle by construction (we use circumcircle through 3 of
/// them), so checking radial distance of the SAMPLES is meaningless — the
/// real question is "do the CHORDS between samples deviate from the arc
/// by more than `tolerance`?". The chord-arc gap is the sagitta of each
/// chord: `radius - sqrt(radius² - (chord_len / 2)²)`.
fn max_deviation(points: &[Point2], center: Point2, radius: f64) -> f64 {
    // First: any sample that's not actually on the circle (e.g. mid
    // witnesses we added later) blows the fit.
    let mut max = 0.0_f64;
    for p in points {
        let d = ((p.x - center.x).hypot(p.y - center.y) - radius).abs();
        if d > max {
            max = d;
        }
    }
    // Then: per-chord sagitta. A long chord between two points on the
    // circle bulges away from the arc by sagitta; if that exceeds
    // tolerance the polyline doesn't approximate the arc.
    for w in points.windows(2) {
        let chord = w[0].distance(w[1]);
        let half = chord * 0.5;
        if half >= radius {
            return f64::INFINITY;
        }
        let sag = radius - (radius * radius - half * half).sqrt();
        if sag > max {
            max = sag;
        }
    }
    max
}

/// True when the chord (a → b) sweeps CCW around `center`.
fn direction_ccw(center: Point2, a: Point2, b: Point2) -> bool {
    let cross = math::cross_2d(center, a, b);
    cross > 0.0
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    #[test]
    fn single_quadrant_of_circle() {
        // 25 points sampled on a unit circle from angle 0 to π/2.
        let pts: Vec<Point2> = (0..25)
            .map(|i| {
                let t = f64::from(i) * FRAC_PI_2 / 24.0;
                Point2::new(t.cos(), t.sin())
            })
            .collect();
        let out = fit_arc_run(&pts, 0.001);
        match out {
            FitOutput::Arcs(arcs) => {
                assert_eq!(arcs.len(), 1, "expected a single arc for a quadrant");
                let a = arcs[0];
                assert!(a.ccw, "quadrant from +X to +Y is CCW");
                assert!(
                    (a.center.x).abs() < 1e-3,
                    "center.x ≈ 0, got {}",
                    a.center.x
                );
                assert!(
                    (a.center.y).abs() < 1e-3,
                    "center.y ≈ 0, got {}",
                    a.center.y
                );
                assert!((a.end.x).abs() < 1e-3 && (a.end.y - 1.0).abs() < 1e-3);
            }
            FitOutput::Lines(_) => panic!("quadrant should fit, got Lines"),
        }
    }

    #[test]
    fn half_circle_splits_or_keeps() {
        // 50 points spanning exactly π (semicircle).
        let pts: Vec<Point2> = (0..50)
            .map(|i| {
                let t = f64::from(i) * PI / 49.0;
                Point2::new(t.cos(), t.sin())
            })
            .collect();
        let out = fit_arc_run(&pts, 0.001);
        match out {
            FitOutput::Arcs(arcs) => {
                assert!(
                    arcs.len() <= 2,
                    "≤ 2 arcs for a semicircle, got {}",
                    arcs.len()
                );
            }
            FitOutput::Lines(_) => panic!("semicircle should fit"),
        }
    }

    #[test]
    fn tight_tolerance_more_segments() {
        // Noisy arc samples — same points, two tolerances.
        let mut pts: Vec<Point2> = Vec::new();
        for i in 0..40 {
            let t = f64::from(i) * FRAC_PI_2 / 39.0;
            // Small radial perturbation alternating sign.
            let r = 1.0 + if i % 2 == 0 { 0.02 } else { -0.02 };
            pts.push(Point2::new(r * t.cos(), r * t.sin()));
        }
        let loose = fit_arc_run(&pts, 0.05);
        let tight = fit_arc_run(&pts, 0.001);
        let loose_n = match &loose {
            FitOutput::Arcs(a) => a.len(),
            FitOutput::Lines(_) => 0,
        };
        let tight_n = match &tight {
            FitOutput::Arcs(a) => a.len(),
            FitOutput::Lines(p) => p.len(),
        };
        assert!(
            tight_n >= loose_n,
            "tight ({tight_n}) should produce ≥ segments than loose ({loose_n})"
        );
    }

    #[test]
    fn random_polyline_no_spurious_arcs() {
        // Deterministic "random-looking" 20-point polyline that does NOT
        // lie on any circle (alternating zigzag with varying step).
        let pts: Vec<Point2> = (0..20)
            .map(|i| {
                let x = f64::from(i);
                let y = if i % 2 == 0 { 0.0 } else { 5.0 };
                p(x, y)
            })
            .collect();
        let out = fit_arc_run(&pts, 0.01);
        matches!(out, FitOutput::Lines(_));
        match out {
            FitOutput::Lines(ps) => assert_eq!(ps.len(), pts.len()),
            FitOutput::Arcs(a) => panic!("zigzag should not arc-fit, got {} arcs", a.len()),
        }
    }

    #[test]
    fn rounded_corners_preserved() {
        // Square with rounded corners. 4 straight edges + 4 quarter-arc
        // corners tessellated into chord points. fit_arc_run is called
        // PER RUN by the gcode pipeline, so this test exercises both a
        // straight chain (no fit) and an arc-tessellated chain (fits to
        // one arc) on the SAME input by checking each segment type.
        let mut runs: Vec<Vec<Point2>> = Vec::new();
        // Edge 1: bottom (10 mm straight chord-chain, 11 points).
        let edge1: Vec<Point2> = (0..=10).map(|i| p(1.0 + f64::from(i) * 0.8, 0.0)).collect();
        runs.push(edge1);
        // Corner 1: bottom-right (quarter arc 1mm radius around (9,1)).
        let corner1: Vec<Point2> = (0..=10)
            .map(|i| {
                let t = -FRAC_PI_2 + f64::from(i) * FRAC_PI_2 / 10.0;
                p(9.0 + t.cos(), 1.0 + t.sin())
            })
            .collect();
        runs.push(corner1);

        // Straight edge → must be Lines.
        let r0 = fit_arc_run(&runs[0], 0.01);
        matches!(r0, FitOutput::Lines(_));
        match r0 {
            FitOutput::Lines(_) => {}
            FitOutput::Arcs(a) => panic!("straight edge fit as {} arc(s)", a.len()),
        }
        // Rounded corner → must be a single arc.
        let r1 = fit_arc_run(&runs[1], 0.01);
        match r1 {
            FitOutput::Arcs(a) => assert_eq!(a.len(), 1, "expected 1 arc for a quarter"),
            FitOutput::Lines(_) => panic!("rounded corner should fit one arc"),
        }
    }

    #[test]
    fn fewer_than_3_points_falls_through() {
        let pts = vec![p(0.0, 0.0), p(1.0, 0.0)];
        let out = fit_arc_run(&pts, 0.01);
        match out {
            FitOutput::Lines(ps) => assert_eq!(ps, pts),
            FitOutput::Arcs(_) => panic!("2-point run must fall through to Lines"),
        }
    }

    #[test]
    fn knm0_noisy_arc_direction_from_overall_area() {
        // 30-point CCW quarter-arc with a single chord whose midpoint
        // perturbation flips that chord's signed direction. The OVERALL
        // run is still CCW; majority-by-area must pick that, even
        // though the *last* chord briefly looks CW.
        let mut pts: Vec<Point2> = (0..30)
            .map(|i| {
                let t = f64::from(i) * FRAC_PI_2 / 29.0;
                Point2::new(t.cos(), t.sin())
            })
            .collect();
        // Perturb the FINAL inter-point chord so it crosses the
        // immediate-prior direction: copy point n-1 onto point n's
        // position rotated slightly inward, then put n back. The exact
        // perturbation isn't critical — the test just needs the last
        // chord to misbehave compared with the rest of the run.
        let n = pts.len();
        let last = pts[n - 1];
        let prev = pts[n - 2];
        let dx = prev.x - last.x;
        let dy = prev.y - last.y;
        // Replace the prev point with a small step PAST the last point
        // along the chord — this makes the (prev → last) chord cross
        // sign on the cross product. The end point is left alone.
        pts[n - 2] = Point2::new(last.x - 0.001 * dx, last.y - 0.001 * dy);
        let out = fit_arc_run(&pts, 0.05);
        match out {
            FitOutput::Arcs(arcs) => {
                // The dominant arc must be CCW — the overall sweep
                // direction of the input. Even if the fitter has to
                // break into multiple arcs on the disturbed chord,
                // each arc that fits must agree with the overall run
                // orientation.
                let first = arcs[0];
                assert!(
                    first.ccw,
                    "noisy CCW arc must be tagged CCW despite a single noisy chord — got CW",
                );
            }
            FitOutput::Lines(_) => {
                // Acceptable fallback — but the moment we DO fit, the
                // direction must still be the overall-run signal.
            }
        }
    }

    #[test]
    fn nxmq_last_arc_end_snap_is_tighter_than_tol() {
        // Quarter-arc cleanly fit, then a final point that's far
        // enough off the circle that `max_deviation` rejects the
        // grow-step (so `best.end` stops one point short of
        // `points.last()`). The cross-chord distance from `best.end`
        // to `points.last()` lands in the gap (tol/4, tol] — which
        // the OLD clamp (= tol) accepted (silently dropping the last
        // polyline point and leaving downstream arc-chains off by
        // up to tol per chain), but the NEW clamp (= tol/4) rejects
        // so the run falls back to Lines and the operator's
        // geometry isn't quietly drifted.
        let tol = 0.01;
        let mut pts: Vec<Point2> = (0..24)
            .map(|i| {
                let t = f64::from(i) * FRAC_PI_2 / 24.0;
                Point2::new(t.cos(), t.sin())
            })
            .collect();
        // Stray final point: same theta as pts[23], offset radially
        // by ~1.5×tol (outside `max_deviation`'s window). The
        // grow-step REJECTS this point, so the last accepted arc end
        // is pts[23]. The chord from pts[23] to the stray point is
        // ~1.5×tol — way past the new snap clamp of tol/4 (0.0025),
        // so the run falls back to Lines. Under the old clamp the
        // 1.5×tol gap would have been REJECTED too (it's bigger than
        // tol), so to actually exercise the regime the new clamp
        // changes, use a chord just barely above tol/4 and just
        // barely below tol.
        let near = pts[23];
        // Pick a stray point 0.5×tol away from pts[23] in a direction
        // that pushes max_deviation past tol when the arc tries to
        // extend (chord sagitta on a tiny segment vs the curved
        // arc). Simplest: a near-tangential offset of 0.5×tol.
        pts.push(Point2::new(near.x + 0.5 * tol, near.y));
        let out = fit_arc_run(&pts, tol);
        // Two regimes are acceptable:
        //   - Lines: snap clamp rejected the chain because the gap
        //     between best.end and pts.last() exceeds tol/4. This is
        //     the new (correct) behaviour.
        //   - Arcs that REACH the last point (best.end == pts.last()
        //     within tol/4): the fit happened to consume everything.
        match &out {
            FitOutput::Lines(_) => {
                // OK.
            }
            FitOutput::Arcs(arcs) => {
                let a = arcs.last().unwrap().end;
                let p = pts.last().copied().unwrap();
                let drift = (a.x - p.x).hypot(a.y - p.y);
                assert!(
                    drift <= tol * 0.25 + 1e-9,
                    "expected snap clamp ≤ tol/4 ({}), got drift {drift} for arcs: {arcs:?}",
                    tol * 0.25,
                );
            }
        }
    }

    #[test]
    fn mj85_exact_180_splits_into_two_arcs() {
        // Sample exactly π of a unit circle. Single-arc emission of a
        // 180° sweep has ambiguous direction (chord midpoint coincides
        // with the center), so the fitter must split it into two
        // sub-arcs.
        let pts: Vec<Point2> = (0..=40)
            .map(|i| {
                let t = f64::from(i) * PI / 40.0;
                Point2::new(t.cos(), t.sin())
            })
            .collect();
        let out = fit_arc_run(&pts, 0.001);
        match out {
            FitOutput::Arcs(arcs) => assert!(
                arcs.len() >= 2,
                "exactly-180° run must split (got {} arc(s))",
                arcs.len()
            ),
            FitOutput::Lines(_) => panic!("semicircle should still fit as arcs"),
        }
    }
}
