import type { ProjectFn } from './types';
import { stockHandleScreenPositions, type WorldBox } from '../stock-gizmo';

/// Static canvas chrome — grid, axes, machine work-area, stock outline.
/// Pure painters: callers resolve theme colors and pass them in, so the
/// modules never touch getComputedStyle / component state.

export interface GridColors {
  minor: string;
  major: string;
}

/// Major grid every 10 units, minor every 1, when the unit is small enough.
export function drawGrid(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  scale: number,
  offX: number,
  offY: number,
  colors: GridColors,
) {
  const majorStep = 10;
  const minorStep = 1;
  const px = Math.abs(scale * minorStep);
  if (px < 6) return; // too tight to be useful
  ctx.lineWidth = 1;
  for (const [step, color] of [
    [minorStep, colors.minor],
    [majorStep, colors.major],
  ] as const) {
    ctx.strokeStyle = color;
    const start = Math.floor(-offX / scale / step) * step;
    const end = Math.ceil((w - offX) / scale / step) * step;
    ctx.beginPath();
    for (let x = start; x <= end; x += step) {
      const X = x * scale + offX;
      ctx.moveTo(X, 0);
      ctx.lineTo(X, h);
    }
    const ystart = Math.floor((offY - h) / scale / step) * step;
    const yend = Math.ceil(offY / scale / step) * step;
    for (let y = ystart; y <= yend; y += step) {
      const Y = offY - y * scale;
      ctx.moveTo(0, Y);
      ctx.lineTo(w, Y);
    }
    ctx.stroke();
  }
}

export interface AxisColors {
  x: string;
  y: string;
}

export function drawAxes(
  ctx: CanvasRenderingContext2D,
  w: number,
  h: number,
  offX: number,
  offY: number,
  colors: AxisColors,
) {
  ctx.lineWidth = 1.5;
  ctx.strokeStyle = colors.x;
  ctx.beginPath();
  ctx.moveTo(0, offY);
  ctx.lineTo(w, offY);
  ctx.stroke();
  ctx.strokeStyle = colors.y;
  ctx.beginPath();
  ctx.moveTo(offX, 0);
  ctx.lineTo(offX, h);
  ctx.stroke();
}

/// Dashed rectangle showing the machine work-area envelope in the
/// XY plane (0,0) → (workArea.x, workArea.y). Sits under the
/// imported geometry so the user always sees the cuttable area
/// regardless of what's loaded. Pairs with the dashed wireframe
/// the 3D scene draws for the full XYZ envelope.
export function drawWorkArea(
  ctx: CanvasRenderingContext2D,
  p: ProjectFn,
  workArea: { x: number; y: number } | null | undefined,
  color: string,
) {
  if (!workArea || workArea.x <= 0 || workArea.y <= 0) return;
  const [x0, y0] = p(0, 0);
  const [x1, y1] = p(workArea.x, workArea.y);
  const minX = Math.min(x0, x1);
  const maxX = Math.max(x0, x1);
  const minY = Math.min(y0, y1);
  const maxY = Math.max(y0, y1);
  ctx.save();
  ctx.lineWidth = 1.2;
  ctx.strokeStyle = color;
  ctx.setLineDash([6, 4]);
  ctx.globalAlpha = 0.75;
  ctx.strokeRect(minX, minY, maxX - minX, maxY - minY);
  ctx.restore();
}

export interface Footprint {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

/// Solid outline of the workpiece bounds in XY. Mirrors the
/// translucent stock box the 3D scene already paints, so users can see
/// whether their drawing sits inside the stock without flipping to 3D.
export function drawStock(
  ctx: CanvasRenderingContext2D,
  p: ProjectFn,
  fp: Footprint,
  color: string,
) {
  const sizeX = fp.maxX - fp.minX;
  const sizeY = fp.maxY - fp.minY;
  if (sizeX <= 0 || sizeY <= 0) return;
  const [x0, y0] = p(fp.minX, fp.minY);
  const [x1, y1] = p(fp.maxX, fp.maxY);
  const minX = Math.min(x0, x1);
  const maxX = Math.max(x0, x1);
  const minY = Math.min(y0, y1);
  const maxY = Math.max(y0, y1);
  ctx.save();
  ctx.lineWidth = 1;
  ctx.strokeStyle = color;
  ctx.globalAlpha = 0.85;
  ctx.strokeRect(minX, minY, maxX - minX, maxY - minY);
  ctx.restore();
}

/// On-canvas stock gizmo handles (7jug.15, phone only). Draws the eight
/// resize squares plus the centre move puck over the already-drawn stock
/// outline. `box` is the stock footprint in world mm; the same
/// `stock-gizmo` geometry the hit-tester uses places the handles, so what
/// the user sees is exactly what they can grab. `activeKind` highlights
/// the handle currently being dragged.
export function drawStockGizmo(
  ctx: CanvasRenderingContext2D,
  box: WorldBox,
  scale: number,
  offX: number,
  offY: number,
  handlePx: number,
  color: string,
  accent: string,
  fill: string,
  activeKind: string | null,
) {
  if (box.maxX - box.minX <= 0 || box.maxY - box.minY <= 0) return;
  const pos = stockHandleScreenPositions(box, { scale, offX, offY });
  const r = handlePx * 0.5;
  ctx.save();
  ctx.lineWidth = 1.5;
  for (const [kind, p] of Object.entries(pos)) {
    const on = kind === activeKind;
    if (kind === 'move') {
      // Move puck: a filled circle with a plus, distinct from the square
      // resize handles so its role reads at a glance.
      ctx.globalAlpha = on ? 1 : 0.9;
      ctx.fillStyle = on ? accent : color;
      ctx.beginPath();
      ctx.arc(p.x, p.y, r + 1, 0, Math.PI * 2);
      ctx.fill();
      ctx.strokeStyle = fill;
      ctx.beginPath();
      ctx.moveTo(p.x - r * 0.6, p.y);
      ctx.lineTo(p.x + r * 0.6, p.y);
      ctx.moveTo(p.x, p.y - r * 0.6);
      ctx.lineTo(p.x, p.y + r * 0.6);
      ctx.stroke();
    } else {
      ctx.globalAlpha = on ? 1 : 0.9;
      ctx.fillStyle = on ? accent : fill;
      ctx.fillRect(p.x - r, p.y - r, handlePx, handlePx);
      ctx.strokeStyle = color;
      ctx.strokeRect(p.x - r, p.y - r, handlePx, handlePx);
    }
  }
  ctx.restore();
}
