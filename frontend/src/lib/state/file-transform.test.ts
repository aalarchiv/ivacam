/// File-transform engine tests (bww). Covers the geometry pipeline that
/// projects an imported ImportResponse through a FileTransform.

import { describe, expect, it } from 'vitest';
import { applyFileTransform } from './file-transform';
import { identityFileTransform, type FileTransform } from './project-types';
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

function imp(segments: Segment[]): ImportResponse {
  // Bbox over endpoints only — fine for these toy cases.
  let minX = Infinity,
    minY = Infinity,
    maxX = -Infinity,
    maxY = -Infinity;
  for (const s of segments) {
    minX = Math.min(minX, s.start.x, s.end.x);
    minY = Math.min(minY, s.start.y, s.end.y);
    maxX = Math.max(maxX, s.start.x, s.end.x);
    maxY = Math.max(maxY, s.start.y, s.end.y);
  }
  return {
    filename: 't.dxf',
    format: 'dxf',
    segments,
    bbox: { min_x: minX, min_y: minY, max_x: maxX, max_y: maxY },
    layers: [{ name: '0', color: 7, segment_count: segments.length }],
    unit_scale: 1,
    warnings: [],
    objects: segments.map(() => 1),
    object_meta: [],
  };
}

function tx(patch: Partial<FileTransform>): FileTransform {
  return { ...identityFileTransform(), ...patch };
}

describe('applyFileTransform', () => {
  it('identity short-circuits to the same reference', () => {
    const i = imp([line(0, 0, 10, 0)]);
    expect(applyFileTransform(i, identityFileTransform())).toBe(i);
  });

  it('translate moves every endpoint by (dx, dy)', () => {
    const i = imp([line(0, 0, 10, 0)]);
    const out = applyFileTransform(i, tx({ translate: { x: 5, y: 3 } }));
    expect(out.segments[0].start).toEqual({ x: 5, y: 3 });
    expect(out.segments[0].end).toEqual({ x: 15, y: 3 });
    expect(out.bbox).toEqual({ min_x: 5, min_y: 3, max_x: 15, max_y: 3 });
  });

  it('rotate 90° around bbox center swaps axes (square)', () => {
    // 10x10 square: bbox center is (5, 5). Rotation around it maps
    // (0,0) → (10,0) for +90° (counter-clockwise).
    const i = imp([
      line(0, 0, 10, 0),
      line(10, 0, 10, 10),
      line(10, 10, 0, 10),
      line(0, 10, 0, 0),
    ]);
    const out = applyFileTransform(i, tx({ rotateDeg: 90 }));
    // First segment was bottom edge (0,0)→(10,0). After +90° around
    // (5,5) it becomes the right edge (10,0)→(10,10).
    const s0 = out.segments[0];
    expect(s0.start.x).toBeCloseTo(10);
    expect(s0.start.y).toBeCloseTo(0);
    expect(s0.end.x).toBeCloseTo(10);
    expect(s0.end.y).toBeCloseTo(10);
  });

  it('scale 2× around bbox center doubles extents around center', () => {
    const i = imp([line(0, 0, 10, 10)]);
    const out = applyFileTransform(i, tx({ scale: 2 }));
    // pivot = (5,5); (0,0) - pivot = (-5,-5), ×2 = (-10,-10), + pivot = (-5,-5)
    expect(out.segments[0].start).toEqual({ x: -5, y: -5 });
    expect(out.segments[0].end).toEqual({ x: 15, y: 15 });
  });

  it('mirror X flips Y around bbox center and negates bulge', () => {
    const i = imp([arc(0, 0, 10, 10, 5, 5, 0.5)]);
    const out = applyFileTransform(i, tx({ mirrorX: true }));
    const s = out.segments[0];
    // bbox center y = 5; (0,0).y mirrored → 10; (10,10).y mirrored → 0
    expect(s.start.y).toBeCloseTo(10);
    expect(s.end.y).toBeCloseTo(0);
    // Bulge negated.
    expect(s.bulge).toBeCloseTo(-0.5);
    // X unchanged.
    expect(s.start.x).toBeCloseTo(0);
    expect(s.end.x).toBeCloseTo(10);
  });

  it('mirror X + mirror Y cancels the bulge sign (two negations)', () => {
    const i = imp([arc(0, 0, 10, 10, 5, 5, 0.3)]);
    const out = applyFileTransform(i, tx({ mirrorX: true, mirrorY: true }));
    expect(out.segments[0].bulge).toBeCloseTo(0.3);
  });

  it('arc centers transform too', () => {
    const i = imp([arc(0, 0, 10, 10, 5, 5, 0.5)]);
    const out = applyFileTransform(i, tx({ translate: { x: 10, y: 0 } }));
    expect(out.segments[0].center).toEqual({ x: 15, y: 5 });
  });

  it('bbox recomputes after transform (rotated square stays 10x10)', () => {
    const i = imp([
      line(0, 0, 10, 0),
      line(10, 0, 10, 10),
      line(10, 10, 0, 10),
      line(0, 10, 0, 0),
    ]);
    const out = applyFileTransform(i, tx({ rotateDeg: 45 }));
    // Rotated 45° around its center, the bbox of the diamond is ~14.14
    // on each side. Center stays at (5,5).
    const w = out.bbox.max_x - out.bbox.min_x;
    const h = out.bbox.max_y - out.bbox.min_y;
    expect(w).toBeCloseTo(Math.SQRT2 * 10, 2);
    expect(h).toBeCloseTo(Math.SQRT2 * 10, 2);
  });
});
