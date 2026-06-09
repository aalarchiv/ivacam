import { describe, it, expect } from 'vitest';
import type { Segment } from '../../api/types';
import { drawImportedWireframe, drawEntityHalos, type EntityHaloParams } from './entities';
import { stubCtx, flipY } from './stub-ctx';

function seg(layer: string): Segment {
  return {
    start: { x: 0, y: 0 },
    end: { x: 10, y: 0 },
    bulge: 0,
    center: { x: 0, y: 0 },
    layer,
    color: 7,
  } as Segment;
}

const COLORS = { hover: '#hov', halo: '#halo', accent: '#acc' };

function params(over: Partial<EntityHaloParams>): EntityHaloParams {
  return {
    segments: [seg('a'), seg('a')],
    objects: [1, 2],
    visibleLayers: new Set(['a']),
    selectedObjects: new Set(),
    hoverObjectId: 0,
    objectToOps: new Map(),
    selectedOpId: null,
    opColor: (id, emph) => `op${id}${emph ? '!' : ''}`,
    colors: COLORS,
    ...over,
  };
}

describe('drawImportedWireframe', () => {
  it('strokes only visible layers in their ACI colors', () => {
    const s = stubCtx();
    drawImportedWireframe(
      s.ctx,
      flipY,
      [seg('a'), seg('hidden')],
      new Set(['a']),
      1.5,
      () => '#aci',
    );
    expect(s.ops('stroke')).toHaveLength(1);
    expect(s.ops('stroke')[0].strokeStyle).toBe('#aci');
    expect(s.ops('stroke')[0].lineWidth).toBe(1.5);
  });
});

describe('drawEntityHalos', () => {
  it('paints nothing for idle, unassigned objects', () => {
    const s = stubCtx();
    drawEntityHalos(s.ctx, flipY, params({}));
    expect(s.calls).toHaveLength(0);
  });

  it('hover gets the hover stroke; selection gets halo + accent', () => {
    const s = stubCtx();
    drawEntityHalos(s.ctx, flipY, params({ hoverObjectId: 1, selectedObjects: new Set([2]) }));
    const strokes = s.ops('stroke');
    // seg1: hover (1 stroke); seg2: halo + accent (2 strokes).
    expect(strokes).toHaveLength(3);
    expect(strokes[0].strokeStyle).toBe('#hov');
    expect(strokes[1].strokeStyle).toBe('#halo');
    expect(strokes[1].globalAlpha).toBe(0.6);
    expect(strokes[2].strokeStyle).toBe('#acc');
  });

  it('orders assignment rings widest-first with the selected op innermost', () => {
    const s = stubCtx();
    drawEntityHalos(
      s.ctx,
      flipY,
      params({
        segments: [seg('a')],
        objects: [1],
        objectToOps: new Map([[1, [5, 9]]]),
        selectedOpId: 5,
      }),
    );
    const strokes = s.ops('stroke');
    // contrast halo + 2 rings
    expect(strokes).toHaveLength(3);
    expect(strokes[0].strokeStyle).toBe('#halo');
    // op 9 (not selected) outermost / widest; selected op 5 innermost + emphasized.
    expect(strokes[1].strokeStyle).toBe('op9');
    expect(strokes[2].strokeStyle).toBe('op5!');
    expect(strokes[1].lineWidth).toBeGreaterThan(strokes[2].lineWidth);
  });
});
