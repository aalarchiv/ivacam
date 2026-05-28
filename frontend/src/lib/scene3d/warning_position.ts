// SimWarning → world-XYZ position lookup. Extracted from Scene3D.svelte
// (2o8s, l8u6 follow-up) so the per-kind position resolution can be
// unit-tested without a THREE.js scene or the rune runtime.
//
// The Scene3D marker layer and the playback-bar / generate-bar warning
// chips all need to answer "where does this warning live in the work
// volume?" — and the answer is kind-shaped:
//
//   * point-shaped warnings (rapid_through_material, fixture_collision,
//     holder_collision) carry coordinates directly,
//   * span-shaped warnings (engagement, dragging) fall back to the
//     start of the first toolpath segment in the span so the marker
//     still appears at a meaningful point.

import type { SimWarning, ToolpathSegment } from '../api/types';
import { simWarningSegmentIdx } from '../sim/warnings';

export interface WarningPosition {
  x: number;
  y: number;
  z: number;
}

/// Resolve a `SimWarning` to a world-XYZ position. Returns `null` only
/// when the warning is span-shaped AND the referenced toolpath segment
/// can't be located — every other branch is total.
export function warningPosition(
  w: SimWarning,
  toolpath: readonly ToolpathSegment[] | undefined,
): WarningPosition | null {
  if (w.kind === 'rapid_through_material') {
    return { x: w.worst_x, y: w.worst_y, z: w.worst_cell_z };
  }
  if (w.kind === 'fixture_collision') {
    return { x: w.nearest_x, y: w.nearest_y, z: 0 };
  }
  if (w.kind === 'holder_collision') {
    return { x: w.worst_x, y: w.worst_y, z: w.wall_z };
  }
  // Engagement / dragging are span-shaped, not point-shaped — fall back
  // to the toolpath segment endpoint so the marker still appears.
  const segIdx = simWarningSegmentIdx(w);
  const seg = toolpath ? toolpath[segIdx] : undefined;
  if (!seg) return null;
  return { x: seg.from.x, y: seg.from.y, z: seg.from.z };
}
