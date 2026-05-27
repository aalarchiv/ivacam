// Ghost-tab projection: map a canvas-space cursor onto the closest
// op-source contour and resolve which snap (raw contour / vertex /
// midpoint / existing tab) the staged tab should adopt.
//
// Extracted from EntityCanvas2D.svelte (l8u6) so the snap-precedence
// rules can be unit-tested without a canvas or the rune runtime. The
// component keeps only the thin glue that reads $state/$derived and
// assembles the context object below.

import { polylineProject, polylineAtT, type ObjectPolyline } from '../cam/tabs';
import { findOSnap, type OSnapTargets, type OSnapSettings } from './osnap';

export type GhostTabSnap = 'contour' | 'vertex' | 'midpoint' | 'existing';

export interface GhostTab {
  x: number;
  y: number;
  objectId: number;
  t: number;
  snap: GhostTabSnap;
}

export interface GhostTabContext {
  /// Canvas → data transform (mirror of the draw transform):
  /// dataX = (cx - offX) / scale, dataY = (offY - cy) / scale.
  transform: { scale: number; offX: number; offY: number };
  /// The op's source contours, already chained into polylines.
  polylines: ObjectPolyline[];
  /// Op-source object filter; undefined/empty means "all chained objects".
  sourceObjects?: number[] | null;
  /// Existing tab placements on this op (for the existing-tab snap).
  tabPlacements?: ReadonlyArray<{ objectId: number; t: number }>;
  /// Alt held: disable the secondary snaps (CAD-convention escape hatch).
  altDown: boolean;
  osnapTargets: OSnapTargets;
  osnapSettings: OSnapSettings;
}

/// Project canvas-space (cx, cy) onto the closest op-source contour and
/// return the ghost-tab position, or `null` when no contour is within
/// 6 screen-px / the op has no matching closed source.
///
/// Snap precedence (1q3): vertex within 4 screen-px > midpoint within
/// 4 screen-px > existing tab on this op within 2 mm data-space > raw
/// contour projection within 6 screen-px. `altDown` disables every
/// secondary snap, leaving the bare contour projection.
export function projectGhostTab(cx: number, cy: number, ctx: GhostTabContext): GhostTab | null {
  const { transform, polylines, sourceObjects, tabPlacements, altDown, osnapTargets, osnapSettings } =
    ctx;
  const { scale, offX, offY } = transform;
  // Canvas → data XY (mirror of the draw transform).
  const dataX = (cx - offX) / scale;
  const dataY = (offY - cy) / scale;
  const tolPx = 6;
  const snapPx = 4;
  const existingTabTolMm = 2;
  const tolData = tolPx / scale;
  const snapTolData = snapPx / scale;
  // Op-source filter: only project onto contours the op actually consumes.
  const allow = (id: number): boolean => {
    if (sourceObjects && sourceObjects.length > 0) return sourceObjects.includes(id);
    // 'all' or layer-source: every chained object qualifies.
    return true;
  };
  let best: {
    x: number;
    y: number;
    objectId: number;
    t: number;
    d2: number;
    snap: GhostTabSnap;
  } | null = null;
  for (const obj of polylines) {
    if (!allow(obj.objectId)) continue;
    const { t, snap, d2 } = polylineProject(obj.pts, { x: dataX, y: dataY }, obj.closed);
    if (d2 > tolData * tolData) continue;
    if (best && d2 >= best.d2) continue;
    best = { x: snap.x, y: snap.y, objectId: obj.objectId, t, d2, snap: 'contour' };
  }
  if (!best) return null;
  if (altDown) {
    // CAD-style escape hatch: bare contour projection only.
    return { x: best.x, y: best.y, objectId: best.objectId, t: best.t, snap: best.snap };
  }
  // Promote to vertex / midpoint / intersection via the shared OSnap
  // engine so the tab path respects the user's per-kind toggles (li0m)
  // and supports intersection snaps (ffhp). The OSnap result is in
  // whole-drawing coordinates; project it back to (objectId, t) so the
  // tab stays attached to a specific contour through transforms.
  const osnap = findOSnap(osnapTargets, dataX, dataY, snapTolData, osnapSettings);
  let promoted: {
    t: number;
    x: number;
    y: number;
    snap: 'vertex' | 'midpoint' | 'existing';
    d2: number;
    objectId: number;
  } | null = null;
  if (osnap && osnap.kind !== 'grid') {
    let proj: { objectId: number; t: number; x: number; y: number; d2: number } | null = null;
    for (const o of polylines) {
      if (!allow(o.objectId)) continue;
      const { t, snap, d2 } = polylineProject(o.pts, osnap, o.closed);
      if (proj && d2 >= proj.d2) continue;
      proj = { objectId: o.objectId, t, x: snap.x, y: snap.y, d2 };
    }
    // The OSnap point has to lie on (or very near) an op-source contour
    // for the tab to make sense — discard the snap when the user is
    // hovering a vertex belonging to geometry the op doesn't touch.
    if (proj && proj.d2 <= snapTolData * snapTolData) {
      // 'intersection' collapses to 'vertex' in the tab snap-kind enum
      // since both are discrete points (no separate visual).
      const snapKind: 'vertex' | 'midpoint' = osnap.kind === 'midpoint' ? 'midpoint' : 'vertex';
      promoted = {
        t: proj.t,
        x: osnap.x,
        y: osnap.y,
        snap: snapKind,
        d2: 0,
        objectId: proj.objectId,
      };
    }
  }
  // Existing-tab snap on the SAME op + object, within 2 mm data-space.
  // Kept as a separate scan because tab placements aren't OSnap targets
  // (they live on the op, not the imported geometry).
  const targetObj = polylines.find((o) => o.objectId === best!.objectId);
  if (targetObj) {
    for (const tp of tabPlacements ?? []) {
      if (tp.objectId !== best.objectId) continue;
      const wp = polylineAtT(targetObj.pts, tp.t, targetObj.closed).point;
      const dx = wp.x - dataX;
      const dy = wp.y - dataY;
      const d2 = dx * dx + dy * dy;
      if (d2 > existingTabTolMm * existingTabTolMm) continue;
      if (promoted && d2 >= promoted.d2) continue;
      promoted = {
        t: tp.t,
        x: wp.x,
        y: wp.y,
        snap: 'existing',
        d2,
        objectId: best.objectId,
      };
    }
  }
  if (promoted) {
    return {
      x: promoted.x,
      y: promoted.y,
      objectId: promoted.objectId,
      t: promoted.t,
      snap: promoted.snap,
    };
  }
  return { x: best.x, y: best.y, objectId: best.objectId, t: best.t, snap: best.snap };
}
