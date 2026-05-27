/// Stock-footprint resolution — the auto/manual/margin geometry that
/// turns the stock UI config + imported bbox into an axis-aligned box.
/// Extracted from `driver.ts` (which pulls in THREE) so the pure logic
/// can be imported by the THREE-free API layer (`build-project.ts`,
/// vrrr) as well as the 3D scene. `driver.ts` re-exports it for the
/// existing import sites.
import type { ImportResponse } from '../api/types';

/// Compute the simulator footprint from the imported geometry + stock
/// config. Defaults to imported bbox plus a small margin; manual mode
/// uses customX/Y centered on the bbox.
export function computeFootprint(
  imported: ImportResponse | null,
  stock: {
    mode: 'auto' | 'manual';
    margin: number;
    customX: number;
    customY: number;
    offsetX?: number;
    offsetY?: number;
  },
  workArea?: { x: number; y: number } | null,
): { minX: number; minY: number; maxX: number; maxY: number } {
  const ox = stock.offsetX ?? 0;
  const oy = stock.offsetY ?? 0;
  // Manual mode: footprint is exactly customX × customY centered on
  // the imported geometry's bbox center (or origin when none).
  if (stock.mode === 'manual') {
    let cx = 0;
    let cy = 0;
    if (imported) {
      const { min_x, min_y, max_x, max_y } = imported.bbox;
      cx = (min_x + max_x) * 0.5;
      cy = (min_y + max_y) * 0.5;
    }
    return {
      minX: cx - stock.customX * 0.5 + ox,
      minY: cy - stock.customY * 0.5 + oy,
      maxX: cx + stock.customX * 0.5 + ox,
      maxY: cy + stock.customY * 0.5 + oy,
    };
  }
  // Auto mode WITH geometry: bbox + margin (the legacy behavior).
  if (imported) {
    const { min_x, min_y, max_x, max_y } = imported.bbox;
    const m = Math.max(0, stock.margin);
    return {
      minX: min_x - m + ox,
      minY: min_y - m + oy,
      maxX: max_x + m + ox,
      maxY: max_y + m + oy,
    };
  }
  // Auto mode WITHOUT geometry: default to the machine work-area
  // footprint anchored at the origin.
  if (workArea && workArea.x > 0 && workArea.y > 0) {
    return { minX: ox, minY: oy, maxX: workArea.x + ox, maxY: workArea.y + oy };
  }
  // Final fallback for clients that don't pass a work area.
  return { minX: ox, minY: oy, maxX: 100 + ox, maxY: 100 + oy };
}
