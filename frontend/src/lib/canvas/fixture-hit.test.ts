/// Tests for the fixture hit-test. Each shape kind gets one
/// inside / outside pair; multi-fixture order picks the first
/// match.

import { describe, expect, it } from 'vitest';
import { fixtureAt } from './fixture-hit';
import type { Fixture } from '../state/project-types';

function box(id: number, ox: number, oy: number, w: number, d: number): Fixture {
  return {
    id,
    origin: [ox, oy],
    z_bottom: 0,
    z_top: 1,
    color: '#fff',
    kind: { shape: 'box', width: w, depth: d },
  };
}
function cyl(id: number, ox: number, oy: number, r: number): Fixture {
  return {
    id,
    origin: [ox, oy],
    z_bottom: 0,
    z_top: 1,
    color: '#fff',
    kind: { shape: 'cylinder', radius: r },
  };
}
function poly(id: number, ox: number, oy: number, verts: [number, number][]): Fixture {
  return {
    id,
    origin: [ox, oy],
    z_bottom: 0,
    z_top: 1,
    color: '#fff',
    kind: { shape: 'polygon', vertices: verts },
  };
}

describe('fixtureAt', () => {
  it('returns null for an empty fixture list', () => {
    expect(fixtureAt([], 0, 0)).toBeNull();
  });

  it('hits a Box by AABB inclusion', () => {
    const f = box(7, 10, 20, 4, 6); // half-w=2, half-d=3 around (10,20)
    expect(fixtureAt([f], 10, 20)).toBe(7);
    expect(fixtureAt([f], 11.9, 22.9)).toBe(7); // inside corner
    expect(fixtureAt([f], 12.1, 20)).toBeNull(); // just past right edge
    expect(fixtureAt([f], 10, 23.1)).toBeNull(); // just past top edge
  });

  it('hits a Cylinder by disc inclusion', () => {
    const f = cyl(3, 0, 0, 5);
    expect(fixtureAt([f], 0, 0)).toBe(3);
    expect(fixtureAt([f], 4.9, 0)).toBe(3);
    expect(fixtureAt([f], 3, 3)).toBe(3); // dist = sqrt(18) ≈ 4.24
    expect(fixtureAt([f], 4, 4)).toBeNull(); // dist = sqrt(32) ≈ 5.66
  });

  it('hits a Polygon by even-odd ray-cast in local frame', () => {
    // Triangle in local coords (0,0)-(10,0)-(5,10), translated to origin (5, 5).
    const f = poly(11, 5, 5, [
      [0, 0],
      [10, 0],
      [5, 10],
    ]);
    // Point (10, 7) → local (5, 2) → inside.
    expect(fixtureAt([f], 10, 7)).toBe(11);
    // Point (5, 5) → local (0, 0) → on edge; pointInPolygon returns
    // false for boundary points by convention.
    expect(fixtureAt([f], 0, 0)).toBeNull();
    // Outside the triangle.
    expect(fixtureAt([f], 20, 20)).toBeNull();
  });

  it('returns the first matching fixture when several overlap', () => {
    // Two boxes around the same point — the one listed first wins.
    const a = box(1, 0, 0, 10, 10);
    const b = box(2, 0, 0, 10, 10);
    expect(fixtureAt([a, b], 0, 0)).toBe(1);
    expect(fixtureAt([b, a], 0, 0)).toBe(2);
  });
});
