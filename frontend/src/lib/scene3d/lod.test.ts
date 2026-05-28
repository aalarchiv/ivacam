import { describe, it, expect } from 'vitest';
import { pixelsPerCell } from './lod';

describe('pixelsPerCell', () => {
  it('projects a cell to the closed-form pixel size for a perspective camera', () => {
    // fov 90° ⇒ tan(45°) = 1, so ppc = cellSize * renderHeight / (2 * distance).
    const ppc = pixelsPerCell({
      cellSizeMm: 1,
      cameraDistance: 100,
      fovDeg: 90,
      renderHeightPx: 1000,
    });
    expect(ppc).toBeCloseTo((1 * 1000) / (2 * 100 * 1)); // 5 px/cell
  });

  it('shrinks as the camera pulls back (coarser LOD when far away)', () => {
    const near = pixelsPerCell({
      cellSizeMm: 1,
      cameraDistance: 50,
      fovDeg: 60,
      renderHeightPx: 800,
    })!;
    const far = pixelsPerCell({
      cellSizeMm: 1,
      cameraDistance: 500,
      fovDeg: 60,
      renderHeightPx: 800,
    })!;
    expect(near).toBeGreaterThan(far);
    // Inverse-distance: 10× the distance ⇒ ~1/10 the pixels.
    expect(far).toBeCloseTo(near / 10);
  });

  it('returns null on a non-positive camera distance', () => {
    expect(
      pixelsPerCell({ cellSizeMm: 1, cameraDistance: 0, fovDeg: 60, renderHeightPx: 800 }),
    ).toBeNull();
  });

  it('returns null when the render surface has no height yet', () => {
    expect(
      pixelsPerCell({ cellSizeMm: 1, cameraDistance: 100, fovDeg: 60, renderHeightPx: 0 }),
    ).toBeNull();
  });
});
