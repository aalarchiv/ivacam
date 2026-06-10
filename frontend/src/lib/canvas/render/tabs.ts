import { polylineAtT, type ObjectPolyline } from '../../cam/tabs';
import type { TabPlacement, TabPlacementMode } from '../../state/project-types';
import type { GhostTab } from '../ghost-tab';
import type { ProjectFn } from './types';

/// The slice of a contour op (profile / pocket) the tab painter reads.
/// Structural, so the component can pass its richer OpEntry objects
/// (pre-filtered through isContourOp) without the render layer importing
/// the op model.
export interface TabRenderOp {
  id: number;
  tabMode?: TabPlacementMode;
  tabsActive?: boolean;
  tabPlacements?: readonly TabPlacement[];
  sourceObjects?: readonly number[] | null;
  tabWidth?: number;
  tabHeight?: number;
}

export interface TabColors {
  /// Manual-placement marker fill.
  fill: string;
  /// Auto-spaced marker fill (lighter, so manual placements stand out).
  auto: string;
  /// Marker outline — the canvas bg color so markers read as cutouts.
  stroke: string;
  /// Snap-indicator dot accent.
  accent: string;
}

/// Walk every op with tabs ON: render auto-spaced (per kind), manual
/// placements, and the ghost (if the selected op). Tabs are only
/// meaningful for closed-contour ops (profile + pocket), so callers
/// pass a pre-narrowed list.
export function drawTabs(
  ctx: CanvasRenderingContext2D,
  p: ProjectFn,
  scale: number,
  ops: readonly TabRenderOp[],
  objects: readonly ObjectPolyline[],
  ghost: { tab: GhostTab; op: TabRenderOp } | null,
  colors: TabColors,
) {
  for (const op of ops) {
    const mode = op.tabMode?.kind ?? 'off';
    const tabsActive = op.tabsActive ?? false;
    // Skip ops with no tabs to draw.
    if (mode === 'off' && (op.tabPlacements?.length ?? 0) === 0 && !tabsActive) continue;
    const allowedObjects = op.sourceObjects;
    const objFilter = (id: number) =>
      !allowedObjects || allowedObjects.length === 0 || allowedObjects.includes(id);
    // Manual / Mixed placements.
    if (mode === 'manual' || mode === 'mixed') {
      for (const tp of op.tabPlacements ?? []) {
        const obj = objects.find((o) => o.objectId === tp.objectId);
        if (!obj || !objFilter(obj.objectId)) continue;
        const { point, tangent } = polylineAtT(obj.pts, tp.t, obj.closed);
        drawTabMarker(
          ctx,
          p,
          scale,
          point.x,
          point.y,
          tangent.x,
          tangent.y,
          tp.widthOverrideMm ?? op.tabWidth ?? 10,
          tp.heightOverrideMm ?? op.tabHeight ?? 1,
          colors.fill,
          colors.stroke,
        );
      }
    }
    // Auto / Mixed: N evenly spaced tabs per allowed object.
    if (op.tabMode?.kind === 'auto' || op.tabMode?.kind === 'mixed') {
      const count = op.tabMode.kind === 'auto' ? op.tabMode.count : op.tabMode.autoCount;
      if (count > 0) {
        for (const obj of objects) {
          if (!objFilter(obj.objectId)) continue;
          const ts = obj.closed
            ? Array.from({ length: count }, (_, i) => i / count)
            : Array.from({ length: count }, (_, i) => (i + 0.5) / count);
          for (const t of ts) {
            const { point, tangent } = polylineAtT(obj.pts, t, obj.closed);
            drawTabMarker(
              ctx,
              p,
              scale,
              point.x,
              point.y,
              tangent.x,
              tangent.y,
              op.tabWidth ?? 10,
              op.tabHeight ?? 1,
              colors.auto,
              colors.stroke,
            );
          }
        }
      }
    }
  }
  // Ghost (selected op + manual/mixed mode + cursor over contour).
  if (ghost) {
    const obj = objects.find((o) => o.objectId === ghost.tab.objectId);
    if (obj) {
      const { tangent } = polylineAtT(obj.pts, ghost.tab.t, obj.closed);
      ctx.save();
      ctx.globalAlpha = 0.4;
      ctx.setLineDash([4, 3]);
      drawTabMarker(
        ctx,
        p,
        scale,
        ghost.tab.x,
        ghost.tab.y,
        tangent.x,
        tangent.y,
        ghost.op.tabWidth ?? 10,
        ghost.op.tabHeight ?? 1,
        colors.fill,
        colors.stroke,
      );
      ctx.restore();
      // Snap indicator: a small accent dot next to the
      // ghost when the cursor caught a secondary snap target
      // (vertex / midpoint / existing tab).
      if (ghost.tab.snap !== 'contour') {
        const [gx, gy] = p(ghost.tab.x, ghost.tab.y);
        ctx.beginPath();
        ctx.arc(gx, gy, 3.5, 0, Math.PI * 2);
        ctx.fillStyle = colors.accent;
        ctx.fill();
        ctx.lineWidth = 1;
        ctx.strokeStyle = colors.stroke;
        ctx.stroke();
      }
    }
  }
}

/// Draw one tab marker oriented along the contour tangent. Falls
/// back to a 6-px pill when the data-space size collapses too small
/// on screen so the marker stays visible at extreme zoom-out.
export function drawTabMarker(
  ctx: CanvasRenderingContext2D,
  p: ProjectFn,
  scale: number,
  dataX: number,
  dataY: number,
  tanX: number,
  tanY: number,
  widthMm: number,
  heightMm: number,
  fill: string,
  stroke: string,
) {
  const [cx, cy] = p(dataX, dataY);
  const halfLenPx = Math.max(3, widthMm * 0.5 * scale);
  const halfThickPx = Math.max(2, heightMm * scale);
  // Canvas Y is flipped vs data Y. Mirror the tangent Y so the
  // rendered orientation matches the contour in screen space.
  const txPx = tanX;
  const tyPx = -tanY;
  const tLen = Math.hypot(txPx, tyPx) || 1;
  const ux = txPx / tLen;
  const uy = tyPx / tLen;
  // Perpendicular (left of tangent in canvas space).
  const px = -uy;
  const py = ux;
  ctx.beginPath();
  const corners: [number, number][] = [
    [cx - ux * halfLenPx - px * halfThickPx, cy - uy * halfLenPx - py * halfThickPx],
    [cx + ux * halfLenPx - px * halfThickPx, cy + uy * halfLenPx - py * halfThickPx],
    [cx + ux * halfLenPx + px * halfThickPx, cy + uy * halfLenPx + py * halfThickPx],
    [cx - ux * halfLenPx + px * halfThickPx, cy - uy * halfLenPx + py * halfThickPx],
  ];
  ctx.moveTo(corners[0][0], corners[0][1]);
  for (let i = 1; i < corners.length; i++) ctx.lineTo(corners[i][0], corners[i][1]);
  ctx.closePath();
  ctx.fillStyle = fill;
  ctx.fill();
  ctx.lineWidth = 1.25;
  ctx.strokeStyle = stroke;
  ctx.stroke();
}
