// op-source predicates. Used wherever code needs to answer "does this op
// consume this object?" without dragging the rest of the project state
// along (Scene3D's overlay coloring is the original caller; the 2D
// canvas hit tests are next).

import type { ImportResponse } from '../api/types';

/// Returns whether `op` consumes a given chained-object `objectId` from
/// the imported geometry. Resolution order matches the pipeline:
///
/// 1. Explicit `sourceObjects` list (non-empty) — only those ids match.
/// 2. `sourceLayers` (non-empty) — every object whose first segment's
///    layer is in the list matches.
/// 3. Otherwise the op consumes everything ("all chained objects").
///
/// Pure: takes the import response (for the per-segment objects[] →
/// layer mapping) explicitly so the helper has no closure dependencies.
export function opIncludesObject(
  op: { sourceLayers: string[] | null; sourceObjects?: number[] },
  objectId: number,
  imp: ImportResponse,
): boolean {
  if (op.sourceObjects && op.sourceObjects.length > 0) {
    return op.sourceObjects.includes(objectId);
  }
  if (op.sourceLayers && op.sourceLayers.length > 0) {
    // Look up this object's layer via the first segment that maps to it
    // (objects[] is per-segment; layers come from segments[]).
    for (let i = 0; i < (imp.objects?.length ?? 0); i++) {
      if (imp.objects?.[i] === objectId) {
        const layer = imp.segments[i]?.layer ?? '';
        return op.sourceLayers.includes(layer);
      }
    }
    return false;
  }
  return true;
}
