/// Pure helpers shared between Modal.svelte and its unit tests. Kept in a
/// .ts file so vitest can import them without the Svelte plugin.

/// Module-level scroll cache, shared across modal instances by persistKey.
/// Survives close+reopen of the same dialog until the page reloads.
export const __scrollCache = new Map<string, number>();

/// Module-level cache of dragged/resized modal geometry, keyed by
/// persistKey, so a draggable dialog reopens where the user left it
/// null width/height means "not yet resized" → intrinsic size.
export interface ModalGeom {
  left: number;
  top: number;
  width: number | null;
  height: number | null;
}
export const __geomCache = new Map<string, ModalGeom>();

/// Centered top-left for a modal of size (w, h) in a viewport (vw, vh).
/// Clamped to >= 0 so an oversized modal starts at the top-left corner
/// rather than off-screen-negative.
export function centeredModalPosition(
  w: number,
  h: number,
  vw: number,
  vh: number,
): { left: number; top: number } {
  return {
    left: Math.max(0, Math.round((vw - w) / 2)),
    top: Math.max(0, Math.round((vh - h) / 2)),
  };
}

/// Clamp a proposed modal top-left so the drag handle (header) stays
/// grabbable: never let the header go above the viewport top or below
/// its bottom, and always keep `edgeMargin` px of the modal reachable
/// on each horizontal side.
export function clampModalPosition(
  left: number,
  top: number,
  modalW: number,
  vw: number,
  vh: number,
  headerH: number,
  edgeMargin = 60,
): { left: number; top: number } {
  const margin = Math.min(edgeMargin, modalW);
  const minLeft = margin - modalW;
  const maxLeft = Math.max(minLeft, vw - margin);
  const maxTop = Math.max(0, vh - Math.max(1, headerH));
  return {
    left: Math.min(maxLeft, Math.max(minLeft, left)),
    top: Math.min(maxTop, Math.max(0, top)),
  };
}

/// Selector for elements that should participate in the focus trap.
export const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/// Handle Escape / Tab inside the modal. Escape calls onClose; Tab wraps
/// focus to the first or last focusable element when it would otherwise
/// leave the body. If the currently active element is OUTSIDE the modal
/// body (e.g. focus is still on the trigger after open, or on the body
/// itself), Tab forces focus into the first focusable so the trap engages.
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
    // Duck-type `body.contains` rather than relying on `instanceof Node`
    // — the unit tests run in plain Node (no jsdom), so DOM globals
    // are absent and the stand-in body has only the methods the trap
    // actually uses.
    const bodyContains = (body as { contains?: (n: unknown) => boolean }).contains;
    const insideModal =
      active != null && typeof bodyContains === 'function'
        ? bodyContains.call(body, active)
        : false;
    if (!insideModal) {
      // Focus has not yet entered the modal — pull it in regardless of
      // shift direction. Without this, the first Tab after opening
      // a modal sends focus to the next element in document order
      // (often a button OUTSIDE the dialog), defeating the trap.
      e.preventDefault();
      (e.shiftKey ? last : first).focus();
      return;
    }
    if (e.shiftKey && active === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  }
}
