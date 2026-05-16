//! Trochoidal pocket strategy: walk the cutter along a centerline while
//! looping repeatedly around it to bound radial engagement. Used for
//! stock removal in hard materials on hobby-rigidity machines where
//! Spiral or Cascade would chatter.
//!
//! The algorithm:
//!   1. Build a centerline by chaining the cascade rings at a tight
//!      `step_main` = `tool_diameter` * (1 - `main_overlap`), with
//!      `main_overlap` = 1 - `sin(engagement_angle_deg` / 2).
//!   2. Subdivide chord segments longer than 2 × `step_main` so concave
//!      boundary jumps don't degenerate loop placement.
//!   3. At every centerline vertex, place a loop circle of radius
//!      `tool_radius` * `loop_radius_factor` offset from the vertex on the
//!      side opposite engagement. Each loop sweeps ≈300° (climb = CCW,
//!      conventional = CW), leaving a small gap that overlaps with the
//!      next loop's entry.
//!   4. Connect consecutive loops with a short G1 along the centerline.
//!
//! Returns None when the centerline can't be stitched without crossing
//! the pocket boundary (re-entrant shape with a non-fitting bridge),
//! same containment guard as the spiral strategy.

// # CAM/sim pedantic-lint exemptions
// Trochoidal milling formulas use `dx`/`dy`/`r`/`a` from the epicycloid
// parametrization in Estlcam's trochoidal-loop literature; step-count casts
// are bounded by tool/stepover geometry.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
)]


use crate::cam::offsets::{
    bridge_stays_inside_polygon, pocket_cascade_with_islands, point_in_polygon_pts,
    stitch_rings_to_polyline,
};
use crate::geometry::{Point2, Segment};
use crate::pipeline::CancelToken;

#[must_use] pub fn pocket_trochoidal(
    boundary_pts: &[Point2],
    islands: &[Vec<Point2>],
    tool_radius: f64,
    engagement_angle_deg: f64,
    loop_radius_factor: f64,
    climb: bool,
    layer: &str,
    color: i32,
) -> Option<Vec<Segment>> {
    pocket_trochoidal_cancellable(
        boundary_pts,
        islands,
        tool_radius,
        engagement_angle_deg,
        loop_radius_factor,
        climb,
        layer,
        color,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
#[must_use] pub fn pocket_trochoidal_cancellable(
    boundary_pts: &[Point2],
    islands: &[Vec<Point2>],
    tool_radius: f64,
    engagement_angle_deg: f64,
    loop_radius_factor: f64,
    climb: bool,
    layer: &str,
    color: i32,
    cancel: Option<&CancelToken>,
) -> Option<Vec<Segment>> {
    let is_cancelled = || cancel.is_some_and(super::super::pipeline::CancelToken::is_cancelled);
    if boundary_pts.len() < 3 || tool_radius <= 0.0 {
        return Some(Vec::new());
    }
    let eng = engagement_angle_deg.clamp(5.0, 90.0).to_radians();
    let main_overlap = 1.0 - (eng * 0.5).sin();
    let tool_diameter = tool_radius * 2.0;
    let step_main = (tool_diameter * (1.0 - main_overlap)).max(tool_radius * 0.05);
    let r_loop = (tool_radius * loop_radius_factor.clamp(0.3, 1.0)).max(1e-3);

    let rings = pocket_cascade_with_islands(boundary_pts, islands, step_main);
    if rings.is_empty() {
        return Some(Vec::new());
    }
    let centerline = stitch_rings_to_polyline(&rings)?;
    if centerline.len() < 2 {
        return Some(Vec::new());
    }

    // Subdivide long chords so loops at concave corners don't end up
    // spaced too far for the radial-engagement bound to hold.
    let max_chord = (step_main * 2.0).max(1e-3);
    let centerline = subdivide_chords(&centerline, max_chord);
    let outer = &rings[0];

    let mut out: Vec<Segment> = Vec::new();
    // Sweep angle for each loop. 300° leaves a 60° gap between
    // consecutive loops so the next entry overlaps cleanly with the
    // previous exit.
    let sweep = 300f64.to_radians();
    let bulge_per_loop = (sweep * 0.25).tan() * if climb { 1.0 } else { -1.0 };

    let mut prev_exit: Option<Point2> = None;

    for i in 0..centerline.len() {
        if is_cancelled() {
            return Some(out);
        }
        let p = centerline[i];
        // Step direction: tangent of the segment AFTER p when present,
        // otherwise the segment INTO p. Used to pick the loop-center
        // side and the arc start angle.
        let tangent = if i + 1 < centerline.len() {
            unit(p, centerline[i + 1])
        } else if i > 0 {
            unit(centerline[i - 1], p)
        } else {
            (1.0, 0.0)
        };
        // Side opposite engagement: for climb (CCW loops), put the
        // loop center on the LEFT of the step (rotate tangent +90°).
        // For conventional (CW loops), put it on the RIGHT.
        let normal: (f64, f64) = if climb {
            (-tangent.1, tangent.0)
        } else {
            (tangent.1, -tangent.0)
        };
        let center = Point2::new(p.x + normal.0 * r_loop, p.y + normal.1 * r_loop);
        // Don't place a loop whose disc strays outside the safe
        // pocket interior — would cut into the wall. Skipped loops
        // leave a short G1 along the centerline at this point.
        if !disc_inside_polygon(center, r_loop, outer, islands) {
            if let Some(prev) = prev_exit {
                if prev.distance(p) > 1e-9 {
                    out.push(Segment::line(prev, p, layer, color));
                }
                prev_exit = Some(p);
            } else {
                prev_exit = Some(p);
            }
            continue;
        }

        // Entry point on the loop circle: p (which is at distance
        // r_loop from center by construction).
        let entry = p;
        // Exit point: rotate the entry around the center by the sweep
        // angle (signed for cw/ccw).
        let signed_sweep = if climb { sweep } else { -sweep };
        let entry_angle = (entry.y - center.y).atan2(entry.x - center.x);
        let exit_angle = entry_angle + signed_sweep;
        let exit = Point2::new(
            center.x + r_loop * exit_angle.cos(),
            center.y + r_loop * exit_angle.sin(),
        );

        if let Some(prev) = prev_exit {
            if prev.distance(entry) > 1e-9 {
                out.push(Segment::line(prev, entry, layer, color));
            }
        }
        // Arc from entry to exit around `center`. Bulge convention:
        // tan(included_angle / 4); positive = CCW.
        out.push(Segment::arc(
            entry,
            exit,
            bulge_per_loop,
            Some(center),
            layer,
            color,
        ));
        prev_exit = Some(exit);
    }

    Some(out)
}

fn unit(a: Point2, b: Point2) -> (f64, f64) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = dx.hypot(dy).max(1e-12);
    (dx / len, dy / len)
}

fn subdivide_chords(pts: &[Point2], max_chord: f64) -> Vec<Point2> {
    if pts.len() < 2 || max_chord <= 0.0 {
        return pts.to_vec();
    }
    let mut out: Vec<Point2> = Vec::with_capacity(pts.len());
    out.push(pts[0]);
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let d = a.distance(b);
        if d <= max_chord {
            out.push(b);
            continue;
        }
        let n = (d / max_chord).ceil() as usize;
        for k in 1..=n {
            let t = (k as f64) / (n as f64);
            out.push(Point2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
        }
    }
    out
}

/// True when the closed disc of radius `r` around `center` lies fully
/// inside `outer` (the inset pocket boundary) and doesn't intersect any
/// island. Sampled at 8 boundary points + the center; not exact but
/// catches the failure mode that matters here (loop center too close
/// to a re-entrant corner).
fn disc_inside_polygon(center: Point2, r: f64, outer: &[Point2], islands: &[Vec<Point2>]) -> bool {
    if !point_in_polygon_pts(outer, center.x, center.y) {
        return false;
    }
    let samples = 12;
    for i in 0..samples {
        let theta = f64::from(i) * std::f64::consts::TAU / f64::from(samples);
        let px = center.x + r * theta.cos();
        let py = center.y + r * theta.sin();
        if !point_in_polygon_pts(outer, px, py) {
            return false;
        }
        for island in islands {
            if point_in_polygon_pts(island, px, py) {
                return false;
            }
        }
    }
    // Bridge sanity from center to a sample on the disc — re-uses the
    // centerline guard so the disc isn't on the wrong side of a
    // narrow passage.
    let sample = Point2::new(center.x + r, center.y);
    bridge_stays_inside_polygon(center, sample, outer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: f64, h: f64) -> Vec<Point2> {
        vec![
            Point2::new(0.0, 0.0),
            Point2::new(w, 0.0),
            Point2::new(w, h),
            Point2::new(0.0, h),
        ]
    }

    #[test]
    fn rectangular_pocket_emits_arcs_at_loop_radius() {
        let segs = pocket_trochoidal(&rect(100.0, 60.0), &[], 3.0, 30.0, 0.6, true, "0", 7)
            .expect("trochoidal should not fail on a convex rectangle");
        assert!(!segs.is_empty(), "expected at least one segment");
        let arcs: Vec<_> = segs
            .iter()
            .filter(|s| s.kind == crate::geometry::SegmentKind::Arc)
            .collect();
        assert!(
            arcs.len() > 5,
            "expected many loop arcs, got {}",
            arcs.len()
        );
        // Each arc center should sit at distance ≈ 1.8 (= 3 * 0.6)
        // from both endpoints.
        let r_loop = 3.0 * 0.6;
        for arc in &arcs {
            let c = arc.center.expect("arc has center");
            let r_start = arc.start.distance(c);
            let r_end = arc.end.distance(c);
            assert!(
                (r_start - r_loop).abs() < 0.1,
                "loop start radius {r_start} ≠ {r_loop}"
            );
            assert!(
                (r_end - r_loop).abs() < 0.1,
                "loop end radius {r_end} ≠ {r_loop}"
            );
        }
    }

    #[test]
    fn l_shaped_pocket_stays_inside_walls() {
        // Same L-shape as the spiral fallback test in pipeline.rs.
        let l_shape = vec![
            Point2::new(0.0, 0.0),
            Point2::new(30.0, 0.0),
            Point2::new(30.0, 10.0),
            Point2::new(10.0, 10.0),
            Point2::new(10.0, 30.0),
            Point2::new(0.0, 30.0),
        ];
        let segs = pocket_trochoidal(&l_shape, &[], 3.0, 30.0, 0.6, true, "0", 7);
        // Either Some(non-empty) or None (containment guard fired).
        // If we got segments, they must all stay inside the L-shape's
        // (relaxed) bbox — a quick sanity check that no arc center
        // wandered outside. Strict wall-avoidance is exercised by the
        // disc-inside-polygon check during emission.
        if let Some(segs) = segs {
            for s in &segs {
                for p in [s.start, s.end] {
                    let inside = (p.x >= -0.5 && p.x <= 30.5)
                        && (p.y >= -0.5 && p.y <= 30.5)
                        && !(p.x > 10.5 && p.y > 10.5);
                    assert!(inside, "trochoidal point outside L-shape: {p:?}");
                }
            }
        }
    }

    #[test]
    fn climb_emits_ccw_arcs() {
        let segs =
            pocket_trochoidal(&rect(100.0, 60.0), &[], 3.0, 30.0, 0.6, true, "0", 7).unwrap();
        let arcs: Vec<_> = segs
            .iter()
            .filter(|s| s.kind == crate::geometry::SegmentKind::Arc)
            .collect();
        assert!(arcs.iter().all(|a| a.bulge > 0.0));
    }

    /// Approximate coverage check: the cutter envelope (a tool-radius
    /// disc swept along every emitted segment) should cover most of
    /// the pocket interior. We rasterize a coarse grid over the
    /// 100×60 rectangle, mark a cell as "cut" if it's within
    /// `tool_radius` of any emitted point, and require ≥95% coverage of
    /// cells that lie within the safe-pocket inset (boundary inflated
    /// inward by `tool_radius`). 95% is the brief's threshold.
    #[test]
    fn rectangular_pocket_covers_95pct_of_safe_interior() {
        let tool_r = 3.0_f64;
        let segs = pocket_trochoidal(&rect(100.0, 60.0), &[], tool_r, 30.0, 0.6, true, "0", 7)
            .expect("trochoidal pocket failed");
        // Sample every emitted segment endpoint AND midpoints to
        // approximate the cutter path; for arc midpoints we use a
        // crude approximation that's good enough for coverage.
        let mut path_pts: Vec<Point2> = Vec::new();
        for s in &segs {
            path_pts.push(s.start);
            // For arcs, add a midpoint sampled on the arc's chord.
            if s.kind == crate::geometry::SegmentKind::Arc {
                if let Some(c) = s.center {
                    let r = s.start.distance(c);
                    let a0 = (s.start.y - c.y).atan2(s.start.x - c.x);
                    let a1 = (s.end.y - c.y).atan2(s.end.x - c.x);
                    let mut sweep = a1 - a0;
                    if s.bulge > 0.0 && sweep < 0.0 {
                        sweep += std::f64::consts::TAU;
                    }
                    if s.bulge < 0.0 && sweep > 0.0 {
                        sweep -= std::f64::consts::TAU;
                    }
                    let n = 16;
                    for k in 1..n {
                        let a = a0 + sweep * f64::from(k) / f64::from(n);
                        path_pts.push(Point2::new(c.x + r * a.cos(), c.y + r * a.sin()));
                    }
                }
            }
            path_pts.push(s.end);
        }
        // Coarse grid over the safe interior (rectangle inset by tool_r).
        let cell = 0.5_f64;
        let safe = (tool_r, 100.0 - tool_r, tool_r, 60.0 - tool_r);
        let mut total = 0usize;
        let mut covered = 0usize;
        let mut x = safe.0;
        while x <= safe.1 {
            let mut y = safe.2;
            while y <= safe.3 {
                total += 1;
                let mut cut = false;
                for p in &path_pts {
                    let d = ((p.x - x).powi(2) + (p.y - y).powi(2)).sqrt();
                    if d <= tool_r {
                        cut = true;
                        break;
                    }
                }
                if cut {
                    covered += 1;
                }
                y += cell;
            }
            x += cell;
        }
        let pct = covered as f64 / total as f64;
        assert!(
            pct >= 0.95,
            "trochoidal coverage {pct:.3} < 0.95 ({covered}/{total})"
        );
    }

    #[test]
    fn conventional_emits_cw_arcs() {
        let segs =
            pocket_trochoidal(&rect(100.0, 60.0), &[], 3.0, 30.0, 0.6, false, "0", 7).unwrap();
        let arcs: Vec<_> = segs
            .iter()
            .filter(|s| s.kind == crate::geometry::SegmentKind::Arc)
            .collect();
        assert!(arcs.iter().all(|a| a.bulge < 0.0));
    }
}
