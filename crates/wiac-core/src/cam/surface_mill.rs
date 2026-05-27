//! Drop-cutter ball-nose raster surfacing engine (f60x-B).
//!
//! Given a target [`SurfaceField`] (the surface we want to cut, f60x-A), a
//! ball-nose radius, and a stepover, produce gouge-free XYZ tool paths that
//! a ball end-mill can follow to finish the relief.
//!
//! ## The drop-cutter
//!
//! Place the ball so its tip is at `(x, y, z_tip)`; the ball CENTER is then
//! at `z_tip + R`, and the ball's lower surface at radial offset `d` from
//! the axis sits `R - √(R² − d²)` above the tip (this is exactly the
//! [`crate::sim::heightmap::ToolProfile::BallNose`] profile). For the ball
//! not to gouge the target at a neighbour `(x+dx, y+dy)` we need
//!
//! ```text
//! z_tip + (R − √(R² − d²)) ≥ target(x+dx, y+dy)
//! ```
//!
//! so the deepest the tip can sit without gouging anywhere in its footprint
//! is the MAX over the disc of `target(neighbour) − offset(d)`. Computing
//! that for every grid cell is a grayscale morphological dilation of the
//! target by the (negated) ball — the "dropped" field. A ball-nose tip
//! following the dropped field touches the surface but never cuts into it;
//! features narrower than the ball are automatically rounded over (the tip
//! rides up on the surrounding high ground), which is the physically
//! correct behaviour.
//!
//! ## Cost
//!
//! [`drop_cutter`] is `O(cells · kernel)` where the kernel is the ~`πR²`
//! cells inside the ball footprint. Fine for typical reliefs; for very
//! large grids or large `R/cell` the ball offset can be approximated by a
//! parabola and computed with a separable parabolic distance transform
//! (Felzenszwalb) in `O(cells)`. Left as a future optimization — the exact
//! sphere kernel is correct and simple.

// f64 ↔ grid-index casts throughout, as in the sibling surface / heightmap
// modules.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless
)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::surface::{SurfaceField, SURFACE_TOP_Z};

/// Direction the parallel finishing scanlines run. `AlongX` lines sweep in
/// X and step over in Y; `AlongY` is the transpose. (Diagonal raster is a
/// future addition — X/Y cover the common cases and keep coverage math
/// simple.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanDirection {
    #[default]
    AlongX,
    AlongY,
}

/// Inputs to [`surface_mill`]. The caller (the op driver, f60x-C) resolves
/// these from the op + tool: `z_floor_mm` is the deepest tip Z allowed
/// (`max(op.depth, -flute_reach)`), `z_top_mm` the ceiling (stock top, 0).
#[derive(Debug, Clone, Copy)]
pub struct SurfaceMillParams {
    /// Ball-nose radius (mm). Must be > 0.
    pub tool_radius_mm: f64,
    /// Target scallop height between adjacent passes (mm). Drives the
    /// stepover via [`stepover_from_scallop`] unless `stepover_mm` overrides.
    pub scallop_height_mm: f64,
    /// Explicit stepover override (mm). `Some(s > 0)` wins over the scallop
    /// computation; `None` (or non-positive) falls back to the scallop.
    pub stepover_mm: Option<f64>,
    /// Sampling pitch ALONG each scanline (mm). Finer = smoother path, more
    /// points. Clamped to at least a quarter cell.
    pub along_step_mm: f64,
    /// Scanline direction.
    pub direction: ScanDirection,
    /// Deepest tip Z allowed (mm, negative). Tip is clamped to this floor —
    /// the caller folds in op depth and tool flute reach.
    pub z_floor_mm: f64,
    /// Ceiling for the tip Z (mm). Usually 0 (stock top): the tip never
    /// rises above the stock surface.
    pub z_top_mm: f64,
}

/// The ball's surface height above its tip at radial offset `d_mm`:
/// `R − √(R² − d²)` for `d ≤ R`, and `R` (the equator height) beyond. This
/// is the [`crate::sim::heightmap::ToolProfile::BallNose`] profile, lifted
/// here so the drop-cutter and the sim agree on the cutter shape.
#[must_use]
pub fn ball_drop_offset(radius_mm: f64, d_mm: f64) -> f64 {
    if d_mm >= radius_mm {
        radius_mm
    } else {
        radius_mm - (radius_mm * radius_mm - d_mm * d_mm).max(0.0).sqrt()
    }
}

/// Stepover (mm) that leaves a given scallop height between two adjacent
/// ball passes on a flat floor: `s = 2·√(2Rh − h²)`. Clamped to the tool
/// diameter (`h ≥ R` would otherwise ask for an impossibly wide step).
#[must_use]
pub fn stepover_from_scallop(tool_radius_mm: f64, scallop_height_mm: f64) -> f64 {
    let r = tool_radius_mm;
    let h = scallop_height_mm.clamp(0.0, r);
    (2.0 * (2.0 * r * h - h * h).max(0.0).sqrt()).min(2.0 * r)
}

/// Read the target Z at signed cell indices, returning [`SURFACE_TOP_Z`]
/// (stock top — no relief) for indices off the grid so the ball sees uncut
/// stock past the edge and won't drop below it.
#[inline]
fn target_signed(field: &SurfaceField, ix: i64, iy: i64) -> f32 {
    if ix < 0 || iy < 0 || ix >= field.cols as i64 || iy >= field.rows as i64 {
        SURFACE_TOP_Z
    } else {
        field.z[(iy as usize) * (field.cols as usize) + (ix as usize)]
    }
}

/// Precomputed ball footprint: `(dx, dy, offset)` for every cell whose
/// center lies within `R` of the axis.
fn ball_kernel(radius_mm: f64, cell: f64) -> Vec<(i32, i32, f32)> {
    let kr = (radius_mm / cell).ceil() as i32;
    let mut k = Vec::with_capacity(((2 * kr + 1) * (2 * kr + 1)) as usize);
    for dy in -kr..=kr {
        for dx in -kr..=kr {
            let dmm = (((dx as f64) * cell).powi(2) + ((dy as f64) * cell).powi(2)).sqrt();
            if dmm <= radius_mm {
                k.push((dx, dy, ball_drop_offset(radius_mm, dmm) as f32));
            }
        }
    }
    k
}

/// Compute the "dropped" field: tip Z at every cell such that a ball of
/// `tool_radius_mm` touches the target but never gouges it. The result has
/// the same grid as `field`; every cell satisfies `dropped ≥ target` (the
/// `d = 0` kernel term, whose offset is 0). Panics if `tool_radius_mm ≤ 0`.
#[must_use]
pub fn drop_cutter(field: &SurfaceField, tool_radius_mm: f64) -> SurfaceField {
    assert!(tool_radius_mm > 0.0, "tool radius must be > 0");
    let kernel = ball_kernel(tool_radius_mm, field.cell);
    let cols = field.cols as i64;
    let rows = field.rows as i64;
    let mut out = vec![SURFACE_TOP_Z; field.z.len()];
    for iy in 0..rows {
        for ix in 0..cols {
            let mut best = f32::NEG_INFINITY;
            for &(dx, dy, off) in &kernel {
                let cand = target_signed(field, ix + dx as i64, iy + dy as i64) - off;
                if cand > best {
                    best = cand;
                }
            }
            out[(iy * cols + ix) as usize] = best;
        }
    }
    SurfaceField::new(field.origin, field.cell, field.cols, field.rows, out)
}

/// Effective stepover: explicit override (if positive) else the scallop
/// computation, floored at half a cell so the line spacing always makes
/// progress and the count stays finite.
fn effective_stepover(field: &SurfaceField, p: &SurfaceMillParams) -> f64 {
    let s = match p.stepover_mm {
        Some(v) if v > 0.0 => v,
        _ => stepover_from_scallop(p.tool_radius_mm, p.scallop_height_mm),
    };
    s.max(field.cell * 0.5)
}

/// Axis sample positions from `lo` to `hi` (inclusive of both ends) spaced
/// by `step`. Always includes `hi` as the final position so the far edge is
/// covered even when `step` doesn't divide the span evenly.
fn axis_positions(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    if hi <= lo + 1e-9 {
        return vec![lo];
    }
    let mut v = Vec::new();
    let mut p = lo;
    while p < hi - 1e-9 {
        v.push(p);
        p += step;
    }
    v.push(hi);
    v
}

/// Generate gouge-free ball-nose finishing scanlines over the target
/// surface. Returns one XYZ polyline per scanline, boustrophedon-ordered
/// (alternate lines reverse direction) so consecutive lines join end-to-end
/// without a long rapid back to the start. Tip Z is clamped to
/// `[z_floor_mm, z_top_mm]`.
///
/// The paths sweep the cell-center region of the field; sampling the
/// dropped field bilinearly between cells is safe because dilation smooths
/// the surface (the dropped field's slope is bounded by the ball).
#[must_use]
pub fn surface_mill(field: &SurfaceField, params: &SurfaceMillParams) -> Vec<Vec<(f64, f64, f64)>> {
    assert!(params.tool_radius_mm > 0.0, "tool radius must be > 0");
    let dropped = drop_cutter(field, params.tool_radius_mm);
    let step = effective_stepover(field, params);
    let along = params.along_step_mm.max(field.cell * 0.25).max(1e-3);

    // Sample within the cell-center region so bilinear `sample` returns real
    // interpolated values (the field's outer half-cell ring reads as top).
    let x0 = field.origin.x + 0.5 * field.cell;
    let x1 = field.origin.x + (field.cols as f64 - 0.5) * field.cell;
    let y0 = field.origin.y + 0.5 * field.cell;
    let y1 = field.origin.y + (field.rows as f64 - 0.5) * field.cell;

    let clamp_z = |z: f32| -> f64 { (z as f64).clamp(params.z_floor_mm, params.z_top_mm) };

    let mut polylines = Vec::new();
    match params.direction {
        ScanDirection::AlongX => {
            for (k, &y) in axis_positions(y0, y1, step).iter().enumerate() {
                let mut xs = axis_positions(x0, x1, along);
                if k % 2 == 1 {
                    xs.reverse();
                }
                let poly = xs
                    .into_iter()
                    .map(|x| (x, y, clamp_z(dropped.sample(x, y))))
                    .collect();
                polylines.push(poly);
            }
        }
        ScanDirection::AlongY => {
            for (k, &x) in axis_positions(x0, x1, step).iter().enumerate() {
                let mut ys = axis_positions(y0, y1, along);
                if k % 2 == 1 {
                    ys.reverse();
                }
                let poly = ys
                    .into_iter()
                    .map(|y| (x, y, clamp_z(dropped.sample(x, y))))
                    .collect();
                polylines.push(poly);
            }
        }
    }
    polylines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point2;

    fn approx(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() < eps, "expected {b}, got {a}");
    }

    /// Flat surface in, scallop 0.1 mm on a 3 mm-radius ball.
    #[test]
    fn stepover_from_scallop_matches_formula_and_clamps() {
        // s = 2*sqrt(2*3*0.1 - 0.1^2) = 2*sqrt(0.59).
        approx(stepover_from_scallop(3.0, 0.1), 2.0 * 0.59_f64.sqrt(), 1e-9);
        // h >= R clamps to the diameter.
        approx(stepover_from_scallop(2.0, 5.0), 4.0, 1e-9);
        // h = 0 → zero stepover (caller floors it).
        approx(stepover_from_scallop(2.0, 0.0), 0.0, 1e-12);
    }

    #[test]
    fn ball_offset_is_zero_at_axis_and_radius_at_equator() {
        approx(ball_drop_offset(3.0, 0.0), 0.0, 1e-12);
        // At d = R the surface has risen the full radius.
        approx(ball_drop_offset(3.0, 3.0), 3.0, 1e-12);
        // Beyond R it saturates at R.
        approx(ball_drop_offset(3.0, 9.0), 3.0, 1e-12);
        // Midway: 3 - sqrt(9 - 2.25) = 3 - sqrt(6.75).
        approx(ball_drop_offset(3.0, 1.5), 3.0 - 6.75_f64.sqrt(), 1e-12);
    }

    /// The dropped field never gouges: for every cell `p` and every cell `q`
    /// within the ball footprint, `dropped(p) ≥ target(q) − offset(|p−q|)`.
    /// In particular `dropped ≥ target` everywhere.
    #[test]
    fn drop_cutter_is_gouge_free() {
        // A bumpy 8x8 target with a deep narrow pit and a raised ridge.
        let cols = 8u32;
        let rows = 8u32;
        let mut z = vec![0.0f32; (cols * rows) as usize];
        for iy in 0..rows {
            for ix in 0..cols {
                let v = -0.5 * (ix as f32) - 0.3 * (iy as f32);
                z[(iy * cols + ix) as usize] = v;
            }
        }
        z[(3 * cols + 3) as usize] = -20.0; // narrow pit
        let field = SurfaceField::new(Point2::new(0.0, 0.0), 1.0, cols, rows, z);
        let r = 2.0;
        let dropped = drop_cutter(&field, r);

        for py in 0..rows as i64 {
            for px in 0..cols as i64 {
                let dp = target_signed(&dropped, px, py);
                // dropped >= target at the same cell.
                assert!(
                    dp + 1e-4 >= target_signed(&field, px, py),
                    "dropped < target at ({px},{py})"
                );
                // No-gouge against every neighbour in the footprint.
                let kr = (r / field.cell).ceil() as i64;
                for dy in -kr..=kr {
                    for dx in -kr..=kr {
                        let dmm = (((dx as f64) * field.cell).powi(2)
                            + ((dy as f64) * field.cell).powi(2))
                        .sqrt();
                        if dmm > r {
                            continue;
                        }
                        let tq = target_signed(&field, px + dx, py + dy);
                        let needed = tq as f64 - ball_drop_offset(r, dmm);
                        assert!(
                            dp as f64 + 1e-4 >= needed,
                            "gouge at ({px},{py}) vs ({},{}): dropped {dp} < {needed}",
                            px + dx,
                            py + dy
                        );
                    }
                }
            }
        }
    }

    /// A pit narrower than the ball can't be reached — the tip rides up on
    /// the surrounding flat (stays near 0), nowhere near the pit's -20.
    #[test]
    fn narrow_pit_is_unreachable() {
        let cols = 9u32;
        let rows = 9u32;
        let mut z = vec![0.0f32; (cols * rows) as usize];
        z[(4 * cols + 4) as usize] = -20.0;
        let field = SurfaceField::new(Point2::new(0.0, 0.0), 1.0, cols, rows, z);
        let dropped = drop_cutter(&field, 3.0);
        // At the pit cell the tip is held up by the ring of 0-height
        // neighbours one cell away: best ≈ -offset(1mm), far above -20.
        let at_pit = dropped.at(4, 4);
        assert!(
            at_pit > -1.0,
            "ball should ride over the narrow pit, tip = {at_pit}"
        );
    }

    /// A flat floor much wider than the ball IS reachable — interior cells
    /// drop all the way to the floor (the ball sits flush).
    #[test]
    fn wide_flat_floor_is_reachable() {
        let cols = 20u32;
        let rows = 20u32;
        let z = vec![-5.0f32; (cols * rows) as usize];
        let field = SurfaceField::new(Point2::new(0.0, 0.0), 1.0, cols, rows, z);
        let dropped = drop_cutter(&field, 3.0);
        // A deep-interior cell, far from the (top-valued) off-grid edge.
        approx(dropped.at(10, 10) as f64, -5.0, 1e-4);
    }

    #[test]
    fn surface_mill_flat_target_follows_floor_and_boustrophedons() {
        // Flat target at -2, plenty of headroom to the floor.
        let cols = 20u32;
        let rows = 20u32;
        let z = vec![-2.0f32; (cols * rows) as usize];
        let field = SurfaceField::new(Point2::new(0.0, 0.0), 1.0, cols, rows, z);
        let params = SurfaceMillParams {
            tool_radius_mm: 2.0,
            scallop_height_mm: 0.0,
            stepover_mm: Some(4.0),
            along_step_mm: 2.0,
            direction: ScanDirection::AlongX,
            z_floor_mm: -10.0,
            z_top_mm: 0.0,
        };
        let lines = surface_mill(&field, &params);
        assert!(lines.len() >= 2, "expected several scanlines");
        // Interior Z follows the floor (edge samples may read shallower as
        // the dropped field rises toward the top-valued off-grid ring).
        let mid = &lines[lines.len() / 2];
        let midpt = mid[mid.len() / 2];
        approx(midpt.2, -2.0, 1e-3);
        // Boustrophedon: line 0 runs +x, line 1 runs −x.
        assert!(lines[0][0].0 < lines[0][lines[0].len() - 1].0);
        assert!(lines[1][0].0 > lines[1][lines[1].len() - 1].0);
        // All lines share the same Y per line, stepping over in Y.
        assert!(lines[0][0].1 < lines[1][0].1);
    }

    #[test]
    fn surface_mill_clamps_to_z_floor() {
        // Target deeper than the floor → every tip clamps to the floor.
        let cols = 16u32;
        let rows = 16u32;
        let z = vec![-20.0f32; (cols * rows) as usize];
        let field = SurfaceField::new(Point2::new(0.0, 0.0), 1.0, cols, rows, z);
        let params = SurfaceMillParams {
            tool_radius_mm: 1.0,
            scallop_height_mm: 0.0,
            stepover_mm: Some(2.0),
            along_step_mm: 1.0,
            direction: ScanDirection::AlongY,
            z_floor_mm: -5.0,
            z_top_mm: 0.0,
        };
        let lines = surface_mill(&field, &params);
        for line in &lines {
            for &(_, _, z) in line {
                assert!(z >= -5.0 - 1e-9, "tip {z} below the floor");
            }
        }
        // AlongY: a single line varies in Y at fixed X.
        let l0 = &lines[0];
        approx(l0[0].0, l0[l0.len() - 1].0, 1e-9);
        assert!((l0[0].1 - l0[l0.len() - 1].1).abs() > 1.0);
    }
}
