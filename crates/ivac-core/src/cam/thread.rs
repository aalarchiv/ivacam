//! Helical thread emitter (`Prog_Thread` / internal / external threads).
//!
//! Given a circular bore (or stud) and a single-point thread cutter,
//! walk the cutter on a helix that descends Z by one `pitch` per
//! revolution. The result is the XYZ polyline a post-processor turns
//! into a thread profile cut.
//!
//! The emitter is geometry-only — no post-processor hooks here. It
//! produces a polyline of `(x, y, z)` waypoints; the gcode emitter
//! converts it to G1 moves (or G2/G3 helical arcs once the post
//! supports them).
//!
//! ## Math
//!
//! For a circle of `radius` around `center`, with the helix descending
//! from `top_z` (= start) to `bottom_z` (= end, more negative), at
//! `pitch_mm` Z per revolution:
//!
//! ```text
//!   revolutions = |bottom_z - top_z| / pitch_mm
//!   steps_per_rev = chord-tessellation count (default 64)
//!   total_steps = ceil(revolutions * steps_per_rev)
//! ```
//!
//! On a right-hand spindle (M3) the climb-cut direction depends on whether
//! the cutter is walking inside the bore (internal) or around the stud
//! (external):
//!
//! | internal | climb | winding |
//! |----------|-------|---------|
//! | true     | true  | CCW     |
//! | true     | false | CW      |
//! | false    | true  | CW      |
//! | false    | false | CCW     |
//!
//! i.e. `ccw = climb XOR !internal`. The `internal` flag matters: on a
//! stud, `internal=false` with `climb=true` must not silently wind out
//! conventional cuts.
//!
//! For a LEFT-hand spindle (M4 / left-hand thread cutter), the
//! cutting edge rotates the other way, so the GEOMETRIC winding that
//! produces a "climb" cut is flipped. The right-hand truth table above
//! is XOR'd with the spindle bit so the user's climb-vs-conventional
//! intent matches the physical chip evacuation direction regardless of
//! M3/M4. Previously the helix hard-coded right-hand and silently inverted
//! the cut on left-hand thread mills (M4 + climb cut conventional →
//! stripped first pass on hard material).
//!
//! ## Internal vs external
//!
//! Whether the cutter goes inside or outside the source circle is the
//! caller's choice — this module just takes a final `radius`. The
//! pipeline driver computes:
//!
//! * Internal (cutter walks inside the bore): `helix_radius = bore_radius - tool_radius`
//! * External (cutter walks around a stud): `helix_radius = stud_radius + tool_radius`
//!
//! ## Exit retract
//!
//! After the helix reaches `bottom_z` the cutter is touching the freshly
//! cut thread crest. A straight G0 lift would scrape it. The
//! emitter therefore appends a single radial retract waypoint at the
//! same Z:
//!
//! * Internal: retract to the bore center (cutter walks across the
//!   cleared bore air).
//! * External: retract radially outward to a clear radius equal to
//!   `helix_radius + 2 * tool_radius + EXTERNAL_RETRACT_SAFETY_MM`
//!   — one full tool diameter past the cutter centerline plus a small
//!   safety margin.

// # CAM/sim pedantic-lint exemptions
// Thread milling derives helix radius / Z-pitch from the bore/tool geometry;
// `r`, `n` follow the helix-parametrization convention. Step-count casts are
// bounded by the pitch/depth ratio.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use crate::geometry::Point2;

/// Default chord-tessellation density per full revolution. 64 segments
/// keeps the chord error below ~0.2% of `radius` even at small bores,
/// which is well under the typical thread tolerance.
const DEFAULT_STEPS_PER_REV: usize = 64;

/// Extra clearance (mm) added past the cutter when retracting outward
/// from an external thread. One full tool diameter would already clear
/// the cutter; this margin keeps the move safe against runout and
/// thread crest variation without growing the bounding box much.
const EXTERNAL_RETRACT_SAFETY_MM: f64 = 0.5;

/// Emit the helical thread path as a list of (x, y, z) waypoints.
/// The first waypoint sits at `start_angle_rad` (radians CCW from
/// +X) at `top_z`; the last helix waypoint sits at the same
/// angular offset advanced by the helix winding at `bottom_z`. A
/// final retract waypoint at `bottom_z` is appended so the caller's
/// vertical G0 lift doesn't scrape the just-cut thread (see the
/// module docs for the retract geometry). Empty when the inputs
/// collapse to a no-op (radius <= 0, pitch <= 0, no Z range).
///
/// `internal=true` means the cutter walks inside a bore; `false` means
/// it walks around a stud. The direction is chosen so the cut is true
/// climb (or true conventional) on a right-hand spindle regardless of
/// orientation — see the module-level truth table.
///
/// `spindle` selects between right-hand (M3, CW) and left-hand
/// (M4, CCW) spindle rotation. Left-hand flips the geometric winding
/// so "climb"/"conventional" intent matches physical chipload on either
/// spindle.
///
/// For short helices (`|dz| < pitch_mm`), `revolutions` would round
/// down to a fraction and emit a single G1 diagonal across the bore,
/// so it is floored at 1.0 — the cutter always completes at least one
/// full turn at `pitch_mm` per rev. Callers that need to
/// detect the shallow case should compare `|dz|` against `pitch_mm`
/// themselves and surface a `thread_dz_less_than_pitch` warning.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn helix_waypoints(
    center: Point2,
    radius: f64,
    top_z: f64,
    bottom_z: f64,
    pitch_mm: f64,
    climb: bool,
    internal: bool,
    tool_radius: f64,
    start_angle_rad: f64,
    spindle: crate::project::tool::SpindleDirection,
) -> Vec<(f64, f64, f64)> {
    helix_waypoints_with_density(
        center,
        radius,
        top_z,
        bottom_z,
        pitch_mm,
        climb,
        internal,
        tool_radius,
        DEFAULT_STEPS_PER_REV,
        start_angle_rad,
        spindle,
    )
}

/// Variant of [`helix_waypoints`] that takes an explicit chord
/// density (segments per revolution). Lower numbers produce coarser
/// polylines; higher numbers produce smoother but bigger gcode.
///
/// # Panics
///
/// Never panics in practice: `out.last().expect(..)` runs only after
/// the `0..=total_steps` loop pushes at least one point, and the
/// `total_steps == 0` early-return guards against the empty case.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn helix_waypoints_with_density(
    center: Point2,
    radius: f64,
    top_z: f64,
    bottom_z: f64,
    pitch_mm: f64,
    climb: bool,
    internal: bool,
    tool_radius: f64,
    steps_per_rev: usize,
    start_angle_rad: f64,
    spindle: crate::project::tool::SpindleDirection,
) -> Vec<(f64, f64, f64)> {
    use crate::project::tool::SpindleDirection;
    if radius <= 0.0 || pitch_mm <= 0.0 || steps_per_rev < 4 {
        return Vec::new();
    }
    let dz = bottom_z - top_z;
    if dz.abs() < 1e-9 {
        return Vec::new();
    }
    // Even when |dz| < pitch, we still need at least one full
    // revolution at the configured pitch. The previous formula
    // `revolutions = max(|dz|/pitch, 1/steps_per_rev)` rounded a
    // 5 µm finishing pass to ≈1.5 % of a rev, which collapsed to a
    // single G1 diagonal across the bore — full-bore crash on
    // internal, slam-into-stud on external. Guard with `.max(1.0)`
    // so the cutter always traces a full helix; the driver surfaces
    // a `thread_dz_less_than_pitch` warning when it makes this call.
    //
    // Number of revolutions; we round UP and let the last point land
    // exactly at bottom_z so the caller gets full-depth coverage even
    // if the Z range isn't an exact multiple of pitch.
    let revolutions = (dz.abs() / pitch_mm).max(1.0);
    let total_steps = (revolutions * steps_per_rev as f64).ceil() as usize;
    if total_steps == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(total_steps + 2);
    let two_pi = std::f64::consts::TAU;
    // Right-hand spindle climb truth table: ccw = climb XOR !internal.
    //   internal+climb       → CCW (+Δθ)
    //   internal+conventional→ CW  (-Δθ)
    //   external+climb       → CW  (-Δθ)
    //   external+conventional→ CCW (+Δθ)
    let ccw_rh = climb ^ !internal;
    // Left-hand spindle flips the geometric winding because the
    // cutting edge rotates the other way.
    let ccw = match spindle {
        SpindleDirection::Cw => ccw_rh,
        SpindleDirection::Ccw => !ccw_rh,
    };
    let dir: f64 = if ccw { 1.0 } else { -1.0 };
    for i in 0..=total_steps {
        let t = i as f64 / total_steps as f64;
        // Anchor at `start_angle_rad` so the cutter enters at
        // the caller-chosen angular position. Default 0 starts at
        // the +X axis.
        let theta = start_angle_rad + dir * t * revolutions * two_pi;
        let x = center.x + radius * theta.cos();
        let y = center.y + radius * theta.sin();
        let z = top_z + t * dz;
        out.push((x, y, z));
    }
    // Final radial retract at bottom_z so the caller's vertical lift
    // never drags the cutter against the freshly cut thread.
    // Internal: pull to bore center. External: push out by one full
    // tool diameter past the helix radius, plus a safety margin.
    let (rx, ry, rz) = *out.last().expect("loop pushed at least one point");
    let (retract_x, retract_y) = if internal {
        (center.x, center.y)
    } else {
        let dx = rx - center.x;
        let dy = ry - center.y;
        let len = (dx * dx + dy * dy).sqrt();
        // `radius` here is the helix radius (cutter centerline), so
        // moving out by 2*tool_radius+safety puts the cutter edge a
        // full tool diameter clear of the thread crest.
        let target = radius + 2.0 * tool_radius.max(0.0) + EXTERNAL_RETRACT_SAFETY_MM;
        if len > 1e-9 {
            let s = target / len;
            (center.x + dx * s, center.y + dy * s)
        } else {
            // Degenerate (helix endpoint on center) — push along +X.
            (center.x + target, center.y)
        }
    };
    out.push((retract_x, retract_y, rz));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::tool::SpindleDirection;

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    /// A single full revolution at pitch 1.0 descends exactly 1 mm,
    /// regardless of the chord tessellation count. The final point is a
    /// radial retract (internal → bore center) at the same Z.
    #[test]
    fn single_revolution_at_pitch_descends_z_by_pitch() {
        let wps = helix_waypoints(
            p(0.0, 0.0),
            5.0,
            0.0,
            -1.0,
            1.0,
            true,
            true,
            0.5,
            0.0,
            SpindleDirection::Cw,
        );
        assert!(wps.len() >= 3);
        // Helix endpoint is the second-to-last waypoint; retract sits
        // on top of it at the same Z.
        let helix_end = wps[wps.len() - 2];
        assert!((helix_end.2 - (-1.0)).abs() < 1e-9, "got {}", helix_end.2);
        let (_, _, retract_z) = *wps.last().unwrap();
        assert!(
            (retract_z - (-1.0)).abs() < 1e-9,
            "retract must stay at bottom_z; got {retract_z}"
        );
        // First point sits on +X axis at top_z.
        let (x0, y0, z0) = wps[0];
        assert!((x0 - 5.0).abs() < 1e-9);
        assert!(y0.abs() < 1e-9);
        assert!(z0.abs() < 1e-9);
    }

    /// Every helix waypoint (everything except the final retract) sits
    /// on the helix circle.
    #[test]
    fn every_waypoint_is_on_the_circle() {
        let wps = helix_waypoints(
            p(10.0, 20.0),
            3.0,
            0.0,
            -3.0,
            1.0,
            true,
            true,
            0.5,
            0.0,
            SpindleDirection::Cw,
        );
        assert!(wps.len() >= 2);
        // Drop the final retract waypoint; it intentionally leaves the
        // circle to clear the just-cut thread.
        for (x, y, _) in &wps[..wps.len() - 1] {
            let dx = x - 10.0;
            let dy = y - 20.0;
            let r = (dx * dx + dy * dy).sqrt();
            assert!((r - 3.0).abs() < 1e-6, "got radius {r}");
        }
    }

    /// Internal + climb=false walks CW (negative Δθ); the second
    /// waypoint sits below the X axis.
    #[test]
    fn internal_conventional_winds_clockwise() {
        let wps = helix_waypoints_with_density(
            p(0.0, 0.0),
            5.0,
            0.0,
            -1.0,
            1.0,
            false,
            true,
            0.5,
            64,
            0.0,
            SpindleDirection::Cw,
        );
        let (_, y1, _) = wps[1];
        assert!(
            y1 < 0.0,
            "internal+conventional must wind CW (second point in -Y); got y={y1}"
        );
    }

    /// Internal + climb=true walks CCW (positive Δθ); the second
    /// waypoint sits above the X axis.
    #[test]
    fn internal_climb_winds_counterclockwise() {
        let wps = helix_waypoints_with_density(
            p(0.0, 0.0),
            5.0,
            0.0,
            -1.0,
            1.0,
            true,
            true,
            0.5,
            64,
            0.0,
            SpindleDirection::Cw,
        );
        let (_, y1, _) = wps[1];
        assert!(
            y1 > 0.0,
            "internal+climb must wind CCW (second point in +Y); got y={y1}"
        );
    }

    /// External + climb=true walks CW on a right-hand spindle:
    /// the cutter is on the outside of the stud, so CCW would scrape
    /// in the conventional direction. Second waypoint sits below +X.
    #[test]
    fn external_climb_winds_cw() {
        let wps = helix_waypoints_with_density(
            p(0.0, 0.0),
            5.0,
            0.0,
            -1.0,
            1.0,
            true,
            false,
            0.5,
            64,
            0.0,
            SpindleDirection::Cw,
        );
        let (_, y1, _) = wps[1];
        assert!(
            y1 < 0.0,
            "external+climb must wind CW (second point in -Y); got y={y1}"
        );
    }

    /// External + climb=false (conventional) walks CCW. Second
    /// waypoint sits above +X.
    #[test]
    fn external_conventional_winds_ccw() {
        let wps = helix_waypoints_with_density(
            p(0.0, 0.0),
            5.0,
            0.0,
            -1.0,
            1.0,
            false,
            false,
            0.5,
            64,
            0.0,
            SpindleDirection::Cw,
        );
        let (_, y1, _) = wps[1];
        assert!(
            y1 > 0.0,
            "external+conventional must wind CCW (second point in +Y); got y={y1}"
        );
    }

    /// Multi-revolution descent (Z range = 4 mm, pitch = 1 mm) covers
    /// exactly the requested Z range. Both the helix end and the
    /// retract waypoint sit at `bottom_z`.
    #[test]
    fn multi_revolution_descent_reaches_bottom() {
        let wps = helix_waypoints(
            p(0.0, 0.0),
            5.0,
            0.0,
            -4.0,
            1.0,
            true,
            true,
            0.5,
            0.0,
            SpindleDirection::Cw,
        );
        let helix_end = wps[wps.len() - 2];
        assert!((helix_end.2 - (-4.0)).abs() < 1e-9, "got {}", helix_end.2);
        let (_, _, retract_z) = *wps.last().unwrap();
        assert!((retract_z - (-4.0)).abs() < 1e-9, "got {retract_z}");
    }

    /// Internal threads must end with a retract to the bore center so
    /// the post-helix G0 lift travels through cleared air.
    #[test]
    fn internal_retract_pulls_cutter_to_bore_center() {
        let wps = helix_waypoints(
            p(10.0, 20.0),
            4.5,
            0.0,
            -3.0,
            1.0,
            true,
            true,
            0.5,
            0.0,
            SpindleDirection::Cw,
        );
        let (lx, ly, _) = *wps.last().unwrap();
        assert!(
            (lx - 10.0).abs() < 1e-9 && (ly - 20.0).abs() < 1e-9,
            "internal retract must land on bore center; got ({lx}, {ly})"
        );
    }

    /// External threads must end radially outside the helix circle so
    /// the post-helix G0 lift doesn't drag the cutter through the
    /// freshly cut crest.
    #[test]
    fn external_retract_pushes_cutter_clear_of_thread() {
        let center = p(0.0, 0.0);
        let helix_radius = 5.0;
        let tool_radius = 1.0;
        let wps = helix_waypoints(
            center,
            helix_radius,
            0.0,
            -1.0,
            1.0,
            true,
            false,
            tool_radius,
            0.0,
            SpindleDirection::Cw,
        );
        let (lx, ly, _) = *wps.last().unwrap();
        let r = (lx * lx + ly * ly).sqrt();
        // Clear radius is helix_radius + tool_diameter + safety;
        // assert we cleared the helix by at least one tool diameter.
        assert!(
            r >= helix_radius + 2.0 * tool_radius,
            "external retract must clear helix_radius + 2*tool_radius; got r={r}"
        );
    }

    /// Degenerate inputs collapse to empty: radius=0, pitch=0, or
    /// equal top/bottom Z.
    #[test]
    fn degenerate_inputs_return_empty() {
        assert!(helix_waypoints(
            p(0.0, 0.0),
            0.0,
            0.0,
            -1.0,
            1.0,
            true,
            true,
            0.5,
            0.0,
            SpindleDirection::Cw,
        )
        .is_empty());
        assert!(helix_waypoints(
            p(0.0, 0.0),
            5.0,
            0.0,
            -1.0,
            0.0,
            true,
            true,
            0.5,
            0.0,
            SpindleDirection::Cw,
        )
        .is_empty());
        assert!(helix_waypoints(
            p(0.0, 0.0),
            5.0,
            0.0,
            0.0,
            1.0,
            true,
            true,
            0.5,
            0.0,
            SpindleDirection::Cw,
        )
        .is_empty());
    }

    /// A left-hand spindle (`SpindleDirection::Ccw`, M4 mode)
    /// flips the geometric winding so the user's climb intent matches
    /// physical chipload. Internal+climb on a RH spindle winds CCW;
    /// on a LH spindle the same intent must wind CW so the cutting edge
    /// (rotating the other way) still climbs.
    #[test]
    fn internal_climb_lefthand_spindle_winds_cw() {
        let wps = helix_waypoints_with_density(
            p(0.0, 0.0),
            5.0,
            0.0,
            -1.0,
            1.0,
            true,
            true,
            0.5,
            64,
            0.0,
            SpindleDirection::Ccw,
        );
        let (_, y1, _) = wps[1];
        assert!(
            y1 < 0.0,
            "internal+climb on LH spindle must wind CW (second point in -Y); got y={y1}"
        );
    }

    /// Short helices (|dz| < pitch) must not emit a single G1
    /// diagonal across the bore: revolutions would round to a fraction,
    /// so the function clamps to at least one full revolution. With
    /// dz=0.005, pitch=1.0 a naive fraction gives 2 waypoints (a
    /// straight diagonal); the clamp instead emits a full helix's worth
    /// of waypoints on the helix circle.
    #[test]
    fn shallow_dz_emits_at_least_one_full_turn() {
        let wps = helix_waypoints(
            p(0.0, 0.0),
            5.0,
            0.0,
            -0.005, // 5 µm — well under pitch
            1.0,    // 1 mm pitch
            true,
            true,
            0.5,
            0.0,
            SpindleDirection::Cw,
        );
        // Should have many waypoints (a full turn at default 64
        // steps/rev), not just 2 collinear points.
        assert!(
            wps.len() >= 4,
            "shallow dz must emit a full turn, got {} waypoints",
            wps.len()
        );
        // Every helix waypoint sits on the helix circle.
        for (x, y, _) in &wps[..wps.len() - 1] {
            let r = (x * x + y * y).sqrt();
            assert!(
                (r - 5.0).abs() < 1e-3,
                "shallow-dz helix waypoint off-circle: r={r}"
            );
        }
        // Last waypoint of the helix ring must reach the requested
        // bottom_z (= -0.005).
        let helix_end = wps[wps.len() - 2];
        assert!(
            (helix_end.2 - (-0.005)).abs() < 1e-9,
            "shallow helix must still reach bottom_z; got {}",
            helix_end.2
        );
    }
}
