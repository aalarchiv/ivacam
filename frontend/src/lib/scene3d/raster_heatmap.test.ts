import { describe, it, expect } from 'vitest';
import { powerAtWorld, heatColor, type HeatGrid } from './raster_heatmap';

describe('powerAtWorld', () => {
  const grid: HeatGrid = { originX: 10, originY: 20, cell: 0.5, cols: 4, rows: 4 };
  // Distinct value per cell so index math is observable.
  const powers = Array.from({ length: 16 }, (_, i) => i * 10);

  it('samples the cell containing the point (row 0 = world bottom)', () => {
    // Just inside the min corner ⇒ cell (ix=0, iy=0) ⇒ powers[0].
    expect(powerAtWorld(10.01, 20.01, grid, powers)).toBe(0);
    // One cell up in Y ⇒ iy=1 ⇒ powers[1*4 + 0] = 40.
    expect(powerAtWorld(10.01, 20.6, grid, powers)).toBe(40);
    // One cell right in X ⇒ ix=1 ⇒ powers[0*4 + 1] = 10.
    expect(powerAtWorld(10.6, 20.01, grid, powers)).toBe(10);
  });

  it('returns null outside the placed grid', () => {
    expect(powerAtWorld(9.5, 21, grid, powers)).toBeNull(); // left of origin
    expect(powerAtWorld(11, 19.5, grid, powers)).toBeNull(); // below origin
    expect(powerAtWorld(12.1, 21, grid, powers)).toBeNull(); // right past 4 cells (2mm)
  });

  it('returns null for a degenerate / mismatched grid', () => {
    expect(powerAtWorld(10.1, 20.1, { ...grid, cell: 0 }, powers)).toBeNull();
    expect(powerAtWorld(10.1, 20.1, grid, [1, 2, 3])).toBeNull();
  });
});

describe('heatColor', () => {
  it('runs blue → cyan → green → yellow → red', () => {
    expect(heatColor(0)).toEqual([0, 0, 1]); // blue
    expect(heatColor(0.25)).toEqual([0, 1, 1]); // cyan
    expect(heatColor(0.5)).toEqual([0, 1, 0]); // green
    expect(heatColor(0.75)).toEqual([1, 1, 0]); // yellow
    expect(heatColor(1)).toEqual([1, 0, 0]); // red
  });

  it('clamps out-of-range input', () => {
    expect(heatColor(-1)).toEqual([0, 0, 1]);
    expect(heatColor(2)).toEqual([1, 0, 0]);
  });

  it('every channel stays within [0,1] across the ramp', () => {
    for (let i = 0; i <= 20; i++) {
      const [r, g, b] = heatColor(i / 20);
      for (const ch of [r, g, b]) {
        expect(ch).toBeGreaterThanOrEqual(0);
        expect(ch).toBeLessThanOrEqual(1);
      }
    }
  });
});
