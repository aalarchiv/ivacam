import type { OSnapCandidate } from '../osnap';
import type { ProjectFn } from './types';

export interface ApproachPreview {
  x: number;
  y: number;
  /// Snap-kind classification so the renderer paints the matching
  /// glyph (square / triangle / X / + / ring); null = free pick.
  snap: OSnapCandidate['kind'] | null;
}

export interface ApproachColors {
  marker: string;
  /// green = locked-to-vertex (matches EstlCam). Callers pull from
  /// `--success` so light theme gets the deeper forest green instead
  /// of #3c3 which gets lost against pale canvas backgrounds.
  snap: string;
  ring: string;
}

/// Paint the approach-point marker (n79) for the currently selected
/// op when it has one set, plus the live preview while in pick mode
/// or actively dragging (preview != null).
export function drawApproachPoint(
  ctx: CanvasRenderingContext2D,
  p: ProjectFn,
  committed: readonly [number, number] | null,
  preview: ApproachPreview | null,
  colors: ApproachColors,
): void {
  // The committed point, when present.
  if (committed) {
    const [sx, sy] = p(committed[0], committed[1]);
    ctx.beginPath();
    ctx.arc(sx, sy, 6, 0, Math.PI * 2);
    ctx.fillStyle = colors.marker;
    ctx.fill();
    ctx.lineWidth = 1.5;
    ctx.strokeStyle = colors.ring;
    ctx.stroke();
    // Inner dot for precision feel.
    ctx.beginPath();
    ctx.arc(sx, sy, 1.5, 0, Math.PI * 2);
    ctx.fillStyle = colors.ring;
    ctx.fill();
  }

  // Live preview during pick / drag.
  if (preview) {
    const [sx, sy] = p(preview.x, preview.y);
    const color = preview.snap ? colors.snap : colors.marker;
    // Dashed ring while picking (vs solid for the committed point)
    // so the user sees clearly which is provisional.
    ctx.save();
    if (!committed) {
      // No committed point yet — make the preview the focal element.
      ctx.beginPath();
      ctx.arc(sx, sy, 6, 0, Math.PI * 2);
      ctx.fillStyle = color;
      ctx.globalAlpha = 0.5;
      ctx.fill();
      ctx.globalAlpha = 1;
    }
    ctx.setLineDash([3, 3]);
    ctx.lineWidth = 1.5;
    ctx.strokeStyle = color;
    ctx.beginPath();
    ctx.arc(sx, sy, 9, 0, Math.PI * 2);
    ctx.stroke();
    ctx.setLineDash([]);
    // Snap glyph by kind (64p):
    //   endpoint     → ■ filled square
    //   midpoint     → ▲ filled triangle
    //   intersection → ✕ diagonal cross
    //   center       → ◯ ring
    //   grid         → + plus sign
    if (preview.snap) {
      drawOSnapGlyph(ctx, sx, sy, preview.snap, colors.snap);
    }
    ctx.restore();
  }
}

/// Paint the OSnap classification glyph (64p) at canvas position
/// (sx, sy). The glyph reads at a glance which CAD feature the
/// cursor latched onto.
export function drawOSnapGlyph(
  ctx: CanvasRenderingContext2D,
  sx: number,
  sy: number,
  kind: OSnapCandidate['kind'],
  color: string,
): void {
  ctx.strokeStyle = color;
  ctx.fillStyle = color;
  ctx.lineWidth = 1.5;
  const r = 7;
  switch (kind) {
    case 'endpoint': {
      // Filled square outline.
      ctx.beginPath();
      ctx.rect(sx - r, sy - r, r * 2, r * 2);
      ctx.stroke();
      break;
    }
    case 'midpoint': {
      // Triangle pointing up, outline only.
      ctx.beginPath();
      ctx.moveTo(sx, sy - r);
      ctx.lineTo(sx + r, sy + r * 0.8);
      ctx.lineTo(sx - r, sy + r * 0.8);
      ctx.closePath();
      ctx.stroke();
      break;
    }
    case 'intersection': {
      // Diagonal cross.
      ctx.beginPath();
      ctx.moveTo(sx - r, sy - r);
      ctx.lineTo(sx + r, sy + r);
      ctx.moveTo(sx - r, sy + r);
      ctx.lineTo(sx + r, sy - r);
      ctx.stroke();
      break;
    }
    case 'center': {
      // Ring (concentric with the preview ring, slightly smaller).
      ctx.beginPath();
      ctx.arc(sx, sy, r * 0.7, 0, Math.PI * 2);
      ctx.stroke();
      break;
    }
    case 'grid': {
      // Axis-aligned plus.
      ctx.beginPath();
      ctx.moveTo(sx - r, sy);
      ctx.lineTo(sx + r, sy);
      ctx.moveTo(sx, sy - r);
      ctx.lineTo(sx, sy + r);
      ctx.stroke();
      break;
    }
  }
}
