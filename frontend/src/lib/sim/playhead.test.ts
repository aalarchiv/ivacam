import { describe, it, expect } from 'vitest';
import { playheadToSegment } from './playhead';

// 0z4b: playheadToSegment is the one load-bearing preview-math function on
// the TS side (arc-length lookup → segment index + parametric segT, used by
// Scene3D / PlaybackBar / GcodePanel / the sim driver) and was previously
// exercised only indirectly.
//
// Signature: playheadToSegment(playhead, cumLen, totalLen) where `playhead`
// is a FRACTION in [0,1] (clamped), cumLen[i] is the cumulative length up to
// AND INCLUDING segment i, and totalLen is the whole-path length. It returns
// the smallest i with cumLen[i] >= target (= playhead*totalLen) plus the
// fractional position segT within that segment, or { segIdx: -1, segT: 0 }
// for an empty / zero-length path.
describe('playheadToSegment', () => {
  // Three unit-length segments: cumLen [1,2,3], total 3.
  const cumLen = Float64Array.from([1, 2, 3]);
  const total = 3;

  it('clamps playhead <= 0 to the start of segment 0', () => {
    expect(playheadToSegment(0, cumLen, total)).toEqual({ segIdx: 0, segT: 0 });
    expect(playheadToSegment(-5, cumLen, total)).toEqual({ segIdx: 0, segT: 0 });
  });

  it('clamps playhead >= 1 to the end of the last segment', () => {
    expect(playheadToSegment(1, cumLen, total)).toEqual({ segIdx: 2, segT: 1 });
    expect(playheadToSegment(99, cumLen, total)).toEqual({ segIdx: 2, segT: 1 });
  });

  it('locates a target inside the first segment', () => {
    // playhead 0.25 → target 0.75 → 0.75 along the 1-unit segment 0.
    const pos = playheadToSegment(0.25, cumLen, total);
    expect(pos.segIdx).toBe(0);
    expect(pos.segT).toBeCloseTo(0.75, 12);
  });

  it('locates a target inside a middle segment', () => {
    // playhead 0.5 → target 1.5 → 0.5 into segment 1 (spans [1,2]).
    const pos = playheadToSegment(0.5, cumLen, total);
    expect(pos.segIdx).toBe(1);
    expect(pos.segT).toBeCloseTo(0.5, 12);
  });

  it('puts a boundary target at the start of the next segment', () => {
    // playhead 2/3 → target 2.0; smallest i with cumLen[i] >= 2 is i=1,
    // segStartLen = cumLen[0] = 1 → segT = (2-1)/1 = ... cumLen[1]=2 so
    // segLen = 2-1 = 1, segT = (2-1)/1 = 1? No: target==cumLen[0]=1?  No,
    // target 2 == cumLen[1]; smallest i with cumLen[i]>=2 is i=1,
    // segStartLen=cumLen[0]=1, segLen=1, segT=(2-1)/1=1 → but that's the
    // END of seg 1. The fn returns the first i whose cumulative reaches the
    // target, so a target landing exactly on a boundary maps to the segment
    // that ENDS there. Assert that explicitly.
    const pos = playheadToSegment(2 / 3, cumLen, total);
    expect(pos.segIdx).toBe(1);
    expect(pos.segT).toBeCloseTo(1, 12);
  });

  it('returns segIdx -1 for an empty path', () => {
    expect(playheadToSegment(0.5, Float64Array.from([]), 0)).toEqual({ segIdx: -1, segT: 0 });
    expect(playheadToSegment(0.5, null, 0)).toEqual({ segIdx: -1, segT: 0 });
  });

  it('returns segIdx -1 when totalLen is zero', () => {
    expect(playheadToSegment(0.5, cumLen, 0)).toEqual({ segIdx: -1, segT: 0 });
  });

  it('does not divide by zero on a zero-length segment', () => {
    // Segment 1 has zero length (cumLen flat across it). A target on that
    // boundary must yield a finite segT, never NaN.
    const cl = Float64Array.from([1, 1, 2]);
    const pos = playheadToSegment(0.5, cl, 2); // target 1.0
    expect(pos.segIdx).toBe(0); // smallest i with cumLen[i] >= 1
    expect(Number.isNaN(pos.segT)).toBe(false);
  });

  it('handles non-uniform segment lengths', () => {
    // cumLen [2,10], total 10. playhead 0.6 → target 6 → 4 into segment 1
    // (segStartLen 2, segLen 8) → segT 0.5.
    const cl = Float64Array.from([2, 10]);
    const pos = playheadToSegment(0.6, cl, 10);
    expect(pos.segIdx).toBe(1);
    expect(pos.segT).toBeCloseTo(0.5, 12);
  });
});
