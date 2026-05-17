/// Tests for the entity-canvas spatial index — buildHitIndex /
/// queryHit. The component (`EntityCanvas2D.svelte`) delegates to
/// these for every mousemove, so the unit tests cover the bbox cell
/// math, dedup across cell boundaries, layer-visibility filtering,
/// the `null`-index fallback, and the empty-input edge cases.

import { describe, expect, it } from 'vitest';
import { buildHitIndex, queryHit, type SpatialSource } from './spatial-index';

function source(segments: SpatialSource['segments']): SpatialSource {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const s of segments) {
    minX = Math.min(minX, s.start.x, s.end.x);
    minY = Math.min(minY, s.start.y, s.end.y);
    maxX = Math.max(maxX, s.start.x, s.end.x);
    maxY = Math.max(maxY, s.start.y, s.end.y);
  }
  if (segments.length === 0) {
    minX = 0;
    minY = 0;
    maxX = 0;
    maxY = 0;
  }
  return { bbox: { min_x: minX, min_y: minY, max_x: maxX, max_y: maxY }, segments };
}

const allLayers = () => true;

describe('buildHitIndex', () => {
  it('returns null for empty / missing input', () => {
    expect(buildHitIndex(null)).toBeNull();
    expect(buildHitIndex(undefined)).toBeNull();
    expect(buildHitIndex(source([]))).toBeNull();
  });

  it('uses the minimum cell count (8x8) for tiny inputs', () => {
    // One short segment in a 100×100 bbox — the AABB-based bbox is
    // 100×100, the segment occupies one diagonal cell. (The bbox
    // would normally come from project.imported; we widen it here to
    // test the cell-population logic, not bbox-of-segments.)
    const data: SpatialSource = {
      bbox: { min_x: 0, min_y: 0, max_x: 100, max_y: 100 },
      segments: [{ start: { x: 0, y: 0 }, end: { x: 10, y: 10 }, layer: '0' }],
    };
    const idx = buildHitIndex(data)!;
    expect(idx.cols).toBe(8);
    expect(idx.rows).toBe(8);
    // 64 cells, segment touches the lower-left diagonal cells only.
    const populated = idx.cells.filter((c) => c).length;
    expect(populated).toBeGreaterThan(0);
    expect(populated).toBeLessThan(8);
  });

  it('targets ~sqrt(N) cells per side and caps at 128', () => {
    // 200 segments → target = floor(sqrt(200)) = 14.
    const segs = Array.from({ length: 200 }, (_, i) => ({
      start: { x: i, y: 0 },
      end: { x: i, y: 1 },
      layer: '0',
    }));
    const idx = buildHitIndex(source(segs))!;
    expect(idx.cols).toBe(14);

    // 100k segments → would target 316; capped at 128. Use distinct
    // bboxes so each segment lands in a single cell (otherwise
    // build cost grows N × cells_per_seg and the test takes minutes
    // when every segment crosses every cell).
    const huge = Array.from({ length: 100_000 }, (_, i) => ({
      start: { x: i % 1000, y: Math.floor(i / 1000) },
      end: { x: (i % 1000) + 0.01, y: Math.floor(i / 1000) + 0.01 },
      layer: '0',
    }));
    const bigIdx = buildHitIndex(source(huge))!;
    expect(bigIdx.cols).toBe(128);
  });

  it('places each segment in every cell its AABB touches', () => {
    // 10×10 bbox, ~3 cells per side (sqrt(8) ≈ 2.83 floor → 8 min)
    const data = source([
      // Horizontal line spanning multiple cells
      { start: { x: 0, y: 5 }, end: { x: 10, y: 5 }, layer: '0' },
    ]);
    const idx = buildHitIndex(data)!;
    // Count distinct cells the segment landed in by collecting all
    // unique seg indices across cells.
    const cellsTouchedBySeg0 = idx.cells.filter((c) => c?.includes(0)).length;
    // 8-cell grid spanning Y, the line at y=5 lands in one row of
    // cells (8 columns).
    expect(cellsTouchedBySeg0).toBe(8);
  });
});

describe('queryHit', () => {
  it('returns null when there is no geometry', () => {
    expect(queryHit(null, null, 0, 0, 1, allLayers)).toBeNull();
    expect(queryHit(source([]), null, 0, 0, 1, allLayers)).toBeNull();
  });

  it('finds the closest segment within tolerance', () => {
    const data = source([
      { start: { x: 0, y: 0 }, end: { x: 10, y: 0 }, layer: '0' },
      { start: { x: 0, y: 5 }, end: { x: 10, y: 5 }, layer: '0' },
    ]);
    const idx = buildHitIndex(data);
    // Cursor at (5, 0.2): seg 0 is 0.2 away, seg 1 is 4.8 away.
    expect(queryHit(data, idx, 5, 0.2, 1.0, allLayers)).toBe(0);
    // Cursor at (5, 4.8): seg 1 wins.
    expect(queryHit(data, idx, 5, 4.8, 1.0, allLayers)).toBe(1);
    // Cursor at (5, 2.5): both 2.5 away, but the smaller distance
    // wins — they tie exactly so the iteration order picks seg 0
    // since it gets checked first (insertion order in the grid).
    // Use a tolerance under 2.5 to assert "neither in range".
    expect(queryHit(data, idx, 5, 2.5, 0.5, allLayers)).toBeNull();
  });

  it('filters out hidden layers via the predicate', () => {
    const data = source([
      { start: { x: 0, y: 0 }, end: { x: 10, y: 0 }, layer: 'hidden' },
      { start: { x: 0, y: 0.5 }, end: { x: 10, y: 0.5 }, layer: 'visible' },
    ]);
    const idx = buildHitIndex(data);
    const onlyVisible = (l: string) => l === 'visible';
    // Even though `hidden` seg 0 is closer to (5, 0), the predicate
    // skips it and seg 1 wins.
    expect(queryHit(data, idx, 5, 0, 1.0, onlyVisible)).toBe(1);
    // Drop visibility entirely → no hit.
    expect(queryHit(data, idx, 5, 0, 1.0, () => false)).toBeNull();
  });

  it('falls back to a linear scan when the index is null', () => {
    const data = source([
      { start: { x: 0, y: 0 }, end: { x: 10, y: 0 }, layer: '0' },
    ]);
    expect(queryHit(data, null, 5, 0.1, 1.0, allLayers)).toBe(0);
    // Out of tolerance → null even on the linear path.
    expect(queryHit(data, null, 5, 5, 1.0, allLayers)).toBeNull();
  });

  it('dedups segments that land in multiple cells', () => {
    // A diagonal seg crossing cell boundaries: only one hit
    // returned, not one-per-cell.
    const data = source([
      { start: { x: 0, y: 0 }, end: { x: 100, y: 100 }, layer: '0' },
    ]);
    const idx = buildHitIndex(data);
    // Cursor on the line — the seg lives in many cells but the
    // result is still a single index, not a list.
    expect(queryHit(data, idx, 50, 50, 0.5, allLayers)).toBe(0);
  });
});
