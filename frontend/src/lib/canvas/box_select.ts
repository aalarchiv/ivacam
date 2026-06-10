// Box-select containment hit logic extracted from EntityCanvas2D.svelte
// The canvas component still owns the input
// gestures + state; this module is the pure "which object ids land
// inside this canvas-pixel rectangle" calculation.

import type { BBox } from '../api/types';

/// One imported-object metadata row, in the shape the importer
/// produces. Spelled minimally so the helper doesn't pin a particular
/// `ImportResponse.object_meta` Type — anything structurally compatible
/// works.
export interface ObjectMeta {
  id: number;
  layer: string;
  bbox: BBox;
}

/// Canvas → data transform (mirror of `computeViewportTransform`):
/// `dataX = (canvasX - offX) / scale`, `dataY = (offY - canvasY) / scale`.
export interface BoxSelectTransform {
  scale: number;
  offX: number;
  offY: number;
}

/// Return the object ids whose bbox is fully contained inside the
/// canvas-pixel rectangle (x0,y0)-(x1,y1). Hidden layers are skipped
/// (the user can't accidentally pick something they can't see), with
/// `stockOutlineLayer` exempt because that synthetic layer isn't in
/// the user's visible-layers set but should still be pickable.
export function objectsContainedInBox(
  meta: readonly ObjectMeta[],
  visibleLayers: ReadonlySet<string>,
  transform: BoxSelectTransform,
  x0: number,
  y0: number,
  x1: number,
  y1: number,
  stockOutlineLayer: string,
): number[] {
  const { scale, offX, offY } = transform;
  const px2dx = (x: number): number => (x - offX) / scale;
  const px2dy = (y: number): number => (offY - y) / scale;
  const minX = Math.min(px2dx(x0), px2dx(x1));
  const maxX = Math.max(px2dx(x0), px2dx(x1));
  // Canvas Y is inverted relative to data Y, so the data-space min
  // comes from the LOWER pixel y.
  const minY = Math.min(px2dy(y0), px2dy(y1));
  const maxY = Math.max(px2dy(y0), px2dy(y1));
  const out: number[] = [];
  for (const m of meta) {
    if (m.layer !== stockOutlineLayer && !visibleLayers.has(m.layer)) continue;
    const b = m.bbox;
    // Containment: every corner of the object's bbox must lie inside
    // the selection rectangle.
    if (b.min_x < minX || b.max_x > maxX || b.min_y < minY || b.max_y > maxY) continue;
    out.push(m.id);
  }
  return out;
}
