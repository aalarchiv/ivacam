import { describe, it, expect } from 'vitest';
import {
  worldToScreen,
  stockHandleScreenPositions,
  hitStockHandle,
  dragStockBox,
  boxToStock,
  type WorldBox,
  type ScreenXform,
} from './stock-gizmo';

const box: WorldBox = { minX: 0, minY: 0, maxX: 100, maxY: 60 };
// scale 2 px/mm, origin offset so world (0,0) → screen (50, 200).
const t: ScreenXform = { scale: 2, offX: 50, offY: 200 };

describe('worldToScreen', () => {
  it('maps world +Y up to smaller screen y', () => {
    expect(worldToScreen(0, 0, t)).toEqual({ x: 50, y: 200 });
    expect(worldToScreen(100, 60, t)).toEqual({ x: 250, y: 80 });
  });
});

describe('stockHandleScreenPositions', () => {
  const pos = stockHandleScreenPositions(box, t);
  it('places corners at the projected box extremes', () => {
    // nw = screen-top-left = world (minX, maxY)
    expect(pos.nw).toEqual(worldToScreen(0, 60, t));
    expect(pos.se).toEqual(worldToScreen(100, 0, t));
  });
  it('places the move puck at the box centre', () => {
    expect(pos.move).toEqual(worldToScreen(50, 30, t));
  });
  it('places edge handles at edge midpoints', () => {
    expect(pos.n).toEqual(worldToScreen(50, 60, t));
    expect(pos.e).toEqual(worldToScreen(100, 30, t));
  });
});

describe('hitStockHandle', () => {
  const pos = stockHandleScreenPositions(box, t);
  it('hits a corner when within tolerance', () => {
    expect(hitStockHandle(box, t, pos.ne.x + 3, pos.ne.y - 2, 12)).toBe('ne');
  });
  it('hits the move puck at centre', () => {
    expect(hitStockHandle(box, t, pos.move.x, pos.move.y, 12)).toBe('move');
  });
  it('returns null when no handle is near', () => {
    expect(hitStockHandle(box, t, pos.move.x + 40, pos.move.y, 8)).toBeNull();
  });
  it('prefers a corner over an edge when both are in range', () => {
    // A huge tolerance would catch several handles; corner must win.
    expect(hitStockHandle(box, t, pos.ne.x, pos.ne.y, 1000)).toBe('ne');
  });
});

describe('dragStockBox', () => {
  const grab = { x: 100, y: 60 }; // grabbing the NE corner in world space
  it('moves only the dragged edges (east + north) of a corner', () => {
    const out = dragStockBox('ne', box, grab, { x: 120, y: 90 }, 1);
    expect(out).toEqual({ minX: 0, minY: 0, maxX: 120, maxY: 90 });
  });
  it('keeps the opposite edge fixed when dragging a single edge', () => {
    const out = dragStockBox('e', box, { x: 100, y: 30 }, { x: 140, y: 30 }, 1);
    expect(out).toEqual({ minX: 0, minY: 0, maxX: 140, maxY: 60 });
  });
  it('clamps to min size by pinning the dragged edge', () => {
    // Drag east edge left past the west edge → clamp to minX + minSize.
    const out = dragStockBox('e', box, { x: 100, y: 30 }, { x: -50, y: 30 }, 5);
    expect(out.maxX).toBe(5);
    expect(out.minX).toBe(0);
  });
  it('clamps the west edge against a fixed east edge', () => {
    const out = dragStockBox('w', box, { x: 0, y: 30 }, { x: 200, y: 30 }, 5);
    expect(out.minX).toBe(95); // maxX(100) - minSize(5)
    expect(out.maxX).toBe(100);
  });
});

describe('boxToStock', () => {
  it('inverts a box to manual size + recentring offset (no geometry)', () => {
    const out = boxToStock({ minX: 10, minY: 20, maxX: 110, maxY: 80 }, null);
    expect(out).toEqual({
      mode: 'manual',
      customX: 100,
      customY: 60,
      offsetX: 60, // centre (60,50) - origin (0,0)
      offsetY: 50,
    });
  });
  it('offsets relative to the imported bbox centre', () => {
    const out = boxToStock({ minX: 0, minY: 0, maxX: 100, maxY: 60 }, { x: 50, y: 30 });
    // box centre equals bbox centre → zero offset.
    expect(out.offsetX).toBe(0);
    expect(out.offsetY).toBe(0);
    expect(out.customX).toBe(100);
    expect(out.customY).toBe(60);
  });
  it('round-trips a corner resize through footprint centring', () => {
    // Start 100×60 centred at bbox (50,30) with zero offset → drag NE out.
    const start: WorldBox = { minX: 0, minY: 0, maxX: 100, maxY: 60 };
    const dragged = dragStockBox('ne', start, { x: 100, y: 60 }, { x: 120, y: 80 }, 1);
    const stock = boxToStock(dragged, { x: 50, y: 30 });
    expect(stock.customX).toBe(120);
    expect(stock.customY).toBe(80);
    // New centre (60,40) shifts +10,+10 off the bbox centre.
    expect(stock.offsetX).toBe(10);
    expect(stock.offsetY).toBe(10);
  });
});
