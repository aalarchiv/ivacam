//! Target 3D surface for relief / ball-nose surfacing.
//!
//! A [`SurfaceField`] is the INPUT counterpart to the simulator's
//! [`crate::sim::heightmap::Heightmap`]: same row-major grid + bilinear
//! sampling, but it describes the surface we WANT to cut, not the carved
//! result. Cell `z[iy * cols + ix]` is the target Z at that grid point,
//! with the stock top at `z = 0` and relief carved downward (negative Z).
//!
//! The surface SOURCE is pluggable — the first one is a grayscale
//! image mapped through [`SurfaceField::from_grayscale`]; a future STL
//! rasterizer feeds the very same type. The drop-cutter surfacing engine
//! reads it through [`SurfaceField::sample`].
//!
//! Outside the field footprint `sample` returns `0.0` (the stock top) —
//! there is no relief beyond the image, so a ball-nose probing past the
//! edge sees uncut stock and won't gouge below it.

// f64 ↔ u32 grid-coordinate plumbing means a lot of intentional casts,
// mirroring the sibling heightmap module.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geometry::Point2;

/// Z (mm) returned when sampling outside the field footprint: the stock
/// top, i.e. "no relief here, don't cut below the surface".
pub const SURFACE_TOP_Z: f32 = 0.0;

/// A target Z(x,y) surface over a rectangular footprint. Row-major
/// `cols * rows` cells; cell `(ix, iy)`'s center sits at
/// `origin + ((ix + 0.5) * cell, (iy + 0.5) * cell)`, matching the
/// simulator heightmap's cell-center convention so the two grids align.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SurfaceField {
    /// World XY of the field's min corner (the (0,0) cell's lower-left).
    pub origin: Point2,
    /// Cell size in mm (square cells).
    pub cell: f64,
    pub cols: u32,
    pub rows: u32,
    /// Row-major target Z per cell (mm). Length must be `cols * rows`.
    /// Convention: stock top at 0, relief carved downward (Z <= 0).
    pub z: Vec<f32>,
}

impl SurfaceField {
    /// Build a field from an explicit Z grid.
    ///
    /// # Panics
    ///
    /// Panics if `cell <= 0`, either dimension is 0, the `cols * rows`
    /// product overflows `usize`, or `z.len() != cols * rows`.
    #[must_use]
    pub fn new(origin: Point2, cell: f64, cols: u32, rows: u32, z: Vec<f32>) -> Self {
        assert!(cell > 0.0, "SurfaceField cell size must be > 0");
        assert!(cols > 0 && rows > 0, "SurfaceField dimensions must be > 0");
        let len = (cols as usize)
            .checked_mul(rows as usize)
            .expect("surface dim overflow");
        assert_eq!(z.len(), len, "SurfaceField z length must equal cols * rows");
        Self {
            origin,
            cell,
            cols,
            rows,
            z,
        }
    }

    /// Map a normalized-brightness grid (each value in `[0, 1]`, row-major
    /// `cols * rows`) into a target surface, the relief-milling source.
    /// Brightness is linearly mapped to Z in `[z_min_mm, z_max_mm]`:
    /// by default bright = high (toward `z_max_mm`, the shallow/top end),
    /// dark = low (toward `z_min_mm`, the deepest cut) — the standard
    /// white-is-high relief convention. `invert` flips that (useful for
    /// negatives / engrave-the-light-areas reliefs).
    ///
    /// `z_min_mm` is the deepest (most negative) Z and `z_max_mm` the
    /// shallowest; they're sorted internally so callers can't invert the
    /// span by accident. Brightness values are clamped to `[0, 1]`.
    ///
    /// # Panics
    ///
    /// Panics under the same dimension rules as [`SurfaceField::new`]:
    /// `cell` must be > 0, both dimensions must be > 0, and
    /// `brightness.len()` must equal `cols * rows`.
    #[must_use]
    pub fn from_grayscale(
        origin: Point2,
        cell: f64,
        cols: u32,
        rows: u32,
        brightness: &[f32],
        z_min_mm: f64,
        z_max_mm: f64,
        invert: bool,
    ) -> Self {
        let len = (cols as usize)
            .checked_mul(rows as usize)
            .expect("surface dim overflow");
        assert_eq!(
            brightness.len(),
            len,
            "brightness length must equal cols * rows"
        );
        // Tolerate a flipped span: lo is always the deepest cut.
        let lo = z_min_mm.min(z_max_mm) as f32;
        let hi = z_min_mm.max(z_max_mm) as f32;
        let z = brightness
            .iter()
            .map(|&b| {
                let mut t = b.clamp(0.0, 1.0);
                if invert {
                    t = 1.0 - t;
                }
                // t = 1 (bright) → hi (shallow/top); t = 0 (dark) → lo (deep).
                lo + t * (hi - lo)
            })
            .collect();
        Self::new(origin, cell, cols, rows, z)
    }

    /// Target Z at cell `(ix, iy)`. Returns [`SURFACE_TOP_Z`] for indices
    /// outside the grid (no relief there).
    #[must_use]
    pub fn at(&self, ix: u32, iy: u32) -> f32 {
        if ix >= self.cols || iy >= self.rows {
            return SURFACE_TOP_Z;
        }
        self.z[(iy as usize) * (self.cols as usize) + (ix as usize)]
    }

    /// Bilinear sample of the target Z at world XY. Cell `(i, j)`'s center
    /// is `origin + (i + 0.5) * cell`; positions outside the sampleable
    /// region return [`SURFACE_TOP_Z`] (stock top — no relief beyond the
    /// footprint). Mirrors [`crate::sim::heightmap::Heightmap::sample`].
    #[must_use]
    pub fn sample(&self, x: f64, y: f64) -> f32 {
        let fx = (x - self.origin.x) / self.cell - 0.5;
        let fy = (y - self.origin.y) / self.cell - 0.5;
        if !fx.is_finite() || !fy.is_finite() {
            return SURFACE_TOP_Z;
        }
        let cols_max = self.cols as f64 - 1.0;
        let rows_max = self.rows as f64 - 1.0;
        if fx < 0.0 || fy < 0.0 || fx > cols_max || fy > rows_max {
            return SURFACE_TOP_Z;
        }
        let i0 = fx.floor();
        let j0 = fy.floor();
        let tx = (fx - i0) as f32;
        let ty = (fy - j0) as f32;
        let i0 = i0 as usize;
        let j0 = j0 as usize;
        let cols = self.cols as usize;
        let i1 = (i0 + 1).min(cols - 1);
        let j1 = (j0 + 1).min(self.rows as usize - 1);
        let v00 = self.z[j0 * cols + i0];
        let v10 = self.z[j0 * cols + i1];
        let v01 = self.z[j1 * cols + i0];
        let v11 = self.z[j1 * cols + i1];
        let a = v00 * (1.0 - tx) + v10 * tx;
        let b = v01 * (1.0 - tx) + v11 * tx;
        a * (1.0 - ty) + b * ty
    }

    /// World-space max corner (`origin + (cols, rows) * cell`).
    #[must_use]
    pub fn max_x(&self) -> f64 {
        self.origin.x + self.cols as f64 * self.cell
    }
    /// World-space max corner Y.
    #[must_use]
    pub fn max_y(&self) -> f64 {
        self.origin.y + self.rows as f64 * self.cell
    }

    /// `(min, max)` of the stored target Z, or `(0, 0)` for an empty grid.
    #[must_use]
    pub fn z_range(&self) -> (f32, f32) {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for &v in &self.z {
            lo = lo.min(v);
            hi = hi.max(v);
        }
        if lo.is_finite() {
            (lo, hi)
        } else {
            (0.0, 0.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-5, "expected {b}, got {a}");
    }

    #[test]
    fn at_reads_cells_row_major_and_clamps_out_of_bounds() {
        // 3x2 grid: z = ix + 10*iy.
        let z = vec![0.0, 1.0, 2.0, 10.0, 11.0, 12.0];
        let f = SurfaceField::new(Point2::new(0.0, 0.0), 1.0, 3, 2, z);
        approx(f.at(0, 0), 0.0);
        approx(f.at(2, 0), 2.0);
        approx(f.at(0, 1), 10.0);
        approx(f.at(2, 1), 12.0);
        // Out of bounds → stock top.
        approx(f.at(3, 0), SURFACE_TOP_Z);
        approx(f.at(0, 2), SURFACE_TOP_Z);
    }

    #[test]
    fn sample_hits_cell_centers_and_interpolates_midpoints() {
        // 2x2 grid, cell 2mm, origin (0,0). Cell centers at 1mm and 3mm.
        let z = vec![0.0, -2.0, -4.0, -6.0];
        let f = SurfaceField::new(Point2::new(0.0, 0.0), 2.0, 2, 2, z);
        // Cell centers reproduce stored values exactly.
        approx(f.sample(1.0, 1.0), 0.0);
        approx(f.sample(3.0, 1.0), -2.0);
        approx(f.sample(1.0, 3.0), -4.0);
        approx(f.sample(3.0, 3.0), -6.0);
        // Midpoint between the 4 centers = mean.
        approx(f.sample(2.0, 2.0), -3.0);
        // Horizontal midpoint of the bottom row.
        approx(f.sample(2.0, 1.0), -1.0);
    }

    #[test]
    fn sample_outside_footprint_returns_stock_top() {
        let z = vec![-5.0; 4];
        let f = SurfaceField::new(Point2::new(0.0, 0.0), 2.0, 2, 2, z);
        // Left/below the first cell center, and past the last.
        approx(f.sample(-1.0, -1.0), SURFACE_TOP_Z);
        approx(f.sample(100.0, 100.0), SURFACE_TOP_Z);
        // NaN guard.
        approx(f.sample(f64::NAN, 1.0), SURFACE_TOP_Z);
    }

    #[test]
    fn from_grayscale_maps_bright_high_dark_low_by_default() {
        // 1x2 column: dark (0.0) then bright (1.0). z in [-5, 0].
        let f = SurfaceField::from_grayscale(
            Point2::new(0.0, 0.0),
            1.0,
            1,
            2,
            &[0.0, 1.0],
            -5.0,
            0.0,
            false,
        );
        approx(f.at(0, 0), -5.0); // dark → deepest
        approx(f.at(0, 1), 0.0); // bright → top
                                 // Mid-grey lands halfway.
        let g = SurfaceField::from_grayscale(
            Point2::new(0.0, 0.0),
            1.0,
            1,
            1,
            &[0.5],
            -5.0,
            0.0,
            false,
        );
        approx(g.at(0, 0), -2.5);
    }

    #[test]
    fn from_grayscale_invert_flips_and_span_order_is_tolerated() {
        // invert: bright → deep.
        let f = SurfaceField::from_grayscale(
            Point2::new(0.0, 0.0),
            1.0,
            2,
            1,
            &[0.0, 1.0],
            -4.0,
            0.0,
            true,
        );
        approx(f.at(0, 0), 0.0); // dark → top (inverted)
        approx(f.at(1, 0), -4.0); // bright → deep (inverted)

        // Passing the span flipped (max first) yields the same mapping as
        // the sorted form: lo is always the deepest.
        let g = SurfaceField::from_grayscale(
            Point2::new(0.0, 0.0),
            1.0,
            2,
            1,
            &[0.0, 1.0],
            0.0,
            -4.0,
            false,
        );
        approx(g.at(0, 0), -4.0);
        approx(g.at(1, 0), 0.0);
    }

    #[test]
    fn from_grayscale_clamps_out_of_range_brightness() {
        let f = SurfaceField::from_grayscale(
            Point2::new(0.0, 0.0),
            1.0,
            2,
            1,
            &[-0.5, 1.5],
            -2.0,
            0.0,
            false,
        );
        approx(f.at(0, 0), -2.0); // clamped to 0 brightness → deep
        approx(f.at(1, 0), 0.0); // clamped to 1 brightness → top
    }

    #[test]
    fn z_range_reports_min_max() {
        let f = SurfaceField::new(
            Point2::new(0.0, 0.0),
            1.0,
            2,
            2,
            vec![-3.0, -1.0, -5.0, 0.0],
        );
        let (lo, hi) = f.z_range();
        approx(lo, -5.0);
        approx(hi, 0.0);
    }

    #[test]
    fn max_corner_helpers() {
        let f = SurfaceField::new(Point2::new(1.0, 2.0), 2.0, 3, 4, vec![0.0; 12]);
        approx((f.max_x() - 7.0) as f32, 0.0); // 1 + 3*2
        approx((f.max_y() - 10.0) as f32, 0.0); // 2 + 4*2
    }

    #[test]
    #[should_panic(expected = "z length must equal")]
    fn new_rejects_mismatched_z_length() {
        let _ = SurfaceField::new(Point2::new(0.0, 0.0), 1.0, 2, 2, vec![0.0; 3]);
    }
}
