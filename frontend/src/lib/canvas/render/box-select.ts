/// Translucent rectangle for the active box-select drag (canvas
/// coords). Drawn last so it sits above everything else.
export function drawBoxSelect(
  ctx: CanvasRenderingContext2D,
  rect: { startX: number; startY: number; curX: number; curY: number },
  accent: string,
) {
  const x = Math.min(rect.startX, rect.curX);
  const y = Math.min(rect.startY, rect.curY);
  const w = Math.abs(rect.curX - rect.startX);
  const h = Math.abs(rect.curY - rect.startY);
  ctx.save();
  ctx.fillStyle = `${accent}22`;
  ctx.strokeStyle = accent;
  ctx.lineWidth = 1;
  ctx.setLineDash([4, 3]);
  ctx.fillRect(x, y, w, h);
  ctx.strokeRect(x, y, w, h);
  ctx.restore();
}
