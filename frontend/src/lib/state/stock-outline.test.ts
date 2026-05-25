import { describe, expect, it } from 'vitest';
import {
  augmentWithStockOutline,
  stockOutlineSegments,
  stockOutlineId,
  STOCK_OUTLINE_LAYER,
  type Footprint,
} from './stock-outline';
import type { ImportResponse } from '../api/types';

const FP: Footprint = { minX: 0, minY: 0, maxX: 100, maxY: 50 };

function baseImport(): ImportResponse {
  return {
    bbox: { min_x: 10, min_y: 10, max_x: 40, max_y: 30 },
    filename: 'part.dxf',
    format: 'dxf',
    layers: [{ name: '0', color: 7, segment_count: 2 }],
    object_meta: [
      { bbox: { min_x: 10, min_y: 10, max_x: 40, max_y: 30 }, closed: true, color: 7, id: 1, layer: '0' },
    ],
    objects: [1, 1],
    segments: [
      { type: 'LINE', start: { x: 10, y: 10 }, end: { x: 40, y: 10 }, bulge: 0, layer: '0', color: 7 },
      { type: 'LINE', start: { x: 40, y: 10 }, end: { x: 40, y: 30 }, bulge: 0, layer: '0', color: 7 },
    ],
    unit_scale: 1,
    warnings: [],
  } as ImportResponse;
}

describe('stockOutlineSegments', () => {
  it('builds a closed 4-segment rectangle from the footprint', () => {
    const segs = stockOutlineSegments(FP);
    expect(segs).toHaveLength(4);
    // first starts at min corner, last ends back at min corner (closed)
    expect(segs[0].start).toEqual({ x: 0, y: 0 });
    expect(segs[3].end).toEqual({ x: 0, y: 0 });
    expect(segs.every((s) => s.layer === STOCK_OUTLINE_LAYER)).toBe(true);
  });
});

describe('augmentWithStockOutline', () => {
  it('returns the SAME object (identity) on a degenerate footprint', () => {
    const base = baseImport();
    expect(augmentWithStockOutline(base, { minX: 0, minY: 0, maxX: 0, maxY: 50 })).toBe(base);
    expect(augmentWithStockOutline(base, { minX: 0, minY: 0, maxX: NaN, maxY: 5 })).toBe(base);
  });

  it('returns null identity when base is null AND footprint degenerate', () => {
    expect(augmentWithStockOutline(null, { minX: 0, minY: 0, maxX: 0, maxY: 0 })).toBeNull();
  });

  it('appends the outline object to a non-null base without mutating it', () => {
    const base = baseImport();
    const out = augmentWithStockOutline(base, FP)!;
    // base untouched
    expect(base.segments).toHaveLength(2);
    expect(base.objects).toEqual([1, 1]);
    // augmented view has the 4 outline segments + their object ids.
    // base has 1 object → outline takes the next sequential id (2).
    const id = stockOutlineId(1);
    expect(id).toBe(2);
    expect(out.segments).toHaveLength(6);
    expect(out.objects.slice(2)).toEqual([id, id, id, id]);
    const meta = out.object_meta[out.object_meta.length - 1];
    expect(meta.id).toBe(id);
    expect(meta.closed).toBe(true);
    expect(meta.bbox).toEqual({ min_x: 0, min_y: 0, max_x: 100, max_y: 50 });
    expect(out.layers.some((l) => l.name === STOCK_OUTLINE_LAYER)).toBe(true);
  });

  it('synthesizes a minimal ImportResponse when base is null', () => {
    const out = augmentWithStockOutline(null, FP)!;
    // no base objects → outline is id 1
    expect(out.segments).toHaveLength(4);
    expect(out.objects).toEqual([1, 1, 1, 1]);
    expect(out.object_meta).toHaveLength(1);
    expect(out.object_meta[0].id).toBe(1);
    expect(out.bbox).toEqual({ min_x: 0, min_y: 0, max_x: 100, max_y: 50 });
  });

  it('does not duplicate the outline layer if it already exists', () => {
    const base = baseImport();
    base.layers.push({ name: STOCK_OUTLINE_LAYER, color: 7, segment_count: 4 });
    const out = augmentWithStockOutline(base, FP)!;
    expect(out.layers.filter((l) => l.name === STOCK_OUTLINE_LAYER)).toHaveLength(1);
  });
});
