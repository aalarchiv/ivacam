/// Pure spatial index for fast point-vs-segments hit testing on the
/// 2D entity canvas. Bbox-cell bucket grid: each segment lives in
/// every cell its AABB touches. Queries probe only the cells
/// overlapping (cursor ± tolerance) and bail early past that.
///
/// Build cost amortises across many `queryHit` calls: a 5 k-segment
/// DXF builds the index once at import time and every mousemove
/// drops from O(N) to O(cells_touched * segs_per_cell), typically
/// 10-50 candidates regardless of N.
///
/// Pure-TS, no DOM, no `$state` — vitest covers it without mounting
/// the canvas (audit y0ez).
//
// Casts to (number) are deliberate — `Math.floor((dataX - minX) /
// cellW)` produces non-negative bounded integers (clamped to
// [0, cols-1]) before we use them as array indices.

import type { Segment } from '../api/types';
import { clamp, distanceToSegment, type BBox2D } from './selection-geometry';

export interface HitIndex {
  cellW: number;
  cellH: number;
  minX: number;
  minY: number;
  cols: number;
  rows: number;
  /// One `Uint32Array` per cell with the segment indices that
  /// touch it. Cells with zero hits stay `undefined` to keep
  /// memory bounded on sparse imports.
  cells: (Uint32Array | undefined)[];
}

export interface SpatialSource {
  bbox: BBox2D;
  segments: ReadonlyArray<Pick<Segment, 'start' | 'end' | 'layer'>>;
}

/// Build the bucket grid for `data`. Returns `null` when there's no
/// geometry — the caller can fall back to a linear scan (or, more
/// usefully, just bail because there's nothing to hit).
///
/// Cell count targets `sqrt(N)` per side, capped at [8, 128]: tiny
/// imports don't get a sparse grid (which would waste comparisons),
/// huge imports don't blow the memory budget on cell metadata.
export function buildHitIndex(data: SpatialSource | null | undefined): HitIndex | null {
  if (!data || data.segments.length === 0) return null;
  const { min_x, min_y, max_x, max_y } = data.bbox;
  const dx = Math.max(max_x - min_x, 1e-6);
  const dy = Math.max(max_y - min_y, 1e-6);
  const target = Math.min(128, Math.max(8, Math.floor(Math.sqrt(data.segments.length))));
  const cols = target;
  const rows = target;
  const cellW = dx / cols;
  const cellH = dy / rows;
  // Two-pass build: count buckets first, then write segment indices
  // into pre-sized Uint32Arrays. Avoids growing arrays per-insert.
  const counts = new Uint32Array(cols * rows);
  const segs = data.segments;
  const visit = (cb: (cellIdx: number, segIdx: number) => void) => {
    for (let i = 0; i < segs.length; i++) {
      const s = segs[i];
      const sxMin = Math.min(s.start.x, s.end.x);
      const syMin = Math.min(s.start.y, s.end.y);
      const sxMax = Math.max(s.start.x, s.end.x);
      const syMax = Math.max(s.start.y, s.end.y);
      const c0 = clamp(Math.floor((sxMin - min_x) / cellW), 0, cols - 1);
      const c1 = clamp(Math.floor((sxMax - min_x) / cellW), 0, cols - 1);
      const r0 = clamp(Math.floor((syMin - min_y) / cellH), 0, rows - 1);
      const r1 = clamp(Math.floor((syMax - min_y) / cellH), 0, rows - 1);
      for (let r = r0; r <= r1; r++) {
        for (let c = c0; c <= c1; c++) {
          cb(r * cols + c, i);
        }
      }
    }
  };
  visit((cellIdx) => {
    counts[cellIdx]++;
  });
  const cells: (Uint32Array | undefined)[] = new Array(cols * rows);
  const writeIdx = new Uint32Array(cols * rows);
  for (let i = 0; i < counts.length; i++) {
    if (counts[i] > 0) cells[i] = new Uint32Array(counts[i]);
  }
  visit((cellIdx, segIdx) => {
    const buf = cells[cellIdx]!;
    buf[writeIdx[cellIdx]++] = segIdx;
  });
  return { cellW, cellH, minX: min_x, minY: min_y, cols, rows, cells };
}

/// Find the segment in `data` closest to `(dataX, dataY)` within
/// `tolData` units. Returns its index in `data.segments`, or `null`
/// if nothing's in range. Uses the spatial index when supplied,
/// falls back to a linear scan otherwise (a transient state during
/// the canvas's initial mount before the `$effect` builds the
/// index).
///
/// `isLayerVisible` filters out segments the user has hidden — it
/// runs once per candidate, before the more expensive
/// `distanceToSegment`. Keeping the predicate as a callback (rather
/// than a Set) lets callers plug in arbitrary visibility rules
/// (layer-level, op-scope, etc.) without re-shaping the API.
export function queryHit(
  data: SpatialSource | null | undefined,
  index: HitIndex | null,
  dataX: number,
  dataY: number,
  tolData: number,
  isLayerVisible: (layer: string) => boolean,
): number | null {
  if (!data || data.segments.length === 0) return null;
  let bestIdx: number | null = null;
  let bestDist = Infinity;
  const segs = data.segments;
  if (index) {
    const { cellW, cellH, minX, minY, cols, rows, cells } = index;
    const c0 = clamp(Math.floor((dataX - tolData - minX) / cellW), 0, cols - 1);
    const c1 = clamp(Math.floor((dataX + tolData - minX) / cellW), 0, cols - 1);
    const r0 = clamp(Math.floor((dataY - tolData - minY) / cellH), 0, rows - 1);
    const r1 = clamp(Math.floor((dataY + tolData - minY) / cellH), 0, rows - 1);
    // Dedup across cells — a single segment can land in multiple
    // cells when its bbox crosses cell boundaries.
    const seen = new Set<number>();
    for (let r = r0; r <= r1; r++) {
      for (let c = c0; c <= c1; c++) {
        const buf = cells[r * cols + c];
        if (!buf) continue;
        for (let k = 0; k < buf.length; k++) {
          const i = buf[k];
          if (seen.has(i)) continue;
          seen.add(i);
          const s = segs[i];
          if (!isLayerVisible(s.layer)) continue;
          const d = distanceToSegment(s.start, s.end, dataX, dataY);
          if (d < tolData && d < bestDist) {
            bestIdx = i;
            bestDist = d;
          }
        }
      }
    }
    return bestIdx;
  }
  // Fallback for the rare case the index hasn't been built yet (first
  // mousemove during the initial mount before the $effect fires).
  for (let i = 0; i < segs.length; i++) {
    const s = segs[i];
    if (!isLayerVisible(s.layer)) continue;
    const d = distanceToSegment(s.start, s.end, dataX, dataY);
    if (d < tolData && d < bestDist) {
      bestIdx = i;
      bestDist = d;
    }
  }
  return bestIdx;
}
