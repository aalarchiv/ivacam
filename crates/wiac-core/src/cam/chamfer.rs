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
//! Constant-Z toolpaths don't need `finish_step` or step-down; this
//! emitter pins the depth and emits a single pass plus an optional
//! second pass tagged `is_finish` so the tool's finish-set rates
//! (rt1.27) drive the surface-quality cleanup.

/// Compute the cone-tip Z (negative) needed to chamfer an edge by
/// `width_mm` with a V-bit whose full apex angle is
/// `tip_angle_deg`. Result is clamped to ≤ 0 so a misconfigured
/// pair (e.g. width 0) collapses to a no-op cut rather than an
/// upward Z move.
#[must_use] pub fn chamfer_depth(width_mm: f64, tip_angle_deg: f64) -> f64 {
    if width_mm <= 0.0 {
        return 0.0;
    }
    let half = (tip_angle_deg.clamp(1.0, 179.0) * 0.5).to_radians();
    let t = half.tan().max(1e-6);
    -(width_mm / t)
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
}
