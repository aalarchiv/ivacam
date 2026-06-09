// Pure 2D viewport math extracted from EntityCanvas2D.svelte (2o8s,
// l8u6 follow-up). The canvas component still owns the cached
// `lastTransform` / `lastBaseTransform` state and the user-pan/zoom
// fields — what's pure is the formula from (bbox, viewport, user view)
// to scale + offset.

import type { BBox, Segment } from '../api/types';

export interface ViewportSize {
  /// CSS-pixel width of the canvas surface.
  w: number;
  /// CSS-pixel height of the canvas surface.
  h: number;
}

export interface UserView {
  /// Multiplied on top of the fit-to-view scale (1 = fit, > 1 = zoomed in).
  zoom: number;
  /// Pan offsets in canvas pixels, applied after the fit-and-zoom transform.
  panX: number;
  panY: number;
}

export interface CanvasTransform {
  /// Final canvas-pixel-per-data-unit scale (`baseScale * userView.zoom`).
  scale: number;
  /// Final canvas-X offset (`baseOffX + userView.panX`).
  offX: number;
  /// Final canvas-Y offset (`baseOffY + userView.panY`). Y is flipped:
  /// DXF y-up vs canvas y-down.
  offY: number;
  /// The fit-to-view transform without pan/zoom, kept so the component
  /// can compare "where would auto-fit place it" against the active
  /// transform (e.g. to render an out-of-view hint).
  baseScale: number;
  baseOffX: number;
  baseOffY: number;
  /// Convenience projector: `(dataX, dataY) → [canvasX, canvasY]`.
  project2: (x: number, y: number) => [number, number];
}

/// Fit-to-view transform for a data-space bounding box rendered into a
/// canvas of `viewport.w × viewport.h` pixels, padded by `margin` px and
/// post-multiplied by the user's zoom + pan. Used as the single source
/// of truth for the active viewport transform; the canvas component
/// caches the result for hit-testing.
export function computeViewportTransform(
  bbox: BBox,
  viewport: ViewportSize,
  user: UserView,
  margin = 32,
): CanvasTransform {
  const { w, h } = viewport;
  const dataW = Math.max(bbox.max_x - bbox.min_x, 1e-6);
  const dataH = Math.max(bbox.max_y - bbox.min_y, 1e-6);
  const baseScale = Math.min((w - 2 * margin) / dataW, (h - 2 * margin) / dataH);
  const baseOffX = margin - bbox.min_x * baseScale + (w - 2 * margin - dataW * baseScale) / 2;
  // Y flipped: DXF y-up, canvas y-down.
  const baseOffY = h - margin - -bbox.min_y * baseScale - (h - 2 * margin - dataH * baseScale) / 2;
  const scale = baseScale * user.zoom;
  const offX = baseOffX + user.panX;
  const offY = baseOffY + user.panY;
  const project2 = (px: number, py: number): [number, number] => [
    px * scale + offX,
    offY - py * scale,
  ];
  return { scale, offX, offY, baseScale, baseOffX, baseOffY, project2 };
}

/// An axis-aligned rectangle in data space.
export interface Rect {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

/// Union of placement rects, expanded by `marginFrac` of each axis span
/// (with a 1-unit floor so a zero-span axis still gets padding), as a
/// `BBox`. Returns `null` for an empty list. rt1.12 (fvb0): the fallback
/// viewport extent for a geometry-less raster-engrave project, used when
/// no machine work area provides a stable bed to fit to.
export function placementsBBox(rects: readonly Rect[], marginFrac = 0.1): BBox | null {
  if (rects.length === 0) return null;
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const r of rects) {
    minX = Math.min(minX, r.minX);
    minY = Math.min(minY, r.minY);
    maxX = Math.max(maxX, r.maxX);
    maxY = Math.max(maxY, r.maxY);
  }
  const mx = Math.max(1, (maxX - minX) * marginFrac);
  const my = Math.max(1, (maxY - minY) * marginFrac);
  return { min_x: minX - mx, min_y: minY - my, max_x: maxX + mx, max_y: maxY + my };
}

/// Axis-aligned bbox over a segment list's endpoints (good enough for
/// view-fit + a drag hit-test; arc bulges are ignored). Null for an
/// empty list.
export function segsBBox(segs: readonly Segment[]): Rect | null {
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
  if (!Number.isFinite(minX)) return null;
  return { minX, minY, maxX, maxY };
}
