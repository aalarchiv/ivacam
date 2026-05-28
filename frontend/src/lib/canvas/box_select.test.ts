import { describe, it, expect } from 'vitest';
import { objectsContainedInBox, type ObjectMeta } from './box_select';

const STOCK = '__stock_outline__';

// Identity-ish transform: dataX = canvasX, dataY = -canvasY.
const ID_TRANSFORM = { scale: 1, offX: 0, offY: 0 };

// Two objects on 'top', one on a hidden layer, one stock-outline.
const META: ObjectMeta[] = [
  { id: 1, layer: 'top', bbox: { min_x: 0, min_y: 0, max_x: 10, max_y: 10 } },
  { id: 2, layer: 'top', bbox: { min_x: 20, min_y: 20, max_x: 30, max_y: 30 } },
  { id: 3, layer: 'hidden', bbox: { min_x: 5, min_y: 5, max_x: 8, max_y: 8 } },
  { id: 4, layer: STOCK, bbox: { min_x: 0, min_y: 0, max_x: 50, max_y: 50 } },
];

describe('objectsContainedInBox', () => {
  it('returns only objects whose bbox lies fully inside the canvas rectangle', () => {
    // Selection rectangle in canvas px ⇒ data rect (0,-15) to (15,0).
    // Object 1 (0,0)-(10,10) lies inside the data rect when Y-flipped.
    const ids = objectsContainedInBox(META, new Set(['top']), ID_TRANSFORM, 0, -15, 15, 0, STOCK);
    expect(ids).toContain(1);
    expect(ids).not.toContain(2); // Object 2 is at (20-30, 20-30), outside.
  });

  it('excludes objects whose layer is hidden', () => {
    const ids = objectsContainedInBox(META, new Set(['top']), ID_TRANSFORM, 0, -15, 15, 0, STOCK);
    expect(ids).not.toContain(3);
  });

  it('always picks the synthetic stock-outline layer even when not in visibleLayers', () => {
    const ids = objectsContainedInBox(META, new Set<string>(), ID_TRANSFORM, 0, -60, 60, 0, STOCK);
    expect(ids).toContain(4);
  });

  it('returns an empty list when the rectangle clears every object', () => {
    const ids = objectsContainedInBox(
      META,
      new Set(['top']),
      ID_TRANSFORM,
      1000,
      -1100,
      1100,
      -1000,
      STOCK,
    );
    expect(ids).toEqual([]);
  });

  it('rectangle direction-agnostic: dragging right-to-left or up-to-down works', () => {
    const a = objectsContainedInBox(META, new Set(['top']), ID_TRANSFORM, 0, -15, 15, 0, STOCK);
    const b = objectsContainedInBox(META, new Set(['top']), ID_TRANSFORM, 15, 0, 0, -15, STOCK);
    expect(a).toEqual(b);
  });
});
