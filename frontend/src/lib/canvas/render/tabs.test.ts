import { describe, it, expect } from 'vitest';
import type { ObjectPolyline } from '../../cam/tabs';
import { drawTabs, drawTabMarker, type TabRenderOp } from './tabs';
import { stubCtx, flipY } from './stub-ctx';

const COLORS = { fill: '#manual', auto: '#auto', stroke: '#bg', accent: '#acc' };

// 100×100 closed square, object id 1.
const SQUARE: ObjectPolyline = {
  objectId: 1,
  pts: [
    { x: 0, y: 0 },
    { x: 100, y: 0 },
    { x: 100, y: 100 },
    { x: 0, y: 100 },
  ],
  closed: true,
};

function op(over: Partial<TabRenderOp>): TabRenderOp {
  return { id: 1, ...over };
}

describe('drawTabs', () => {
  it('skips ops with tabs off and nothing placed', () => {
    const s = stubCtx();
    drawTabs(s.ctx, flipY, 1, [op({ tabMode: { kind: 'off' } })], [SQUARE], null, COLORS);
    expect(s.calls).toHaveLength(0);
  });

  it('renders manual placements in the manual fill', () => {
    const s = stubCtx();
    drawTabs(
      s.ctx,
      flipY,
      1,
      [op({ tabMode: { kind: 'manual' }, tabPlacements: [{ objectId: 1, t: 0.125 }] })],
      [SQUARE],
      null,
      COLORS,
    );
    const fills = s.ops('fill');
    expect(fills).toHaveLength(1);
    expect(fills[0].fillStyle).toBe('#manual');
  });

  it('renders N auto tabs per closed contour in the auto fill', () => {
    const s = stubCtx();
    drawTabs(
      s.ctx,
      flipY,
      1,
      [op({ tabMode: { kind: 'auto', count: 4 } })],
      [SQUARE],
      null,
      COLORS,
    );
    const fills = s.ops('fill');
    expect(fills).toHaveLength(4);
    expect(fills.every((f) => f.fillStyle === '#auto')).toBe(true);
  });

  it('honors the sourceObjects filter', () => {
    const s = stubCtx();
    drawTabs(
      s.ctx,
      flipY,
      1,
      [op({ tabMode: { kind: 'auto', count: 4 }, sourceObjects: [99] })],
      [SQUARE],
      null,
      COLORS,
    );
    expect(s.ops('fill')).toHaveLength(0);
  });

  it('draws the ghost dashed + a snap dot for non-contour snaps', () => {
    const s = stubCtx();
    drawTabs(
      s.ctx,
      flipY,
      1,
      [],
      [SQUARE],
      { tab: { x: 50, y: 0, objectId: 1, t: 0.125, snap: 'midpoint' }, op: op({}) },
      COLORS,
    );
    expect(s.ops('setLineDash')[0].args).toEqual([[4, 3]]);
    // Snap dot: a fill in accent (the dot's arc precedes the style set).
    const dot = s.ops('fill').find((c) => c.fillStyle === '#acc');
    expect(dot).toBeDefined();
  });
});

describe('drawTabMarker', () => {
  it('clamps the marker to a visible pill at extreme zoom-out', () => {
    const s = stubCtx();
    // scale 0.001 → data size collapses; expect the 3px/2px floors.
    drawTabMarker(s.ctx, flipY, 0.001, 0, 0, 1, 0, 10, 1, '#f', '#s');
    const [x0] = s.ops('moveTo')[0].args as [number, number];
    const [x1] = s.ops('lineTo')[0].args as [number, number];
    expect(Math.abs(x1 - x0)).toBeCloseTo(6); // 2 × 3px min half-length
  });
});
