import { describe, it, expect } from 'vitest';
import { drawGrid, drawAxes, drawWorkArea, drawStock } from './chrome';
import { stubCtx, flipY } from './stub-ctx';

const GRID_COLORS = { minor: '#111', major: '#222' };

describe('drawGrid', () => {
  it('skips entirely when the minor pitch is under 6px', () => {
    const s = stubCtx();
    drawGrid(s.ctx, 100, 100, 5, 0, 100, GRID_COLORS); // 5px per unit
    expect(s.calls).toHaveLength(0);
  });

  it('strokes minor then major passes in their own colors', () => {
    const s = stubCtx();
    drawGrid(s.ctx, 100, 100, 10, 0, 100, GRID_COLORS);
    const strokes = s.ops('stroke');
    expect(strokes).toHaveLength(2);
    expect(strokes[0].strokeStyle).toBe('#111');
    expect(strokes[1].strokeStyle).toBe('#222');
  });
});

describe('drawAxes', () => {
  it('draws the X axis at offY and the Y axis at offX', () => {
    const s = stubCtx();
    drawAxes(s.ctx, 200, 100, 30, 70, { x: 'red', y: 'green' });
    const moves = s.ops('moveTo');
    expect(moves[0].args).toEqual([0, 70]); // X axis horizontal line
    expect(moves[0].strokeStyle).toBe('red');
    expect(moves[1].args).toEqual([30, 0]); // Y axis vertical line
    expect(s.ops('lineTo')[1].strokeStyle).toBe('green');
  });
});

describe('drawWorkArea', () => {
  it('no-ops without a positive work area', () => {
    const s = stubCtx();
    drawWorkArea(s.ctx, flipY, null, '#888');
    drawWorkArea(s.ctx, flipY, { x: 0, y: 100 }, '#888');
    expect(s.calls).toHaveLength(0);
  });

  it('strokes a dashed rect from (0,0) to (x,y)', () => {
    const s = stubCtx();
    drawWorkArea(s.ctx, flipY, { x: 100, y: 50 }, '#888');
    expect(s.ops('setLineDash')[0].args).toEqual([[6, 4]]);
    expect(s.ops('strokeRect')[0].args).toEqual([0, -50, 100, 50]);
  });
});

describe('drawStock', () => {
  it('no-ops on an empty footprint', () => {
    const s = stubCtx();
    drawStock(s.ctx, flipY, { minX: 0, minY: 0, maxX: 0, maxY: 10 }, '#888');
    expect(s.calls).toHaveLength(0);
  });

  it('strokes the footprint rect', () => {
    const s = stubCtx();
    drawStock(s.ctx, flipY, { minX: 10, minY: 20, maxX: 110, maxY: 70 }, '#888');
    expect(s.ops('strokeRect')[0].args).toEqual([10, -70, 100, 50]);
  });
});
