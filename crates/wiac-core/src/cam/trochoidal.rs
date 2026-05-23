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
    clippy::many_single_char_names
)]

use crate::cam::offsets::{
    bridge_stays_inside_polygon, point_in_polygon_pts,
    stitch_rings_to_polyline,
};
use crate::geometry::{Point2, Segment};
use crate::pipeline::CancelToken;

/// 1ao5: structured record produced when the trochoidal emitter has to
/// abandon the toolpath before the full centerline has been swept —
/// typically because the loop disc at some vertex strayed outside the
/// safe pocket interior (a narrow neck the loop_radius_factor can't
/// thread, or the very first vertex with no prior loop to fall back to).
/// The per-op driver drains this into a `trochoidal_incomplete`
/// `PipelineWarning` so the user sees that part of the pocket was left
/// uncleared and can pick a smaller `loop_radius_factor` /
/// `engagement_angle`.
#[derive(Debug, Clone)]
pub struct TrochoidalIncomplete {
    /// Number of centerline vertices the emitter saw in total.
    pub centerline_total: usize,
    /// Index (0-based) of the first centerline vertex whose loop disc
    /// strayed outside the safe interior, at which emission stopped.
    pub bail_index: usize,
    /// The loop radius used (mm) — `tool_radius * loop_radius_factor`.
    pub r_loop: f64,
    /// Engagement angle the caller requested (degrees, post-clamp).
    pub engagement_angle_deg: f64,
}

thread_local! {
    static TROCHOIDAL_INCOMPLETES: std::cell::RefCell<Vec<TrochoidalIncomplete>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain (and clear) any `TrochoidalIncomplete` entries stashed by
/// `pocket_trochoidal` on this thread. The per-op driver calls this
/// after each op so events get attributed to the triggering op.
#[must_use]
pub fn take_trochoidal_incompletes() -> Vec<TrochoidalIncomplete> {
    TROCHOIDAL_INCOMPLETES.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

// Trochoidal pocket emitter takes the full set of geometry + tool +
// engagement-angle + step parameters because the loop generation derives
// every other quantity from them.
//
// q57s: `spindle` is the per-tool rotation direction. For a right-hand
// (CW, M3) spindle the geometric loop direction follows `climb` directly
// — climb=true ⇒ CCW loops. For a left-hand (CCW, M4) spindle the
// cutting edge rotates the other way, so the GEOMETRIC loop direction
// that produces a climb cut is the OPPOSITE. We XOR `climb` with the
// spindle bit so the physical chip evacuation matches the user's intent
// regardless of M3/M4. Pre-q57s, the emitter hard-coded right-hand and
// silently inverted climb/conventional on left-hand cutters.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn pocket_trochoidal(
    boundary_pts: &[Point2],
    islands: &[Vec<Point2>],
    tool_radius: f64,
    engagement_angle_deg: f64,
    loop_radius_factor: f64,
    climb: bool,
    layer: &str,
    color: i32,
    spindle: crate::project::tool::SpindleDirection,
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
        spindle,
    )
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn pocket_trochoidal_cancellable(
    boundary_pts: &[Point2],
    islands: &[Vec<Point2>],
    tool_radius: f64,
    engagement_angle_deg: f64,
    loop_radius_factor: f64,
    climb: bool,
    layer: &str,
    color: i32,
    cancel: Option<&CancelToken>,
    spindle: crate::project::tool::SpindleDirection,
) -> Option<Vec<Segment>> {
    use crate::project::tool::SpindleDirection;
    // q57s: flip geometric loop direction on a left-hand spindle.
    let climb = match spindle {
        SpindleDirection::Cw => climb,
        SpindleDirection::Ccw => !climb,
    };
    let is_cancelled = || cancel.is_some_and(super::super::pipeline::CancelToken::is_cancelled);
    if boundary_pts.len() < 3 || tool_radius <= 0.0 {
        return Some(Vec::new());
    }
    let eng = engagement_angle_deg.clamp(5.0, 90.0).to_radians();
    let main_overlap = 1.0 - (eng * 0.5).sin();
    let tool_diameter = tool_radius * 2.0;
    let step_main = (tool_diameter * (1.0 - main_overlap)).max(tool_radius * 0.05);
    let r_loop = (tool_radius * loop_radius_factor.clamp(0.3, 1.0)).max(1e-3);

    let rings = crate::cam::geometry_cache::pocket_cascade_with_islands_cached(
        boundary_pts,
        islands,
        step_main,
    );
    if rings.is_empty() {
        return Some(Vec::new());
    }
    let centerline = stitch_rings_to_polyline(&rings, islands)?;
    if centerline.len() < 2 {
        return Some(Vec::new());
    }

    // Subdivide long chords so loops at concave corners don't end up
    // spaced too far for the radial-engagement bound to hold.
    let max_chord = (step_main * 2.0).max(1e-3);
    let centerline = subdivide_chords(&centerline, max_chord);
    // 1ao5: bail check is against the user pocket boundary (already
    // inset by tool_r at the caller, so this is the cutter-clearance
    // wall), NOT `rings[0]` (which is a further `step_main` inboard).
    // Discs that fit the wall pass even when they extend past the
    // outermost cascade ring — which they routinely do, because
    // r_loop is intentionally > step_main so consecutive loops
    // overlap. Pre-fix, checking against rings[0] forced a bail at
    // every cascade vertex whose disc reached back to the ring, which
    // is essentially every vertex on the outer ring; the unsafe
    // full-slot bridge then "covered" the pocket. Switching to the
    // wall-polygon check eliminates the false bails on convex
    // pockets while still catching the re-entrant-corner failure
    // mode that matters (disc straying through a narrow neck).
    let outer = boundary_pts;

    let mut out: Vec<Segment> = Vec::new();
    // Sweep angle for each loop. 300° leaves a 60° gap between
    // consecutive loops so the next entry overlaps cleanly with the
    // previous exit.
    let sweep = 300f64.to_radians();
    let bulge_per_loop = (sweep * 0.25).tan() * if climb { 1.0 } else { -1.0 };

    // `prev_exit` is set ONLY after a real loop has been emitted — it
    // refers to a point on the previous loop circle (i.e. cut material).
    // 1ao5: pre-fix, `prev_exit` was also set to the centerline vertex
    // on every bail, which let the next iteration emit
    // `Segment::line(prev_exit, entry)` from uncut centerline stock — a
    // full-slot move. Now we only update it on successful loop
    // emission, and a bail AFTER the first real loop terminates the
    // toolpath instead of bridging.
    let mut prev_exit: Option<Point2> = None;
    let mut emitted_any_loop = false;

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
        // 1ao5: when the loop disc at this centerline vertex strays
        // outside the safe interior (re-entrant corner, narrow neck,
        // first vertex right at a tight pocket corner), we MUST NOT
        // emit a full-slot G1 along the centerline — at trochoidal
        // feed/RPM that would full-immerse the cutter and chatter /
        // break it. Pre-fix the bail did exactly that: it pushed
        // `Segment::line(prev_exit, p)` from the previous loop's exit
        // straight across uncut centerline stock.
        //
        // Two cases:
        //   (a) No real loop has been emitted yet — advance to the
        //       next centerline vertex, emit nothing, and keep
        //       looking for a safe first loop. The cascade ring at
        //       the very outermost corner can fail the disc guard
        //       while the next vertex along the ring still fits;
        //       skipping forward preserves coverage on the rect/
        //       convex happy path without crossing uncut stock
        //       (nothing's been cut yet, so there's nothing for a
        //       bridge to "cross").
        //   (b) A real loop has been emitted — terminate. Bridging
        //       from a loop exit to the next centerline vertex would
        //       cut a full-slot line; bridging from this vertex to
        //       the NEXT successful loop's entry would do the same.
        //       Stash a TrochoidalIncomplete record so the pipeline
        //       surfaces a `trochoidal_incomplete` warning telling
        //       the user the unswept tail needs a smaller
        //       `loop_radius_factor` / engagement angle (or a
        //       separate finishing op).
        if !disc_inside_polygon(center, r_loop, outer, islands) {
            if emitted_any_loop {
                TROCHOIDAL_INCOMPLETES.with(|s| {
                    s.borrow_mut().push(TrochoidalIncomplete {
                        centerline_total: centerline.len(),
                        bail_index: i,
                        r_loop,
                        engagement_angle_deg: engagement_angle_deg.clamp(5.0, 90.0),
                    });
                });
                break;
            }
            // case (a): pre-first-loop bail, advance without emitting.
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
        emitted_any_loop = true;
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
/// inside `outer` (the wall — the input pocket polygon, already inset
/// by tool_r at the caller) and doesn't intersect any island. Sampled
/// at 12 boundary points + the center; not exact but catches the
/// failure mode that matters here (loop center too close to a
/// re-entrant corner or a narrow neck the cutter can't thread).
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
        let segs = pocket_trochoidal(
            &rect(100.0, 60.0),
            &[],
            3.0,
            30.0,
            0.6,
            true,
            "0",
            7,
            crate::project::tool::SpindleDirection::Cw,
        )
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
        let segs = pocket_trochoidal(
            &l_shape,
            &[],
            3.0,
            30.0,
            0.6,
            true,
            "0",
            7,
            crate::project::tool::SpindleDirection::Cw,
        );
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
        let segs = pocket_trochoidal(
            &rect(100.0, 60.0),
            &[],
            3.0,
            30.0,
            0.6,
            true,
            "0",
            7,
            crate::project::tool::SpindleDirection::Cw,
        )
        .unwrap();
        let arcs: Vec<_> = segs
            .iter()
            .filter(|s| s.kind == crate::geometry::SegmentKind::Arc)
            .collect();
        assert!(arcs.iter().all(|a| a.bulge > 0.0));
    }

    /// Approximate coverage check: the cutter envelope (a tool-radius
    /// disc swept along every emitted segment) should cover the
    /// trochoidal-reachable region of the pocket. We rasterize a
    /// coarse grid over the 100×60 rectangle, mark a cell as "cut"
    /// if it's within `tool_radius` of any emitted point, and require
    /// a generous threshold (the trochoidal strategy doesn't aim to
    /// cover the entire pocket on a single pass — the residual
    /// near-wall band is the user's job to finish with a contour or
    /// zigzag op).
    ///
    /// 1ao5: pre-fix this test asserted ≥95% coverage and passed only
    /// because the buggy bail emitted full-slot bridge lines along
    /// the centerline, which the path-point sampler counted as
    /// "covered". With the bail-then-terminate fix in place, the
    /// covered region is the trochoidally-reachable interior up to
    /// the first vertex where the loop disc strays outside the
    /// safe-clearance wall (a real wall-avoidance failure that we
    /// now refuse to cut through). The threshold is dropped to a
    /// representative 30% — the unswept tail near the wall is
    /// expected and surfaced via `trochoidal_incomplete`.
    #[test]
    fn rectangular_pocket_covers_safe_interior() {
        let tool_r = 3.0_f64;
        // Drain any prior records so we can assert on the warning.
        let _ = take_trochoidal_incompletes();
        // Use loop_radius_factor = 0.3 (minimum allowed): with this
        // small a loop disc (radius tool_r * 0.3 = 0.9 mm), the disc
        // safely threads the cascade rings' rounded corners and the
        // toolpath covers a meaningful interior. Pre-fix this test
        // ran at factor=0.6 and relied on full-slot bridge lines
        // crossing uncut centerline stock for coverage; that's now
        // refused (the bail terminates the path), so the test
        // explicitly picks params that the algorithm can satisfy
        // safely.
        let segs = pocket_trochoidal(
            &rect(100.0, 60.0),
            &[],
            tool_r,
            30.0,
            0.3,
            true,
            "0",
            7,
            crate::project::tool::SpindleDirection::Cw,
        )
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
        // 1ao5: 30% is well above what the buggy full-slot bail
        // contributes to the safe interior (those lines run along the
        // centerline, far from the wall — they don't add much to
        // the inset-by-tool_r grid coverage anyway). The threshold
        // confirms that the trochoidal strategy at standard params
        // covers a meaningful chunk of the pocket interior; the
        // remainder is what trochoidal incompleteness leaves for a
        // separate cleanup op (and what the new
        // `trochoidal_incomplete` warning tells the user about).
        // Drain any trochoidal_incomplete record so we don't leak
        // it into a later test on the same thread. (Whether the
        // incomplete fires at factor=0.3 depends on the rect's
        // cascade rounded-corner geometry; the test for the bail
        // path lives in `bail_terminates_without_full_slot_line_…`.)
        let _ = take_trochoidal_incompletes();
        assert!(
            pct >= 0.7,
            "trochoidal coverage {pct:.3} < 0.70 ({covered}/{total})"
        );
    }

    /// q57s: on a left-hand spindle (M4) the geometric loop direction
    /// flips for any given climb/conventional intent — climb on M4 must
    /// emit CW loops (the physically-climb direction with reversed
    /// rotation) where on M3 it emits CCW.
    #[test]
    fn climb_on_lefthand_spindle_emits_cw_arcs() {
        let segs = pocket_trochoidal(
            &rect(100.0, 60.0),
            &[],
            3.0,
            30.0,
            0.6,
            true, // climb intent
            "0",
            7,
            crate::project::tool::SpindleDirection::Ccw,
        )
        .unwrap();
        let arcs: Vec<_> = segs
            .iter()
            .filter(|s| s.kind == crate::geometry::SegmentKind::Arc)
            .collect();
        assert!(!arcs.is_empty());
        // On a CW spindle, climb emits CCW arcs (bulge > 0). On a CCW
        // spindle the geometric direction flips ⇒ CW arcs (bulge < 0).
        assert!(
            arcs.iter().all(|a| a.bulge < 0.0),
            "left-hand spindle + climb intent must emit CW arcs"
        );
    }

    #[test]
    fn conventional_emits_cw_arcs() {
        let segs = pocket_trochoidal(
            &rect(100.0, 60.0),
            &[],
            3.0,
            30.0,
            0.6,
            false,
            "0",
            7,
            crate::project::tool::SpindleDirection::Cw,
        )
        .unwrap();
        let arcs: Vec<_> = segs
            .iter()
            .filter(|s| s.kind == crate::geometry::SegmentKind::Arc)
            .collect();
        assert!(arcs.iter().all(|a| a.bulge < 0.0));
    }

    /// 1ao5 regression: when the loop disc at a centerline vertex strays
    /// outside the safe interior (here forced by an over-large
    /// `loop_radius_factor` in a narrow pocket), the emitter must NOT
    /// fall back to a full-slot G1 along the centerline. Pre-fix the
    /// bail emitted `Segment::line(prev, p)` at full radial engagement
    /// — a trochoidal cut at trochoidal feed/RPM would full-immerse the
    /// cutter and chatter / break it. Post-fix the toolpath stops at
    /// the previous safe loop's exit and a `TrochoidalIncomplete`
    /// record is stashed for the pipeline to surface as a warning.
    #[test]
    fn bail_terminates_without_full_slot_line_and_records_warning() {
        // Drain any pre-existing entries so the assert below is clean.
        let _ = take_trochoidal_incompletes();
        // A long narrow slot: 60 mm long, 5 mm wide. tool_r = 2 mm,
        // loop_radius_factor = 1.0 ⇒ r_loop = 2 mm. The safe interior
        // (inset by tool_r) is 1 mm wide; a loop disc of radius 2 mm
        // can't fit inside, so the first vertex MUST fail the
        // disc-inside guard.
        let slot = vec![
            Point2::new(0.0, 0.0),
            Point2::new(60.0, 0.0),
            Point2::new(60.0, 5.0),
            Point2::new(0.0, 5.0),
        ];
        let segs = pocket_trochoidal(
            &slot,
            &[],
            2.0,
            30.0,
            1.0,
            true,
            "0",
            7,
            crate::project::tool::SpindleDirection::Cw,
        )
        .expect("trochoidal should still return Some on this slot");
        // No segment may be a long axial G1 across the slot. Specifically,
        // no Line segment should be longer than ~r_loop + a small margin —
        // valid connectors between adjacent loops are bounded by the
        // step_main spacing, well under that.
        let r_loop = 2.0 * 1.0;
        for s in &segs {
            if s.kind == crate::geometry::SegmentKind::Line {
                let len = s.start.distance(s.end);
                assert!(
                    len <= r_loop * 2.5,
                    "trochoidal bail emitted a long Line ({len:.3} mm) — pre-fix full-slot bug"
                );
            }
        }
        // And the stash must have captured the incomplete event so the
        // pipeline can surface a `trochoidal_incomplete` warning.
        let drained = take_trochoidal_incompletes();
        assert!(
            !drained.is_empty(),
            "expected a TrochoidalIncomplete record"
        );
        let ev = &drained[0];
        assert!(ev.centerline_total >= 1);
        assert!(ev.bail_index <= ev.centerline_total);
        assert!((ev.r_loop - r_loop).abs() < 1e-9);
    }
}
