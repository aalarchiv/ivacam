// Pure 2D viewport math extracted from EntityCanvas2D.svelte (2o8s,
// l8u6 follow-up). The canvas component still owns the cached
// `lastTransform` / `lastBaseTransform` state and the user-pan/zoom
// fields — what's pure is the formula from (bbox, viewport, user view)
// to scale + offset.

import type { BBox } from '../api/types';

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
