import type { Segment } from '../../api/types';
import { drawSegment } from './segment';
import type { ProjectFn } from './types';

/// Imported segments — painted in BASE layer color only, on the heavy
/// bg layer. State-bearing overlays (selection / hover / op-assignment
/// halos) go through drawEntityHalos on the overlay canvas, so editing
/// those does NOT invalidate this layer. `colorFor` resolves an ACI
/// color index to a CSS color (theme-aware, so it stays a callback).
export function drawImportedWireframe(
  ctx: CanvasRenderingContext2D,
  p: ProjectFn,
  segments: readonly Segment[],
  visibleLayers: ReadonlySet<string>,
  lineWidth: number,
  colorFor: (aci: number) => string,
) {
  ctx.lineWidth = lineWidth;
  for (const seg of segments) {
    if (!visibleLayers.has(seg.layer)) continue;
    ctx.strokeStyle = colorFor(seg.color);
    drawSegment(ctx, seg, p);
  }
}

export interface EntityHaloParams {
  segments: readonly Segment[];
  /// Per-segment 1-based object ids (parallel to `segments`).
  objects: readonly number[] | undefined;
  visibleLayers: ReadonlySet<string>;
  selectedObjects: ReadonlySet<number>;
  /// Object id under the cursor (0 = none).
  hoverObjectId: number;
  /// Inverted index objectId → opIds referencing it.
  objectToOps: ReadonlyMap<number, readonly number[]>;
  selectedOpId: number | null;
  /// Per-op source tint — same hue as the op's toolpath in 3D.
  opColor: (opId: number, emphasis: boolean) => string;
  colors: {
    hover: string;
    /// High-contrast outline drawn UNDER selected / hovered / op-assigned
    /// objects so the state stays visible even when the underlying
    /// layer's ACI color happens to match the state color.
    halo: string;
    accent: string;
  };
}

/// State-bearing entity strokes on the overlay canvas: per-op assignment
/// rings, hover highlight, selection halo + accent.
export function drawEntityHalos(
  ctx: CanvasRenderingContext2D,
  p: ProjectFn,
  params: EntityHaloParams,
) {
  const { segments, objects, colors } = params;
  for (let i = 0; i < segments.length; i++) {
    const seg = segments[i];
    if (!params.visibleLayers.has(seg.layer)) continue;
    const objId = objects?.[i] ?? 0;
    if (objId === 0) continue;
    const selected = params.selectedObjects.has(objId);
    const hovered = objId === params.hoverObjectId;
    const assignedOps = params.objectToOps.get(objId);
    if (!selected && !hovered && !assignedOps) continue;

    // Per-op assignment outlines (concentric rings, one band per op).
    // Each assigned op gets the SAME hue here as its toolpath in 3D.
    // When an object belongs to several ops we draw nested rings —
    // widest (outermost) first so narrower bands paint on top:
    // "outline, outline of outline, …". The selected op is ordered
    // innermost and rendered brighter so it reads as the primary
    // assignment without hiding the others.
    if (assignedOps && assignedOps.length > 0) {
      // Selected op last → drawn innermost / on top.
      const ids = [...assignedOps].sort(
        (a, b) =>
          (a === params.selectedOpId ? 1 : 0) - (b === params.selectedOpId ? 1 : 0) || a - b,
      );
      const n = ids.length;
      const step = 2.4;
      const innerWidth = 2.0;
      // Faint contrast halo behind the widest band.
      const prevAlpha = ctx.globalAlpha;
      ctx.globalAlpha = 0.35;
      ctx.lineWidth = innerWidth + (n - 1) * step + 3;
      ctx.strokeStyle = colors.halo;
      drawSegment(ctx, seg, p);
      ctx.globalAlpha = prevAlpha;
      for (let k = 0; k < n; k++) {
        const opId = ids[k];
        // k=0 is the outermost (widest) band; the last is innermost.
        ctx.lineWidth = innerWidth + (n - 1 - k) * step;
        ctx.strokeStyle = params.opColor(opId, opId === params.selectedOpId);
        drawSegment(ctx, seg, p);
      }
    }

    // Hover / selection strokes paint on top so they stay legible even
    // over the assignment rings.
    if (hovered && !selected) {
      ctx.lineWidth = 1.8;
      ctx.strokeStyle = colors.hover;
      drawSegment(ctx, seg, p);
    }
    if (selected) {
      const prevAlpha = ctx.globalAlpha;
      ctx.globalAlpha = 0.6;
      ctx.lineWidth = 2.4 + 3;
      ctx.strokeStyle = colors.halo;
      drawSegment(ctx, seg, p);
      ctx.globalAlpha = prevAlpha;
      ctx.lineWidth = 2.4;
      ctx.strokeStyle = colors.accent;
      drawSegment(ctx, seg, p);
    }
  }
}
