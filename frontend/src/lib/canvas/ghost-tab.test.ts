import { describe, it, expect } from 'vitest';
import { projectGhostTab, type GhostTabContext } from './ghost-tab';
import { polylineAtT, type ObjectPolyline } from '../cam/tabs';
import { DEFAULT_OSNAP_SETTINGS, type OSnapTargets } from './osnap';

// A 100×100 closed square, object id 1. Bottom edge runs (0,0)→(100,0).
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

const NO_TARGETS: OSnapTargets = { endpoints: [], midpoints: [], intersections: [], centers: [] };

// Identity-ish transform: dataX = cx, dataY = -cy (mirror of the draw
// transform with scale 1 and no offset).
function ctx(over: Partial<GhostTabContext> = {}): GhostTabContext {
  return {
    transform: { scale: 1, offX: 0, offY: 0 },
    polylines: [SQUARE],
    sourceObjects: null,
    tabPlacements: [],
    altDown: false,
    osnapTargets: NO_TARGETS,
    osnapSettings: DEFAULT_OSNAP_SETTINGS,
    ...over,
  };
}

describe('projectGhostTab', () => {
  it('returns null when the cursor is farther than 6 screen-px from any contour', () => {
    // (50, -50) in data: dead centre of the square, 50 mm from the nearest edge.
    expect(projectGhostTab(50, 50, ctx())).toBeNull();
  });

  it('snaps to the raw contour projection when no osnap target is near', () => {
    // Cursor at data (50, 0): on the bottom edge midpoint.
    const g = projectGhostTab(50, 0, ctx());
    expect(g).not.toBeNull();
    expect(g!.snap).toBe('contour');
    expect(g!.objectId).toBe(1);
    expect(g!.x).toBeCloseTo(50);
    expect(g!.y).toBeCloseTo(0);
  });

  it('promotes to a vertex snap when an endpoint osnap target is within 4 px', () => {
    // Cursor at data (2, 0); endpoint target at the (0,0) corner.
    const g = projectGhostTab(2, 0, ctx({ osnapTargets: { ...NO_TARGETS, endpoints: [{ x: 0, y: 0 }] } }));
    expect(g!.snap).toBe('vertex');
    expect(g!.x).toBeCloseTo(0);
    expect(g!.y).toBeCloseTo(0);
  });

  it('Alt disables secondary snaps — bare contour even with a vertex in range', () => {
    const g = projectGhostTab(
      2,
      0,
      ctx({ altDown: true, osnapTargets: { ...NO_TARGETS, endpoints: [{ x: 0, y: 0 }] } }),
    );
    expect(g!.snap).toBe('contour');
    expect(g!.x).toBeCloseTo(2);
  });

  it('snaps to an existing tab on the same object within 2 mm', () => {
    // Place an existing tab at t=0.1 and aim the cursor exactly at it.
    const t = 0.1;
    const { point } = polylineAtT(SQUARE.pts, t, SQUARE.closed);
    // data (point.x, point.y) ⇒ cx = point.x, cy = -point.y.
    const g = projectGhostTab(point.x, -point.y, ctx({ tabPlacements: [{ objectId: 1, t }] }));
    expect(g!.snap).toBe('existing');
    expect(g!.t).toBeCloseTo(t);
  });

  it('respects the op-source filter — ignores objects the op does not consume', () => {
    // Op only consumes object 2; cursor is on object 1's contour.
    const g = projectGhostTab(50, 0, ctx({ sourceObjects: [2] }));
    expect(g).toBeNull();
  });
});
