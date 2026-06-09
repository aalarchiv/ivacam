import type { Segment } from '../../api/types';
import type { ProjectFn } from './types';
import { drawSegment } from './segment';

export interface TextPreviewLayer {
  /// Cached preview segments, already translated to the layer's current
  /// origin (k9cz) — a drag repositions the glyphs with no re-render.
  segments: readonly Segment[];
  isActive: boolean;
}

export interface TextPreviewColors {
  accent: string;
  halo: string;
  idle: string;
}

/// Render every TextLayer's cached preview segments. The active layer
/// gets a bright halo + accent stroke; idle layers render in the
/// muted assigned-other tint so they're visible but don't draw the
/// eye.
export function drawTextPreview(
  ctx: CanvasRenderingContext2D,
  p: ProjectFn,
  layers: readonly TextPreviewLayer[],
  colors: TextPreviewColors,
) {
  for (const layer of layers) {
    if (layer.segments.length === 0) continue;
    const baseWidth = layer.isActive ? 1.8 : 1.4;
    const haloAlpha = layer.isActive ? 0.55 : 0.3;
    for (const seg of layer.segments) {
      const prevAlpha = ctx.globalAlpha;
      ctx.globalAlpha = haloAlpha;
      ctx.lineWidth = baseWidth + 2.5;
      ctx.strokeStyle = colors.halo;
      drawSegment(ctx, seg, p);
      ctx.globalAlpha = prevAlpha;
      ctx.lineWidth = baseWidth;
      ctx.strokeStyle = layer.isActive ? colors.accent : colors.idle;
      drawSegment(ctx, seg, p);
    }
  }
}
