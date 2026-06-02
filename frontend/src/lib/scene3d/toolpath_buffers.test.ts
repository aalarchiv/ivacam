import { describe, expect, it } from 'vitest';
import { computeArrowChevron, arrowSpacingMm, type ArrowParams } from './toolpath_buffers';

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
