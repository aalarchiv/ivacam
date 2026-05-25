/// Synthesize the stock material's outline as a selectable geometry
/// object (8jce). The Rust pipeline has no concept of stock — it's a
/// frontend-only construct — so to let an op (chamfer the perimeter,
/// profile, engrave a border) target the workpiece edge we inject a
/// closed rectangle, built from the effective stock footprint, into the
/// geometry the canvas selects and the wire payload sends. It rides the
/// same `segments` / `objects` / `object_meta` model as any imported
/// object, so no Rust change is needed — it's just another contour.
///
/// CRITICAL: this must NOT be folded into the `transformedImport` that
/// feeds auto-stock sizing (`computeFootprint`), or the outline would
/// derive from the footprint which derives from the bbox which derives
/// from the outline — a feedback loop. The augmented view is a separate
/// derivation; auto-stock keeps reading the raw import.

import type { ImportResponse, Segment } from '../api/types';

/// Synthetic layer name for the outline — mirrors the `__text_<id>`
/// convention for pipeline-side synthetic geometry. The outline is
/// recognised by this layer (its object id is dynamic — see below).
export const STOCK_OUTLINE_LAYER = '__stock_outline';

/// The outline's 1-based object id for a base with `objectCount` existing
/// objects. It MUST be the next sequential id (count + 1) and the outline
/// segments MUST be appended last, because the Rust pipeline re-chains the
/// sent segments and assigns ids by chain order (`idx + 1`, see
/// pipeline/selection.rs) — it does NOT honour a frontend-chosen id. So
/// the outline can only be targeted by an op if its frontend id matches
/// the id the pipeline will give the last-chained object.
export function stockOutlineId(objectCount: number): number {
  return objectCount + 1;
}

export interface Footprint {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

function lineSeg(x0: number, y0: number, x1: number, y1: number): Segment {
  return {
    type: 'LINE',
    start: { x: x0, y: y0 },
    end: { x: x1, y: y1 },
    bulge: 0,
    layer: STOCK_OUTLINE_LAYER,
    color: 7,
  };
}

/// The four closed-rectangle segments (CCW from bottom-left) for a
/// footprint. Exported for tests / callers that want just the geometry.
export function stockOutlineSegments(fp: Footprint): Segment[] {
  const { minX, minY, maxX, maxY } = fp;
  return [
    lineSeg(minX, minY, maxX, minY),
    lineSeg(maxX, minY, maxX, maxY),
    lineSeg(maxX, maxY, minX, maxY),
    lineSeg(minX, maxY, minX, minY),
  ];
}

/// True when a footprint is too degenerate to bother outlining (empty,
/// non-finite, or sub-µm on either axis).
function degenerate(fp: Footprint): boolean {
  const w = fp.maxX - fp.minX;
  const h = fp.maxY - fp.minY;
  return (
    !Number.isFinite(w) ||
    !Number.isFinite(h) ||
    !Number.isFinite(fp.minX) ||
    !Number.isFinite(fp.minY) ||
    w <= 1e-6 ||
    h <= 1e-6
  );
}

/// Return `base` augmented with the stock-outline object, or `base`
/// UNCHANGED (referential identity) when the footprint is degenerate —
/// so callers that gate on `view === transformedImport` see no change
/// in the common/disabled case. When `base` is null (no drawing loaded)
/// but the footprint is valid, a minimal `ImportResponse` carrying only
/// the outline is synthesized so the stock edge is still selectable.
export function augmentWithStockOutline(
  base: ImportResponse | null,
  fp: Footprint,
): ImportResponse | null {
  if (degenerate(fp)) return base;
  const segs = stockOutlineSegments(fp);
  // Next sequential 1-based id so the pipeline's re-chain (which numbers
  // objects by chain order) gives the last-appended outline this same id.
  const id = stockOutlineId(base?.object_meta?.length ?? 0);
  const meta = {
    bbox: { min_x: fp.minX, min_y: fp.minY, max_x: fp.maxX, max_y: fp.maxY },
    closed: true,
    color: 7,
    id,
    layer: STOCK_OUTLINE_LAYER,
  };
  const ids = segs.map(() => id);
  if (!base) {
    return {
      bbox: { min_x: fp.minX, min_y: fp.minY, max_x: fp.maxX, max_y: fp.maxY },
      filename: '',
      format: 'synthetic',
      layers: [{ name: STOCK_OUTLINE_LAYER, color: 7, segment_count: 4 }],
      object_meta: [meta],
      objects: ids,
      segments: segs,
      unit_scale: 1,
      warnings: [],
    } as ImportResponse;
  }
  return {
    ...base,
    layers: base.layers.some((l) => l.name === STOCK_OUTLINE_LAYER)
      ? base.layers
      : [...base.layers, { name: STOCK_OUTLINE_LAYER, color: 7, segment_count: 4 }],
    object_meta: [...(base.object_meta ?? []), meta],
    objects: [...(base.objects ?? []), ...ids],
    segments: [...base.segments, ...segs],
  };
}
