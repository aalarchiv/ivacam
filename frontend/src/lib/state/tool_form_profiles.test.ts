import { describe, it, expect } from 'vitest';
import { dovetailProfile, tslotProfile } from './tool_form_profiles';

describe('dovetailProfile', () => {
  it('tapers inward from tip radius by the flank angle over the height', () => {
    // 12.7 mm dia (r=6.35), 14° flank, 9.5 mm tall.
    const p = dovetailProfile({ diaMm: 12.7, angleDeg: 14, heightMm: 9.5 });
    expect(p).toHaveLength(2);
    expect(p[0]).toEqual({ zMm: 0, rMm: 6.35 });
    expect(p[1].zMm).toBe(9.5);
    // rTop = 6.35 - 9.5*tan(14°) = 6.35 - 2.3686… = 3.981 (round3).
    expect(p[1].rMm).toBeCloseTo(3.981, 3);
    expect(p[1].rMm).toBeLessThan(p[0].rMm);
  });

  it('clamps a negative top radius to zero for steep/short cutters', () => {
    const p = dovetailProfile({ diaMm: 2, angleDeg: 45, heightMm: 10 });
    expect(p[1].rMm).toBe(0);
  });
});

describe('tslotProfile', () => {
  it('emits a wide head disk then a narrow neck', () => {
    const p = tslotProfile({ headDiaMm: 12.7, headThickMm: 3, neckDiaMm: 6, neckLenMm: 6 });
    expect(p).toHaveLength(4);
    expect(p[0]).toEqual({ zMm: 0, rMm: 6.35 });
    expect(p[1]).toEqual({ zMm: 3, rMm: 6.35 });
    expect(p[2]).toEqual({ zMm: 3, rMm: 3 });
    expect(p[3]).toEqual({ zMm: 9, rMm: 3 });
  });

  it('clamps the neck radius to the head radius', () => {
    const p = tslotProfile({ headDiaMm: 6, headThickMm: 2, neckDiaMm: 20, neckLenMm: 4 });
    // neck (r=10) would exceed the head (r=3) → clamped to head radius.
    expect(p[2].rMm).toBe(3);
    expect(p[3].rMm).toBe(3);
  });
});
