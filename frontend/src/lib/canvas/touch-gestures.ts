// Pure two-finger gesture math for EntityCanvas2D (bwt7). The canvas
// component owns the live pointer Map, the gesture timers, and the
// reactive user-pan/zoom fields; what's pure (and unit-tested here) is
// the formula that turns a two-pointer pinch/pan step into a new
// `UserView`, plus the trivial helpers the long-press / multi-touch
// bookkeeping leans on.
//
// The pinch transform is the SAME cursor-pivot zoom the wheel handler
// uses (`onWheel`), generalized from one cursor to the two-finger
// centroid and from a fixed 1.15 step to the live finger-distance
// ratio. Because it anchors the data-space point under the PREVIOUS
// centroid onto the CURRENT centroid, a pure translation of both
// fingers (distance ratio ≈ 1) falls out as a two-finger pan for free —
// no separate pan branch needed.

import type { UserView } from './viewport';

/// A pointer position in canvas-relative pixels (`clientX - rect.left`).
export interface PointerPos {
  x: number;
  y: number;
}

/// The fit-to-view base transform the user view is layered on top of —
/// mirrors the canvas component's cached `lastBaseTransform`.
export interface BaseView {
  scale: number;
  offX: number;
  offY: number;
}

/// Inclusive zoom clamp. Defaults match the wheel handler's
/// `[0.05, 80]` range so pinch and wheel zoom share one ceiling/floor.
export interface ZoomLimits {
  min: number;
  max: number;
}

export const DEFAULT_ZOOM_LIMITS: ZoomLimits = { min: 0.05, max: 80 };

/// Long-press (touch right-click substitute): hold this long without
/// moving past `LONG_PRESS_MOVE_TOL_PX` to open the context menu.
export const LONG_PRESS_MS = 500;

/// Movement budget (canvas px) a held finger may wander before the hold
/// is reclassified as a drag and the long-press is cancelled.
export const LONG_PRESS_MOVE_TOL_PX = 8;

/// Euclidean distance between two pointer positions.
export function pointerDistance(a: PointerPos, b: PointerPos): number {
  return Math.hypot(a.x - b.x, a.y - b.y);
}

/// Midpoint of two pointer positions — the pinch/pan anchor.
export function pointerCentroid(a: PointerPos, b: PointerPos): PointerPos {
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
}

/// `true` while `curr` is still within tap tolerance of `start` — i.e.
/// the press has not yet become a drag, so a pending long-press stands.
export function withinTapTolerance(
  start: PointerPos,
  curr: PointerPos,
  tol: number = LONG_PRESS_MOVE_TOL_PX,
): boolean {
  return pointerDistance(start, curr) <= tol;
}

/// Apply one pinch/pan step. Given the two fingers' previous and current
/// positions, return the new user view that (a) scales by the
/// finger-distance ratio about the gesture centroid and (b) pans by the
/// centroid translation. Degenerate inputs (a zero-length previous
/// span, or a non-finite ratio) collapse to a pure pan so a momentary
/// finger overlap can't blow up the zoom.
export function applyPinch(
  view: UserView,
  base: BaseView,
  prev: { a: PointerPos; b: PointerPos },
  curr: { a: PointerPos; b: PointerPos },
  limits: ZoomLimits = DEFAULT_ZOOM_LIMITS,
): UserView {
  const prevDist = pointerDistance(prev.a, prev.b);
  const currDist = pointerDistance(curr.a, curr.b);
  const ratio = prevDist > 1e-6 ? currDist / prevDist : 1;
  const factor = Number.isFinite(ratio) && ratio > 0 ? ratio : 1;

  const prevC = pointerCentroid(prev.a, prev.b);
  const currC = pointerCentroid(curr.a, curr.b);

  const oldScale = base.scale * view.zoom;
  const oldOffX = base.offX + view.panX;
  const oldOffY = base.offY + view.panY;

  // Guard a not-yet-staged transform (scale 0) — nothing sane to do.
  if (oldScale <= 0) return view;

  // Data-space point under the previous centroid.
  const dataX = (prevC.x - oldOffX) / oldScale;
  const dataY = (oldOffY - prevC.y) / oldScale;

  const nextZoom = Math.max(limits.min, Math.min(limits.max, view.zoom * factor));
  const newScale = base.scale * nextZoom;

  // Place that same data point under the current centroid: this folds
  // the centroid translation (pan) and the scale change into one solve.
  const newOffX = currC.x - dataX * newScale;
  const newOffY = currC.y + dataY * newScale;

  return {
    zoom: nextZoom,
    panX: newOffX - base.offX,
    panY: newOffY - base.offY,
  };
}
