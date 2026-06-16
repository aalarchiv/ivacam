/// Windowed-virtualization math for the G-code panel, extracted as pure
/// functions so vitest can cover the index arithmetic without the rune
/// runtime or a real scroll container.
///
/// The panel is a flat sequence of variable-height items (gcode rows +
/// the occasional chapter header). We keep a cumulative-offset array and
/// binary-search it per scroll frame, so only the handful of items in the
/// viewport (plus an overscan margin) are ever rendered — turning the
/// mount/scroll cost from O(lines) into O(visible).

export interface VirtualWindow {
  /// First item index to render (inclusive).
  first: number;
  /// Last item index to render (inclusive). `-1` when there are no items.
  last: number;
  /// Spacer height (px) standing in for the items above `first`.
  padTop: number;
  /// Spacer height (px) standing in for the items below `last`.
  padBottom: number;
}

/// `offsets` is the cumulative item-height array: `offsets[i]` is the
/// pixel top of item `i` and `offsets[count]` is the total content
/// height. Returns the inclusive item range overlapping
/// `[scrollTop, scrollTop + viewportH]`, grown by `overscan` items on
/// each side, plus the top/bottom spacer heights that preserve the
/// scrollbar extent.
export function computeWindow(
  offsets: ArrayLike<number>,
  count: number,
  scrollTop: number,
  viewportH: number,
  overscan: number,
): VirtualWindow {
  if (count <= 0) return { first: 0, last: -1, padTop: 0, padBottom: 0 };

  const total = offsets[count];
  const top = clamp(scrollTop, 0, total);
  const bottom = Math.min(total, top + Math.max(0, viewportH));

  // First item whose bottom edge (offsets[i+1]) is strictly past `top`.
  let first = firstItemEndingAfter(offsets, count, top);
  // Last item whose top edge (offsets[i]) is before `bottom`.
  let last = lastItemStartingBefore(offsets, count, bottom);

  first = Math.max(0, first - overscan);
  last = Math.min(count - 1, last + overscan);
  // Degenerate viewport (e.g. height not measured yet): still render the
  // landing item so the panel is never blank.
  if (last < first) last = first;

  return {
    first,
    last,
    padTop: offsets[first],
    padBottom: total - offsets[last + 1],
  };
}

/// Smallest `i ∈ [0, count)` with `offsets[i + 1] > y`, i.e. the first
/// item whose bottom edge lies past `y`. `count - 1` if none.
function firstItemEndingAfter(offsets: ArrayLike<number>, count: number, y: number): number {
  let lo = 0;
  let hi = count - 1;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (offsets[mid + 1] > y) hi = mid;
    else lo = mid + 1;
  }
  return lo;
}

/// Largest `i ∈ [0, count)` with `offsets[i] < y`, i.e. the last item
/// whose top edge lies before `y`. `0` if none.
function lastItemStartingBefore(offsets: ArrayLike<number>, count: number, y: number): number {
  let lo = 0;
  let hi = count - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >>> 1;
    if (offsets[mid] < y) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

/// Cumulative pixel offsets for one-item-per-gcode-line virtualization.
/// Each line is `rowH` tall; a line that starts a chapter also carries
/// the `chapterH` header stacked above it (rendered together). Returns a
/// `Float64Array` of length `count + 1` where entry `i` is the pixel top
/// of line `i` and the final entry is the total content height.
///
/// `chapterStart[i]` is truthy when line `i` (0-based) begins a chapter.
export function buildRowOffsets(
  chapterStart: ArrayLike<number>,
  rowH: number,
  chapterH: number,
): Float64Array {
  const n = chapterStart.length;
  const offsets = new Float64Array(n + 1);
  for (let i = 0; i < n; i++) {
    offsets[i + 1] = offsets[i] + rowH + (chapterStart[i] ? chapterH : 0);
  }
  return offsets;
}
