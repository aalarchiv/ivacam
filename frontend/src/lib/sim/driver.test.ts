/// Pure-logic tests for the simulator driver helpers. Today this only
/// covers `computeFootprint`, the small function shared with Scene3D's
/// stock-box visualisation. The Simulator/HeightfieldMesh side requires
/// a browser + the wasm-pack-built pkg and is exercised by the e2e
/// build instead.

import { describe, expect, it } from 'vitest';
import { computeFootprint } from './driver';
import type { ImportResponse } from '../api/types';

function importedWithBbox(
  min_x: number,
  min_y: number,
  max_x: number,
  max_y: number,
): ImportResponse {
  return {
    bbox: { min_x, min_y, max_x, max_y },
  } as unknown as ImportResponse;
}

describe('computeFootprint', () => {
  it('returns a 100x100 default when no import is present', () => {
    const fp = computeFootprint(null, { mode: 'auto', margin: 5, customX: 50, customY: 50 });
    expect(fp).toEqual({ minX: 0, minY: 0, maxX: 100, maxY: 100 });
  });

  it('expands the bbox by the configured margin in auto mode', () => {
    const imp = importedWithBbox(10, 20, 30, 50);
    const fp = computeFootprint(imp, { mode: 'auto', margin: 5, customX: 0, customY: 0 });
    expect(fp).toEqual({ minX: 5, minY: 15, maxX: 35, maxY: 55 });
  });

  it('clamps negative margins to zero in auto mode', () => {
    const imp = importedWithBbox(0, 0, 10, 10);
    const fp = computeFootprint(imp, { mode: 'auto', margin: -3, customX: 0, customY: 0 });
    expect(fp).toEqual({ minX: 0, minY: 0, maxX: 10, maxY: 10 });
  });

  it('centers a manual footprint on the bbox center', () => {
    const imp = importedWithBbox(0, 0, 20, 10); // center (10, 5)
    const fp = computeFootprint(imp, { mode: 'manual', margin: 0, customX: 40, customY: 30 });
    expect(fp).toEqual({ minX: -10, minY: -10, maxX: 30, maxY: 20 });
  });

  it('ignores margin in manual mode', () => {
    const imp = importedWithBbox(0, 0, 10, 10);
    const a = computeFootprint(imp, { mode: 'manual', margin: 0, customX: 20, customY: 20 });
    const b = computeFootprint(imp, { mode: 'manual', margin: 99, customX: 20, customY: 20 });
    expect(a).toEqual(b);
  });
});
