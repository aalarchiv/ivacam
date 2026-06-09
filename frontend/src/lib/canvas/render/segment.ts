import type { Segment } from '../../api/types';
import type { ProjectFn } from './types';

/// Stroke one imported segment (line / arc / point) in the current
/// ctx.strokeStyle. The shared primitive under every layer painter —
/// imported geometry, selection halos, text previews.
export function drawSegment(ctx: CanvasRenderingContext2D, seg: Segment, p: ProjectFn) {
  const [sx, sy] = p(seg.start.x, seg.start.y);
  const [ex, ey] = p(seg.end.x, seg.end.y);

  if (seg.type === 'POINT') {
    ctx.fillStyle = ctx.strokeStyle;
    ctx.beginPath();
    ctx.arc(sx, sy, 2, 0, Math.PI * 2);
    ctx.fill();
    return;
  }

  if (Math.abs(seg.bulge) < 1e-9) {
    ctx.beginPath();
    ctx.moveTo(sx, sy);
    ctx.lineTo(ex, ey);
    ctx.stroke();
    return;
  }

  // Bulge-based arc. Recompute center for robustness — the importer
  // sometimes leaves center=(0,0) on bulged polyline segments.
  const dx = seg.end.x - seg.start.x;
  const dy = seg.end.y - seg.start.y;
  const chord = Math.hypot(dx, dy);
  if (chord < 1e-9) return;
  const bulge = seg.bulge;
  const sagitta = (bulge * chord) / 2;
  // Radius from chord and sagitta.
  const radius = (chord / 2) ** 2 / (2 * Math.abs(sagitta)) + Math.abs(sagitta) / 2;
  // Midpoint of the chord.
  const mx = (seg.start.x + seg.end.x) / 2;
  const my = (seg.start.y + seg.end.y) / 2;
  // Perpendicular unit vector pointing toward the center.
  const ux = -dy / chord;
  const uy = dx / chord;
  // Offset from midpoint to center.
  const h = radius - Math.abs(sagitta);
  const sign = bulge > 0 ? 1 : -1;
  const cx = mx + ux * h * sign;
  const cy = my + uy * h * sign;

  const startAng = Math.atan2(seg.start.y - cy, seg.start.x - cx);
  const endAng = Math.atan2(seg.end.y - cy, seg.end.x - cx);
  const counterClockwise = bulge > 0;

  const [pcx, pcy] = p(cx, cy);
  // 7iej.19: screen-space radius by projecting a point `radius` away from
  // the center and measuring. The viewport transform is a uniform scale,
  // so direction is irrelevant — and this avoids the div-by-near-zero the
  // old `(sx - pcx) / (seg.start.x - cx)` ratio hit on a vertical chord
  // (start directly above/below the center).
  const [prx, pry] = p(cx + radius, cy);
  const r = Math.hypot(prx - pcx, pry - pcy);
  // Reverse the y-flip on angles for canvas coords.
  ctx.beginPath();
  ctx.arc(pcx, pcy, r, -startAng, -endAng, counterClockwise);
  ctx.stroke();
}
