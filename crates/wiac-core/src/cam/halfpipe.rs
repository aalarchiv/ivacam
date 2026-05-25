//! Halfpipe slot machining (rt1.19 — Estlcam `Prog_Halfpipe`).
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
//! into the existing ratchet emitter (`vcarve_emit::ratchet_emit`) and
//! V-Carve gcode block.

use crate::cam::vcarve::VPoint;
use crate::geometry::Point2;
use crate::project::HalfpipeProfile;

/// Compute the Z depth for one medial-axis vertex `v` under `profile`,
/// then clamp to `z_cap` (the op's max depth, absolute value;
/// `Some(d)` ⇒ result ≥ `-|d|`) and to `tool_reach_z` (898l — the
/// tool's flute length: a ball-nose's shank starts engaging stock past
/// the flutes, so we cap to keep the shank above z=0).
///
/// `at_corner` (mchy): true when `v` sits near a re-entrant boundary
/// corner (the two nearest boundary footings are on different segments,
/// subtending an angle < ~170° at `v`). At such points the floor must
/// match the *corner-arc* radius (= the configured profile R for
/// `CircularArc`; the cone formula at the user-provided angle for
/// `VBottom`) — the incircle `r` reflects the slot-narrowing toward
/// the corner, not the desired fillet radius, so the previous
/// behaviour (use `r` for depth everywhere) produced a wrong Z at
/// corners.
///
/// Returns `(z, depth_limited, tool_reach_limited)`. `depth_limited`
/// is true iff the profile cap (`CircularArc` with `r > R`) OR
/// `z_cap` clipped the result. `tool_reach_limited` is true iff
/// `tool_reach_z` clipped the result — surfaced separately so the
/// pipeline can emit a distinct `halfpipe_tool_reach_exceeded`
/// warning (898l).
#[must_use]
pub fn depth_at(
    v: &VPoint,
    profile: HalfpipeProfile,
    z_cap: Option<f64>,
    tool_reach_z: Option<f64>,
    at_corner: bool,
) -> (f64, bool, bool) {
    let r = v.r.max(0.0);
    let (mut z, mut limited) = match profile {
        HalfpipeProfile::CircularArc { radius_mm } => {
            let radius = radius_mm.max(0.0);
            if radius < 1e-9 {
                (0.0, true)
            } else if at_corner {
                // mchy: at a re-entrant corner the floor is a ball-nose
                // fillet of radius = the profile R, not a chord of the
                // local-slot half-circle. Use -R directly.
                (-radius, false)
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
            // mchy: VBottom is depth = -r / tan(half). At a corner the
            // depth corresponds to the bit's apex point sitting at the
            // bisector terminus; the natural mapping is the same
            // formula evaluated at the inscribed radius the corner
            // imposes (which IS `r`), so no override needed. But when
            // r=0 (right at the corner vertex) the formula returns 0 —
            // surface, no cut. That's the correct VBottom geometry
            // (the V apex perfectly fills the corner at depth 0
            // because the cone collapses to a point).
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
    let mut tool_reach_limited = false;
    if let Some(reach) = tool_reach_z {
        let cap = -reach.abs();
        if z < cap {
            z = cap;
            tool_reach_limited = true;
        }
    }
    (z, limited, tool_reach_limited)
}

/// Convert a medial-axis polyline to an XYZ polyline using the
/// halfpipe profile. The output tuple is `(x, y, z, r_inscribed)` so
/// downstream emitters that want the radius for sim / tabbing get it
/// (mirrors `cam/vcarve.rs::polyline_to_z`'s shape). Returns
/// `(points, depth_limited_anywhere, tool_reach_limited_anywhere)`.
///
/// `boundary` is the flattened set of boundary edges (outer ring + any
/// hole rings concatenated as `(start, end)` segments). Used by mchy
/// corner detection: a vertex whose two nearest boundary footings sit
/// on different segments subtending a sharp angle is treated as a
/// re-entrant corner. Pass `None` to disable corner detection (back-
/// compat for tests / non-corner-aware callers).
///
/// `tool_reach_z` (898l) is the ball-nose tool's flute length — the
/// maximum |z| the cutting flutes can engage stock before the shank
/// starts dragging. Passes `None` to skip the cap (test compat).
/// Return of [`polyline_to_z`]: the per-sample `(x, y, z, width)` track
/// plus `(depth_limited, tool_reach_limited)` flags.
pub type HalfpipeZProfile = (Vec<(f64, f64, f64, f64)>, bool, bool);

#[must_use]
pub fn polyline_to_z(
    axis: &[VPoint],
    profile: HalfpipeProfile,
    z_cap: Option<f64>,
    tool_reach_z: Option<f64>,
    boundary: Option<&[(Point2, Point2)]>,
) -> HalfpipeZProfile {
    let mut any_limited = false;
    let mut any_reach_limited = false;
    let mut out = Vec::with_capacity(axis.len());
    for v in axis {
        let at_corner = boundary.is_some_and(|segs| is_at_reentrant_corner(v, segs));
        let (z, limited, reach_limited) = depth_at(v, profile, z_cap, tool_reach_z, at_corner);
        if limited {
            any_limited = true;
        }
        if reach_limited {
            any_reach_limited = true;
        }
        out.push((v.x, v.y, z, v.r));
    }
    (out, any_limited, any_reach_limited)
}

/// mchy: a medial-axis vertex sits at a re-entrant corner iff its two
/// nearest boundary footings are on different (non-adjacent in the
/// equidistant-witness sense) segments AND the angle the two footings
/// subtend at the vertex is sharper than ~150°. At a straight-slot
/// medial-axis point the angle is ~180° (the two footings are on
/// opposite-facing edges across the slot); at a re-entrant corner of
/// internal angle θ the subtended angle equals θ, so any θ < 150° is
/// a confident corner detection.
fn is_at_reentrant_corner(v: &VPoint, boundary: &[(Point2, Point2)]) -> bool {
    if boundary.len() < 2 {
        return false;
    }
    let p = Point2::new(v.x, v.y);
    let mut nearest: Option<(usize, f64, Point2)> = None;
    let mut second: Option<(usize, f64, Point2)> = None;
    for (i, &(a, b)) in boundary.iter().enumerate() {
        let foot = closest_point_on_seg(a, b, p);
        let d = (foot.x - p.x).hypot(foot.y - p.y);
        match nearest {
            None => nearest = Some((i, d, foot)),
            Some((_, nd, _)) if d < nd => {
                second = nearest;
                nearest = Some((i, d, foot));
            }
            _ => match second {
                None => second = Some((i, d, foot)),
                Some((_, sd, _)) if d < sd => second = Some((i, d, foot)),
                _ => {}
            },
        }
    }
    let (Some((i1, d1, f1)), Some((i2, d2, f2))) = (nearest, second) else {
        return false;
    };
    if i1 == i2 {
        return false;
    }
    // Both footings must be roughly equidistant — the medial-axis
    // point is by definition equidistant from at least two boundary
    // features. Allow 20 % slack for numerical noise / sampled
    // boundaries.
    let close = d1.max(d2);
    let far = d1.min(d2).max(1e-9);
    if close > far * 1.2 + 0.01 {
        return false;
    }
    // Angle subtended at v by the two footing vectors.
    let v1x = f1.x - v.x;
    let v1y = f1.y - v.y;
    let v2x = f2.x - v.x;
    let v2y = f2.y - v.y;
    let n1 = (v1x.hypot(v1y)).max(1e-12);
    let n2 = (v2x.hypot(v2y)).max(1e-12);
    let cos_theta = (v1x * v2x + v1y * v2y) / (n1 * n2);
    let cos_theta = cos_theta.clamp(-1.0, 1.0);
    let theta = cos_theta.acos();
    // 150° = 2.618 rad. Below this the footings are decisively
    // non-opposite — the vertex sits at a sharp/re-entrant corner.
    theta < (150.0_f64).to_radians()
}

fn closest_point_on_seg(a: Point2, b: Point2, p: Point2) -> Point2 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-18 {
        return a;
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len_sq).clamp(0.0, 1.0);
    Point2::new(a.x + t * dx, a.y + t * dy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(x: f64, y: f64, r: f64) -> VPoint {
        VPoint { x, y, r }
    }

    /// `CircularArc` profile with R = 5: at r = 0, z = 0; at r = R, z = -R;
    /// at r = R/sqrt(2), z = -R + R/sqrt(2).
    #[test]
    fn circular_arc_profile_depth_curve() {
        let p = HalfpipeProfile::CircularArc { radius_mm: 5.0 };
        let (z0, _, _) = depth_at(&vp(0.0, 0.0, 0.0), p, None, None, false);
        assert!(z0.abs() < 1e-9, "got {z0}");
        let (z_full, lim, _) = depth_at(&vp(0.0, 0.0, 5.0), p, None, None, false);
        // At r >= R, the slot is at-or-wider than the pipe; depth
        // caps at -R and we mark depth_limited so the warning fires.
        assert!((z_full - (-5.0)).abs() < 1e-9, "got {z_full}");
        assert!(
            lim,
            "r>=R must report depth_limited (slot is at/beyond the pipe envelope)"
        );
        // r > R clamps to -R + depth_limited
        let (z_over, lim, _) = depth_at(&vp(0.0, 0.0, 7.0), p, None, None, false);
        assert!((z_over - (-5.0)).abs() < 1e-9, "got {z_over}");
        assert!(lim, "r > R must report depth_limited");
        // r = R/√2 ≈ 3.5355: z = -(R - √(R² - r²)) = -(5 - √(25 - 12.5)) ≈ -1.464.
        let (z_mid, lim, _) =
            depth_at(&vp(0.0, 0.0, 5.0_f64 / std::f64::consts::SQRT_2), p, None, None, false);
        assert!(
            (z_mid - (-1.464_466_094_067_261_9)).abs() < 1e-9,
            "got {z_mid}"
        );
        assert!(!lim);
    }

    /// `VBottom` profile with 60° apex (half=30°): tan(30°) ≈ 0.5774;
    /// z = -r / 0.5774 ≈ -r * 1.7321.
    #[test]
    fn v_bottom_profile_depth_curve() {
        let p = HalfpipeProfile::VBottom {
            included_angle_deg: 60.0,
        };
        let (z, lim, _) = depth_at(&vp(0.0, 0.0, 1.0), p, None, None, false);
        assert!((z - (-1.732_050_8)).abs() < 1e-6, "got {z}");
        assert!(!lim);
    }

    /// `z_cap` clips both profiles. `CircularArc` with R=5 and `z_cap=2`:
    /// at r=R, natural z=-5 → clipped to -2 (`depth_limited=true`).
    #[test]
    fn z_cap_clips_both_profiles() {
        let p = HalfpipeProfile::CircularArc { radius_mm: 5.0 };
        let (z, lim, _) = depth_at(&vp(0.0, 0.0, 5.0), p, Some(2.0), None, false);
        assert!((z - (-2.0)).abs() < 1e-9, "got {z}");
        assert!(lim);
    }

    /// `polyline_to_z` propagates the `depth_limited` flag.
    #[test]
    fn polyline_propagates_depth_limited_flag() {
        let axis = vec![vp(0.0, 0.0, 1.0), vp(1.0, 0.0, 8.0), vp(2.0, 0.0, 0.5)];
        let p = HalfpipeProfile::CircularArc { radius_mm: 5.0 };
        let (pts, lim, _reach) = polyline_to_z(&axis, p, None, None, None);
        assert_eq!(pts.len(), 3);
        // Middle vertex r=8 > R=5 → depth_limited=true overall.
        assert!(lim);
    }

    /// 898l: a ball-nose tool with a 5mm flute length cutting a
    /// profile R=20 must clip Z to -5mm and report `tool_reach_limited`
    /// at the deepest medial-axis vertex.
    #[test]
    fn tool_reach_caps_circular_arc_depth() {
        let p = HalfpipeProfile::CircularArc { radius_mm: 20.0 };
        // At r=20 (slot width = 40mm), natural z = -20. With a 5mm
        // flute the cap should fire and clamp to z=-5.
        let (z, _, reach_lim) = depth_at(&vp(0.0, 0.0, 20.0), p, None, Some(5.0), false);
        assert!((z - (-5.0)).abs() < 1e-9, "expected z=-5, got {z}");
        assert!(reach_lim, "expected tool_reach_limited=true");
    }

    /// 898l: when the profile depth is shallower than the tool flute,
    /// the cap doesn't fire.
    #[test]
    fn tool_reach_cap_does_not_fire_when_deeper_flute_available() {
        let p = HalfpipeProfile::CircularArc { radius_mm: 3.0 };
        // At r=3 (slot width = 6mm), natural z = -3. With a 10mm flute
        // the tool can easily reach the floor.
        let (z, _, reach_lim) = depth_at(&vp(0.0, 0.0, 3.0), p, None, Some(10.0), false);
        assert!((z - (-3.0)).abs() < 1e-9, "expected z=-3, got {z}");
        assert!(!reach_lim);
    }

    /// 898l: `polyline_to_z` propagates the tool-reach flag.
    #[test]
    fn polyline_propagates_tool_reach_flag() {
        let axis = vec![vp(0.0, 0.0, 1.0), vp(1.0, 0.0, 20.0), vp(2.0, 0.0, 0.5)];
        let p = HalfpipeProfile::CircularArc { radius_mm: 20.0 };
        let (_pts, _lim, reach_lim) = polyline_to_z(&axis, p, None, Some(5.0), None);
        assert!(reach_lim);
    }

    /// mchy: at a re-entrant corner the ball-nose floor depth must be
    /// the *corner-arc* radius (= profile R), not the incircle r. Use
    /// `at_corner = true` on a small-r vertex and verify the depth
    /// equals -R instead of the slot-width formula.
    #[test]
    fn halfpipe_z_at_reentrant_corner_uses_corner_radius_not_incircle() {
        let p = HalfpipeProfile::CircularArc { radius_mm: 5.0 };
        // Incircle r = 1 at a corner point. Without corner detection:
        // z = -(5 - sqrt(25 - 1)) ≈ -0.101. With corner detection:
        // z = -5 (the full ball radius).
        let v = vp(0.0, 0.0, 1.0);
        let (z_no_corner, _, _) = depth_at(&v, p, None, None, false);
        let (z_corner, _, _) = depth_at(&v, p, None, None, true);
        assert!(
            (z_no_corner - (-0.101_020_5)).abs() < 1e-4,
            "non-corner depth changed: got {z_no_corner}",
        );
        assert!(
            (z_corner - (-5.0)).abs() < 1e-9,
            "corner depth must equal -R (profile radius), got {z_corner}",
        );
    }

    /// mchy: `is_at_reentrant_corner` returns true for a vertex on the
    /// bisector of a 60° re-entrant corner, and false for a vertex
    /// inside a straight slot.
    #[test]
    fn is_at_corner_detects_reentrant_geometry() {
        use crate::geometry::Point2;
        // Two boundary segments meeting at the origin at 60°. The
        // medial-axis bisector is the x-axis (positive direction).
        let half = (60.0_f64 * 0.5).to_radians();
        let s1 = (
            Point2::new(0.0, 0.0),
            Point2::new(10.0 * half.cos(), 10.0 * half.sin()),
        );
        let s2 = (
            Point2::new(0.0, 0.0),
            Point2::new(10.0 * half.cos(), -10.0 * half.sin()),
        );
        // Vertex on the bisector, 1 mm from the corner.
        let v_corner = vp(1.0, 0.0, 1.0 * half.sin());
        assert!(is_at_reentrant_corner(&v_corner, &[s1, s2]));
        // Vertex in a straight 4-mm-wide slot (two parallel walls).
        let p_top = (Point2::new(-20.0, 2.0), Point2::new(20.0, 2.0));
        let p_bot = (Point2::new(-20.0, -2.0), Point2::new(20.0, -2.0));
        let v_slot = vp(0.0, 0.0, 2.0);
        assert!(
            !is_at_reentrant_corner(&v_slot, &[p_top, p_bot]),
            "straight slot should NOT be flagged as a corner",
        );
    }
}
