/// Reactive layout-tier singleton: tracks the viewport width tier
/// (`phone` / `tablet` / `desktop`) via `window.matchMedia` so the shell
/// and components can branch on it reactively. The pure tier math lives
/// in `layout.ts`; this file is just the rune-bound wrapper.
///
/// matchMedia listeners (vs. a resize handler) fire only when a
/// breakpoint is actually crossed, so this costs nothing on every resize
/// frame. In a window-less environment (SSR / node test runner) the
/// constructor no-ops and the tier stays at its `desktop` default.

import {
  tierForWidth,
  isNarrowTier,
  PHONE_TABLET_BREAKPOINT_PX,
  TABLET_DESKTOP_BREAKPOINT_PX,
  type LayoutTier,
} from './layout';

class LayoutState {
  /// Current viewport tier. Defaults to `desktop` so a window-less
  /// import (tests / SSR) renders the unchanged desktop layout.
  tier = $state<LayoutTier>('desktop');

  constructor() {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
      return;
    }
    this.#recompute();
    // One listener per breakpoint edge; either crossing recomputes the
    // tier from the live width. `change` only fires on a threshold cross.
    const onChange = () => this.#recompute();
    for (const px of [PHONE_TABLET_BREAKPOINT_PX, TABLET_DESKTOP_BREAKPOINT_PX]) {
      window.matchMedia(`(min-width: ${px}px)`).addEventListener('change', onChange);
    }
  }

  #recompute() {
    this.tier = tierForWidth(window.innerWidth);
  }

  /// Phone tier (< 640px): Design surface + Operations bottom sheet.
  get isPhone(): boolean {
    return this.tier === 'phone';
  }

  /// Tablet tier (640–1024px): single canvas + collapsible sidebar.
  get isTablet(): boolean {
    return this.tier === 'tablet';
  }

  /// Desktop tier (>= 1024px): today's 3-column grid, unchanged.
  get isDesktop(): boolean {
    return this.tier === 'desktop';
  }

  /// True for any tier narrower than desktop — the shell collapses the
  /// 3-column grid when this holds.
  get isNarrow(): boolean {
    return isNarrowTier(this.tier);
  }
}

export const layout = new LayoutState();
