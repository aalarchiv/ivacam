/// File-level transform engine (bww). A non-destructive 2D transform
/// stored on the project that re-positions / re-orients / scales the
/// imported drawing on the stock. Backend primitives live in
/// `crates/wiac-core/src/cam.rs`; the same math is replicated here in
/// TypeScript so every consumer (canvas draw, OSnap precompute, 3D
/// scene, build-project payload, sim) sees the same geometry without
/// a server round trip.
///
/// Application order, single pivot = ORIGINAL file bbox center:
///   1. uniform scale around pivot
///   2. mirror X (negate Y around pivot, negate bulge)
///   3. mirror Y (negate X around pivot, negate bulge)
///   4. rotate around pivot
///   5. translate by (dx, dy) in world coords
///
/// Bulge: only mirrors flip its sign. Two mirrors cancel (the result is
/// a 180° rotation of the geometry — bulge unchanged).
///
/// The function returns the same `ImportResponse` reference when the
/// transform is identity so consumers that compare by reference can skip
/// downstream recomputation.

import type { BBox, ImportResponse, Point2, Segment } from '../api/types';
import {
  isIdentityFileTransform,
  type FileTransform,
} from './project-types';

/// Apply `t` to a full ImportResponse. Object ids, layers, object_meta,
/// warnings, unit_scale, format, filename — all unchanged. Only segments
/// and bbox are recomputed.
export function applyFileTransform(
  imp: ImportResponse,
  t: FileTransform,
): ImportResponse {
  if (isIdentityFileTransform(t)) return imp;
  const pivot = bboxCenter(imp.bbox);
  const transformedSegments = imp.segments.map((s) => transformSegment(s, t, pivot));
  return {
    ...imp,
    segments: transformedSegments,
    bbox: computeBbox(transformedSegments, imp.bbox),
  };
}

/// Apply the same transform to a single (x, y) world point. Used by the
/// canvas to map a click in transformed-space back to the right spot.
export function applyFileTransformToPoint(
  p: { x: number; y: number },
  t: FileTransform,
  bbox: BBox,
): { x: number; y: number } {
  if (isIdentityFileTransform(t)) return p;
  return transformPoint(p, t, bboxCenter(bbox));
}

function bboxCenter(b: BBox): Point2 {
  return { x: (b.min_x + b.max_x) / 2, y: (b.min_y + b.max_y) / 2 };
}

function transformSegment(s: Segment, t: FileTransform, pivot: Point2): Segment {
  const start = transformPoint(s.start, t, pivot);
  const end = transformPoint(s.end, t, pivot);
  const center = s.center ? transformPoint(s.center, t, pivot) : s.center;
  // Each mirror negates bulge; both mirrors cancel.
  const mirrors = (t.mirrorX ? 1 : 0) + (t.mirrorY ? 1 : 0);
  const bulge = mirrors % 2 === 1 ? -s.bulge : s.bulge;
  return { ...s, start, end, center, bulge };
}

function transformPoint(
  p: { x: number; y: number },
  t: FileTransform,
  pivot: Point2,
): { x: number; y: number } {
  // 1. Scale around pivot.
  let x = pivot.x + (p.x - pivot.x) * t.scale;
  let y = pivot.y + (p.y - pivot.y) * t.scale;
  // 2. Mirror X (negate Y around pivot.y).
  if (t.mirrorX) y = 2 * pivot.y - y;
  // 3. Mirror Y (negate X around pivot.x).
  if (t.mirrorY) x = 2 * pivot.x - x;
  // 4. Rotate around pivot.
  if (t.rotateDeg !== 0) {
    const rad = (t.rotateDeg * Math.PI) / 180;
    const cos = Math.cos(rad);
    const sin = Math.sin(rad);
    const dx = x - pivot.x;
    const dy = y - pivot.y;
    x = pivot.x + dx * cos - dy * sin;
    y = pivot.y + dx * sin + dy * cos;
  }
  // 5. Translate.
  x += t.translate.x;
  y += t.translate.y;
  return { x, y };
}

/// Recompute axis-aligned bbox by scanning the transformed segments.
/// `fallback` covers the degenerate empty-segments case (which already
/// existed pre-transform — keep the original bbox).
function computeBbox(segments: readonly Segment[], fallback: BBox): BBox {
  if (segments.length === 0) return fallback;
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const s of segments) {
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
