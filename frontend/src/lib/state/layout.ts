/// Pure responsive-layout breakpoint logic, kept rune-free so it's unit
/// testable without the Svelte runtime. The reactive singleton that wires
/// these to `window.matchMedia` lives in `layout.svelte.ts`.
///
/// Three width tiers drive the whole responsive shell:
///   - `phone`   (< 640px): full-screen Design surface + Operations sheet
///   - `tablet`  (640–1024px): single canvas + collapsible sidebar
///   - `desktop` (>= 1024px): today's 3-column grid, unchanged
/// `desktop` is the safe default (matches the existing layout) when no
/// window is available (SSR / node test env).

/// Boundary (px) between the `phone` and `tablet` tiers. A viewport
/// strictly narrower than this is a phone.
export const PHONE_TABLET_BREAKPOINT_PX = 640;

/// Boundary (px) between the `tablet` and `desktop` tiers. A viewport at
/// least this wide gets the untouched desktop 3-column grid.
export const TABLET_DESKTOP_BREAKPOINT_PX = 1024;

export type LayoutTier = 'phone' | 'tablet' | 'desktop';

/// Map a viewport width (px) to its layout tier. Boundaries are
/// lower-inclusive: exactly 640 is `tablet`, exactly 1024 is `desktop`,
/// matching the `(min-width: …)` media queries the reactive slice uses.
export function tierForWidth(widthPx: number): LayoutTier {
  if (widthPx < PHONE_TABLET_BREAKPOINT_PX) return 'phone';
  if (widthPx < TABLET_DESKTOP_BREAKPOINT_PX) return 'tablet';
  return 'desktop';
}

/// True when the tier should collapse the desktop 3-column grid — i.e.
/// anything narrower than `desktop`. The shell keys overlay/swap layout
/// off this; per-tier divergence (sheet vs. drawer) keys off the tier.
export function isNarrowTier(tier: LayoutTier): boolean {
  return tier !== 'desktop';
}
