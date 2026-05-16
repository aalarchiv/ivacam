//! Helical thread emitter (rt1.17 — Estlcam Prog_Thread / IG / AG).
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
//! `climb=true` walks CCW (positive Δθ) — the standard climb-cut
//! direction on a right-hand spindle. `climb=false` walks CW.
//!
//! ## Internal vs external
//!
//! Whether the cutter goes inside or outside the source circle is the
//! caller's choice — this module just takes a final `radius`. The
//! pipeline driver computes:
//!
//! * Internal (cutter walks inside the bore): `helix_radius = bore_radius - tool_radius`
//! * External (cutter walks around a stud): `helix_radius = stud_radius + tool_radius`

use crate::geometry::Point2;

/// Default chord-tessellation density per full revolution. 64 segments
/// keeps the chord error below ~0.2% of `radius` even at small bores,
/// which is well under the typical thread tolerance.
const DEFAULT_STEPS_PER_REV: usize = 64;

/// Emit the helical thread path as a list of (x, y, z) waypoints.
/// The first waypoint sits on the start angle (0 rad from +X) at
/// `top_z`; the last waypoint sits on the same angle (or near it,
/// depending on the revolution count) at `bottom_z`. Empty when the
/// inputs collapse to a no-op (radius <= 0, pitch <= 0, no Z range).
pub fn helix_waypoints(
    center: Point2,
    radius: f64,
    top_z: f64,
    bottom_z: f64,
    pitch_mm: f64,
    climb: bool,
) -> Vec<(f64, f64, f64)> {
    helix_waypoints_with_density(
        center,
        radius,
        top_z,
        bottom_z,
        pitch_mm,
        climb,
        DEFAULT_STEPS_PER_REV,
    )
}

/// Variant of [`helix_waypoints`] that takes an explicit chord
/// density (segments per revolution). Lower numbers produce coarser
/// polylines; higher numbers produce smoother but bigger gcode.
pub fn helix_waypoints_with_density(
    center: Point2,
    radius: f64,
    top_z: f64,
    bottom_z: f64,
    pitch_mm: f64,
    climb: bool,
    steps_per_rev: usize,
) -> Vec<(f64, f64, f64)> {
    if radius <= 0.0 || pitch_mm <= 0.0 || steps_per_rev < 4 {
        return Vec::new();
    }
    let dz = bottom_z - top_z;
    if dz.abs() < 1e-9 {
        return Vec::new();
    }
    // Number of revolutions; we round UP and let the last point land
    // exactly at bottom_z so the caller gets full-depth coverage even
    // if the Z range isn't an exact multiple of pitch.
    let revolutions = (dz.abs() / pitch_mm).max(1.0 / steps_per_rev as f64);
    let total_steps = (revolutions * steps_per_rev as f64).ceil() as usize;
    if total_steps == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(total_steps + 1);
    let two_pi = std::f64::consts::TAU;
    // climb=true → CCW on right-hand spindle. We don't need to know
    // the spindle direction here; the caller picks `climb` based on
    // the desired chip-load behavior.
    let dir: f64 = if climb { 1.0 } else { -1.0 };
    for i in 0..=total_steps {
        let t = i as f64 / total_steps as f64;
        let theta = dir * t * revolutions * two_pi;
        let x = center.x + radius * theta.cos();
        let y = center.y + radius * theta.sin();
        let z = top_z + t * dz;
        out.push((x, y, z));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    /// A single full revolution at pitch 1.0 descends exactly 1 mm,
    /// regardless of the chord tessellation count.
    #[test]
    fn single_revolution_at_pitch_descends_z_by_pitch() {
        let wps = helix_waypoints(p(0.0, 0.0), 5.0, 0.0, -1.0, 1.0, true);
        assert!(!wps.is_empty());
        let (_, _, last_z) = *wps.last().unwrap();
        assert!((last_z - (-1.0)).abs() < 1e-9, "got {last_z}");
        // First point sits on +X axis at top_z.
        let (x0, y0, z0) = wps[0];
        assert!((x0 - 5.0).abs() < 1e-9);
        assert!(y0.abs() < 1e-9);
        assert!(z0.abs() < 1e-9);
    }

    /// Every waypoint sits on the helix circle: distance from center
    /// matches the chosen radius.
    #[test]
    fn every_waypoint_is_on_the_circle() {
        let wps = helix_waypoints(p(10.0, 20.0), 3.0, 0.0, -3.0, 1.0, true);
        for (x, y, _) in &wps {
            let dx = x - 10.0;
            let dy = y - 20.0;
            let r = (dx * dx + dy * dy).sqrt();
            assert!((r - 3.0).abs() < 1e-6, "got radius {r}");
        }
    }

    /// climb=false walks CW (negative Δθ); the second waypoint sits
    /// below the X axis.
    #[test]
    fn climb_false_winds_clockwise() {
        let wps = helix_waypoints_with_density(p(0.0, 0.0), 5.0, 0.0, -1.0, 1.0, false, 64);
        let (_, y1, _) = wps[1];
        assert!(
            y1 < 0.0,
            "CW direction must put second point in -Y; got y={y1}"
        );
    }

    /// climb=true walks CCW (positive Δθ); the second waypoint sits
    /// above the X axis.
    #[test]
    fn climb_true_winds_counterclockwise() {
        let wps = helix_waypoints_with_density(p(0.0, 0.0), 5.0, 0.0, -1.0, 1.0, true, 64);
        let (_, y1, _) = wps[1];
        assert!(
            y1 > 0.0,
            "CCW direction must put second point in +Y; got y={y1}"
        );
    }

    /// Multi-revolution descent (Z range = 4 mm, pitch = 1 mm) covers
    /// exactly the requested Z range.
    #[test]
    fn multi_revolution_descent_reaches_bottom() {
        let wps = helix_waypoints(p(0.0, 0.0), 5.0, 0.0, -4.0, 1.0, true);
        let (_, _, last_z) = *wps.last().unwrap();
        assert!((last_z - (-4.0)).abs() < 1e-9, "got {last_z}");
    }

    /// Degenerate inputs collapse to empty: radius=0, pitch=0, or
    /// equal top/bottom Z.
    #[test]
    fn degenerate_inputs_return_empty() {
        assert!(helix_waypoints(p(0.0, 0.0), 0.0, 0.0, -1.0, 1.0, true).is_empty());
        assert!(helix_waypoints(p(0.0, 0.0), 5.0, 0.0, -1.0, 0.0, true).is_empty());
        assert!(helix_waypoints(p(0.0, 0.0), 5.0, 0.0, 0.0, 1.0, true).is_empty());
    }
}
