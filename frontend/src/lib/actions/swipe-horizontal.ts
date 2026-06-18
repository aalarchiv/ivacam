/// Horizontal swipe / flick detection as a Svelte action.
///
/// Used on the phone top "‹ screen ›" panel to move between screens. The
/// on-canvas edge zones (EdgeSwipeNav) can't be used on Android because the
/// system back gesture owns the left/right screen edges and consumes the
/// swipe before the WebView sees it — so a screen-switch gesture has to live
/// AWAY from the edges, on the top app bar.
///
/// A swipe counts when travel is mostly horizontal AND either far enough
/// (a deliberate drag) or fast enough (a flick). Taps fall through, so the
/// chevron buttons under the same element keep working.

/// Pure decision so the thresholds are unit-testable without the DOM.
/// `null` = not a horizontal swipe. `'left'` = finger moved left (→ next),
/// `'right'` = finger moved right (→ previous).
export function swipeDirection(
  dx: number,
  dy: number,
  dtMs: number,
  opts: { triggerPx?: number; flickPx?: number; flickMs?: number } = {},
): 'left' | 'right' | null {
  const triggerPx = opts.triggerPx ?? 36;
  const flickPx = opts.flickPx ?? 18;
  const flickMs = opts.flickMs ?? 250;
  // Mostly vertical → let it scroll, ignore.
  if (Math.abs(dx) <= Math.abs(dy)) return null;
  const far = Math.abs(dx) >= triggerPx;
  const flick = Math.abs(dx) >= flickPx && dtMs <= flickMs;
  if (!far && !flick) return null;
  return dx < 0 ? 'left' : 'right';
}

export interface SwipeOpts {
  /// Finger moved left (page forward) → go to the next screen.
  onLeft: () => void;
  /// Finger moved right (page back) → go to the previous screen.
  onRight: () => void;
}

export function swipeHorizontal(node: HTMLElement, opts: SwipeOpts) {
  let o = opts;
  let startX = 0;
  let startY = 0;
  let startT = 0;
  let pid: number | null = null;

  function down(e: PointerEvent) {
    pid = e.pointerId;
    startX = e.clientX;
    startY = e.clientY;
    startT = e.timeStamp;
  }
  function up(e: PointerEvent) {
    if (e.pointerId !== pid) return;
    pid = null;
    const dir = swipeDirection(e.clientX - startX, e.clientY - startY, e.timeStamp - startT);
    if (dir === 'left') o.onLeft();
    else if (dir === 'right') o.onRight();
  }

  node.addEventListener('pointerdown', down);
  node.addEventListener('pointerup', up);
  node.addEventListener('pointercancel', up);
  return {
    update(next: SwipeOpts) {
      o = next;
    },
    destroy() {
      node.removeEventListener('pointerdown', down);
      node.removeEventListener('pointerup', up);
      node.removeEventListener('pointercancel', up);
    },
  };
}
