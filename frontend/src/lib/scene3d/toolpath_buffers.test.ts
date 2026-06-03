import { describe, expect, it } from 'vitest';
import {
  computeArrowChevron,
  arrowSpacingMm,
  moveBoost,
  resolveSegmentColor,
  fadeColor,
  type ArrowParams,
  type Rgb,
} from './toolpath_buffers';

const P: ArrowParams = {
  minLen: 1.0,
  maxSize: 4.0,
  sizeFrac: 0.2,
  halfWing: Math.tan((30 * Math.PI) / 180),
};

describe('arrowSpacingMm', () => {
  it('disables arrows at density 0 (Infinity spacing)', () => {
    expect(arrowSpacingMm(0)).toBe(Infinity);
  });
  it('packs arrows closer as density rises', () => {
    expect(arrowSpacingMm(1)).toBeCloseTo(3.0);
    expect(arrowSpacingMm(2)).toBeCloseTo(1.5);
  });
});

describe('computeArrowChevron', () => {
  it('returns null for a segment shorter than minLen', () => {
    expect(computeArrowChevron({ x: 0, y: 0, z: 0 }, { x: 0.5, y: 0, z: 0 }, P)).toBeNull();
  });

  it('builds a chevron pointing along a +X move with ±normal wings', () => {
    const c = computeArrowChevron({ x: 0, y: 0, z: 0 }, { x: 10, y: 0, z: 0 }, P);
    expect(c).not.toBeNull();
    // A = min(10*0.2, 4) = 2; apex at midpoint (5,0,0); wings 2mm back
    // (x=3) and ±A*halfWing in Y.
    const side = 2 * P.halfWing;
    expect(c!.mid).toEqual([5, 0, 0]);
    expect(c!.wing1[0]).toBeCloseTo(3);
    expect(c!.wing1[1]).toBeCloseTo(side);
    expect(c!.wing1[2]).toBeCloseTo(0);
    expect(c!.wing2[0]).toBeCloseTo(3);
    expect(c!.wing2[1]).toBeCloseTo(-side);
    // Wings are symmetric about the move axis.
    expect(c!.wing1[1]).toBeCloseTo(-c!.wing2[1]);
  });

  it('caps arrow size at maxSize on a long move', () => {
    const c = computeArrowChevron({ x: 0, y: 0, z: 0 }, { x: 100, y: 0, z: 0 }, P);
    // A = min(100*0.2=20, 4) = 4 → wings 4mm behind the midpoint (x=46).
    expect(c!.wing1[0]).toBeCloseTo(46);
    expect(c!.wing2[0]).toBeCloseTo(46);
  });

  it('falls back to a +X normal for a pure-Z (plunge) move', () => {
    const c = computeArrowChevron({ x: 0, y: 0, z: 0 }, { x: 0, y: 0, z: 5 }, P);
    expect(c).not.toBeNull();
    // A = min(5*0.2=1, 4) = 1; apex (0,0,2.5); wings 1mm back in Z (z=1.5)
    // and ±halfWing in X (the fallback normal).
    const side = 1 * P.halfWing;
    expect(c!.mid).toEqual([0, 0, 2.5]);
    expect(c!.wing1[0]).toBeCloseTo(side);
    expect(c!.wing1[2]).toBeCloseTo(1.5);
    expect(c!.wing2[0]).toBeCloseTo(-side);
    expect(c!.wing2[2]).toBeCloseTo(1.5);
  });
});

describe('moveBoost', () => {
  it('rapids are dimmest, plunge/retract mid, cuts brightest', () => {
    expect(moveBoost('rapid')).toBe(0.5);
    expect(moveBoost('plunge')).toBe(0.85);
    expect(moveBoost('retract')).toBe(0.85);
    expect(moveBoost('cut')).toBe(1.15);
    expect(moveBoost('arc')).toBe(1.15);
    // Unknown kinds fall into the "brightest" default.
    expect(moveBoost('whatever')).toBe(1.15);
  });
});

describe('resolveSegmentColor', () => {
  const moveTint: Rgb = [0.2, 0.6, 1.0];
  const opColor: Rgb = [0.4, 0.5, 0.6];

  it('op_id 0 uses the move tint verbatim', () => {
    expect(resolveSegmentColor(0, 'cut', moveTint, opColor)).toEqual([0.2, 0.6, 1.0]);
    // Move kind is irrelevant when unstamped.
    expect(resolveSegmentColor(0, 'rapid', moveTint, opColor)).toEqual([0.2, 0.6, 1.0]);
  });

  it('a stamped op scales its hue color by the move boost', () => {
    const [r, g, b] = resolveSegmentColor(3, 'cut', moveTint, opColor);
    expect(r).toBeCloseTo(0.4 * 1.15);
    expect(g).toBeCloseTo(0.5 * 1.15);
    expect(b).toBeCloseTo(0.6 * 1.15);
    // Rapid dims the same op color.
    const rapid = resolveSegmentColor(3, 'rapid', moveTint, opColor);
    expect(rapid[0]).toBeCloseTo(0.4 * 0.5);
  });
});

describe('fadeColor', () => {
  const base: Rgb = [0.8, 0.4, 0.2];
  it('past moves keep the full base color', () => {
    expect(fadeColor(base, true, 0.25, 0.05)).toEqual([0.8, 0.4, 0.2]);
  });
  it('future moves dim to base*factor + offset (visible floor, not black)', () => {
    const [r, g, b] = fadeColor(base, false, 0.25, 0.05);
    expect(r).toBeCloseTo(0.8 * 0.25 + 0.05);
    expect(g).toBeCloseTo(0.4 * 0.25 + 0.05);
    expect(b).toBeCloseTo(0.2 * 0.25 + 0.05);
    // A black base still floors at the offset so it's not invisible.
    expect(fadeColor([0, 0, 0], false, 0.25, 0.05)).toEqual([0.05, 0.05, 0.05]);
  });
});
