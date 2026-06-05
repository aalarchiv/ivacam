/// rt1.12: TS port of `crates/ivac-core/src/cam/raster.rs` — the pure
/// brightness-grid → laser-power (`S`) mapping that drives the
/// RasterEngraveSection live preview + histogram. The Rust module is the
/// ORACLE: its unit tests (linear midpoint, threshold binary, F-S ~50%
/// on uniform grey, classic 4×4 Bayer indices) are mirrored in
/// `raster_preview.test.ts`. Keep the two in sync — the preview must
/// match what the backend actually burns.
///
/// Convention (same as Rust): dark pixels burn hotter. brightness 0.0
/// (black) ⇒ highest power; 1.0 (white) ⇒ none.

import type { PowerCurve, RasterLink, ScanDirection } from './op_types';

function clamp01(b: number): number {
  return b < 0 ? 0 : b > 1 ? 1 : b;
}

/// Power at brightness `b ∈ [0, 1]`: `max` at black (0), `min` at white
/// (1), linear between, rounded to the nearest non-negative integer `S`.
/// Mirrors `lerp_power` in raster.rs.
function lerpPower(min: number, max: number, b: number): number {
  const lo = min;
  const hi = max;
  return Math.max(0, Math.round(hi + (lo - hi) * b));
}

/// Floyd–Steinberg error diffusion, left-to-right then top-to-bottom,
/// with the classic 7/3/5/1 (÷16) weights. Black (below `level`) emits
/// `power`, white emits 0. Mirrors `floyd_steinberg` in raster.rs.
function floydSteinberg(
  brightness: readonly number[],
  cols: number,
  rows: number,
  level: number,
  power: number,
): number[] {
  const buf = brightness.map(clamp01);
  const out = new Array<number>(cols * rows).fill(0);
  for (let y = 0; y < rows; y++) {
    for (let x = 0; x < cols; x++) {
      const i = y * cols + x;
      const old = buf[i];
      const black = old < level;
      const quant = black ? 0 : 1;
      out[i] = black ? power : 0;
      const err = old - quant;
      if (x + 1 < cols) buf[i + 1] += err * (7 / 16);
      if (y + 1 < rows) {
        if (x > 0) buf[i + cols - 1] += err * (3 / 16);
        buf[i + cols] += err * (5 / 16);
        if (x + 1 < cols) buf[i + cols + 1] += err * (1 / 16);
      }
    }
  }
  return out;
}

/// Recursive integer Bayer index matrix (values `0..n²`), row-major.
/// `M(2n) = [[4M+0, 4M+2], [4M+3, 4M+1]]`. Mirrors `bayer_indices`.
export function bayerIndices(n: number): number[] {
  if (n <= 1) return [0];
  const h = n / 2;
  const half = bayerIndices(h);
  const m = new Array<number>(n * n).fill(0);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < h; x++) {
      const v = half[y * h + x];
      m[y * n + x] = 4 * v;
      m[y * n + (x + h)] = 4 * v + 2;
      m[(y + h) * n + x] = 4 * v + 3;
      m[(y + h) * n + (x + h)] = 4 * v + 1;
    }
  }
  return m;
}

/// Normalized Bayer thresholds of side `n`, each in (0, 1) at cell
/// centres: `(index + 0.5) / n²`. Mirrors `bayer_thresholds`.
function bayerThresholds(n: number): number[] {
  const denom = n * n;
  return bayerIndices(n).map((v) => (v + 0.5) / denom);
}

/// Ordered (Bayer) dither. A pixel burns at `power` when its brightness
/// is below the tile-local threshold. `matrixSize` must be a power of
/// two (2 / 4 / 8); other values fall back to 4. Mirrors `bayer_dither`.
function bayerDither(
  brightness: readonly number[],
  cols: number,
  rows: number,
  matrixSize: number,
  power: number,
): number[] {
  const n = matrixSize === 2 || matrixSize === 4 || matrixSize === 8 ? matrixSize : 4;
  const matrix = bayerThresholds(n);
  const out = new Array<number>(cols * rows).fill(0);
  for (let y = 0; y < rows; y++) {
    for (let x = 0; x < cols; x++) {
      const i = y * cols + x;
      const t = matrix[(y % n) * n + (x % n)];
      out[i] = clamp01(brightness[i]) < t ? power : 0;
    }
  }
  return out;
}

/// Map a normalized-brightness grid (row-major, length `cols * rows`,
/// each value in [0, 1]) to a row-major grid of per-pixel laser-power
/// (`S`) values. A mismatched length or empty grid yields `[]`. Mirrors
/// `PowerCurve::power_grid`.
export function powerGrid(
  curve: PowerCurve,
  brightness: readonly number[],
  cols: number,
  rows: number,
): number[] {
  if (cols <= 0 || rows <= 0 || brightness.length !== cols * rows) return [];
  switch (curve.kind) {
    case 'linear':
      return brightness.map((b) => lerpPower(curve.min, curve.max, clamp01(b)));
    case 'threshold':
      return brightness.map((b) => (clamp01(b) < curve.level ? curve.power : 0));
    case 'floyd_steinberg':
      return floydSteinberg(brightness, cols, rows, curve.level, curve.power);
    case 'bayer':
      return bayerDither(brightness, cols, rows, curve.matrixSize, curve.power);
  }
}

/// The peak `S` a curve can command — the denominator for normalizing
/// the preview's burn intensity. Linear uses its hotter endpoint
/// (`min`/`max` may be inverted); the binary curves use `power`.
export function maxPower(curve: PowerCurve): number {
  switch (curve.kind) {
    case 'linear':
      return Math.max(curve.min, curve.max);
    case 'threshold':
    case 'floyd_steinberg':
    case 'bayer':
      return curve.power;
  }
}

/// Render a power grid to top-down RGBA bytes for a `<canvas>` preview.
/// The grid is world-oriented (row 0 = world bottom, Y-flipped by
/// `grayscaleDownsample`); canvas ImageData is top-down, so we flip back
/// here — exactly once — so the preview reads right-way-up. Burn
/// intensity shows as darkness: power 0 ⇒ white, peak power ⇒ black.
export function powerGridToRgba(
  power: readonly number[],
  cols: number,
  rows: number,
  peak: number,
): Uint8ClampedArray {
  const out = new Uint8ClampedArray(cols * rows * 4);
  const denom = peak > 0 ? peak : 1;
  for (let y = 0; y < rows; y++) {
    // Flip: image row y reads world row (rows-1-y).
    const wy = rows - 1 - y;
    for (let x = 0; x < cols; x++) {
      const p = power[wy * cols + x] ?? 0;
      const gray = Math.round(255 * (1 - Math.min(1, p / denom)));
      const o = (y * cols + x) * 4;
      out[o] = gray;
      out[o + 1] = gray;
      out[o + 2] = gray;
      out[o + 3] = 255;
    }
  }
  return out;
}

/// Inputs for the dialog's burn-time estimate. All distances in mm;
/// `feedMmMin` is the laser's feed rate (mm/min).
export interface RasterBurnInput {
  widthMm: number;
  heightMm: number;
  /// Effective row pitch (mm). Caller resolves 0 ⇒ native cell size
  /// before calling.
  resolutionMm: number;
  feedMmMin: number;
  link: RasterLink;
  overscanFactor: number;
  scanDirection: ScanDirection;
}

/// Rough burn-time estimate (seconds) so a multi-hour engrave isn't
/// kicked off blind. Model: each scanline runs the full row length plus
/// overscan on both edges; `lift_between` pays a return traverse per row
/// while `bidirectional` doesn't; the per-row step-over adds the pitch.
/// Everything is charged at the feed rate (rapids ignored) — deliberately
/// conservative; the real toolpath is the source of truth. Returns 0 for
/// a degenerate input (no feed / no area / no pitch).
export function estimateBurnSeconds(i: RasterBurnInput): number {
  const { widthMm, heightMm, resolutionMm, feedMmMin, link, overscanFactor } = i;
  if (!(feedMmMin > 0) || !(resolutionMm > 0) || !(widthMm > 0) || !(heightMm > 0)) return 0;
  // along_x: rows step in Y, each row spans the width. along_y is the
  // transpose.
  const rowLen = i.scanDirection === 'along_x' ? widthMm : heightMm;
  const stepSpan = i.scanDirection === 'along_x' ? heightMm : widthMm;
  const nRows = Math.max(1, Math.round(stepSpan / resolutionMm));
  const engravePerRow = rowLen * (1 + 2 * Math.max(0, overscanFactor));
  const totalEngrave = nRows * engravePerRow;
  const returnTraverse = link === 'lift_between' ? nRows * rowLen : 0;
  const stepMoves = nRows * resolutionMm;
  const mmPerSec = feedMmMin / 60;
  return (totalEngrave + returnTraverse + stepMoves) / mmPerSec;
}

/// Render a normalized-brightness grid to top-down RGBA bytes for a
/// `<canvas>` — the source photo as grayscale (bright = white), used by
/// the 2D canvas placement preview. Like `powerGridToRgba` the grid is
/// world-oriented (row 0 = world bottom) so we flip back to top-down
/// exactly once. Opacity is left to the caller (drawImage globalAlpha).
export function brightnessToRgba(
  brightness: readonly number[],
  cols: number,
  rows: number,
): Uint8ClampedArray {
  const out = new Uint8ClampedArray(cols * rows * 4);
  for (let y = 0; y < rows; y++) {
    const wy = rows - 1 - y;
    for (let x = 0; x < cols; x++) {
      const gray = Math.round(255 * clamp01(brightness[wy * cols + x] ?? 0));
      const o = (y * cols + x) * 4;
      out[o] = gray;
      out[o + 1] = gray;
      out[o + 2] = gray;
      out[o + 3] = 255;
    }
  }
  return out;
}

/// Histogram of brightness values into `bins` equal buckets over [0, 1].
/// Bucket `bins-1` also catches exactly-1.0. Drives the section's
/// brightness histogram (with the threshold cursor overlaid).
export function brightnessHistogram(brightness: readonly number[], bins = 32): number[] {
  const hist = new Array<number>(bins).fill(0);
  if (bins <= 0) return hist;
  for (const b of brightness) {
    const idx = Math.min(bins - 1, Math.max(0, Math.floor(clamp01(b) * bins)));
    hist[idx]++;
  }
  return hist;
}
