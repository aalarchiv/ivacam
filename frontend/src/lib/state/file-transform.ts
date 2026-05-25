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
import { isIdentityFileTransform, type FileTransform, type ImportEntry } from './project-types';

/// Apply `t` to a full ImportResponse. Object ids, layers, warnings,
/// unit_scale, format, filename — all unchanged. Segments, top-level
/// bbox, and per-object object_meta[i].bbox are recomputed against the
/// transformed segments.
export function applyFileTransform(imp: ImportResponse, t: FileTransform): ImportResponse {
  if (isIdentityFileTransform(t)) return imp;
  const pivot = bboxCenter(imp.bbox);
  const transformedSegments = imp.segments.map((s) => transformSegment(s, t, pivot));
  return {
    ...imp,
    segments: transformedSegments,
    bbox: computeBbox(transformedSegments, imp.bbox),
    object_meta: recomputeObjectMetaBbox(imp, transformedSegments),
  };
}

/// Recompute each object_meta[i].bbox by walking the transformed
/// segments tagged with that object's id. Matches the endpoint-based
/// bbox that the top-level `computeBbox` already produces (no arc-extent
/// inflation — same limitation, kept symmetric on purpose).
///
/// Falls back to the original meta entry when an object has no
/// segments tagged with its id (degenerate import; preserves the
/// authored bbox rather than emitting Infinity sentinels).
function recomputeObjectMetaBbox(
  imp: ImportResponse,
  transformedSegments: readonly Segment[],
): ImportResponse['object_meta'] {
  const meta = imp.object_meta ?? [];
  if (meta.length === 0) return meta;
  const tags = imp.objects ?? [];
  const byId = new Map<number, { min_x: number; min_y: number; max_x: number; max_y: number }>();
  for (let i = 0; i < transformedSegments.length; i++) {
    const id = tags[i];
    if (!id) continue;
    const seg = transformedSegments[i];
    let b = byId.get(id);
    if (!b) {
      b = { min_x: Infinity, min_y: Infinity, max_x: -Infinity, max_y: -Infinity };
      byId.set(id, b);
    }
    if (seg.start.x < b.min_x) b.min_x = seg.start.x;
    if (seg.start.y < b.min_y) b.min_y = seg.start.y;
    if (seg.end.x < b.min_x) b.min_x = seg.end.x;
    if (seg.end.y < b.min_y) b.min_y = seg.end.y;
    if (seg.start.x > b.max_x) b.max_x = seg.start.x;
    if (seg.start.y > b.max_y) b.max_y = seg.start.y;
    if (seg.end.x > b.max_x) b.max_x = seg.end.x;
    if (seg.end.y > b.max_y) b.max_y = seg.end.y;
  }
  return meta.map((m) => {
    const b = byId.get(m.id);
    return b ? { ...m, bbox: b } : m;
  });
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

/// Inverse of `applyFileTransformToPoint`: map a transformed-world
/// point back to the original raw-import space (43l2). The pivot stays
/// at the ORIGINAL bbox center (the same pivot the forward transform
/// uses), so callers pass the raw `bbox`. Composition with
/// `applyFileTransformToPoint(_, T, bbox)` round-trips to identity
/// within f64 precision.
export function invertFileTransformPoint(
  p: { x: number; y: number },
  t: FileTransform,
  bbox: BBox,
): { x: number; y: number } {
  if (isIdentityFileTransform(t)) return p;
  return inverseTransformPoint(p, t, bboxCenter(bbox));
}

function inverseTransformPoint(
  p: { x: number; y: number },
  t: FileTransform,
  pivot: Point2,
): { x: number; y: number } {
  // Forward order is: scale → mirrorX → mirrorY → rotate → translate.
  // Inverse runs the steps in reverse with each step's inverse:
  // -translate, -rotate, mirrorY (self-inverse), mirrorX, 1/scale.
  let x = p.x - t.translate.x;
  let y = p.y - t.translate.y;
  if (t.rotateDeg !== 0) {
    const rad = (-t.rotateDeg * Math.PI) / 180;
    const cos = Math.cos(rad);
    const sin = Math.sin(rad);
    const dx = x - pivot.x;
    const dy = y - pivot.y;
    x = pivot.x + dx * cos - dy * sin;
    y = pivot.y + dx * sin + dy * cos;
  }
  if (t.mirrorY) x = 2 * pivot.x - x;
  if (t.mirrorX) y = 2 * pivot.y - y;
  if (t.scale !== 0) {
    x = pivot.x + (x - pivot.x) / t.scale;
    y = pivot.y + (y - pivot.y) / t.scale;
  }
  return { x, y };
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

/// Merge N imports into a single ImportResponse (wrsu Phase 2). Each
/// entry's `fileTransform` is applied to its own copy first; then
/// segments / objects / layers / object_meta are concatenated.
///
/// Object id namespacing: each entry contributes a range `[idOffset+1,
/// idOffset+maxLocalId]` to the combined output, where idOffset = sum
/// of previous entries' maxLocalId. Existing op references to entries[0]
/// stay valid (idOffset = 0); later imports occupy unused id ranges.
///
/// Layers from different imports are unioned by name (segment counts
/// summed). Existing ops that reference layers by name continue to
/// resolve against the union — Phase 3 will add per-import namespacing
/// if collisions become a real problem.
///
/// The single-entry case short-circuits to `applyFileTransform(entry.source,
/// entry.fileTransform)` — same path as before Phase 2.
export function combineImports(entries: readonly ImportEntry[]): ImportResponse | null {
  if (entries.length === 0) return null;
  if (entries.length === 1) {
    return applyFileTransform(entries[0].source, entries[0].fileTransform);
  }
  const segments: Segment[] = [];
  const objects: number[] = [];
  const objectMeta: ImportResponse['object_meta'] = [];
  const layerByName = new Map<string, ImportResponse['layers'][number]>();
  let idOffset = 0;
  let mergedBbox: BBox | null = null;
  let filenameParts: string[] = [];
  for (const entry of entries) {
    const t = applyFileTransform(entry.source, entry.fileTransform);
    segments.push(...t.segments);
    for (const id of t.objects ?? []) {
      objects.push(id === 0 ? 0 : id + idOffset);
    }
    for (const m of t.object_meta ?? []) {
      objectMeta.push({ ...m, id: m.id + idOffset });
    }
    for (const l of t.layers ?? []) {
      const existing = layerByName.get(l.name);
      if (existing) {
        existing.segment_count += l.segment_count;
      } else {
        layerByName.set(l.name, { ...l });
      }
    }
    if (t.segments.length > 0) {
      if (!mergedBbox) {
        mergedBbox = { ...t.bbox };
      } else {
        mergedBbox.min_x = Math.min(mergedBbox.min_x, t.bbox.min_x);
        mergedBbox.min_y = Math.min(mergedBbox.min_y, t.bbox.min_y);
        mergedBbox.max_x = Math.max(mergedBbox.max_x, t.bbox.max_x);
        mergedBbox.max_y = Math.max(mergedBbox.max_y, t.bbox.max_y);
      }
    }
    const localMax = (t.objects ?? []).reduce((m, id) => (id > m ? id : m), 0);
    idOffset += localMax;
    filenameParts.push(t.filename);
  }
  const head = entries[0].source;
  return {
    ...head,
    filename: filenameParts.join(' + '),
    segments,
    objects,
    object_meta: objectMeta,
    layers: Array.from(layerByName.values()),
    bbox: mergedBbox ?? head.bbox,
  };
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
