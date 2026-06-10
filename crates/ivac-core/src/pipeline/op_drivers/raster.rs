//! Laser raster-engrave driver (phase 3).
//!
//! Resolves the op's [`crate::project::ReliefSource`] brightness grid,
//! maps it through [`crate::cam::raster::PowerCurve`] to a per-pixel
//! laser-power (`S`) grid, and emits it row-by-row. Each row is
//! **run-length grouped by power**: a span of equal-power pixels becomes a
//! single `M3 S<power>` + `G1` (the post's `laser_on` is modal-deduped),
//! so smooth gradients and binary dithers both stay compact. `M3 S0` arms
//! the beam cold and `M5` drops it; the laser is dropped for every
//! inter-row reposition and re-armed for the scan.
//!
//! Plot-mode XY only — no Z modulation (laser focus is fixed). Honors the
//! scan direction, the link mode (unidirectional lift-between vs
//! boustrophedon), and an overscan lead-in/-out so the head reaches feed
//! before it crosses the first burning pixel. Laser-only.

use crate::cam::raster::RasterLink;
use crate::cam::setup::Setup;
use crate::cam::surface_mill::ScanDirection;
use crate::gcode::PostProcessor;
use crate::geometry::Point2;
use crate::pipeline::{CancelToken, PipelineError, PipelineWarning};
use crate::project::MachineMode;
use crate::project::{Op, OpKind, Project, ReliefSource};

/// Hard ceiling on resampled pixel count — beyond this the line-buffered
/// post would balloon memory. A real >16 Mpx engrave wants the streaming
/// emit (streaming follow-up); until then we warn and skip rather than OOM.
const MAX_RASTER_PIXELS: usize = 16_000_000;

fn find_source(project: &Project, id: u32) -> Option<&ReliefSource> {
    project.relief_sources.iter().find(|s| s.id == id)
}

/// True when the op references an existing, non-empty source AND the
/// machine is in laser mode — the Level-1 emit gate (mirrors
/// `relief_would_emit`). Raster engraving is meaningless off a laser, so a
/// non-laser machine gates the op out (the op×machine-mode warning already
/// tells the user why).
pub(in crate::pipeline) fn raster_would_emit(op: &Op, project: &Project) -> bool {
    let OpKind::RasterEngrave { source_id, .. } = &op.kind else {
        return false;
    };
    matches!(project.machine.mode, MachineMode::Laser)
        && find_source(project, *source_id).is_some_and(|s| !s.brightness.is_empty())
}

/// Post-resample grid dimensions for a `target_pitch`, computed WITHOUT
/// allocating. `resample` returns exactly these dims, so the emit cap can
/// be enforced against them before the (potentially huge) grid is built —
/// otherwise a tiny `target_pitch` blows up the allocation before the
/// pixel-count guard even runs. Mirrors `resample`'s identity short-circuit
/// (untouched dims when pitch is ≤0, within 1 µm of `cell`, or empty).
fn resampled_dims(cols: usize, rows: usize, cell: f64, target_pitch: f64) -> (usize, usize) {
    if target_pitch <= 0.0 || (target_pitch - cell).abs() < 1e-6 || cols == 0 || rows == 0 {
        return (cols, rows);
    }
    let width = cols as f64 * cell;
    let height = rows as f64 * cell;
    let new_cols = ((width / target_pitch).round() as usize).max(1);
    let new_rows = ((height / target_pitch).round() as usize).max(1);
    (new_cols, new_rows)
}

/// Nearest-neighbour resample of a brightness grid to a new square pitch.
/// Returns `(brightness, cols, rows)` at `target_pitch`. A `target_pitch`
/// at/below 0 or within 1 µm of `cell` returns the grid untouched.
fn resample(
    brightness: &[f32],
    cols: usize,
    rows: usize,
    cell: f64,
    target_pitch: f64,
) -> (Vec<f32>, usize, usize) {
    if target_pitch <= 0.0 || (target_pitch - cell).abs() < 1e-6 || cols == 0 || rows == 0 {
        return (brightness.to_vec(), cols, rows);
    }
    let (new_cols, new_rows) = resampled_dims(cols, rows, cell, target_pitch);
    let mut out = vec![0.0f32; new_cols * new_rows];
    for ny in 0..new_rows {
        // Sample at the centre of each target cell, mapped back to source.
        let sy = (((ny as f64 + 0.5) * target_pitch / cell) as usize).min(rows - 1);
        for nx in 0..new_cols {
            let sx = (((nx as f64 + 0.5) * target_pitch / cell) as usize).min(cols - 1);
            out[ny * new_cols + nx] = brightness[sy * cols + sx];
        }
    }
    (out, new_cols, new_rows)
}

/// Emit a laser raster-engrave op. No-op when the source is missing /
/// empty or the machine isn't a laser (the `would_emit` gate normally
/// screens those out); over-large grids warn and skip.
// `unnecessary_wraps` — the Result<(), _> return is never an Err
// today, but the uniform op-driver signature (sibling run_*_op fns all
// return Result, dispatched polymorphically) keeps the wrapper.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::unnecessary_wraps
)]
pub(in crate::pipeline) fn run_raster_op<P: PostProcessor>(
    op: &Op,
    project: &Project,
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    _cancel: Option<&CancelToken>,
) -> Result<(), PipelineError> {
    let OpKind::RasterEngrave {
        source_id,
        resolution_mm,
        power_curve,
        scan_direction,
        link,
        overscan_factor,
    } = &op.kind
    else {
        return Ok(());
    };
    // Laser-only; the op×machine-mode warning already flagged the misuse.
    if !matches!(project.machine.mode, MachineMode::Laser) {
        return Ok(());
    }
    let Some(source) = find_source(project, *source_id) else {
        return Ok(());
    };
    if source.brightness.is_empty() || source.cols == 0 || source.rows == 0 {
        return Ok(());
    }

    let cell = if source.cell > 0.0 { source.cell } else { 1.0 };
    let in_cols = source.cols as usize;
    let in_rows = source.rows as usize;
    // Enforce the emit cap against the PROJECTED resample dims, before
    // `resample` allocates the grid — a tiny resolution_mm would otherwise
    // balloon the allocation past the cap the guard is meant to enforce.
    let (cols, rows) = resampled_dims(in_cols, in_rows, cell, *resolution_mm);
    if cols
        .checked_mul(rows)
        .map_or(true, |n| n > MAX_RASTER_PIXELS)
    {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "raster_too_large".into(),
            message: format!(
                "raster op '{}' resamples to {cols}×{rows} pixels, over the {MAX_RASTER_PIXELS}-pixel emit cap. Lower the resolution (larger resolution_mm) or crop the image; streaming emit for huge rasters is a follow-up.",
                op.name
            ),
        });
        return Ok(());
    }
    let (brightness, cols, rows) =
        resample(&source.brightness, in_cols, in_rows, cell, *resolution_mm);

    // Per-pixel power, computed once over the whole grid (Floyd–Steinberg
    // diffuses across rows, so the row walk must see the full result).
    let powers = power_curve.power_grid(&brightness, cols, rows);
    // Brightness isn't needed past the power grid; free it before the emit
    // loop so a large raster doesn't hold both grids live during emit.
    drop(brightness);
    if powers.is_empty() {
        return Ok(());
    }

    // The image occupies a FIXED rectangle: pixel (col c, row r) lands at
    // world (origin.x + c*pitch, origin.y + r*pitch). Scan direction only
    // changes the laser's travel orientation, not where pixels land:
    //   AlongX → horizontal scanlines (one per grid row),  sweep X.
    //   AlongY → vertical   scanlines (one per grid column), sweep Y.
    let pitch = if *resolution_mm > 0.0 {
        *resolution_mm
    } else {
        cell
    };
    let ox = source.origin.x;
    let oy = source.origin.y;
    let feed = setup.tool.rate_h.max(1);
    let scan_y = matches!(scan_direction, ScanDirection::AlongY);
    let (num_lines, line_len) = if scan_y { (cols, rows) } else { (rows, cols) };
    let over = overscan_factor.max(0.0) * line_len as f64 * pitch;

    // Power of the k-th pixel along scanline `line`.
    let power_at = |line: usize, k: usize| -> u32 {
        if scan_y {
            powers[k * cols + line] // (col = line, row = k)
        } else {
            powers[line * cols + k] // (row = line, col = k)
        }
    };
    // Fixed cross-axis coord of a scanline (X for AlongY, Y for AlongX).
    let fixed_origin = if scan_y { ox } else { oy };
    let line_fixed = |line: usize| fixed_origin + line as f64 * pitch;
    // Sweep-axis coord of boundary index b (origin oy for AlongY, ox else).
    let sweep_origin = if scan_y { oy } else { ox };
    let boundary = |b: usize| sweep_origin + b as f64 * pitch;
    // Assemble a world point from (fixed cross-axis, sweep) coords.
    let world = |fixed: f64, sweep: f64| -> (f64, f64) {
        if scan_y {
            (fixed, sweep)
        } else {
            (sweep, fixed)
        }
    };

    post.comment(&format!("OP {} raster engrave", op.id));
    post.laser_arm(); // M3 S0 — armed cold
    post.feedrate(feed);
    let mut final_pt = Point2::new(ox, oy);

    for line in 0..num_lines {
        let fixed = line_fixed(line);
        let reverse = matches!(link, RasterLink::Bidirectional) && (line % 2 == 1);
        let order: Vec<usize> = if reverse {
            (0..line_len).rev().collect()
        } else {
            (0..line_len).collect()
        };
        // Run-length group into (power, far-boundary-index) spans.
        let mut spans: Vec<(u32, usize)> = Vec::new();
        let mut i = 0;
        while i < order.len() {
            let p = power_at(line, order[i]);
            let mut j = i;
            while j + 1 < order.len() && power_at(line, order[j + 1]) == p {
                j += 1;
            }
            let last = order[j];
            // forward: pixel `k` spans boundaries [k, k+1] ⇒ far edge k+1;
            // reverse: the far edge is the lower boundary `k`.
            let end_b = if reverse { last } else { last + 1 };
            spans.push((p, end_b));
            i = j + 1;
        }

        let dir = if reverse { -1.0 } else { 1.0 };
        let lead = boundary(if reverse { line_len } else { 0 });
        let start = lead + dir * over;

        // Reposition with the beam off (M5 over the rapid), then re-arm.
        post.laser_off();
        let (sx, sy) = world(fixed, start);
        post.move_to(Some(sx), Some(sy), None);
        // Overscan lead-in at S0 so the head is at feed before burning.
        if over > 0.0 {
            post.laser_on(0);
            let (lx, ly) = world(fixed, lead);
            post.linear(Some(lx), Some(ly), None);
        }
        for (p, end_b) in spans {
            post.laser_on(p);
            let (ex, ey) = world(fixed, boundary(end_b));
            post.linear(Some(ex), Some(ey), None);
            final_pt = Point2::new(ex, ey);
        }
        // Overscan lead-out at S0.
        if over > 0.0 {
            post.laser_on(0);
            let (tx, ty) = world(
                fixed,
                boundary(if reverse { 0 } else { line_len }) + dir * over,
            );
            post.linear(Some(tx), Some(ty), None);
            final_pt = Point2::new(tx, ty);
        }
    }

    post.laser_off(); // M5 — beam down at op end
    *last_pos = final_pt;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_identity_when_pitch_matches_cell() {
        let b = vec![0.0, 0.5, 1.0, 0.25];
        let (out, c, r) = resample(&b, 2, 2, 0.1, 0.1);
        assert_eq!((c, r), (2, 2));
        assert_eq!(out, b);
        // pitch 0 ⇒ untouched too.
        let (out0, _, _) = resample(&b, 2, 2, 0.1, 0.0);
        assert_eq!(out0, b);
    }

    #[test]
    fn resample_halves_resolution() {
        // 4×4 at 0.1 mm cell → 0.2 mm pitch ⇒ 2×2.
        let b: Vec<f32> = (0..16).map(|i| i as f32 / 16.0).collect();
        let (out, c, r) = resample(&b, 4, 4, 0.1, 0.2);
        assert_eq!((c, r), (2, 2));
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn resampled_dims_matches_resample_output() {
        // The cap is enforced against resampled_dims, so it must predict
        // exactly the dims resample returns — for both the identity
        // short-circuit and a genuine resample.
        let b: Vec<f32> = (0..16).map(|i| i as f32 / 16.0).collect();
        for (cell, pitch) in [(0.1, 0.1), (0.1, 0.0), (0.1, 0.2), (0.1, 0.05)] {
            let (_, c, r) = resample(&b, 4, 4, cell, pitch);
            assert_eq!(
                resampled_dims(4, 4, cell, pitch),
                (c, r),
                "dims mismatch at cell={cell}, pitch={pitch}"
            );
        }
    }

    #[test]
    fn resampled_dims_explodes_for_tiny_pitch() {
        // A 2 mm-wide grid at a 0.0001 mm pitch projects to 20_000 px per
        // axis (400 Mpx) — well over MAX_RASTER_PIXELS. The guard reads
        // these dims BEFORE resample allocates, so the cap trips without
        // first materializing a 400 M-element grid.
        let (c, r) = resampled_dims(2, 2, 1.0, 0.0001);
        assert_eq!((c, r), (20_000, 20_000));
        assert!(c.checked_mul(r).is_some_and(|n| n > MAX_RASTER_PIXELS));
    }
}
