// Pure helpers for the 3D raster-engrave toolpath
// heatmap. The wire `ToolpathSegment` carries no laser-power `S`, so the
// frontend re-derives it by sampling the source's power grid (the same
// `cam::raster::power_grid` the backend emits from — ported in
// `raster_preview.ts`) at each cut span's midpoint, then maps the
// normalized power to a colour. Kept side-effect-free + unit-tested so
// the colour ramp and the world→cell sampling don't silently regress.

/// Minimal placed-grid view for sampling — the fields of a
/// `ReliefSource` the sampler needs.
export interface HeatGrid {
  originX: number;
  originY: number;
  /// Square cell size (mm).
  cell: number;
  cols: number;
  rows: number;
}

/// Sample the raw laser power (`S`) at world point `(x, y)` from a
/// row-major power grid (`powers[iy * cols + ix]`, row 0 = world bottom
/// — the same orientation `grayscaleDownsample` produces, so no flip).
/// Returns `null` when the point falls outside the placed grid. A
/// length-mismatched grid also yields `null`.
export function powerAtWorld(
  x: number,
  y: number,
  grid: HeatGrid,
  powers: readonly number[],
): number | null {
  const { originX, originY, cell, cols, rows } = grid;
  if (cell <= 0 || cols <= 0 || rows <= 0 || powers.length !== cols * rows) return null;
  const ix = Math.floor((x - originX) / cell);
  const iy = Math.floor((y - originY) / cell);
  if (ix < 0 || ix >= cols || iy < 0 || iy >= rows) return null;
  return powers[iy * cols + ix];
}

/// Map a normalized value `t ∈ [0, 1]` to a "jet"-style heat colour
/// (each channel 0..1): blue (cold / low power) → cyan → green → yellow
/// → red (hot / high power). Chosen over a black-body ramp so even
/// low-power spans stay visible against the dark 3D background. `t` is
/// clamped to [0, 1].
export function heatColor(t: number): [number, number, number] {
  const c = t < 0 ? 0 : t > 1 ? 1 : t;
  if (c < 0.25) {
    // blue → cyan
    return [0, 4 * c, 1];
  } else if (c < 0.5) {
    // cyan → green
    return [0, 1, 1 - 4 * (c - 0.25)];
  } else if (c < 0.75) {
    // green → yellow
    return [4 * (c - 0.5), 1, 0];
  }
  // yellow → red
  return [1, 1 - 4 * (c - 0.75), 0];
}
