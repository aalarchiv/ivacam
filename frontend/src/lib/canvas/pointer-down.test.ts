import { describe, it, expect, vi } from 'vitest';
import { reducePointerDown, type PointerDownEnv } from './pointer-down';

const RASTER = { opId: 3, sourceId: 7, grabDX: 1, grabDY: 2 };
const TEXT = { id: 4, grabDX: 0.5, grabDY: 0.5 };
const GHOST = { objectId: 1, t: 0.25 };

function env(over: Partial<PointerDownEnv>): PointerDownEnv {
  return {
    button: 0,
    approachPickActive: false,
    tabPlacementActive: false,
    approachMarkerHit: () => false,
    rasterHit: () => null,
    textHit: () => null,
    tabGhost: () => null,
    fixtureHit: () => null,
    ...over,
  };
}

describe('reducePointerDown', () => {
  it('middle button pans regardless of mode', () => {
    expect(reducePointerDown(env({ button: 1, approachPickActive: true })).kind).toBe('pan');
  });

  it('pick mode: left commits, right exits, no hit-tests run', () => {
    const marker = vi.fn(() => true);
    expect(
      reducePointerDown(env({ approachPickActive: true, approachMarkerHit: marker })).kind,
    ).toBe('approach-commit');
    expect(
      reducePointerDown(env({ approachPickActive: true, button: 2, approachMarkerHit: marker }))
        .kind,
    ).toBe('approach-exit');
    expect(marker).not.toHaveBeenCalled();
  });

  it('right-click outside pick mode is ignored — context menu owns it (user-reported regression)', () => {
    const fixture = vi.fn(() => 9);
    expect(reducePointerDown(env({ button: 2, fixtureHit: fixture })).kind).toBe('ignore');
    expect(reducePointerDown(env({ button: 3 })).kind).toBe('ignore');
    expect(fixture).not.toHaveBeenCalled();
  });

  it('approach-marker drag wins over every other left-click target', () => {
    const out = reducePointerDown(
      env({
        approachMarkerHit: () => true,
        rasterHit: () => RASTER,
        textHit: () => TEXT,
        fixtureHit: () => 9,
      }),
    );
    expect(out.kind).toBe('approach-drag');
  });

  it('raster grab beats text grab; text is never probed on a raster hit', () => {
    const text = vi.fn(() => TEXT);
    const out = reducePointerDown(env({ rasterHit: () => RASTER, textHit: text }));
    expect(out).toEqual({ kind: 'raster-drag', grab: RASTER });
    expect(text).not.toHaveBeenCalled();
  });

  it('text grab falls through when raster misses', () => {
    expect(reducePointerDown(env({ textHit: () => TEXT }))).toEqual({
      kind: 'text-drag',
      grab: TEXT,
    });
  });

  it('tab mode swallows raster / text grabs and toggles at the ghost', () => {
    const raster = vi.fn(() => RASTER);
    const out = reducePointerDown(
      env({ tabPlacementActive: true, rasterHit: raster, tabGhost: () => GHOST }),
    );
    expect(out).toEqual({ kind: 'tab-toggle', at: GHOST });
    expect(raster).not.toHaveBeenCalled();
  });

  it('tab mode with no ghost swallows the click (no selection change)', () => {
    const fixture = vi.fn(() => 9);
    expect(reducePointerDown(env({ tabPlacementActive: true, fixtureHit: fixture })).kind).toBe(
      'tab-miss',
    );
    expect(fixture).not.toHaveBeenCalled();
  });

  it('fixture select runs before entity selection; empty canvas is an entity click', () => {
    expect(reducePointerDown(env({ fixtureHit: () => 9 }))).toEqual({
      kind: 'fixture-select',
      id: 9,
    });
    expect(reducePointerDown(env({})).kind).toBe('entity-click');
  });
});
