import { describe, expect, it } from 'vitest';
import { tessellate } from './tessellate';

describe('tessellate', () => {
  it('returns just the start for a POINT', () => {
    expect(
      tessellate({
        type: 'POINT',
        start: { x: 5, y: 7 },
        end: { x: 5, y: 7 },
        bulge: 0,
      }),
    ).toEqual([[5, 7]]);
  });

  it('returns chord endpoints for a straight LINE (zero bulge)', () => {
    expect(
      tessellate({
        type: 'LINE',
        start: { x: 0, y: 0 },
        end: { x: 10, y: 5 },
        bulge: 0,
      }),
    ).toEqual([
      [0, 0],
      [10, 5],
    ]);
  });

  it('returns the start point only when chord is degenerate', () => {
    expect(
      tessellate({
        type: 'ARC',
        start: { x: 3, y: 3 },
        end: { x: 3, y: 3 },
        bulge: 0.5,
      }),
    ).toEqual([[3, 3]]);
  });

  it('densifies a half-circle bulge into ≥9 points', () => {
    const pts = tessellate({
      type: 'ARC',
      start: { x: 0, y: 0 },
      end: { x: 10, y: 0 },
      bulge: 1, // half-circle
    });
    // ≤10° per step ⇒ 180° / 10° = 18 segments → 19 points. The
    // min-8 floor doesn't raise that.
    expect(pts.length).toBeGreaterThanOrEqual(9);
    // First / last points sit on start / end (within floating-point).
    expect(pts[0][0]).toBeCloseTo(0, 9);
    expect(pts[0][1]).toBeCloseTo(0, 9);
    const last = pts[pts.length - 1];
    expect(last[0]).toBeCloseTo(10, 9);
    expect(last[1]).toBeCloseTo(0, 9);
  });

  it('flips arc bow side with bulge sign', () => {
    const pos = tessellate({
      type: 'ARC',
      start: { x: 0, y: 0 },
      end: { x: 10, y: 0 },
      bulge: 1,
    });
    const neg = tessellate({
      type: 'ARC',
      start: { x: 0, y: 0 },
      end: { x: 10, y: 0 },
      bulge: -1,
    });
    // Apex of each arc must land on opposite sides of the chord
    // (whichever side each picks — what matters is they differ).
    const pos_apex_y = pos[Math.floor(pos.length / 2)][1];
    const neg_apex_y = neg[Math.floor(neg.length / 2)][1];
    expect(Math.sign(pos_apex_y)).not.toBe(Math.sign(neg_apex_y));
  });
});
