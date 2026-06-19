/// Long-press tooltip action (7jug.4). On coarse pointers (touch / pen)
/// the native `title=` tooltip never shows — there is no hover — so
/// icon-only controls lose their only label. This Svelte action surfaces
/// an element's `title` (or `aria-label`, or an explicit override) as a
/// floating tooltip after a ~500ms press-and-hold — the Android-standard
/// discoverability gesture — and swallows the tap that would otherwise
/// fire the control. On mouse pointers it no-ops: the native hover tooltip
/// already works, so we don't double up.

import { LONG_PRESS_MS, withinTapTolerance, type PointerPos } from '../canvas/touch-gestures';

/// Gap (px) between the anchor and the tooltip, and the viewport margin
/// the tooltip is kept clear of.
const GAP_PX = 8;
const MARGIN_PX = 4;

export interface Rect {
  top: number;
  bottom: number;
  left: number;
  width: number;
}
export interface Size {
  width: number;
  height: number;
}
export interface Viewport {
  width: number;
}

/// Pure placement math: centre the tooltip horizontally over the anchor
/// and sit it just above; flip below when there's no room above, and clamp
/// horizontally into the viewport. Extracted so it's unit-testable without
/// a DOM.
export function tooltipPosition(
  anchor: Rect,
  tip: Size,
  viewport: Viewport,
): { top: number; left: number } {
  let top = anchor.top - tip.height - GAP_PX;
  if (top < MARGIN_PX) top = anchor.bottom + GAP_PX; // no room above → below
  let left = anchor.left + anchor.width / 2 - tip.width / 2;
  const maxLeft = viewport.width - tip.width - MARGIN_PX;
  left = Math.max(MARGIN_PX, Math.min(left, maxLeft));
  return { top: Math.round(top), left: Math.round(left) };
}

export interface LongpressTooltipOptions {
  /// Explicit tooltip text. Falls back to the element's `title`, then
  /// `aria-label`, when omitted.
  text?: string;
}

export function longpressTooltip(node: HTMLElement, options: LongpressTooltipOptions = {}) {
  let opts = options;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let start: PointerPos = { x: 0, y: 0 };
  let fired = false;
  let tip: HTMLDivElement | null = null;

  function text(): string {
    return opts.text ?? node.getAttribute('title') ?? node.getAttribute('aria-label') ?? '';
  }
  function isCoarse(e: PointerEvent): boolean {
    return e.pointerType === 'touch' || e.pointerType === 'pen';
  }
  function clearTimer() {
    if (timer != null) {
      clearTimeout(timer);
      timer = null;
    }
  }
  function hide() {
    if (tip) {
      tip.remove();
      tip = null;
    }
  }
  function show() {
    const label = text();
    if (!label) return;
    fired = true;
    tip = document.createElement('div');
    tip.className = 'lp-tooltip';
    tip.setAttribute('role', 'tooltip');
    tip.textContent = label;
    document.body.appendChild(tip);
    const anchor = node.getBoundingClientRect();
    const t = tip.getBoundingClientRect();
    const { top, left } = tooltipPosition(
      anchor,
      { width: t.width, height: t.height },
      {
        width: window.innerWidth,
      },
    );
    tip.style.top = `${top}px`;
    tip.style.left = `${left}px`;
  }

  function onPointerDown(e: PointerEvent) {
    if (!isCoarse(e)) return;
    fired = false;
    start = { x: e.clientX, y: e.clientY };
    clearTimer();
    timer = setTimeout(show, LONG_PRESS_MS);
  }
  function onPointerMove(e: PointerEvent) {
    if (timer == null) return;
    if (!withinTapTolerance(start, { x: e.clientX, y: e.clientY })) clearTimer();
  }
  function onPointerUp() {
    clearTimer();
    if (fired) {
      // Swallow the click that follows a fired long-press so the control's
      // own action (delete, duplicate, …) doesn't trigger.
      const swallow = (ev: Event) => {
        ev.preventDefault();
        ev.stopPropagation();
      };
      node.addEventListener('click', swallow, { capture: true, once: true });
      setTimeout(() => node.removeEventListener('click', swallow, { capture: true }), 400);
    }
    if (tip) setTimeout(hide, 1200); // leave it up briefly to be read
  }
  function onPointerCancel() {
    clearTimer();
    hide();
  }

  node.addEventListener('pointerdown', onPointerDown);
  node.addEventListener('pointermove', onPointerMove);
  node.addEventListener('pointerup', onPointerUp);
  node.addEventListener('pointercancel', onPointerCancel);

  return {
    update(next: LongpressTooltipOptions = {}) {
      opts = next;
    },
    destroy() {
      clearTimer();
      hide();
      node.removeEventListener('pointerdown', onPointerDown);
      node.removeEventListener('pointermove', onPointerMove);
      node.removeEventListener('pointerup', onPointerUp);
      node.removeEventListener('pointercancel', onPointerCancel);
    },
  };
}
