/// Pure-helper coverage for project-types.ts. Currently exercises the
/// `inferDefaultWorkOffset` rule that audit gldc added — fresh DXF/SVG
/// imports whose bbox doesn't contain the geometry origin (0, 0) get
/// the WCS shifted to the bbox bottom-left so the user's machine-zero
/// at the stock corner lands inside the geometry footprint.

import { describe, expect, it } from 'vitest';
import { defaultWorkOffset, inferDefaultWorkOffset, type WorkOffset } from './project-types';

function bb(min_x: number, min_y: number, max_x: number, max_y: number) {
  return { min_x, min_y, max_x, max_y };
}

describe('inferDefaultWorkOffset', () => {
  it('shifts WCS to bbox bottom-left when bbox does NOT contain origin', () => {
    const next = inferDefaultWorkOffset(bb(5.76, 5.79, 24.22, 24.24), defaultWorkOffset());
    expect(next.x_mm).toBeCloseTo(5.76);
    expect(next.y_mm).toBeCloseTo(5.79);
    expect(next.z_mm).toBe(0);
    expect(next.wcs).toBe('G54');
  });

  it('handles all-positive bbox where geometry was drawn far from origin', () => {
    const next = inferDefaultWorkOffset(bb(100, 200, 250, 350), defaultWorkOffset());
    expect(next.x_mm).toBe(100);
    expect(next.y_mm).toBe(200);
  });

  it('handles all-negative bbox (geometry drawn in -X / -Y quadrant)', () => {
    const next = inferDefaultWorkOffset(bb(-50, -30, -10, -5), defaultWorkOffset());
    expect(next.x_mm).toBe(-50);
    expect(next.y_mm).toBe(-30);
  });

  it('leaves WCS untouched when bbox already contains origin', () => {
    const cur = defaultWorkOffset();
    expect(inferDefaultWorkOffset(bb(-10, -10, 10, 10), cur)).toEqual(cur);
    expect(inferDefaultWorkOffset(bb(0, 0, 100, 100), cur)).toEqual(cur);
    expect(inferDefaultWorkOffset(bb(-100, -100, 0, 0), cur)).toEqual(cur);
  });

  it('respects the 1e-3 mm slack so paths drawn exactly to origin edge do not shift', () => {
    const cur = defaultWorkOffset();
    // Bbox starts at (0.0005, 0.0005) — within slack, treat as containing origin.
    expect(inferDefaultWorkOffset(bb(0.0005, 0.0005, 100, 100), cur)).toEqual(cur);
  });

  it('does NOT touch a user-set WorkOffset (preserves user intent)', () => {
    const user: WorkOffset = { x_mm: 12, y_mm: 34, z_mm: 0, wcs: 'G54' };
    expect(inferDefaultWorkOffset(bb(5.76, 5.79, 24.22, 24.24), user)).toEqual(user);
  });

  it('does NOT touch a user-selected non-G54 WCS even when xyz are zero', () => {
    const user: WorkOffset = { x_mm: 0, y_mm: 0, z_mm: 0, wcs: 'G55' };
    expect(inferDefaultWorkOffset(bb(5.76, 5.79, 24.22, 24.24), user)).toEqual(user);
  });

  it('bails on null bbox (no imported geometry)', () => {
    const cur = defaultWorkOffset();
    expect(inferDefaultWorkOffset(null, cur)).toEqual(cur);
  });

  it('bails on a non-finite bbox', () => {
    const cur = defaultWorkOffset();
    expect(inferDefaultWorkOffset(bb(NaN, 0, 10, 10), cur)).toEqual(cur);
    expect(inferDefaultWorkOffset(bb(0, 0, Infinity, 10), cur)).toEqual(cur);
  });

  it('bails on a degenerate bbox (max < min)', () => {
    const cur = defaultWorkOffset();
    expect(inferDefaultWorkOffset(bb(10, 10, 5, 5), cur)).toEqual(cur);
  });
});
