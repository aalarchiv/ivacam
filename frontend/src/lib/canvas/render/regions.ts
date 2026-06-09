import type { Point2, RegionPreview } from '../../api/types';

/// Path2D cache for region previews. Tracing each region's polygons by
/// hand on every redraw was O(total tessellated points) per draw, which
/// fires on hover, selection, layer toggle, etc. We build the Path2D
/// objects once in *data space* (no canvas transform applied) and
/// stamp them with ctx.setTransform during draw — re-rebuilt only when
/// the regions array reference actually changes.
export interface RegionPath {
  op_id: number;
  path: Path2D;
}

export class RegionPathCache {
  private regionsRef: unknown = null;
  private cached: RegionPath[] = [];

  paths(regions: readonly RegionPreview[]): RegionPath[] {
    if (this.regionsRef === regions) return this.cached;
    this.cached = regions.map((r) => {
      const path = new Path2D();
      tracePolygonInto(path, r.outer);
      for (const hole of r.holes ?? []) tracePolygonInto(path, hole);
      return { op_id: r.op_id, path };
    });
    this.regionsRef = regions;
    return this.cached;
  }
}

function tracePolygonInto(path: Path2D, pts: readonly Point2[]) {
  if (pts.length < 3) return;
  path.moveTo(pts[0].x, pts[0].y);
  for (let i = 1; i < pts.length; i++) {
    path.lineTo(pts[i].x, pts[i].y);
  }
  path.closePath();
}

/// Paint each region's outer polygon and punch its holes via the
/// even-odd fill rule. The selected op's region is drawn in accent so
/// the user can spot it; others fade so the canvas doesn't get loud.
export function drawRegions(
  ctx: CanvasRenderingContext2D,
  cache: RegionPathCache,
  regions: readonly RegionPreview[],
  scale: number,
  offX: number,
  offY: number,
  selectedOpId: number | null,
  accent: string,
) {
  const paths = cache.paths(regions);
  // Compose data → canvas transform on top of the existing dpr scale.
  // Y is flipped (canvas y-down vs DXF y-up) so we use -scale on Y +
  // offY as the canvas-space origin of data-y=0.
  ctx.save();
  ctx.transform(scale, 0, 0, -scale, offX, offY);
  for (const rp of paths) {
    const isSelected = selectedOpId === rp.op_id;
    // Accent tint, clearly visible so toggling Regions is obvious (the
    // old ~10% muted-grey fill was near-invisible). Selected op's
    // region is brighter. Still translucent so contours read through.
    ctx.fillStyle = isSelected
      ? `${accent}66` // ~40% alpha
      : `${accent}33`; // ~20% alpha
    ctx.fill(rp.path, 'evenodd');
  }
  ctx.restore();
}
