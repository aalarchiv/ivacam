/// Pure-helper coverage for project-types.ts. Currently exercises the
/// `inferDefaultWorkOffset` rule that audit gldc added — fresh DXF/SVG
/// imports whose bbox doesn't contain the geometry origin (0, 0) get
/// the WCS shifted to the bbox bottom-left so the user's machine-zero
/// at the stock corner lands inside the geometry footprint.

import { describe, expect, it } from 'vitest';
import {
  defaultWorkOffset,
  inferDefaultWorkOffset,
  placementFileTransform,
  type WorkOffset,
} from './project-types';

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

describe('placementFileTransform (xeio import placement)', () => {
  const WORK = { x: 200, y: 300, z: 50 };
  const bb = (min_x: number, min_y: number, max_x: number, max_y: number) => ({
    min_x,
    min_y,
    max_x,
    max_y,
  });

  it('leaves a drawing already fully inside the work area untouched', () => {
    const t = placementFileTransform(bb(10, 10, 50, 50), WORK);
    expect(t.translate).toEqual({ x: 0, y: 0 });
    expect(t.scale).toBe(1);
  });

  it('moves a far-away drawing so its bottom-left lands on the origin', () => {
    const t = placementFileTransform(bb(5000, 3000, 5040, 3030), WORK);
    expect(t.translate).toEqual({ x: -5000, y: -3000 });
  });

  it('snaps a negative-origin drawing up to (0,0)', () => {
    const t = placementFileTransform(bb(-12, -8, 30, 20), WORK);
    expect(t.translate).toEqual({ x: 12, y: 8 });
  });

  it('aligns an oversize drawing bottom-left to origin (origin window reachable)', () => {
    // 500x400 drawing on a 200x300 bed — exceeds, so align min → origin.
    const t = placementFileTransform(bb(100, 100, 600, 500), WORK);
    expect(t.translate).toEqual({ x: -100, y: -100 });
  });

  it('returns identity for a degenerate or non-finite bbox', () => {
    expect(placementFileTransform(bb(10, 10, 5, 5), WORK).translate).toEqual({ x: 0, y: 0 });
    expect(placementFileTransform(null, WORK).translate).toEqual({ x: 0, y: 0 });
    expect(placementFileTransform(bb(0, 0, Infinity, 10), WORK).translate).toEqual({ x: 0, y: 0 });
  });

  it('treats an undefined work area as unbounded (positive-quadrant drawing kept)', () => {
    const t = placementFileTransform(bb(5000, 3000, 5040, 3030), undefined);
    expect(t.translate).toEqual({ x: 0, y: 0 });
  });
});
