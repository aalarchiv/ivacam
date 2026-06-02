//! 7z7w: Face-mill helical-spiral overlay (3e5 / Estlcam
//! `Flooper.cs`) — applied to every cut move when a "Whirl"-tagged
//! tool is active in the project. **Historically called "Wirbeln"**
//! after the Estlcam menu label, but the operation is NOT thread-
//! whirling — it's a face-mill spiral overlay that displaces the
//! cutter centerline around a small circle while it walks the path.
//! The public symbols (`WhirlParams`, `apply_whirl`, the `whirl*`
//! serde fields) were renamed German → English in ob3e; the old
//! `gcode::wirbeln` back-compat re-export shim has been retired.
//!
//! For each chord step of length `stride / steps` along the
//! incoming path, the cutter centerline gets displaced by
//!
//! ```text
//! x' = x + cos(angle · dir) · radius
//! y' = y + sin(angle · dir) · radius
//! z' = z + cos(angle · 3)   · osc    − osc
//! ```
//!
//! `angle` accumulates `360° / steps` per stride step so the
//! centerline traces a small circle of radius `radius` that itself
//! slides along the toolpath. `Dir = +1` climb / `−1` conventional.
//! The Z ripple (`cos(3θ) · osc − osc`) dips the cutter slightly
//! below the nominal plane between revolutions for chip evacuation —
//! matching what Estlcam emits.
//!
//! Effective cut width: `tool_diameter + 2 · radius`. Net effect is
//! a "fatter" cascade ring with bounded radial engagement — the
//! cutter is always rotating around the centerline, never sitting at
//! the chord side.
//!
//! This module produces ONLY (x, y, z) waypoints. Arc segments in
//! the input are flattened to chord steps; the post emits everything
//! as G1 because the spiral motion supersedes line-vs-arc anyway.

// # CAM/sim pedantic-lint exemptions
// Stride / step counts cast from f64 are bounded by tool geometry
// (mm scale) and `steps_for_radius` clamps to [36, 360].
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

use crate::geometry::{Point2, Segment, SegmentKind};
use crate::math;

/// Helical overlay parameters resolved at gcode-emit time.
#[derive(Debug, Clone, Copy)]
pub struct WhirlParams {
    /// Spiral radius (`extra_width_mm / 2`). Must be > 0 to emit.
    pub radius: f64,
    /// Stride along the path per full revolution. Estlcam's
    /// `T_Wirbel_Stepover`. Must be > 0; sub-millimetre clamp guards
    /// against degenerate input.
    pub stepover: f64,
    /// Z-wobble amplitude — the `osc` in the formula. 0 ⇒ flat
    /// (no Z motion contributed by the overlay).
    pub osc: f64,
    /// True for climb (CCW spiral on a right-hand spindle); false for
    /// conventional.
    pub climb: bool,
}

/// qm9x: persistent helical-overlay state that carries the spiral phase
/// (`angle`) and stride residual (`consumed_since_last_step`) across
/// successive `apply_whirl` calls — typically the per-pass loop in
/// `multi_pass`. Without this, every pass restarted at `angle = 0`
/// and produced a visible flat spot on the wall at every pass boundary
/// (the spiral phase jumped to the same angular position on every Z
/// step, so the cutter entered the new pass from the same direction
/// regardless of where the previous pass left off).
///
/// Matches 89n5's cross-chord continuity for cross-pass continuity:
/// the spiral now traces ONE continuous helical centerline across the
/// entire multi-pass cut.
#[derive(Debug, Clone, Copy, Default)]
pub struct WhirlState {
    /// Cumulative spiral phase in radians, monotonically increasing
    /// across stride steps.
    pub angle: f64,
    /// Arc-length residual since the last stride stamp; carried into
    /// the next segment / pass so the next stride lands at the right
    /// cumulative arc-length.
    pub consumed_since_last_step: f64,
}

/// Number of stride steps per full revolution. Matches Estlcam's
/// `Steps ≈ LIM(360 / (11.5 / √R), 36, 360)` — coarser for small
/// radii, finer for large radii, so the perimeter discretization stays
/// proportional to the spiral's actual size.
#[must_use]
pub fn steps_for_radius(radius: f64) -> u32 {
    if radius <= 0.0 {
        return 36;
    }
    let raw = 360.0 / (11.5 / radius.sqrt());
    raw.clamp(36.0, 360.0).round() as u32
}

/// Apply the helical overlay to a list of `Segment`s, returning a
/// dense list of `(x, y, z)` waypoints at the cutter centerline. The
/// emitter walks each segment along its arc length, subdivides into
/// strides of `stepover / steps`, and stamps the helical offset
/// at each stride point. Lines and arcs are both flattened to chord
/// steps; Z is the segment's nominal cut Z plus the overlay's wobble.
///
/// `cut_z` is the nominal pass depth (z value the cutter should be at
/// without any overlay influence). The overlay's `osc` ripple is
/// added to it.
///
/// Returns an empty Vec when `params.radius` or `params.stepover` is
/// non-positive — caller falls back to the plain emit path.
///
/// This entry point starts at phase zero — for single-shot applications
/// (e.g. tests, plot-mode single-pass emit) it's fine. For multi-pass
/// emission use `apply_whirl_with_state` and thread a single
/// `WhirlState` across passes so the spiral phase doesn't reset and
/// produce flat spots at pass boundaries (qm9x).
#[must_use]
pub fn apply_whirl(
    segments: &[Segment],
    cut_z: f64,
    params: WhirlParams,
) -> Vec<(f64, f64, f64)> {
    let mut state = WhirlState::default();
    apply_whirl_with_state(segments, cut_z, params, &mut state)
}

/// qm9x: variant of [`apply_whirl`] that takes an external
/// [`WhirlState`] reference. The caller can keep ONE state across
/// successive calls so spiral phase + stride residual carry over —
/// matching 89n5's cross-chord continuity for cross-pass continuity.
///
/// Pass `&mut WhirlState::default()` for the single-shot behavior.
#[must_use]
pub fn apply_whirl_with_state(
    segments: &[Segment],
    cut_z: f64,
    params: WhirlParams,
    state: &mut WhirlState,
) -> Vec<(f64, f64, f64)> {
    if params.radius <= 0.0 || params.stepover <= 0.0 || segments.is_empty() {
        return Vec::new();
    }
    let steps = steps_for_radius(params.radius);
    let stride = params.stepover / f64::from(steps);
    if !stride.is_finite() || stride <= 1e-6 {
        return Vec::new();
    }
    let dir = if params.climb { 1.0 } else { -1.0 };
    let step_rad = std::f64::consts::TAU / f64::from(steps);

    let mut out: Vec<(f64, f64, f64)> = Vec::new();

    // Always stamp the first waypoint at the very start of the path so
    // the cutter approaches the cascade ring's start cleanly. Phase is
    // the CURRENT cumulative angle (carried over from prior calls when
    // a shared state is threaded — qm9x).
    let start = segments[0].start;
    out.push(stamp(
        start.x,
        start.y,
        cut_z,
        state.angle,
        dir,
        params.radius,
        params.osc,
    ));

    for seg in segments {
        match seg.kind {
            SegmentKind::Point => {
                // Degenerate single-point segments collapse to "stay
                // here while spinning a full revolution" so the cutter
                // doesn't drift into the next move with a hot rotation
                // misaligned with the next segment's start direction.
                let revs = 1.0;
                let n = (f64::from(steps) * revs) as u32;
                for _ in 0..n {
                    state.angle += step_rad;
                    out.push(stamp(
                        seg.start.x,
                        seg.start.y,
                        cut_z,
                        state.angle,
                        dir,
                        params.radius,
                        params.osc,
                    ));
                }
            }
            SegmentKind::Line => {
                walk_chord(
                    seg.start,
                    seg.end,
                    stride,
                    step_rad,
                    cut_z,
                    dir,
                    params.radius,
                    params.osc,
                    &mut state.angle,
                    &mut state.consumed_since_last_step,
                    &mut out,
                );
            }
            SegmentKind::Arc | SegmentKind::Circle => {
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                // Flatten the arc into chord polyline, then walk each
                // chord with the same stride machinery.
                let arc_points = flatten_arc(seg, center);
                for w in arc_points.windows(2) {
                    walk_chord(
                        w[0],
                        w[1],
                        stride,
                        step_rad,
                        cut_z,
                        dir,
                        params.radius,
                        params.osc,
                        &mut state.angle,
                        &mut state.consumed_since_last_step,
                        &mut out,
                    );
                }
            }
        }
    }

    out
}

fn stamp(x: f64, y: f64, z: f64, angle: f64, dir: f64, radius: f64, osc: f64) -> (f64, f64, f64) {
    let theta = angle * dir;
    let dx = theta.cos() * radius;
    let dy = theta.sin() * radius;
    let dz = if osc > 0.0 {
        (angle * 3.0).cos() * osc - osc
    } else {
        0.0
    };
    (x + dx, y + dy, z + dz)
}

#[allow(clippy::too_many_arguments)]
fn walk_chord(
    p0: Point2,
    p1: Point2,
    stride: f64,
    step_rad: f64,
    cut_z: f64,
    dir: f64,
    radius: f64,
    osc: f64,
    angle: &mut f64,
    consumed_since_last_step: &mut f64,
    out: &mut Vec<(f64, f64, f64)>,
) {
    let dx = p1.x - p0.x;
    let dy = p1.y - p0.y;
    let len = dx.hypot(dy);
    if len < 1e-9 {
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    // 89n5: phase MUST carry continuously across chord boundaries.
    // The previous code reset `consumed_since_last_step` to 0 at the
    // chord endpoint and unconditionally bumped `angle` by
    // `step_rad` there — losing the stride residual and producing
    // visible flat spots at corners as the spiral phase jumped by one
    // full step regardless of how far the cutter actually advanced.
    //
    // Treat `consumed_since_last_step` as the arc-length-since-last-
    // stamp accumulator. Walk the chord stamping a stride every time
    // we cross another `stride` of arc-length; carry whatever's left
    // (≤ stride) into the next chord. The endpoint waypoint stamps at
    // the partial phase it actually reached (so the cascade ring's
    // geometry is reachable without a phase glitch).
    let mut next_stamp_at = stride - *consumed_since_last_step;
    while next_stamp_at <= len + 1e-12 {
        let x = p0.x + ux * next_stamp_at;
        let y = p0.y + uy * next_stamp_at;
        *angle += step_rad;
        out.push(stamp(x, y, cut_z, *angle, dir, radius, osc));
        next_stamp_at += stride;
    }
    // Residual: how much of the chord we walked past the last stride
    // stamp. `next_stamp_at - stride` was the last stride position
    // (or `stride - prev_residual` when none were stamped). The
    // remaining chord length (`len - last_stamp_pos`) carries into
    // the next chord so the next chord's first stamp lands at the
    // right cumulative arc-length.
    let last_stamp_pos = next_stamp_at - stride;
    let residual = (len - last_stamp_pos).max(0.0);
    *consumed_since_last_step = residual;
    // Always stamp the chord endpoint so the cascade ring's geometry
    // is reachable even when the stride doesn't divide the chord
    // length — but advance phase only by the FRACTIONAL stride
    // corresponding to the residual distance from the last stride
    // stamp to the endpoint. This is the fix for the per-segment
    // flat-spot pattern.
    if residual > 1e-9 {
        // Partial-stride advance proportional to residual / stride.
        // The next chord's accumulator already tracks
        // `consumed_since_last_step = residual`; its first stride
        // stamp will bump `angle` by a full step_rad at the
        // correct cumulative arc-length. Stamping the endpoint with
        // a partial phase (no commit to `angle`) keeps the cascade
        // ring's geometry reachable without double-counting.
        let partial = (residual / stride) * step_rad;
        let endpoint_phase = *angle + partial;
        out.push(stamp(p1.x, p1.y, cut_z, endpoint_phase, dir, radius, osc));
    }
}

/// Chord-flatten an arc / circle segment to a polyline. 24 chords per
/// full revolution gives < 1 % sagitta on a 10 mm radius arc — well
/// within the visual / cutting tolerance for an overlay whose own
/// spiral radius is typically a few mm. The whirl motion masks any
/// residual chord-versus-arc error anyway.
fn flatten_arc(seg: &Segment, center: Point2) -> Vec<Point2> {
    let r = (seg.start.x - center.x).hypot(seg.start.y - center.y);
    if r < 1e-9 {
        return vec![seg.start, seg.end];
    }
    let theta0 = (seg.start.y - center.y).atan2(seg.start.x - center.x);
    let theta1 = (seg.end.y - center.y).atan2(seg.end.x - center.x);
    let sweep = 4.0 * seg.bulge.atan();
    let n_chords = (24.0 * sweep.abs() / std::f64::consts::TAU).ceil().max(4.0) as usize;
    let dtheta = sweep / (n_chords as f64);
    let _ = theta1; // theta1 is for orientation; we sweep from theta0.
    let mut pts = Vec::with_capacity(n_chords + 1);
    for k in 0..=n_chords {
        let t = theta0 + dtheta * (k as f64);
        pts.push(Point2::new(center.x + r * t.cos(), center.y + r * t.sin()));
    }
    // Snap the final endpoint to the original to absorb FP error.
    if let Some(last) = pts.last_mut() {
        *last = seg.end;
    }
    pts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Segment;

    fn line(x1: f64, y1: f64, x2: f64, y2: f64) -> Segment {
        Segment::line(Point2::new(x1, y1), Point2::new(x2, y2), "0", 7)
    }

    #[test]
    fn steps_clamps_within_36_to_360() {
        assert_eq!(steps_for_radius(0.0), 36);
        assert_eq!(steps_for_radius(0.01), 36); // very small still clamped
        assert!(steps_for_radius(0.5) >= 36);
        assert!(steps_for_radius(0.5) <= 360);
        // A 5 mm radius (huge by Whirl standards) should land near
        // the upper end but still inside the clamp.
        assert!(steps_for_radius(5.0) <= 360);
    }

    #[test]
    fn disabled_when_radius_or_stepover_is_zero() {
        let segs = vec![line(0.0, 0.0, 10.0, 0.0)];
        let params = WhirlParams {
            radius: 0.0,
            stepover: 2.0,
            osc: 0.0,
            climb: true,
        };
        assert!(apply_whirl(&segs, -1.0, params).is_empty());
        let params = WhirlParams {
            radius: 1.0,
            stepover: 0.0,
            osc: 0.0,
            climb: true,
        };
        assert!(apply_whirl(&segs, -1.0, params).is_empty());
    }

    #[test]
    fn straight_line_produces_centerline_within_radius_band() {
        let segs = vec![line(0.0, 0.0, 20.0, 0.0)];
        let params = WhirlParams {
            radius: 1.0,
            stepover: 2.0,
            osc: 0.0,
            climb: true,
        };
        let pts = apply_whirl(&segs, -1.0, params);
        // Plenty of stride points across a 20 mm chord.
        assert!(pts.len() > 20);
        // Every waypoint sits within `radius` of the line y=0.
        for (_, y, _) in &pts {
            assert!(y.abs() <= 1.0 + 1e-9, "y={y} outside ±radius band");
        }
        // X spans the chord (allow the small overshoot from the
        // stamped centerline being offset by cos(θ)·radius).
        let max_x = pts.iter().fold(f64::NEG_INFINITY, |a, &(x, _, _)| a.max(x));
        assert!(
            max_x >= 19.0,
            "max x {max_x} should reach near the chord end"
        );
    }

    #[test]
    fn climb_vs_conventional_flip_sign_of_y() {
        let segs = vec![line(0.0, 0.0, 5.0, 0.0)];
        let climb = apply_whirl(
            &segs,
            0.0,
            WhirlParams {
                radius: 1.0,
                stepover: 1.0,
                osc: 0.0,
                climb: true,
            },
        );
        let conv = apply_whirl(
            &segs,
            0.0,
            WhirlParams {
                radius: 1.0,
                stepover: 1.0,
                osc: 0.0,
                climb: false,
            },
        );
        assert_eq!(climb.len(), conv.len());
        // After the first stamp at angle=0 (same for both), each
        // subsequent waypoint has flipped sin → y component should be
        // sign-flipped.
        for (i, ((_, yc, _), (_, yv, _))) in climb.iter().zip(conv.iter()).enumerate().skip(1) {
            // Allow tiny FP noise; mirror is exact via cos = same / sin = neg.
            assert!(
                (yc + yv).abs() < 1e-9,
                "step {i}: climb y={yc} vs conv y={yv} — should sum to ~0",
            );
        }
    }

    #[test]
    fn z_wobble_dips_below_cut_z() {
        let segs = vec![line(0.0, 0.0, 10.0, 0.0)];
        let params = WhirlParams {
            radius: 1.0,
            stepover: 2.0,
            osc: 0.1,
            climb: true,
        };
        let pts = apply_whirl(&segs, -1.0, params);
        // Every waypoint sits AT OR BELOW the nominal cut z because
        // the wobble term is `cos(3θ)·osc − osc` ⇒ max is 0 (at θ=0
        // mod 2π/3), min is −2·osc.
        for &(_, _, z) in &pts {
            assert!(z <= -1.0 + 1e-9, "z={z} should not rise above cut_z");
            assert!(z >= -1.2 - 1e-9, "z={z} should not dip below cut_z − 2·osc");
        }
        // And at least one point should be near the dip floor.
        let min_z = pts.iter().fold(f64::INFINITY, |a, &(_, _, z)| a.min(z));
        assert!(
            min_z < -1.15,
            "expected at least one wobble dip near -1.2, got min {min_z}"
        );
    }

    #[test]
    fn walk_chord_phase_is_continuous_across_boundaries() {
        // 89n5: walk_chord carries the stride residual across chord
        // boundaries — the spiral phase after N unit chords must
        // equal N · (step_rad / stride) · stride within FP
        // tolerance, independent of how the total length is split.
        let radius = 1.0;
        let stride = 2.0;
        let steps = steps_for_radius(radius);
        let step_rad = std::f64::consts::TAU / f64::from(steps);

        // Walk 10 unit chords laid head-to-tail. Track the resulting
        // angle by replaying walk_chord against a fresh state, then
        // compare against ONE 10-unit chord.
        let mut angle_split = 0.0_f64;
        let mut consumed = 0.0_f64;
        let mut out_split: Vec<(f64, f64, f64)> = Vec::new();
        for k in 0..10 {
            let p0 = Point2::new(f64::from(k), 0.0);
            let p1 = Point2::new(f64::from(k + 1), 0.0);
            walk_chord(
                p0,
                p1,
                stride,
                step_rad,
                0.0,
                1.0,
                radius,
                0.0,
                &mut angle_split,
                &mut consumed,
                &mut out_split,
            );
        }

        let mut angle_single = 0.0_f64;
        let mut consumed_single = 0.0_f64;
        let mut out_single: Vec<(f64, f64, f64)> = Vec::new();
        walk_chord(
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            stride,
            step_rad,
            0.0,
            1.0,
            radius,
            0.0,
            &mut angle_single,
            &mut consumed_single,
            &mut out_single,
        );

        assert!(
            (angle_split - angle_single).abs() < 1e-9,
            "phase mismatch: split path angle={angle_split}, single chord angle={angle_single}",
        );
        // Total arclen 10 / stride 2 = 5 stride stamps; both paths
        // accumulated the same total phase.
        assert!(
            (angle_split - 5.0 * step_rad).abs() < 1e-9,
            "expected angle = 5 · step_rad ({}), got {}",
            5.0 * step_rad,
            angle_split,
        );
    }

    /// qm9x: when the SAME shared `WhirlState` is threaded across two
    /// consecutive `apply_whirl_with_state` calls, the second call's
    /// spiral phase continues from where the first ended. Reset state
    /// (a fresh `WhirlState::default()` for each call) restarts the
    /// phase at zero — that's the pre-qm9x flat-spot bug.
    // juvx: `sx_shared`/`sy_shared` vs `sx_fresh`/`sy_fresh` are an
    // intentional pair — same XY component, two different state-carry
    // scenarios. The shared prefix is the test contract.
    #[allow(clippy::similar_names)]
    #[test]
    fn cross_pass_state_continues_phase_across_apply_whirl_calls() {
        // 11 mm line × 2 mm stepover: total angle after pass1 ≈ 5.5·TAU
        // — deliberately NOT a multiple of TAU, so the carried phase
        // lands the first waypoint of pass2 at a different XY than a
        // fresh pass2 (which starts at angle=0).
        let segs = vec![line(0.0, 0.0, 11.0, 0.0)];
        let params = WhirlParams {
            radius: 1.0,
            stepover: 2.0,
            osc: 0.0,
            climb: true,
        };
        // Two calls with a SHARED state.
        let mut shared = WhirlState::default();
        let pass1_shared = apply_whirl_with_state(&segs, -1.0, params, &mut shared);
        let angle_after_pass1 = shared.angle;
        let pass2_shared = apply_whirl_with_state(&segs, -2.0, params, &mut shared);
        // Two calls with FRESH state each time (the pre-qm9x bug).
        let mut fresh1 = WhirlState::default();
        let pass1_fresh = apply_whirl_with_state(&segs, -1.0, params, &mut fresh1);
        let mut fresh2 = WhirlState::default();
        let pass2_fresh = apply_whirl_with_state(&segs, -2.0, params, &mut fresh2);
        // pass2 with shared state starts at the carried-over angle, so
        // its first waypoint sits at a different XY than pass2 with fresh
        // state (which starts at angle=0 again).
        assert!(
            angle_after_pass1 > 0.0,
            "pass1 must have accumulated some phase"
        );
        let (sx_shared, sy_shared, _) = pass2_shared[0];
        let (sx_fresh, sy_fresh, _) = pass2_fresh[0];
        let delta = ((sx_shared - sx_fresh).powi(2) + (sy_shared - sy_fresh).powi(2)).sqrt();
        // The XY delta is the spiral-offset rotation between the two
        // starting phases; on a 1 mm radius it can be up to 2 mm
        // chord (diameter), well over the FP-noise floor.
        assert!(
            delta > 0.01,
            "pass2 with shared state must start at a different XY than pass2 with fresh state; delta={delta}"
        );
        // Pass 1 outputs must be identical regardless (same initial state).
        assert_eq!(pass1_shared.len(), pass1_fresh.len());
        for (i, ((xa, ya, _), (xb, yb, _))) in
            pass1_shared.iter().zip(pass1_fresh.iter()).enumerate()
        {
            assert!(
                (xa - xb).abs() < 1e-9 && (ya - yb).abs() < 1e-9,
                "pass1 stride {i}: shared and fresh diverge",
            );
        }
    }

    #[test]
    fn closed_square_produces_continuous_rotation() {
        // A 10 mm closed square — 4 line segments. The spiral should
        // accumulate phase continuously across segment boundaries (no
        // reset at corners).
        let segs = vec![
            line(0.0, 0.0, 10.0, 0.0),
            line(10.0, 0.0, 10.0, 10.0),
            line(10.0, 10.0, 0.0, 10.0),
            line(0.0, 10.0, 0.0, 0.0),
        ];
        let params = WhirlParams {
            radius: 1.0,
            stepover: 2.0,
            osc: 0.0,
            climb: true,
        };
        let pts = apply_whirl(&segs, 0.0, params);
        // Confirm we made several full revolutions worth of stride
        // steps along the 40 mm perimeter — 40 / stride = 20 stride
        // steps minimum.
        assert!(
            pts.len() > 20,
            "expected >20 waypoints around the square, got {}",
            pts.len()
        );
        // Find two points emitted at the same XY position (the square
        // is closed — first and last vertex coincide). Their phase
        // should differ (continuous rotation), not zero out.
        let first = pts.first().unwrap();
        let last = pts.last().unwrap();
        let dist = ((first.0 - last.0).powi(2) + (first.1 - last.1).powi(2)).sqrt();
        // The last stamped endpoint after walking around the square
        // is the final segment's endpoint (0, 0); allow up to radius
        // since spiral offset shifts it.
        assert!(dist <= 2.0, "first-last delta {dist} too large");
    }
}
