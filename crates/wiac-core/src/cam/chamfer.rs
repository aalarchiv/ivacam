//! Chamfer / edge break (rt1.18 — Estlcam `Prog_Fasen`).
//!
//! A chamfer op walks a V-bit along the source contour at a single
//! constant Z below the workpiece surface. The cone of the V-bit
//! cuts a triangular bevel against the workpiece edge whose
//! horizontal width equals the user-set chamfer width. The cutter
//! centerline rides directly on the source path — no XY offset —
//! and the bevel forms on whichever side of the contour the
//! workpiece sits.
//!
//! Geometry:
//! ```text
//!                  /\  ← V-bit cone, half-angle α (= tip_angle / 2)
//!                 /  \
//!                /    \
//!   workpiece  /╳╳╳╳╳╳\  air
//!  ──────────╳──┬──╳───────  z = 0  (workpiece top)
//!            ╳  │  ╳
//!            ╳  │  ╳         depth = D
//!             ╳ │ ╳
//!              ╲│╱           cone tip at z = -D
//! ```
//! When the cutter centerline (cone tip) sits at `z = -D`, the cone
//! at z = 0 has spread to `D · tan(α)` on each side. The chamfer
//! width on the workpiece side equals that span:
//!
//! ```text
//!     width = D · tan(α)
//!     D     = width / tan(α)
//! ```
//!
//! ## Pause / dwell concerns
//!
//! Constant-Z along the contour: the cutter walks the source path at
//! a pinned final Z. The cone tip's vertical descent from the work
//! surface down to that Z, however, is a plunge into solid stock —
//! it has to follow the tool's normal stepdown schedule like any
//! other op (00ia: forcing single-pass snapped V-bits on deep
//! chamfers). The constant-Z claim only applies to the contour
//! traversal, not to the initial descent.
//!
//! ## Physical reach cap (uo1t)
//!
//! A V-bit can only engage the workpiece as long as the cone is the
//! thing touching it. Past the cone the cutter has shank — engaging
//! deeper drives the shank into stock, scraping / burning / snapping
//! the bit. The cone reaches from the tip (`tip_diameter / 2` from
//! centerline) out to the shank edge (`diameter / 2`); the chamfer
//! width that just barely fills the cone is therefore
//! `(diameter - tip_diameter) / 2`. [`chamfer_depth_capped`] clamps
//! the requested width to this physical max and reports whether the
//! clamp actually fired.

/// Compute the cone-tip Z (negative) needed to chamfer an edge by
/// `width_mm` with a V-bit whose full apex angle is
/// `tip_angle_deg`. Result is clamped to ≤ 0 so a misconfigured
/// pair (e.g. width 0) collapses to a no-op cut rather than an
/// upward Z move.
///
/// Pure cone math with no tool-reach awareness — callers that have
/// the tool geometry should prefer [`chamfer_depth_capped`].
#[must_use]
pub fn chamfer_depth(width_mm: f64, tip_angle_deg: f64) -> f64 {
    if width_mm <= 0.0 {
        return 0.0;
    }
    let half = (tip_angle_deg.clamp(1.0, 179.0) * 0.5).to_radians();
    let t = half.tan().max(1e-6);
    -(width_mm / t)
}

/// Largest chamfer width a V-bit can physically cut without the
/// shank touching stock. Equals the radial distance the cone spans
/// from tip-flat to shank: `(diameter - tip_diameter) / 2`.
///
/// `tip_diameter_mm` is the V-bit's nose-flat diameter (0 for a true
/// pointed bit; commonly a few tenths of a mm for engraving cutters).
#[must_use]
pub fn chamfer_width_cap_mm(diameter_mm: f64, tip_diameter_mm: f64) -> f64 {
    let d = diameter_mm.max(0.0);
    let tip = tip_diameter_mm.max(0.0).min(d);
    (d - tip) * 0.5
}

/// Tool-reach-aware variant of [`chamfer_depth`]. Clamps the requested
/// `width_mm` to [`chamfer_width_cap_mm`] before solving for Z so the
/// returned depth never drives the shank into stock. Returns the
/// clamped Z plus the effective width actually used; the caller can
/// compare against `width_mm` to decide whether to emit a warning.
#[must_use]
pub fn chamfer_depth_capped(
    width_mm: f64,
    tip_angle_deg: f64,
    diameter_mm: f64,
    tip_diameter_mm: f64,
) -> ChamferDepthSolution {
    let cap = chamfer_width_cap_mm(diameter_mm, tip_diameter_mm);
    let requested = width_mm.max(0.0);
    let clamped = requested > cap + 1e-9;
    let effective_width = if clamped { cap } else { requested };
    ChamferDepthSolution {
        z: chamfer_depth(effective_width, tip_angle_deg),
        effective_width_mm: effective_width,
        width_cap_mm: cap,
        clamped_to_reach: clamped,
    }
}

/// Result of [`chamfer_depth_capped`]: the cone-tip Z to cut at, the
/// width that actually fits the cone, and a flag indicating whether
/// the user's requested width was clamped (so the caller can surface
/// a warning).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChamferDepthSolution {
    pub z: f64,
    pub effective_width_mm: f64,
    pub width_cap_mm: f64,
    pub clamped_to_reach: bool,
}

#[cfg(test)]
// Asserts compare chamfer_depth against literal expected values
// (0.0, 30°/60° trig outputs) that are exactly representable in f64.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// 60° V-bit, 1mm chamfer width → depth = 1 / tan(30°) ≈ -1.732 mm.
    #[test]
    fn depth_60_degree_one_mm_width() {
        let z = chamfer_depth(1.0, 60.0);
        assert!((z - (-1.732_050)).abs() < 1e-3, "got {z}");
    }

    /// 90° V-bit, 2mm chamfer width → depth = 2 / tan(45°) = -2 mm.
    #[test]
    fn depth_90_degree_two_mm_width() {
        let z = chamfer_depth(2.0, 90.0);
        assert!((z - (-2.0)).abs() < 1e-6, "got {z}");
    }

    /// width <= 0 yields zero depth (no-op).
    #[test]
    fn zero_width_is_zero_depth() {
        assert_eq!(chamfer_depth(0.0, 60.0), 0.0);
        assert_eq!(chamfer_depth(-1.0, 60.0), 0.0);
    }

    /// Out-of-range tip angles get clamped rather than panicking.
    #[test]
    fn extreme_tip_angles_dont_panic() {
        let _ = chamfer_depth(1.0, 0.0);
        let _ = chamfer_depth(1.0, 180.0);
        let _ = chamfer_depth(1.0, -5.0);
    }

    /// uo1t: width cap = (diameter - tip_diameter) / 2. A 6mm V-bit
    /// with 0mm tip can cut a max chamfer width of 3mm.
    #[test]
    fn width_cap_uses_cone_span() {
        assert_eq!(chamfer_width_cap_mm(6.0, 0.0), 3.0);
        assert_eq!(chamfer_width_cap_mm(6.35, 0.1), 3.125);
        // Degenerate inputs collapse safely.
        assert_eq!(chamfer_width_cap_mm(0.0, 0.0), 0.0);
        assert_eq!(chamfer_width_cap_mm(-1.0, 0.5), 0.0);
        // tip > diameter clamps to diameter (cap = 0).
        assert_eq!(chamfer_width_cap_mm(2.0, 5.0), 0.0);
    }

    /// uo1t acceptance: chamfer_depth_capped(10, 60) on a 6mm V-bit
    /// emits a warning (clamped=true) and returns Z clamped to the
    /// physical reach (= cone span / tan(30°) = 3 / 0.5773 ≈ 5.196).
    #[test]
    fn capped_depth_clamps_oversize_width_to_tool_reach() {
        let sol = chamfer_depth_capped(10.0, 60.0, 6.0, 0.0);
        assert!(sol.clamped_to_reach, "expected reach clamp to fire");
        assert!((sol.effective_width_mm - 3.0).abs() < 1e-9);
        assert!((sol.width_cap_mm - 3.0).abs() < 1e-9);
        // 3 / tan(30°) ≈ 5.196152
        assert!(
            (sol.z - (-5.196_152)).abs() < 1e-3,
            "got {z}",
            z = sol.z
        );
        // Without the cap the user would have gotten a Z driving the
        // shank into stock: 10 / tan(30°) ≈ -17.32 mm.
        assert!(chamfer_depth(10.0, 60.0) < sol.z - 1.0);
    }

    /// In-range widths pass through unchanged (no clamp, no warning).
    #[test]
    fn capped_depth_passes_in_range_widths_through() {
        let sol = chamfer_depth_capped(1.0, 60.0, 6.35, 0.1);
        assert!(!sol.clamped_to_reach);
        assert!((sol.effective_width_mm - 1.0).abs() < 1e-9);
        assert!((sol.z - (-1.732_050)).abs() < 1e-3);
    }
}
