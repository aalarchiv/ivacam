/// Pure helpers backing LoadingOverlay.svelte. Kept in a plain .ts so the
/// logic-only vitest config can cover them without spinning up jsdom.

/// Returns the message to show under the spinner. Falls back to a
/// generic label when the caller didn't supply one — keeps the overlay
/// looking intentional rather than blank.
export function loadingMessage(input: string | null | undefined): string {
  if (input == null) return 'Loading…';
  const trimmed = input.trim();
  return trimmed.length === 0 ? 'Loading…' : trimmed;
}

/// `true` when the overlay should actually render. Separated so the
/// component template stays trivial.
export function shouldShow(visible: boolean): boolean {
  return visible === true;
}
