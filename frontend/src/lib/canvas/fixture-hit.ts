/// Pure fixture hit-test for the 2D entity canvas. Given a point in
/// data-space and the list of fixtures, returns the id of the first
/// fixture whose shape contains the point (or `null` if none does).
///
/// Box / Cylinder use AABB / disc inclusion; Polygon uses an even-
/// odd ray-cast (delegated to `pointInPolygon`).
///
/// No DOM, no \$state — vitest-friendly extraction from
/// `EntityCanvas2D.svelte`'s `fixtureHit` (audit y0ez).

import type { Fixture } from '../state/project-types';
import { pointInPolygon } from './selection-geometry';

export function fixtureAt(
  fixtures: ReadonlyArray<Fixture>,
  dataX: number,
  dataY: number,
): number | null {
  for (const f of fixtures) {
    const [ox, oy] = f.origin;
    if (f.kind.shape === 'box') {
      const hw = f.kind.width / 2;
      const hd = f.kind.depth / 2;
      if (Math.abs(dataX - ox) <= hw && Math.abs(dataY - oy) <= hd) return f.id;
    } else if (f.kind.shape === 'cylinder') {
      const dx = dataX - ox;
      const dy = dataY - oy;
      if (dx * dx + dy * dy <= f.kind.radius * f.kind.radius) return f.id;
    } else if (f.kind.shape === 'polygon') {
      // Translate the probe into the polygon's local frame then run
      // an even-odd ray-cast.
      const lx = dataX - ox;
      const ly = dataY - oy;
      if (pointInPolygon(f.kind.vertices, lx, ly)) return f.id;
    }
  }
  return null;
}
