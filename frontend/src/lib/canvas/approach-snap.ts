/// Snap targets for the approach-point picker (n79). When the user
/// drags the picker cursor over the canvas, the cursor snaps to the
/// nearest geometry vertex of the active op's source-object set —
/// matching EstlCam's `Eingerastet` behaviour. Holding Shift on the
/// caller side disables snap; this module is the data layer only.

import type { ImportResponse, Segment } from '../api/types';
import type { OpEntry } from '../state/op_types';

export interface SnapCandidate {
  x: number;
  y: number;
}

/// Collect every endpoint of every segment that belongs to one of the
/// op's source-object ids. For `op.sourceObjects === undefined`
/// ("source = All") we collect endpoints of every *closed* object
/// (open polylines aren't sensible approach targets). Returned points
/// are in data coordinates with light deduplication (same point twice
/// is the shared vertex between two segments).
export function approachSnapCandidates(
  imported: ImportResponse | null | undefined,
  op: Pick<OpEntry, 'sourceObjects' | 'sourceLayers'> | null | undefined,
): SnapCandidate[] {
  if (!imported || !op) return [];
  const objects = imported.objects ?? [];
  const meta = imported.object_meta ?? [];
  if (imported.segments.length === 0) return [];

  const allowedObjects: Set<number> | null = op.sourceObjects && op.sourceObjects.length > 0
    ? new Set(op.sourceObjects)
    : null;
  const allowedLayers: Set<string> | null =
    op.sourceLayers && op.sourceLayers.length > 0 ? new Set(op.sourceLayers) : null;

  // For source = All (allowedObjects==null and allowedLayers==null),
  // restrict to closed objects so we don't snap to open-polyline ends
  // that would surprise the user.
  const closedIds = new Set<number>(meta.filter((m) => m.closed).map((m) => m.id));

  const out: SnapCandidate[] = [];
  const seen = new Set<string>();
  const push = (x: number, y: number): void => {
    // 1e-4 mm dedup — fine enough for any practical CAD, coarse enough
    // to merge bit-for-bit duplicates from shared segment endpoints.
    const key = `${Math.round(x * 10000)},${Math.round(y * 10000)}`;
    if (seen.has(key)) return;
    seen.add(key);
    out.push({ x, y });
  };

  imported.segments.forEach((s: Segment, i: number) => {
    const objId = objects[i] ?? 0;
    if (allowedObjects) {
      if (!allowedObjects.has(objId)) return;
    } else if (allowedLayers) {
      if (!allowedLayers.has(s.layer)) return;
    } else if (objId === 0 || !closedIds.has(objId)) {
      // source = All ⇒ closed objects only.
      return;
    }
    push(s.start.x, s.start.y);
    push(s.end.x, s.end.y);
    if (s.center) push(s.center.x, s.center.y);
  });
  return out;
}

/// Pick the nearest candidate to `(x, y)` within `toleranceData`
/// (data units = mm in this codebase). Returns `null` when no
/// candidate is within range.
///
/// `toleranceData` is computed by the caller from `pxTolerance /
/// pxPerData` so the snap radius stays constant in screen pixels
/// regardless of zoom — same trick the canvas already uses for hit
/// testing.
export function findNearestSnap(
  candidates: readonly SnapCandidate[],
  x: number,
  y: number,
  toleranceData: number,
): SnapCandidate | null {
  if (candidates.length === 0 || toleranceData <= 0) return null;
  const t2 = toleranceData * toleranceData;
  let best: SnapCandidate | null = null;
  let bestD2 = Infinity;
  for (const c of candidates) {
    const dx = c.x - x;
    const dy = c.y - y;
    const d2 = dx * dx + dy * dy;
    if (d2 < t2 && d2 < bestD2) {
      bestD2 = d2;
      best = c;
    }
  }
  return best;
}
