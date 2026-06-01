import { describe, it, expect } from 'vitest';
import {
  pointerDistance,
  pointerCentroid,
  withinTapTolerance,
  applyPinch,
  DEFAULT_ZOOM_LIMITS,
  LONG_PRESS_MOVE_TOL_PX,
  type BaseView,
} from './touch-gestures';
import type { UserView } from './viewport';

const BASE: BaseView = { scale: 2, offX: 100, offY: 100 };
const IDENTITY: UserView = { zoom: 1, panX: 0, panY: 0 };

describe('pointer helpers', () => {
  it('pointerDistance is Euclidean', () => {
    expect(pointerDistance({ x: 0, y: 0 }, { x: 3, y: 4 })).toBe(5);
  });

  it('pointerCentroid is the midpoint', () => {
    expect(pointerCentroid({ x: 0, y: 0 }, { x: 4, y: 8 })).toEqual({ x: 2, y: 4 });
  });

  it('withinTapTolerance tracks the move budget', () => {
    expect(withinTapTolerance({ x: 0, y: 0 }, { x: 5, y: 0 })).toBe(true);
    expect(withinTapTolerance({ x: 0, y: 0 }, { x: LONG_PRESS_MOVE_TOL_PX + 1, y: 0 })).toBe(false);
  });
});

describe('applyPinch — zoom about the centroid', () => {
  it('spreading the fingers 2× doubles the zoom', () => {
    // Fingers centered on (200,100); span 100 → 200 px ⇒ ratio 2.
    const prev = { a: { x: 150, y: 100 }, b: { x: 250, y: 100 } };
    const curr = { a: { x: 100, y: 100 }, b: { x: 300, y: 100 } };
    const out = applyPinch(IDENTITY, BASE, prev, curr);
    expect(out.zoom).toBeCloseTo(2, 6);
  });

  it('pinching in 0.5× halves the zoom', () => {
    const prev = { a: { x: 100, y: 100 }, b: { x: 300, y: 100 } };
    const curr = { a: { x: 150, y: 100 }, b: { x: 250, y: 100 } };
    const out = applyPinch(IDENTITY, BASE, prev, curr);
    expect(out.zoom).toBeCloseTo(0.5, 6);
  });

  it('keeps the data point under the centroid fixed across the zoom', () => {
    const prev = { a: { x: 150, y: 100 }, b: { x: 250, y: 100 } };
    const curr = { a: { x: 100, y: 100 }, b: { x: 300, y: 100 } };
    // Centroid is unchanged at (200,100) — so the data point under it
    // must map back to the same screen pixel after the zoom.
    const centroid = { x: 200, y: 100 };
    const oldScale = BASE.scale * IDENTITY.zoom;
    const dataX = (centroid.x - (BASE.offX + IDENTITY.panX)) / oldScale;
    const dataY = (BASE.offY + IDENTITY.panY - centroid.y) / oldScale;

    const out = applyPinch(IDENTITY, BASE, prev, curr);
    const newScale = BASE.scale * out.zoom;
    const screenX = dataX * newScale + (BASE.offX + out.panX);
    const screenY = BASE.offY + out.panY - dataY * newScale;
    expect(screenX).toBeCloseTo(centroid.x, 4);
    expect(screenY).toBeCloseTo(centroid.y, 4);
  });
});

describe('applyPinch — two-finger pan', () => {
  it('a pure translation (ratio 1) pans by the centroid delta and leaves zoom alone', () => {
    const prev = { a: { x: 100, y: 100 }, b: { x: 200, y: 100 } };
    // Same span, shifted +40 in x / +25 in y.
    const curr = { a: { x: 140, y: 125 }, b: { x: 240, y: 125 } };
    const out = applyPinch(IDENTITY, BASE, prev, curr);
    expect(out.zoom).toBeCloseTo(1, 6);
    expect(out.panX).toBeCloseTo(40, 4);
    expect(out.panY).toBeCloseTo(25, 4);
  });
});

describe('applyPinch — guards', () => {
  it('clamps zoom to the limits', () => {
    const prev = { a: { x: 199, y: 100 }, b: { x: 201, y: 100 } }; // 2px span
    const curr = { a: { x: 0, y: 100 }, b: { x: 400, y: 100 } }; // 400px span → ratio 200
    const out = applyPinch(IDENTITY, BASE, prev, curr);
    expect(out.zoom).toBe(DEFAULT_ZOOM_LIMITS.max);
  });

  it('a zero-length previous span degrades to a pan, not NaN', () => {
    const prev = { a: { x: 200, y: 100 }, b: { x: 200, y: 100 } }; // coincident
    const curr = { a: { x: 150, y: 100 }, b: { x: 250, y: 100 } };
    const out = applyPinch(IDENTITY, BASE, prev, curr);
    expect(Number.isFinite(out.zoom)).toBe(true);
    expect(Number.isFinite(out.panX)).toBe(true);
    expect(Number.isFinite(out.panY)).toBe(true);
    expect(out.zoom).toBeCloseTo(1, 6); // ratio forced to 1
  });

  it('an unstaged base transform (scale 0) returns the view unchanged', () => {
    const prev = { a: { x: 150, y: 100 }, b: { x: 250, y: 100 } };
    const curr = { a: { x: 100, y: 100 }, b: { x: 300, y: 100 } };
    const out = applyPinch(IDENTITY, { scale: 0, offX: 0, offY: 0 }, prev, curr);
    expect(out).toEqual(IDENTITY);
  });
});
