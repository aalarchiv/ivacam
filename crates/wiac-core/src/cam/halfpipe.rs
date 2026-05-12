//! Halfpipe slot machining (rt1.19 — Estlcam Prog_Halfpipe).
//!
//! Walk the closed region's medial axis at varying Z so the cut floor
//! matches the configured cross-section profile. The slot's width at
//! each medial-axis point (= 2 · inscribed-circle radius) drives the
//! depth via the `HalfpipeProfile`.
//!
//! ## Profiles
//!
//! - **`CircularArc { radius_mm: R }`** — circular cross-section
//!   (ball-bottom slot). At a medial-axis point with inscribed-
//!   circle radius `r`, depth is the height of a chord at distance
//!   `r` from the center of a circle of radius `R`:
//!   `z = -(R - sqrt(R² - r²))`. When `r > R`, the slot is wider than
//!   the desired pipe — depth caps at `-R` (the deepest point of a
//!   half-cylinder). Use with a ball-nose cutter whose radius matches
//!   `R`.
//!
//! - **`VBottom { included_angle_deg: θ }`** — V-bottom cross-section
//!   (same formula as `cam/vcarve.rs`): `z = -r / tan(θ/2)`. Use with
//!   a V-bit. Equivalent to running V-Carve at full depth.
//!
//! Both depths are then clipped to the user-set op `depth` (passed in
//! as `z_cap`). The returned polyline is the medial-axis walked
//! point-by-point with the computed Z; the pipeline driver feeds it
//! into the existing ratchet emitter (vcarve_emit::ratchet_emit) and
//! V-Carve gcode block.

use crate::cam::vcarve::VPoint;
use crate::project::HalfpipeProfile;

/// Compute the Z depth for one medial-axis vertex `v` under `profile`,
/// then clamp to `z_cap` (the op's max depth, absolute value;
/// `Some(d)` ⇒ result ≥ `-|d|`).
///
/// Returns `(z, depth_limited)` — `depth_limited` is true iff either
/// the profile cap (CircularArc with `r > R`) OR `z_cap` clipped the
/// result.
pub fn depth_at(v: &VPoint, profile: HalfpipeProfile, z_cap: Option<f64>) -> (f64, bool) {
    let r = v.r.max(0.0);
    let (mut z, mut limited) = match profile {
        HalfpipeProfile::CircularArc { radius_mm } => {
            let radius = radius_mm.max(0.0);
            if radius < 1e-9 {
                (0.0, true)
            } else if r >= radius {
                // Slot wider than the pipe radius: dip to the
                // half-cylinder's deepest point (-R).
                (-radius, true)
            } else {
                let inside = radius.mul_add(radius, -(r * r)).max(0.0);
                (-(radius - inside.sqrt()), false)
            }
        }
        HalfpipeProfile::VBottom { included_angle_deg } => {
            let half = (included_angle_deg.clamp(1.0, 179.0) * 0.5).to_radians();
            let t = half.tan().max(1e-9);
            (-r / t, false)
        }
    };
    if let Some(c) = z_cap {
        let cap = -c.abs();
        if z < cap {
            z = cap;
            limited = true;
        }
    }
    (z, limited)
}

/// Convert a medial-axis polyline to an XYZ polyline using the
/// halfpipe profile. The output tuple is `(x, y, z, r_inscribed)` so
/// downstream emitters that want the radius for sim / tabbing get it
/// (mirrors `cam/vcarve.rs::polyline_to_z`'s shape). Returns
/// `(points, depth_limited_anywhere)`.
pub fn polyline_to_z(
    axis: &[VPoint],
    profile: HalfpipeProfile,
    z_cap: Option<f64>,
) -> (Vec<(f64, f64, f64, f64)>, bool) {
    let mut any_limited = false;
    let mut out = Vec::with_capacity(axis.len());
    for v in axis {
        let (z, limited) = depth_at(v, profile, z_cap);
        if limited {
            any_limited = true;
        }
        out.push((v.x, v.y, z, v.r));
    }
    (out, any_limited)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(x: f64, y: f64, r: f64) -> VPoint {
        VPoint { x, y, r }
    }

    /// CircularArc profile with R = 5: at r = 0, z = 0; at r = R, z = -R;
    /// at r = R/sqrt(2), z = -R + R/sqrt(2).
    #[test]
    fn circular_arc_profile_depth_curve() {
        let p = HalfpipeProfile::CircularArc { radius_mm: 5.0 };
        let (z0, _) = depth_at(&vp(0.0, 0.0, 0.0), p, None);
        assert!(z0.abs() < 1e-9, "got {z0}");
        let (z_full, lim) = depth_at(&vp(0.0, 0.0, 5.0), p, None);
        // At r >= R, the slot is at-or-wider than the pipe; depth
        // caps at -R and we mark depth_limited so the warning fires.
        assert!((z_full - (-5.0)).abs() < 1e-9, "got {z_full}");
        assert!(lim, "r>=R must report depth_limited (slot is at/beyond the pipe envelope)");
        // r > R clamps to -R + depth_limited
        let (z_over, lim) = depth_at(&vp(0.0, 0.0, 7.0), p, None);
        assert!((z_over - (-5.0)).abs() < 1e-9, "got {z_over}");
        assert!(lim, "r > R must report depth_limited");
        // r = R/√2 ≈ 3.5355: z = -(R - √(R² - r²)) = -(5 - √(25 - 12.5)) ≈ -1.464.
        let (z_mid, lim) = depth_at(&vp(0.0, 0.0, 5.0_f64 / std::f64::consts::SQRT_2), p, None);
        assert!((z_mid - (-1.4644660940672619)).abs() < 1e-9, "got {z_mid}");
        assert!(!lim);
    }

    /// VBottom profile with 60° apex (half=30°): tan(30°) ≈ 0.5774;
    /// z = -r / 0.5774 ≈ -r * 1.7321.
    #[test]
    fn v_bottom_profile_depth_curve() {
        let p = HalfpipeProfile::VBottom { included_angle_deg: 60.0 };
        let (z, lim) = depth_at(&vp(0.0, 0.0, 1.0), p, None);
        assert!((z - (-1.7320508)).abs() < 1e-6, "got {z}");
        assert!(!lim);
    }

    /// z_cap clips both profiles. CircularArc with R=5 and z_cap=2:
    /// at r=R, natural z=-5 → clipped to -2 (depth_limited=true).
    #[test]
    fn z_cap_clips_both_profiles() {
        let p = HalfpipeProfile::CircularArc { radius_mm: 5.0 };
        let (z, lim) = depth_at(&vp(0.0, 0.0, 5.0), p, Some(2.0));
        assert!((z - (-2.0)).abs() < 1e-9, "got {z}");
        assert!(lim);
    }

    /// polyline_to_z propagates the depth_limited flag.
    #[test]
    fn polyline_propagates_depth_limited_flag() {
        let axis = vec![vp(0.0, 0.0, 1.0), vp(1.0, 0.0, 8.0), vp(2.0, 0.0, 0.5)];
        let p = HalfpipeProfile::CircularArc { radius_mm: 5.0 };
        let (pts, lim) = polyline_to_z(&axis, p, None);
        assert_eq!(pts.len(), 3);
        // Middle vertex r=8 > R=5 → depth_limited=true overall.
        assert!(lim);
    }
}
