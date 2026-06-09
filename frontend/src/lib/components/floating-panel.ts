/// Pure geometry helpers shared between FloatingPanel.svelte and its unit
/// tests. Kept in a .ts file so vitest can import them without the Svelte
/// plugin (same convention as modal_behavior.ts).

/// A floating panel's viewport rect. `x`/`y` are null until the panel has
/// been opened once — null means "uncomputed, place at the default spot on
/// first open".
export interface PanelRect {
  x: number | null;
  y: number | null;
  w: number;
  h: number;
}

/// Minimum gap kept between the panel and each viewport edge when clamping
/// a dragged position.
export const PANEL_EDGE_MARGIN = 8;

/// How much smaller than the viewport the panel is forced to be on each
/// axis (so an oversized panel can never fully cover the window).
export const PANEL_VIEWPORT_INSET = 16;

/// Clamp a panel rect to the viewport: size is bounded to
/// [min, viewport - inset], then the position (if computed) is bounded so
/// the whole panel stays at least `PANEL_EDGE_MARGIN` px inside each edge.
/// Size is clamped FIRST so the position bounds use the post-clamp size —
/// otherwise shrinking the window could leave the panel pinned off-screen.
export function clampPanelRect(
  rect: PanelRect,
  vw: number,
  vh: number,
  minW: number,
  minH: number,
): PanelRect {
  const w = Math.max(minW, Math.min(vw - PANEL_VIEWPORT_INSET, rect.w));
  const h = Math.max(minH, Math.min(vh - PANEL_VIEWPORT_INSET, rect.h));
  const x =
    rect.x == null
      ? null
      : Math.max(PANEL_EDGE_MARGIN, Math.min(vw - w - PANEL_EDGE_MARGIN, rect.x));
  const y =
    rect.y == null
      ? null
      : Math.max(PANEL_EDGE_MARGIN, Math.min(vh - h - PANEL_EDGE_MARGIN, rect.y));
  return { x, y, w, h };
}

/// First-open default position: top-right with ~1rem (16 px) margins,
/// just below the toolbar. Matches the inline-absolute position the
/// warnings panel had before it became drag-movable.
export function initialPanelPosition(
  vw: number,
  w: number,
  margin = 16,
  top = 56,
): { x: number; y: number } {
  return { x: Math.max(margin, vw - w - margin), y: top };
}
