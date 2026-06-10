/// FE arc-length cross-check. The TS implementations in
/// `cam/tabs.ts` mirror the Rust helpers in `ivac_core::cam::tabs`.
/// These tests pin a small set of known fixtures so any future drift
/// between the two implementations breaks the build instead of
/// silently landing tab placements on the wrong side of a vertex.
///
/// The "ground truth" values below come from running the Rust
/// reference (`cargo test -p ivac-core --lib cam::tabs`) against
/// the same fixtures — they're not derived from the TS code so a
/// shared bug wouldn't slip past.

import { describe, expect, it } from 'vitest';
import { autoTabTs, polylineAtT, polylineProject, vertexAndMidpointTs } from './tabs';
import type { Point2 } from '../api/types';

function p(x: number, y: number): Point2 {
  return { x, y };
}

const UNIT_SQUARE: Point2[] = [p(0, 0), p(1, 0), p(1, 1), p(0, 1)];

describe('FE arc-length helpers cross-check vs Rust (audit 50u)', () => {
  it('autoTabTs closed: count N starts at 0 with step 1/N', () => {
    expect(autoTabTs(4, true)).toEqual([0, 0.25, 0.5, 0.75]);
    expect(autoTabTs(1, true)).toEqual([0]);
    expect(autoTabTs(0, true)).toEqual([]);
  });

  it('autoTabTs open: inset by 0.5/N', () => {
    expect(autoTabTs(2, false)).toEqual([0.25, 0.75]);
    expect(autoTabTs(4, false).map((t) => +t.toFixed(6))).toEqual([0.125, 0.375, 0.625, 0.875]);
  });

  it('polylineAtT closed unit square: t=0 → first vertex; t=0.25 → midway through second edge', () => {
    // Unit square perimeter = 4. t=0 starts at (0,0). t=0.25 advances
    // 1 unit (one edge length) — to (1, 0). t=0.5 → (1, 1).
    const at0 = polylineAtT(UNIT_SQUARE, 0, true).point;
    expect(at0).toEqual({ x: 0, y: 0 });
    const at_quarter = polylineAtT(UNIT_SQUARE, 0.25, true).point;
    expect(at_quarter.x).toBeCloseTo(1, 9);
    expect(at_quarter.y).toBeCloseTo(0, 9);
    const at_half = polylineAtT(UNIT_SQUARE, 0.5, true).point;
    expect(at_half.x).toBeCloseTo(1, 9);
    expect(at_half.y).toBeCloseTo(1, 9);
  });

  it('polylineAtT open polyline: t=1 clamps just inside the last segment', () => {
    const open = [p(0, 0), p(2, 0), p(2, 2)];
    const at_end = polylineAtT(open, 1, false).point;
    // Open clamp at 1 - 1e-12 ⇒ very close to (2, 2) but not exactly.
    expect(at_end.x).toBeCloseTo(2, 9);
    expect(at_end.y).toBeCloseTo(2, 9);
  });

  it('polylineProject closed: query exactly on a vertex returns t at that vertex', () => {
    // Vertex (1, 0) is at distance 1 along a perimeter of 4, so t=0.25.
    const result = polylineProject(UNIT_SQUARE, p(1, 0), true);
    expect(result.t).toBeCloseTo(0.25, 9);
    expect(result.snap.x).toBeCloseTo(1, 9);
    expect(result.snap.y).toBeCloseTo(0, 9);
  });

  it('polylineProject closed: query off the contour snaps to the nearest edge', () => {
    // Point (0.5, -0.3) is closest to (0.5, 0) on the first edge.
    // Distance along edge 1 from (0, 0) = 0.5; perimeter = 4 ⇒ t = 0.125.
    const result = polylineProject(UNIT_SQUARE, p(0.5, -0.3), true);
    expect(result.t).toBeCloseTo(0.125, 9);
    expect(result.snap.x).toBeCloseTo(0.5, 9);
    expect(result.snap.y).toBeCloseTo(0, 9);
  });

  it('polylineProject + polylineAtT round-trip: project a point, walk t, get same point', () => {
    const query = p(1.2, 0.7);
    const proj = polylineProject(UNIT_SQUARE, query, true);
    const back = polylineAtT(UNIT_SQUARE, proj.t, true).point;
    expect(back.x).toBeCloseTo(proj.snap.x, 9);
    expect(back.y).toBeCloseTo(proj.snap.y, 9);
  });

  it('vertexAndMidpointTs closed unit square: 4 vertices + 4 midpoints at known t', () => {
    const list = vertexAndMidpointTs(UNIT_SQUARE, true);
    // 4 verts at 0, 0.25, 0.5, 0.75; 4 midpoints at 0.125, 0.375, 0.625, 0.875.
    const verts = list.filter((e) => e.kind === 'vertex').map((e) => e.t);
    const mids = list.filter((e) => e.kind === 'midpoint').map((e) => e.t);
    expect(verts.map((t) => +t.toFixed(6))).toEqual([0, 0.25, 0.5, 0.75]);
    expect(mids.map((t) => +t.toFixed(6))).toEqual([0.125, 0.375, 0.625, 0.875]);
  });
});
