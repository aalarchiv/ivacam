import { describe, it, expect } from 'vitest';
import type { Segment } from '../../api/types';
import { drawSegment } from './segment';
import { stubCtx, flipY } from './stub-ctx';

function seg(over: Partial<Segment>): Segment {
  return {
    start: { x: 0, y: 0 },
    end: { x: 10, y: 0 },
    bulge: 0,
    center: { x: 0, y: 0 },
    layer: '0',
    color: 7,
    ...over,
  } as Segment;
}

describe('drawSegment', () => {
  it('strokes a straight line through moveTo/lineTo', () => {
    const s = stubCtx();
    drawSegment(s.ctx, seg({}), flipY);
    expect(s.ops('moveTo')[0].args).toEqual([0, -0]);
    expect(s.ops('lineTo')[0].args).toEqual([10, -0]);
    expect(s.ops('stroke')).toHaveLength(1);
    expect(s.ops('arc')).toHaveLength(0);
  });

  it('draws a POINT as a filled 2px dot', () => {
    const s = stubCtx();
    drawSegment(s.ctx, seg({ type: 'POINT' }), flipY);
    const arcs = s.ops('arc');
    expect(arcs).toHaveLength(1);
    expect(arcs[0].args.slice(0, 3)).toEqual([0, -0, 2]);
    expect(s.ops('fill')).toHaveLength(1);
    expect(s.ops('stroke')).toHaveLength(0);
  });

  it('recomputes the arc center from the bulge (semicircle case)', () => {
    // bulge=1 → semicircle: chord (0,0)→(10,0), center (5,0), radius 5.
    const s = stubCtx();
    drawSegment(s.ctx, seg({ bulge: 1 }), flipY);
    const arcs = s.ops('arc');
    expect(arcs).toHaveLength(1);
    const [cx, cy, r, , , ccw] = arcs[0].args as [number, number, number, number, number, boolean];
    expect(cx).toBeCloseTo(5);
    expect(cy).toBeCloseTo(-0);
    expect(r).toBeCloseTo(5);
    expect(ccw).toBe(true);
  });

  it('survives a vertical chord (start directly above the center) — 7iej.19', () => {
    // Quarter-ish arc with a vertical chord: (0,0)→(0,10), bulge 0.5.
    const s = stubCtx();
    drawSegment(s.ctx, seg({ start: { x: 0, y: 0 }, end: { x: 0, y: 10 }, bulge: 0.5 }), flipY);
    const [, , r] = s.ops('arc')[0].args as [number, number, number];
    expect(Number.isFinite(r)).toBe(true);
    expect(r).toBeGreaterThan(0);
  });

  it('skips degenerate zero-length bulged segments', () => {
    const s = stubCtx();
    drawSegment(s.ctx, seg({ end: { x: 0, y: 0 }, bulge: 1 }), flipY);
    expect(s.ops('arc')).toHaveLength(0);
    expect(s.ops('stroke')).toHaveLength(0);
  });
});
