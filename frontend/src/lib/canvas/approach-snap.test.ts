/// Approach-point snap unit tests (n79). Verifies the pure-TS data
/// layer that drives the canvas picker: candidate gathering filters
/// by op source, free-form picks honor tolerance, and "source = All"
/// restricts to closed objects (open polylines aren't snap targets).

import { describe, expect, it } from 'vitest';
import { approachSnapCandidates, findNearestSnap } from './approach-snap';
import type { ImportResponse, Segment } from '../api/types';
import type { OpEntry } from '../state/op_types';

function line(x1: number, y1: number, x2: number, y2: number, layer = '0', color = 7): Segment {
  return {
    type: 'LINE',
    start: { x: x1, y: y1 },
    end: { x: x2, y: y2 },
    bulge: 0,
    layer,
    color,
  };
}

function imported(
  segments: Segment[],
  objects: number[],
  closedIds: number[],
  layers: string[] = ['0'],
): ImportResponse {
  return {
    filename: 'test.dxf',
    format: 'dxf',
    segments,
    bbox: { min_x: 0, min_y: 0, max_x: 100, max_y: 100 },
    layers: layers.map((name) => ({ name, color: 7, segment_count: segments.length })),
    unit_scale: 1,
    warnings: [],
    objects,
    object_meta: closedIds.map((id) => ({
      id,
      closed: true,
      layer: '0',
      color: 7,
      bbox: { min_x: 0, min_y: 0, max_x: 10, max_y: 10 },
    })),
  };
}

function pocketOp(
  sourceObjects: number[] | undefined,
  sourceLayers: string[] | null,
): Pick<OpEntry, 'sourceObjects' | 'sourceLayers'> {
  return { sourceObjects, sourceLayers };
}

describe('approachSnapCandidates', () => {
  it('returns nothing when nothing is imported', () => {
    expect(approachSnapCandidates(null, pocketOp([1], null))).toEqual([]);
    expect(approachSnapCandidates(undefined, pocketOp([1], null))).toEqual([]);
  });

  it('filters by op.sourceObjects when set', () => {
    const segments = [
      line(0, 0, 10, 0), // object 1
      line(10, 0, 10, 10),
      line(10, 10, 0, 10),
      line(0, 10, 0, 0),
      line(50, 50, 60, 50), // object 2
      line(60, 50, 60, 60),
      line(60, 60, 50, 60),
      line(50, 60, 50, 50),
    ];
    const objects = [1, 1, 1, 1, 2, 2, 2, 2];
    const imp = imported(segments, objects, [1, 2]);
    const candidates = approachSnapCandidates(imp, pocketOp([1], null));
    // Object 1 has 4 unique vertices. Object 2 vertices must NOT appear.
    expect(candidates.length).toBe(4);
    for (const c of candidates) {
      expect(c.x).toBeLessThanOrEqual(10);
      expect(c.y).toBeLessThanOrEqual(10);
    }
  });

  it('filters by op.sourceLayers when sourceObjects is empty', () => {
    const segments = [
      line(0, 0, 10, 0, 'cut'),
      line(50, 50, 60, 50, 'engrave'),
    ];
    const objects = [1, 2];
    const imp = imported(segments, objects, [1, 2], ['cut', 'engrave']);
    const candidates = approachSnapCandidates(imp, pocketOp(undefined, ['cut']));
    // Only the 'cut' segment's two endpoints.
    expect(candidates.length).toBe(2);
    expect(candidates.every((c) => c.x <= 10)).toBe(true);
  });

  it('restricts source = All to closed objects', () => {
    const segments = [
      line(0, 0, 10, 0), // object 1 (closed)
      line(10, 0, 10, 10),
      line(10, 10, 0, 10),
      line(0, 10, 0, 0),
      line(50, 50, 60, 50), // object 2 (open)
    ];
    const objects = [1, 1, 1, 1, 2];
    // Mark only object 1 as closed.
    const imp = imported(segments, objects, [1]);
    // sourceObjects undefined + sourceLayers null = source = All.
    const candidates = approachSnapCandidates(imp, pocketOp(undefined, null));
    expect(candidates.length).toBe(4); // object 1 only
    expect(candidates.every((c) => c.x <= 10)).toBe(true);
  });

  it('dedupes shared vertices between segments', () => {
    // Closed square: each corner shows up twice (end of one segment +
    // start of the next). Should collapse to 4 unique points.
    const segments = [
      line(0, 0, 10, 0),
      line(10, 0, 10, 10),
      line(10, 10, 0, 10),
      line(0, 10, 0, 0),
    ];
    const imp = imported(segments, [1, 1, 1, 1], [1]);
    const candidates = approachSnapCandidates(imp, pocketOp([1], null));
    expect(candidates.length).toBe(4);
  });
});

describe('findNearestSnap', () => {
  it('returns null when no candidates are in range', () => {
    const c = [{ x: 0, y: 0 }, { x: 100, y: 100 }];
    expect(findNearestSnap(c, 50, 50, 1)).toBeNull();
  });

  it('returns the closest candidate inside the tolerance', () => {
    const c = [{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 5, y: 5 }];
    const snap = findNearestSnap(c, 4, 1, 2);
    // (5,5) is too far; (0,0) is √17≈4.1, also out. (10,0) is √37≈6.1, out.
    // Re-pick a query that snaps to (5,5).
    expect(snap).toBeNull();
    const snap2 = findNearestSnap(c, 5.5, 5.5, 2);
    expect(snap2).toEqual({ x: 5, y: 5 });
  });

  it('returns null when tolerance is zero or negative', () => {
    const c = [{ x: 0, y: 0 }];
    expect(findNearestSnap(c, 0, 0, 0)).toBeNull();
    expect(findNearestSnap(c, 0, 0, -1)).toBeNull();
  });

  it('handles an empty candidate list', () => {
    expect(findNearestSnap([], 0, 0, 10)).toBeNull();
  });
});
