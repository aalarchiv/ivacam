/// Pure helpers shared between Modal.svelte and its unit tests. Kept in a
/// .ts file so vitest can import them without the Svelte plugin.

/// Module-level scroll cache, shared across modal instances by persistKey.
/// Survives close+reopen of the same dialog until the page reloads.
export const __scrollCache = new Map<string, number>();

/// Selector for elements that should participate in the focus trap.
export const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/// Handle Escape / Tab inside the modal. Escape calls onClose; Tab wraps
/// focus to the first or last focusable element when it would otherwise
/// leave the body.
export function handleModalKey(
  e: KeyboardEvent,
  body: HTMLElement | null,
  onClose: () => void,
): void {
  if (e.key === 'Escape') {
    e.stopPropagation();
    onClose();
    return;
  }
  if (e.key === 'Tab') {
    if (!body) return;
    const focusables = body.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR);
    if (focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    const active = (body.ownerDocument ?? globalThis.document).activeElement;
    if (e.shiftKey && active === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  }
}
