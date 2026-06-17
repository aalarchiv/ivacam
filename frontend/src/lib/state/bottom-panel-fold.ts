/// Pure fold-snap mechanics for the phone bottom panels (Operations —
/// `.9` — and G-code — `.11`), kept rune-free so it's unit testable. A
/// panel's open height is one of a few discrete fractions of the
/// viewport; dragging the handle picks the nearest on release, tapping
/// toggles between folded and the last open snap (which persists).
///
/// Snaps are fractions of viewport height so the same numbers work on any
/// screen; the component multiplies by the live viewport px.

/// The fold snap positions, as fractions of viewport height: folded, then
/// 33% / 55% / 75% (user-specified). Sorted ascending.
export const FOLD_SNAPS: readonly number[] = [0, 0.33, 0.55, 0.75] as const;

/// Folded (closed) snap — the panel shows only its handle strip.
export const FOLDED = 0;

/// Default open snap when nothing has been persisted yet — the middle
/// "half" position.
export const DEFAULT_OPEN_SNAP = 0.55;

/// The open (non-folded) snaps, in order — what a drag or tap can land on
/// when unfolding.
export const OPEN_SNAPS: readonly number[] = FOLD_SNAPS.filter((s) => s > 0);

/// Snap a free drag fraction (0..1, where 1 = full viewport height) to the
/// nearest configured snap. Ties resolve to the larger snap (a drag landing
/// exactly between two positions opens further rather than less).
export function nearestSnap(fraction: number): number {
  let best = FOLD_SNAPS[0];
  let bestDist = Infinity;
  for (const s of FOLD_SNAPS) {
    const d = Math.abs(s - fraction);
    if (d <= bestDist) {
      best = s;
      bestDist = d;
    }
  }
  return best;
}

/// Pixel height of a snap given the live viewport height.
export function snapHeightPx(snap: number, viewportPx: number): number {
  return Math.round(snap * viewportPx);
}

/// The snap to restore when the panel is opened (tap or programmatic):
/// the last persisted open position, or the default if none/!open was
/// saved. Guards a persisted `0` (folded) — opening to "folded" is a
/// no-op, so fall back to the default.
export function restoreOpenSnap(savedOpenSnap: number | null | undefined): number {
  if (savedOpenSnap == null || savedOpenSnap <= 0) return DEFAULT_OPEN_SNAP;
  // A persisted value that isn't one of the open snaps (config drift)
  // snaps to the nearest valid open position.
  return OPEN_SNAPS.includes(savedOpenSnap) ? savedOpenSnap : nearestOpenSnap(savedOpenSnap);
}

/// Nearest OPEN snap (never folded) — used when restoring a drifted
/// persisted value, or when a drag should not be allowed to fully close.
export function nearestOpenSnap(fraction: number): number {
  let best = OPEN_SNAPS[0];
  let bestDist = Infinity;
  for (const s of OPEN_SNAPS) {
    const d = Math.abs(s - fraction);
    if (d <= bestDist) {
      best = s;
      bestDist = d;
    }
  }
  return best;
}

/// Result of a tap on the handle: toggle between folded and the last open
/// snap. From folded → open to the restored snap; from any open → folded.
export function toggleFold(current: number, savedOpenSnap: number | null | undefined): number {
  return current > 0 ? FOLDED : restoreOpenSnap(savedOpenSnap);
}
