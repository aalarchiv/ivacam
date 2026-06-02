// Pure toolpath-buffer geometry math, extracted from Scene3D.svelte
// (7iej.17) so the direction-arrow chevron math — the kind of vector
// geometry that silently regresses — can be unit-tested without a live
// THREE renderer. Scene3D still owns the buffer assembly + GPU upload;
// these are the side-effect-free pieces it calls.

export interface Vec3 {
  x: number;
  y: number;
  z: number;
}

/// Tunables for the direction-arrow chevrons drawn on cutting moves.
export interface ArrowParams {
  /// Segments shorter than this (mm) never get an arrow.
  minLen: number;
  /// Absolute cap on arrow size (mm).
  maxSize: number;
  /// Arrow size as a fraction of segment length.
  sizeFrac: number;
  /// Half-wing spread: `tan(wing_angle)`. The default 30° wings use
  /// `Math.tan((30 * Math.PI) / 180)`.
  halfWing: number;
}

/// The two wing line-segments of a direction-arrow chevron. Each wing runs
/// from a back-set wing tip to the shared apex at the segment midpoint
/// (matching the `mid → tip` line pairs the fat-line buffer expects).
export interface ArrowChevron {
  /// Segment midpoint — the chevron apex, shared by both wings.
  mid: [number, number, number];
  /// `+normal`-side wing tip.
  wing1: [number, number, number];
  /// `-normal`-side wing tip.
  wing2: [number, number, number];
}

/// Arrow spacing (mm) from the user's density setting. Density 0 ⇒
/// `Infinity` (no segment ever qualifies ⇒ arrows disabled).
export function arrowSpacingMm(density: number): number {
  return density > 0 ? 3.0 / density : Infinity;
}

/// Compute the direction-arrow chevron for a cut move from `from` to `to`,
/// or `null` when the segment is shorter than `p.minLen` (too short to
/// carry a legible arrow). Spacing / move-kind eligibility is the caller's
/// concern — this is pure geometry.
///
/// The arrow points along the move direction: the apex sits at the segment
/// midpoint and the two wings sweep back by `A` along the reversed
/// direction and out by `A * halfWing` along the in-plane normal, where
/// `A = min(len * sizeFrac, maxSize)`. A near-pure-Z move (plunge /
/// retract, no meaningful XY component) uses a fixed `+X` normal so the
/// chevron stays visible from a top-down camera.
export function computeArrowChevron(from: Vec3, to: Vec3, p: ArrowParams): ArrowChevron | null {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const dz = to.z - from.z;
  const len = Math.sqrt(dx * dx + dy * dy + dz * dz);
  if (len < p.minLen) return null;

  const A = Math.min(len * p.sizeFrac, p.maxSize);
  const ux = dx / len;
  const uy = dy / len;
  const uz = dz / len;

  // In-plane normal: rotate the forward direction 90° CCW in XY when the
  // move has a meaningful horizontal component (the common case). A
  // pure-Z move falls back to +X so the arrow reads from any angle.
  let nx: number;
  let ny: number;
  let nz: number;
  const xyLen = Math.hypot(ux, uy);
  if (xyLen > 0.01) {
    nx = -uy / xyLen;
    ny = ux / xyLen;
    nz = 0;
  } else {
    nx = 1;
    ny = 0;
    nz = 0;
  }

  const mx = (from.x + to.x) * 0.5;
  const my = (from.y + to.y) * 0.5;
  const mz = (from.z + to.z) * 0.5;
  const side = A * p.halfWing;
  return {
    mid: [mx, my, mz],
    wing1: [mx - A * ux + side * nx, my - A * uy + side * ny, mz - A * uz + side * nz],
    wing2: [mx - A * ux - side * nx, my - A * uy - side * ny, mz - A * uz - side * nz],
  };
}
