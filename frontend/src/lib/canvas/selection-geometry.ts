// Pure-TypeScript 2D geometry primitives for the entity canvas. Lives
// outside `EntityCanvas2D.svelte` so vitest specs can exercise them
// without mounting the canvas DOM (audit y0ez). The selection / hit-
// test machinery in the component consumes these.

import type { Segment } from '../api/types';

export interface Point2D {
  x: number;
  y: number;
}

export interface BBox2D {
  min_x: number;
  min_y: number;
  max_x: number;
  max_y: number;
}

/// Bound `v` to `[lo, hi]`. Functionally equivalent to `Math.min(hi,
/// Math.max(lo, v))` but skips the comparison when already in range.
export function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

/// Euclidean distance from point `(px, py)` to the line segment between
/// `a` and `b`. Used by the canvas hit-test to pick the nearest segment
/// under a click within the pixel-tolerance circle.
export function distanceToSegment(a: Point2D, b: Point2D, px: number, py: number): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const lenSq = dx * dx + dy * dy;
  if (lenSq < 1e-12) return Math.hypot(px - a.x, py - a.y);
  let t = ((px - a.x) * dx + (py - a.y) * dy) / lenSq;
  t = clamp(t, 0, 1);
  const ix = a.x + t * dx;
  const iy = a.y + t * dy;
  return Math.hypot(px - ix, py - iy);
}

/// Project `(px, py)` onto the line segment between `a` and `b`,
/// clamping to the segment endpoints. Returns the projected point.
export function projectOntoSegment(a: Point2D, b: Point2D, px: number, py: number): Point2D {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const lenSq = dx * dx + dy * dy;
  if (lenSq < 1e-12) return { x: a.x, y: a.y };
  const t = clamp(((px - a.x) * dx + (py - a.y) * dy) / lenSq, 0, 1);
  return { x: a.x + t * dx, y: a.y + t * dy };
}

/// Even-odd point-in-polygon test on a closed polygon `verts` (no
/// implicit closing vertex needed). Returns `false` for degenerate
/// polygons with fewer than 3 vertices.
export function pointInPolygon(verts: [number, number][], px: number, py: number): boolean {
  if (verts.length < 3) return false;
  let inside = false;
  const n = verts.length;
  let j = n - 1;
  for (let i = 0; i < n; i++) {
    const [pix, piy] = verts[i];
    const [pjx, pjy] = verts[j];
    const crosses = piy > py !== pjy > py;
    if (crosses) {
      const xAt = pix + ((py - piy) * (pjx - pix)) / (pjy - piy);
      if (px < xAt) inside = !inside;
    }
    j = i;
  }
  return inside;
}

/// Liang-Barsky line-vs-AABB clip — returns `true` when the closed
/// segment `[p0, p1]` enters or touches the axis-aligned bbox. Used by
/// the canvas series-select (Shift+click) to sweep every object the
/// imaginary anchor→target line crosses.
export function lineCrossesBBox(p0: Point2D, p1: Point2D, b: BBox2D): boolean {
  const dx = p1.x - p0.x;
  const dy = p1.y - p0.y;
  let tMin = 0;
  let tMax = 1;
  if (Math.abs(dx) < 1e-12) {
    if (p0.x < b.min_x || p0.x > b.max_x) return false;
  } else {
    let t1 = (b.min_x - p0.x) / dx;
    let t2 = (b.max_x - p0.x) / dx;
    if (t1 > t2) [t1, t2] = [t2, t1];
    tMin = Math.max(tMin, t1);
    tMax = Math.min(tMax, t2);
    if (tMin > tMax) return false;
  }
  if (Math.abs(dy) < 1e-12) {
    if (p0.y < b.min_y || p0.y > b.max_y) return false;
  } else {
    let t1 = (b.min_y - p0.y) / dy;
    let t2 = (b.max_y - p0.y) / dy;
    if (t1 > t2) [t1, t2] = [t2, t1];
    tMin = Math.max(tMin, t1);
    tMax = Math.min(tMax, t2);
    if (tMin > tMax) return false;
  }
  return true;
}

/// Bounding box of a segment list using segment endpoints (ignores arc
/// bow — same convention as the Rust `BBox::from_segments` does for
/// open polylines, which is the standard "Hausdorff approximation"
/// good enough for click-region hit-testing). Returns a zero-extent
/// bbox when the list is empty.
export function bboxOfSegments(segs: Segment[]): BBox2D {
  if (segs.length === 0) return { min_x: 0, min_y: 0, max_x: 0, max_y: 0 };
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const s of segs) {
    if (s.start.x < minX) minX = s.start.x;
    if (s.start.y < minY) minY = s.start.y;
    if (s.start.x > maxX) maxX = s.start.x;
    if (s.start.y > maxY) maxY = s.start.y;
    if (s.end.x < minX) minX = s.end.x;
    if (s.end.y < minY) minY = s.end.y;
    if (s.end.x > maxX) maxX = s.end.x;
    if (s.end.y > maxY) maxY = s.end.y;
  }
  return { min_x: minX, min_y: minY, max_x: maxX, max_y: maxY };
}

/// Bottom-left (min) corner of the union of the bboxes of the objects
/// in `selectedIds`, read from the import's per-object `object_meta`
/// (245i). This is the "0,0 of the selection bbox" anchor used to place
/// a freshly-added text op at the selection's origin. Returns `null`
/// when no selected object has a finite bbox (empty selection, or ids
/// that don't resolve to a meta entry). Pure so it's unit-testable
/// without the canvas / rune runtime.
export function selectionOrigin(
  objectMeta: readonly { id: number; bbox: BBox2D }[],
  selectedIds: ReadonlySet<number>,
): Point2D | null {
  if (selectedIds.size === 0) return null;
  let minX = Infinity;
  let minY = Infinity;
  let found = false;
  for (const m of objectMeta) {
    if (!selectedIds.has(m.id)) continue;
    const b = m.bbox;
    if (!Number.isFinite(b.min_x) || !Number.isFinite(b.min_y)) continue;
    if (b.min_x < minX) minX = b.min_x;
    if (b.min_y < minY) minY = b.min_y;
    found = true;
  }
  return found ? { x: minX, y: minY } : null;
}
