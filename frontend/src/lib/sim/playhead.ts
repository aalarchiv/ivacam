/// Playhead → segment-index conversion. Extracted from
/// project.svelte.ts so vitest can import it without booting the Svelte
/// rune runtime.

/// Map `playhead ∈ [0,1]` (fraction of total arc length) to a segment
/// index + parametric position within that segment. Returns
/// `{ segIdx, segT }` where `segT ∈ [0,1]` is the fractional distance
/// along segment `segIdx`. Returns `{ segIdx: -1, segT: 0 }` when the
/// toolpath is empty or there is no length to traverse.
///
/// Arc-length-based mapping is what makes playback feel uniform: a
/// 50 mm boundary edge takes ~33× longer than a 1.5 mm zigzag connector
/// at the same `speed`, instead of both consuming `1/total_segments`
/// of playback time.
export function playheadToSegment(
  playhead: number,
  cumLen: Float64Array | null,
  totalLen: number,
): { segIdx: number; segT: number } {
  if (!cumLen || cumLen.length === 0 || totalLen <= 0) {
    return { segIdx: -1, segT: 0 };
  }
  const clamped = Math.max(0, Math.min(1, playhead));
  const target = clamped * totalLen;
  // Binary search for the smallest i where cumLen[i] >= target.
  let lo = 0;
  let hi = cumLen.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (cumLen[mid] < target) lo = mid + 1;
    else hi = mid;
  }
  const segEndLen = cumLen[lo];
  const segStartLen = lo === 0 ? 0 : cumLen[lo - 1];
  const segLen = segEndLen - segStartLen;
  const segT = segLen > 1e-12 ? (target - segStartLen) / segLen : 0;
  return { segIdx: lo, segT };
}
