/// OSnap engine tests (64p). Verifies the pure-TS data layer:
/// target precomputation collects endpoints / midpoints / centers /
/// intersections from imported geometry, and `findOSnap` returns the
/// closest enabled-kind candidate inside the tolerance.

import { describe, expect, it } from 'vitest';
import {
  DEFAULT_OSNAP_SETTINGS,
  findOSnap,
  precomputeOSnapTargets,
  type OSnapSettings,
} from './osnap';
import type { ImportResponse, Segment } from '../api/types';

function line(x1: number, y1: number, x2: number, y2: number): Segment {
  return {
    type: 'LINE',
    start: { x: x1, y: y1 },
    end: { x: x2, y: y2 },
    bulge: 0,
    layer: '0',
    color: 7,
  };
}

function arc(
  x1: number,
  y1: number,
  x2: number,
  y2: number,
  cx: number,
  cy: number,
  bulge: number,
): Segment {
  return {
    type: 'ARC',
    start: { x: x1, y: y1 },
    end: { x: x2, y: y2 },
    center: { x: cx, y: cy },
    bulge,
    layer: '0',
    color: 7,
  };
}

function imported(segments: Segment[]): ImportResponse {
  return {
    filename: 't.dxf',
    format: 'dxf',
    segments,
    bbox: { min_x: 0, min_y: 0, max_x: 100, max_y: 100 },
    layers: [{ name: '0', color: 7, segment_count: segments.length }],
    unit_scale: 1,
    warnings: [],
    objects: segments.map(() => 1),
    object_meta: [],
  };
}

describe('precomputeOSnapTargets', () => {
  it('returns empty for null / empty input', () => {
    expect(precomputeOSnapTargets(null).endpoints).toEqual([]);
    expect(precomputeOSnapTargets(imported([])).endpoints).toEqual([]);
  });

  it('collects endpoints, midpoints, and dedupes shared vertices', () => {
    // Closed square: 4 segments, 4 unique endpoints, 4 midpoints
    const segs = [
      line(0, 0, 10, 0),
      line(10, 0, 10, 10),
      line(10, 10, 0, 10),
      line(0, 10, 0, 0),
    ];
    const t = precomputeOSnapTargets(imported(segs));
    expect(t.endpoints.length).toBe(4);
    expect(t.midpoints.length).toBe(4);
    // Midpoints sit on edges (5, 0), (10, 5), (5, 10), (0, 5).
    const midSet = new Set(t.midpoints.map((m) => `${m.x},${m.y}`));
    expect(midSet.has('5,0')).toBe(true);
    expect(midSet.has('10,5')).toBe(true);
    expect(midSet.has('0,5')).toBe(true);
  });

  it('collects arc centers', () => {
    const segs = [arc(10, 0, 0, 10, 0, 0, 1), arc(0, 10, -10, 0, 0, 0, 1)];
    const t = precomputeOSnapTargets(imported(segs));
    // Both arcs share center (0, 0) — dedupes to one.
    expect(t.centers).toEqual([{ x: 0, y: 0 }]);
  });

  it('computes line-line intersections (strict within-segment)', () => {
    // Two crossed lines forming an X centered at (5, 5).
    const segs = [line(0, 0, 10, 10), line(0, 10, 10, 0)];
    const t = precomputeOSnapTargets(imported(segs));
    expect(t.intersections.length).toBe(1);
    expect(t.intersections[0].x).toBeCloseTo(5);
    expect(t.intersections[0].y).toBeCloseTo(5);
  });

  it('skips intersection at T-joint endpoints (already an endpoint)', () => {
    // Segment B ends on segment A — the T-joint at (5, 0) is an
    // endpoint, not a real intersection. Avoids duplicate snap glyphs.
    const segs = [line(0, 0, 10, 0), line(5, 0, 5, 10)];
    const t = precomputeOSnapTargets(imported(segs));
    expect(t.intersections.length).toBe(0);
  });

  it('skips parallel / collinear lines', () => {
    const segs = [line(0, 0, 10, 0), line(0, 5, 10, 5), line(5, 0, 15, 0)];
    const t = precomputeOSnapTargets(imported(segs));
    expect(t.intersections.length).toBe(0);
  });
});

describe('findOSnap', () => {
  const square = precomputeOSnapTargets(
    imported([
      line(0, 0, 10, 0),
      line(10, 0, 10, 10),
      line(10, 10, 0, 10),
      line(0, 10, 0, 0),
    ]),
  );

  it('snaps to nearest endpoint inside tolerance', () => {
    const s = findOSnap(square, 0.3, -0.2, 1, DEFAULT_OSNAP_SETTINGS);
    expect(s).toEqual({ kind: 'endpoint', x: 0, y: 0 });
  });

  it('snaps to midpoint when endpoint is out of range', () => {
    // Edge midpoint at (5, 0); endpoints are 5 away.
    const s = findOSnap(square, 5.1, 0.2, 1, DEFAULT_OSNAP_SETTINGS);
    expect(s).toEqual({ kind: 'midpoint', x: 5, y: 0 });
  });

  it('returns null when no enabled kind is in range', () => {
    const s = findOSnap(square, 50, 50, 0.5, DEFAULT_OSNAP_SETTINGS);
    expect(s).toBeNull();
  });

  it('respects per-kind disable flags', () => {
    const noEndpoint: OSnapSettings = { ...DEFAULT_OSNAP_SETTINGS, endpoint: false };
    // Cursor right at endpoint (0,0) but endpoint snap disabled →
    // nothing in range (closest other candidate is midpoint at (5,0)
    // or (0,5), both ~5 mm away).
    const s = findOSnap(square, 0.3, -0.2, 1, noEndpoint);
    expect(s).toBeNull();
  });

  it('priority: endpoint beats midpoint at equal distance', () => {
    // Custom layout where an endpoint and a midpoint are both
    // exactly 1 unit from the cursor.
    const targets = {
      endpoints: [{ x: 1, y: 0 }],
      midpoints: [{ x: -1, y: 0 }],
      intersections: [],
      centers: [],
    };
    const s = findOSnap(targets, 0, 0, 2, DEFAULT_OSNAP_SETTINGS);
    expect(s?.kind).toBe('endpoint');
  });

  it('grid snap activates only when enabled', () => {
    const targets = {
      endpoints: [],
      midpoints: [],
      intersections: [],
      centers: [],
    };
    const withGrid: OSnapSettings = {
      ...DEFAULT_OSNAP_SETTINGS,
      grid: true,
      gridStepMm: 5,
    };
    // Cursor at (4.7, 9.8) → nearest grid (5, 10), within tol 0.5.
    const s = findOSnap(targets, 4.7, 9.8, 0.5, withGrid);
    expect(s).toEqual({ kind: 'grid', x: 5, y: 10 });
    // Default (grid off) returns null even though geometry is empty.
    const off = findOSnap(targets, 4.7, 9.8, 0.5, DEFAULT_OSNAP_SETTINGS);
    expect(off).toBeNull();
  });

  it('zero or negative tolerance returns null', () => {
    expect(findOSnap(square, 0, 0, 0, DEFAULT_OSNAP_SETTINGS)).toBeNull();
    expect(findOSnap(square, 0, 0, -1, DEFAULT_OSNAP_SETTINGS)).toBeNull();
  });
});
