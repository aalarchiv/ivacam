/// Fixture color helpers shared by the 2D canvas and 3D scene.
/// A fixture's `color` is a packed 0xRRGGBBAA integer; both renderers
/// unpacked it inline with the same default, so the unpack lives here now.
/// The alpha *treatment* stays per-renderer (the 2D overlay fill is more
/// transparent than the 3D solid), since those are deliberate differences.

/// Default packed RGBA when a fixture omits its color (semi-transparent
/// amber).
export const DEFAULT_FIXTURE_COLOR = 0xffa050c0;

export interface FixtureRgba {
  /// 0–255 channels.
  r: number;
  g: number;
  b: number;
  a: number;
  /// 24-bit RGB (no alpha), e.g. for `THREE.Color` / `new Color(hex)`.
  hex: number;
}

/// Unpack a packed 0xRRGGBBAA fixture color (or the default when null /
/// undefined) into 0–255 channels plus a 24-bit RGB `hex`.
export function unpackFixtureColor(packed: number | null | undefined): FixtureRgba {
  const p = packed ?? DEFAULT_FIXTURE_COLOR;
  const r = (p >>> 24) & 0xff;
  const g = (p >>> 16) & 0xff;
  const b = (p >>> 8) & 0xff;
  const a = p & 0xff;
  return { r, g, b, a, hex: (r << 16) | (g << 8) | b };
}
