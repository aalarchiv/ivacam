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
//! at z = 0 has spread to `tip_radius + D · tan(α)` on each side
//! (`tip_radius = tip_diameter / 2` is the cutter's nose-flat
//! radius; for a perfectly pointed V-bit it's 0). The chamfer width
//! on the workpiece side equals that span:
//!
//! ```text
//!     width = tip_radius + D · tan(α)
//!     D     = (width - tip_radius) / tan(α)
//! ```
//!
//! **e63q**: the pre-fix formula was `D = width / tan(α)` — ignoring
//! the tip flat, the cone was lowered by `tip_radius / tan(α)` too
//! much, producing a chamfer width of `width + tip_radius` instead of
//! `width`. For a 0.3mm-tip 60° V-bit cutting a 1mm chamfer, the
//! over-cut was 0.15mm — visible on signs and significant when the
//! chamfer is a clearance fit.
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
/// `tip_diameter_mm` is the V-bit's nose-flat diameter (0 for a
/// perfectly pointed bit; commonly 0.1–1 mm for engraving / sign-
/// making cutters). The flat means the chamfer width at z=0 is
/// `tip_radius + D * tan(α)` rather than `D * tan(α)` — solving for
/// D gives `(width - tip_radius) / tan(α)`. (e63q)
///
/// Pure cone math with no tool-reach awareness — callers that have
/// the tool geometry should prefer [`chamfer_depth_capped`].
#[must_use]
pub fn chamfer_depth(width_mm: f64, tip_angle_deg: f64, tip_diameter_mm: f64) -> f64 {
    if width_mm <= 0.0 {
        return 0.0;
    }
    let half = (tip_angle_deg.clamp(1.0, 179.0) * 0.5).to_radians();
    let t = half.tan().max(1e-6);
    let tip_radius = (tip_diameter_mm.max(0.0)) * 0.5;
    // If the user's requested width is shallower than the tip flat,
    // the cone never reaches z=0 wider than the tip — the result
    // collapses to a no-cut. Saturate at 0 so we don't emit an
    // upward Z move.
    let effective = (width_mm - tip_radius).max(0.0);
    -(effective / t)
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
        z: chamfer_depth(effective_width, tip_angle_deg, tip_diameter_mm),
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

    /// 60° V-bit (pointed, tip=0), 1mm chamfer width → depth =
    /// 1 / tan(30°) ≈ -1.732 mm.
    #[test]
    fn depth_60_degree_one_mm_width() {
        let z = chamfer_depth(1.0, 60.0, 0.0);
        assert!((z - (-1.732_050)).abs() < 1e-3, "got {z}");
    }

    /// 90° V-bit (pointed), 2mm chamfer width → depth = 2 / tan(45°) = -2 mm.
    #[test]
    fn depth_90_degree_two_mm_width() {
        let z = chamfer_depth(2.0, 90.0, 0.0);
        assert!((z - (-2.0)).abs() < 1e-6, "got {z}");
    }

    /// width <= 0 yields zero depth (no-op).
    #[test]
    fn zero_width_is_zero_depth() {
        assert_eq!(chamfer_depth(0.0, 60.0, 0.0), 0.0);
        assert_eq!(chamfer_depth(-1.0, 60.0, 0.0), 0.0);
    }

    /// Out-of-range tip angles get clamped rather than panicking.
    #[test]
    fn extreme_tip_angles_dont_panic() {
        let _ = chamfer_depth(1.0, 0.0, 0.0);
        let _ = chamfer_depth(1.0, 180.0, 0.0);
        let _ = chamfer_depth(1.0, -5.0, 0.0);
    }

    /// e63q: 60° V-bit with a 0.3mm tip flat cutting a 1mm chamfer
    /// must produce a chamfer width of EXACTLY 1mm at z=0 — the cone
    /// math must subtract the tip radius before dividing by tan(half).
    /// Pre-fix the formula was D = 1/tan(30°) = -1.732 → actual
    /// chamfer width = `tip_r` + D*tan(half) = 0.15 + 1 = 1.15 (15%
    /// over).
    #[test]
    fn depth_compensates_for_tip_flat() {
        let tip_diameter = 0.3;
        let z = chamfer_depth(1.0, 60.0, tip_diameter);
        // Expected: D = (1 - 0.15) / tan(30°) = 0.85 / 0.5774 ≈ -1.472
        assert!(
            (z - (-1.472_242)).abs() < 1e-3,
            "got {z}, expected ≈ -1.472",
        );
        // Forward-check: the cone at z=0 spans `tip_radius + |z| *
        // tan(α)` and must equal `width_mm` exactly.
        let half = 30.0_f64.to_radians();
        let cone_radius_at_surface = tip_diameter * 0.5 + z.abs() * half.tan();
        assert!(
            (cone_radius_at_surface - 1.0).abs() < 1e-6,
            "actual chamfer width = {cone_radius_at_surface}, expected 1.0",
        );
    }

    /// e63q: a width <= `tip_radius` is impossible — the cutter is
    /// already wider than the requested chamfer at z=0. The function
    /// saturates at z=0 rather than emitting an upward (positive) Z.
    #[test]
    fn depth_saturates_to_zero_when_width_under_tip() {
        // 0.5 mm width with a 1mm-tip flat (tip_radius=0.5).
        let z = chamfer_depth(0.4, 60.0, 1.0);
        assert!(z.abs() < 1e-9, "expected 0.0 (no cut), got {z}");
    }

    /// uo1t: width cap = (diameter - `tip_diameter`) / 2. A 6mm V-bit
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

    /// uo1t acceptance: `chamfer_depth_capped(10`, 60) on a 6mm V-bit
    /// emits a warning (clamped=true) and returns Z clamped to the
    /// physical reach (= cone span / tan(30°) = 3 / 0.5773 ≈ 5.196).
    #[test]
    fn capped_depth_clamps_oversize_width_to_tool_reach() {
        let sol = chamfer_depth_capped(10.0, 60.0, 6.0, 0.0);
        assert!(sol.clamped_to_reach, "expected reach clamp to fire");
        assert!((sol.effective_width_mm - 3.0).abs() < 1e-9);
        assert!((sol.width_cap_mm - 3.0).abs() < 1e-9);
        // 3 / tan(30°) ≈ 5.196152
        assert!((sol.z - (-5.196_152)).abs() < 1e-3, "got {z}", z = sol.z);
        // Without the cap the user would have gotten a Z driving the
        // shank into stock: 10 / tan(30°) ≈ -17.32 mm.
        assert!(chamfer_depth(10.0, 60.0, 0.0) < sol.z - 1.0);
    }

    /// In-range widths pass through unchanged (no clamp, no warning).
    /// e63q: with `tip_diameter=0.1` (`tip_radius=0.05`) and a 60° V-bit,
    /// chamfering a 1mm width gives `D = (1 - 0.05) / tan(30°) ≈
    /// -1.645 mm` (NOT -1.732, which was the pre-e63q bug). The
    /// `effective_width_mm` field still reflects the user's requested
    /// width (1.0) — that's what actually got carved at z=0.
    #[test]
    fn capped_depth_passes_in_range_widths_through() {
        let sol = chamfer_depth_capped(1.0, 60.0, 6.35, 0.1);
        assert!(!sol.clamped_to_reach);
        assert!((sol.effective_width_mm - 1.0).abs() < 1e-9);
        assert!(
            (sol.z - (-1.645_448)).abs() < 1e-3,
            "got {}, expected ≈ -1.645",
            sol.z,
        );
    }
}
