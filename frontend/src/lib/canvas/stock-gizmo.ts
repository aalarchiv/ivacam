/// Pure geometry for the on-canvas stock gizmo (7jug.15). On phone there
/// is no Stock sidebar, so the drawn stock rectangle becomes directly
/// manipulable: a centre puck moves it, and eight edge/corner handles
/// resize it. This module holds the rune-free math — handle screen
/// positions (for rendering + hit-testing) and the resize → stock-field
/// inversion — so it's unit-testable without the canvas or the rune
/// runtime. The component owns the pointer plumbing and the undo-coalesced
/// `project.setStock` calls.
///
/// Coordinate conventions match EntityCanvas2D's transform:
///   screen.x = world.x * scale + offX
///   screen.y = offY - world.y * scale      (world +Y is up → smaller screen y)
/// so the screen-top edge is the world `maxY` and screen-bottom is `minY`.

/// Resize handles plus the move puck. n/s/e/w name the SCREEN edge:
/// n(top)=world maxY, s(bottom)=world minY, e(right)=maxX, w(left)=minX.
export type StockHandleKind = 'move' | 'n' | 's' | 'e' | 'w' | 'ne' | 'nw' | 'se' | 'sw';

/// The eight resize handles, no move puck — what `dragStockBox` accepts.
export type StockResizeKind = Exclude<StockHandleKind, 'move'>;

export interface WorldBox {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

export interface ScreenXform {
  scale: number;
  offX: number;
  offY: number;
}

export interface Pt {
  x: number;
  y: number;
}

/// World mm → screen px (EntityCanvas2D's projection).
export function worldToScreen(wx: number, wy: number, t: ScreenXform): Pt {
  return { x: wx * t.scale + t.offX, y: t.offY - wy * t.scale };
}

/// Screen-px positions of every gizmo handle for a stock box.
/// Used by both the renderer (draw the squares) and the hit-tester.
export function stockHandleScreenPositions(
  box: WorldBox,
  t: ScreenXform,
): Record<StockHandleKind, Pt> {
  const cx = (box.minX + box.maxX) * 0.5;
  const cy = (box.minY + box.maxY) * 0.5;
  return {
    nw: worldToScreen(box.minX, box.maxY, t),
    n: worldToScreen(cx, box.maxY, t),
    ne: worldToScreen(box.maxX, box.maxY, t),
    w: worldToScreen(box.minX, cy, t),
    move: worldToScreen(cx, cy, t),
    e: worldToScreen(box.maxX, cy, t),
    sw: worldToScreen(box.minX, box.minY, t),
    s: worldToScreen(cx, box.minY, t),
    se: worldToScreen(box.maxX, box.minY, t),
  };
}

/// Priority order for hit-testing: corners first (they sit on top of the
/// edge handles spatially), then edges, then the centre move puck.
const HIT_ORDER: StockHandleKind[] = ['ne', 'nw', 'se', 'sw', 'n', 's', 'e', 'w', 'move'];

/// Which handle (if any) a screen-space point grabs, within `tolPx`.
/// Returns the highest-priority handle whose centre is within tolerance.
export function hitStockHandle(
  box: WorldBox,
  t: ScreenXform,
  px: number,
  py: number,
  tolPx: number,
): StockHandleKind | null {
  const pos = stockHandleScreenPositions(box, t);
  const tol2 = tolPx * tolPx;
  let best: StockHandleKind | null = null;
  let bestD = tol2;
  for (const kind of HIT_ORDER) {
    const h = pos[kind];
    const dx = h.x - px;
    const dy = h.y - py;
    const d = dx * dx + dy * dy;
    // Strictly-closer within tolerance; ties keep the higher-priority
    // (earlier) handle since we only replace on a smaller distance.
    if (d < bestD) {
      bestD = d;
      best = kind;
    }
  }
  return best;
}

/// New world box after dragging a resize handle to `curWorld`, given the
/// box at grab time and the world point first grabbed. The opposite
/// edge(s) stay fixed; the dragged edge(s) clamp so the box never shrinks
/// below `minSize` in either axis (the clamp pins the dragged edge, not
/// the anchor).
export function dragStockBox(
  kind: StockResizeKind,
  startBox: WorldBox,
  grabWorld: Pt,
  curWorld: Pt,
  minSize: number,
): WorldBox {
  const dx = curWorld.x - grabWorld.x;
  const dy = curWorld.y - grabWorld.y;
  let { minX, minY, maxX, maxY } = startBox;

  if (kind.includes('e')) maxX = startBox.maxX + dx;
  if (kind.includes('w')) minX = startBox.minX + dx;
  if (kind.includes('n')) maxY = startBox.maxY + dy; // top edge = world maxY
  if (kind.includes('s')) minY = startBox.minY + dy; // bottom edge = world minY

  // Min-size clamp: pin the dragged edge so the box can't invert/collapse.
  if (maxX - minX < minSize) {
    if (kind.includes('e')) maxX = minX + minSize;
    else if (kind.includes('w')) minX = maxX - minSize;
  }
  if (maxY - minY < minSize) {
    if (kind.includes('n')) maxY = minY + minSize;
    else if (kind.includes('s')) minY = maxY - minSize;
  }
  return { minX, minY, maxX, maxY };
}

/// Invert a desired world box back into manual-stock fields. The manual
/// footprint is `customX × customY` centred on the imported bbox centre
/// plus `(offsetX, offsetY)` (see sim/footprint.ts), so a resize must set
/// both the size AND the offset that re-centres the box where the user
/// dragged it. `bboxCenter` is the imported geometry's centre, or null
/// (origin) when nothing is loaded.
export function boxToStock(
  box: WorldBox,
  bboxCenter: Pt | null,
): { mode: 'manual'; customX: number; customY: number; offsetX: number; offsetY: number } {
  const bx = bboxCenter?.x ?? 0;
  const by = bboxCenter?.y ?? 0;
  return {
    mode: 'manual',
    customX: box.maxX - box.minX,
    customY: box.maxY - box.minY,
    offsetX: (box.minX + box.maxX) * 0.5 - bx,
    offsetY: (box.minY + box.maxY) * 0.5 - by,
  };
}
