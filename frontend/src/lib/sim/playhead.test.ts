import { describe, it, expect } from 'vitest';
import { playheadToSegment } from './playhead';

// 0z4b: playheadToSegment is the one load-bearing preview-math function on
// the TS side (arc-length binary search → segment index + parametric segT,
// used by Scene3D / PlaybackBar / GcodePanel / the sim driver). It was
// previously exercised only indirectly. cumLen[i] = total length up to AND
// including segment i; segLen[i] = length of segment i.
describe('playheadToSegment', () => {
  // Three unit-length segments: cumLen [1,2,3], segLen [1,1,1].
  const cumLen = [1, 2, 3];
  const segLen = [1, 1, 1];

  it('clamps target <= 0 to the start of segment 0', () => {
    expect(playheadToSegment(cumLen, segLen, 0)).toEqual({ segIdx: 0, segT: 0 });
    expect(playheadToSegment(cumLen, segLen, -5)).toEqual({ segIdx: 0, segT: 0 });
  });

  it('clamps target >= total to the end of the last segment', () => {
    expect(playheadToSegment(cumLen, segLen, 3)).toEqual({ segIdx: 2, segT: 1 });
    expect(playheadToSegment(cumLen, segLen, 99)).toEqual({ segIdx: 2, segT: 1 });
  });

  it('locates a target inside the first segment', () => {
    expect(playheadToSegment(cumLen, segLen, 0.25)).toEqual({ segIdx: 0, segT: 0.25 });
  });

  it('locates a target inside a middle segment', () => {
    // 1.5 mm → 0.5 into segment 1 (which spans [1, 2]).
    expect(playheadToSegment(cumLen, segLen, 1.5)).toEqual({ segIdx: 1, segT: 0.5 });
  });

  it('puts a target exactly on a boundary at the start of the next segment', () => {
    // Smallest i with cumLen[i] >= target: cumLen[1]=2 >= 2 → segIdx 1, segT 0.
    expect(playheadToSegment(cumLen, segLen, 2)).toEqual({ segIdx: 1, segT: 0 });
  });

  it('handles an empty path', () => {
    expect(playheadToSegment([], [], 1)).toEqual({ segIdx: 0, segT: 0 });
  });

  it('returns segT 0 for a zero-length segment instead of dividing by zero', () => {
    // Segment 1 has zero length (cumLen flat across it); target landing on it
    // must not produce NaN.
    const cl = [1, 1, 2];
    const sl = [1, 0, 1];
    const pos = playheadToSegment(cl, sl, 1);
    // Smallest i with cumLen[i] >= 1 is i=0 (cumLen[0]=1), segT = 1/1 = 1.
    expect(pos.segIdx).toBe(0);
    expect(Number.isNaN(pos.segT)).toBe(false);
  });

  it('handles non-uniform segment lengths', () => {
    // segLen [2, 8], cumLen [2, 10]. target 6 → 4 into segment 1 of length 8.
    const cl = [2, 10];
    const sl = [2, 8];
    expect(playheadToSegment(cl, sl, 6)).toEqual({ segIdx: 1, segT: 0.5 });
  });
});
