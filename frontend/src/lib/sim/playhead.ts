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

/// Per-frame plan emitted by the heightfield driver's state machine to
/// move the simulator from `(appliedSeg, partialT)` to a target playhead
/// `(targetSeg, targetT)` (pi8r). Encoded as a small object so the
/// driver dispatches one Sim call per non-null field plus updates its
/// own state, and so vitest can assert call sequence without booting
/// the WASM module.
export interface AdvancePlan {
  /// Whether to reset the simulator and replay forward before applying
  /// any other ops. True iff the target lies strictly before the
  /// current state in lexicographic `(seg, t)` order.
  reset: boolean;
  /// Close out the in-flight partial segment (`partial_advance(seg,
  /// fromT, 1)`) before bulk-advancing past it.
  finalizePartial: { segIdx: number; fromT: number } | null;
  /// Whole-segment bulk catch-up. Includes segments `[from, to)`.
  bulkAdvance: { from: number; to: number } | null;
  /// Start a new partial slice in the target segment, carving
  /// `[startT, endT]`. Driver issues this as `partial_advance(seg,
  /// startT, endT)`.
  startPartial: { segIdx: number; startT: number; endT: number } | null;
  /// New `(appliedSeg, partialT)` state to record after applying the
  /// plan. Already normalized (snapped past `partialT==1` boundaries
  /// when not on the terminal segment).
  newAppliedSeg: number;
  newPartialT: number;
}

const PLAN_NOOP: AdvancePlan = {
  reset: false,
  finalizePartial: null,
  bulkAdvance: null,
  startPartial: null,
  newAppliedSeg: -1,
  newPartialT: 0,
};

/// Pure planner for the heightfield driver. Given the current
/// `(appliedSeg, partialT)` state and a target `(targetSeg, targetT)`,
/// returns the operations the driver should issue and the new state.
/// Returns `null` when the move is a no-op.
///
/// Invariants enforced:
///   * `[0, appliedSeg)` is bulk-carved.
///   * Segment `appliedSeg` is carved up to `partialT ∈ [0, 1]`.
///   * `(appliedSeg, partialT)` snaps from `(N, 1)` to `(N+1, 0)`
///     when `N < total - 1`, so the terminal `(last, 1)` is the only
///     `partialT == 1` resting state.
export function planAdvance(
  appliedSeg: number,
  partialT: number,
  targetSeg: number,
  targetT: number,
  total: number,
): AdvancePlan | null {
  if (total === 0) return null;
  if (targetSeg === appliedSeg && targetT === partialT) return null;

  const backward =
    targetSeg < appliedSeg || (targetSeg === appliedSeg && targetT < partialT);

  let curSeg = backward ? 0 : appliedSeg;
  let curT = backward ? 0 : partialT;

  const plan: AdvancePlan = { ...PLAN_NOOP, reset: backward };

  if (targetSeg > curSeg) {
    if (curT > 0 && curT < 1) {
      plan.finalizePartial = { segIdx: curSeg, fromT: curT };
      curSeg += 1;
      curT = 0;
    }
    if (targetSeg > curSeg) {
      plan.bulkAdvance = { from: curSeg, to: targetSeg };
      curSeg = targetSeg;
    }
  }

  if (targetSeg === curSeg && targetT > curT) {
    plan.startPartial = { segIdx: curSeg, startT: curT, endT: targetT };
    curT = targetT;
  }

  // Snap `(N, 1)` → `(N+1, 0)` for non-terminal boundaries so the next
  // forward call won't re-carve the just-finished segment.
  if (curT >= 1 && curSeg < total - 1) {
    curSeg += 1;
    curT = 0;
  }

  plan.newAppliedSeg = curSeg;
  plan.newPartialT = curT;
  return plan;
}
