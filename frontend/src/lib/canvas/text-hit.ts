// Precise stroke-distance hit-test for text layers on the 2D canvas.
// A text layer's bbox is mostly whitespace, so selecting/grabbing it
// by bbox would steal clicks meant for geometry around the glyphs.
// Instead we measure the distance to the rendered glyph STROKES (the
// same `previewSegmentsFor` segments drawn on the canvas) and only hit
// when the cursor is within the pixel tolerance of an actual stroke.
//
// Pure + unit-tested (the canvas component supplies the per-layer
// segments + the data-space tolerance; this module owns the geometry).

import { distanceToSegment } from './selection-geometry';
import type { Segment } from '../api/types';

/// A text layer reduced to what the hit-test needs: its id + the
/// rendered stroke segments.
export interface TextHitLayer {
  id: number;
  segments: readonly Segment[];
}

/// True min distance from `(x, y)` to any stroke in `segs`. `Infinity`
/// for an empty list.
export function distanceToSegments(segs: readonly Segment[], x: number, y: number): number {
  let best = Infinity;
  for (const s of segs) {
    const d = distanceToSegment(s.start, s.end, x, y);
    if (d < best) best = d;
  }
  return best;
}

/// Axis-aligned bounds over the segment endpoints, or `null` when empty.
function segsBounds(
  segs: readonly Segment[],
): { minX: number; minY: number; maxX: number; maxY: number } | null {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const s of segs) {
    minX = Math.min(minX, s.start.x, s.end.x);
    minY = Math.min(minY, s.start.y, s.end.y);
    maxX = Math.max(maxX, s.start.x, s.end.x);
    maxY = Math.max(maxY, s.start.y, s.end.y);
  }
  return Number.isFinite(minX) ? { minX, minY, maxX, maxY } : null;
}

/// The text layer whose nearest stroke is closest to `(x, y)`, when that
/// distance is within `tolData` (data-space units). Returns `{ id, dist }`
/// or `null` when nothing is within tolerance. A cheap bbox reject
/// (expanded by `tolData`) skips the per-stroke scan for far layers.
/// Ties resolve to the LAST-listed layer (callers pass layers in draw
/// order, so the topmost wins — matching what the user sees).
export function nearestTextLayer(
  layers: readonly TextHitLayer[],
  x: number,
  y: number,
  tolData: number,
): { id: number; dist: number } | null {
  let bestId: number | null = null;
  let bestDist = Infinity;
  for (const layer of layers) {
    const bb = segsBounds(layer.segments);
    if (!bb) continue;
    if (
      x < bb.minX - tolData ||
      x > bb.maxX + tolData ||
      y < bb.minY - tolData ||
      y > bb.maxY + tolData
    ) {
      continue;
    }
    const d = distanceToSegments(layer.segments, x, y);
    // `<=` so a later (topmost) layer wins an exact tie.
    if (d <= bestDist && d <= tolData) {
      bestDist = d;
      bestId = layer.id;
    }
  }
  return bestId == null ? null : { id: bestId, dist: bestDist };
}
