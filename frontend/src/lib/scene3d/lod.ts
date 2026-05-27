// Heightfield level-of-detail projection math, extracted from
// Scene3D.svelte (l8u6) so the threshold tuning tracked in 9gpa can be
// unit-tested without a live THREE renderer.

/// Pixel-projection of a single L0 heightfield cell at the camera target.
///
/// For a perspective camera with vertical FOV, a world-space length `L`
/// at distance `d` projects to `L * (renderHeight / 2) / (d * tan(fov/2))`
/// pixels. The simulator turns this "pixels per cell" hint into a LOD
/// level (coarser mesh when each cell covers fewer than a pixel).
///
/// Returns `null` for the degenerate cases the caller must skip — a
/// non-positive camera distance or render height. (Cell-size validity is
/// the caller's concern: the driver returns `null` before this runs.)
export function pixelsPerCell(opts: {
  cellSizeMm: number;
  cameraDistance: number;
  fovDeg: number;
  renderHeightPx: number;
}): number | null {
  const { cellSizeMm, cameraDistance, fovDeg, renderHeightPx } = opts;
  if (cameraDistance <= 0 || renderHeightPx <= 0) return null;
  const fovRad = (fovDeg * Math.PI) / 180;
  return (cellSizeMm * renderHeightPx) / (2 * cameraDistance * Math.tan(fovRad / 2));
}
