import { describe, it, expect } from 'vitest';
import { computeViewportTransform } from './viewport';
import type { BBox } from '../api/types';

const SQUARE_BBOX: BBox = { min_x: 0, min_y: 0, max_x: 100, max_y: 100 };

describe('computeViewportTransform', () => {
  it('user view {zoom:1, pan:0,0} ⇒ active transform equals base transform', () => {
    const t = computeViewportTransform(
      SQUARE_BBOX,
      { w: 600, h: 400 },
      { zoom: 1, panX: 0, panY: 0 },
    );
    expect(t.scale).toBeCloseTo(t.baseScale);
    expect(t.offX).toBeCloseTo(t.baseOffX);
    expect(t.offY).toBeCloseTo(t.baseOffY);
  });

  it('zoom multiplies the base scale; pan adds to the offsets', () => {
    const base = computeViewportTransform(
      SQUARE_BBOX,
      { w: 600, h: 400 },
      { zoom: 1, panX: 0, panY: 0 },
    );
    const zoomed = computeViewportTransform(
      SQUARE_BBOX,
      { w: 600, h: 400 },
      { zoom: 2, panX: 10, panY: -5 },
    );
    expect(zoomed.scale).toBeCloseTo(base.baseScale * 2);
    expect(zoomed.offX).toBeCloseTo(base.baseOffX + 10);
    expect(zoomed.offY).toBeCloseTo(base.baseOffY - 5);
  });

  it('fit-to-view leaves the configured margin on the limiting axis', () => {
    // 100×100 data in a 200×800 canvas; X is the limiting axis. With
    // margin=32 the available width is 136 px; baseScale = 136/100 = 1.36.
    const t = computeViewportTransform(
      SQUARE_BBOX,
      { w: 200, h: 800 },
      { zoom: 1, panX: 0, panY: 0 },
      32,
    );
    expect(t.baseScale).toBeCloseTo(136 / 100);
  });

  it('project2 flips Y (DXF y-up, canvas y-down)', () => {
    const t = computeViewportTransform(
      SQUARE_BBOX,
      { w: 200, h: 200 },
      { zoom: 1, panX: 0, panY: 0 },
    );
    const [px0, py0] = t.project2(0, 0);
    const [px100, py100] = t.project2(0, 100);
    // Larger data-Y maps to a SMALLER canvas-Y (top of screen).
    expect(py100).toBeLessThan(py0);
    // X axis is not flipped: 0 maps less-than 100.
    expect(px0).toBeLessThanOrEqual(px100 + 1);
  });

  it('handles degenerate (zero-extent) bboxes without dividing by zero', () => {
    const t = computeViewportTransform(
      { min_x: 5, min_y: 5, max_x: 5, max_y: 5 },
      { w: 200, h: 200 },
      { zoom: 1, panX: 0, panY: 0 },
    );
    expect(Number.isFinite(t.scale)).toBe(true);
    expect(Number.isFinite(t.offX)).toBe(true);
    expect(Number.isFinite(t.offY)).toBe(true);
  });
});
