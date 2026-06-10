/// Pure keyboard / menu decision logic extracted from `App.svelte`.
///
/// App.svelte stays the reactive shell: it owns the dialog `$state`, the
/// dynamic-import component slots, and the actual effects (calling
/// `file_ops`, flipping dialog flags). What lives HERE is the
/// *decisions* — "which app action does this keystroke map to?" and "which
/// menu item does this arrow key focus?" — so the shortcut table and the
/// menubar nav math are unit-testable without standing up the rune
/// runtime. The genuinely component-coupled part (dialog open/close
/// state) is left in App.svelte because extracting it cleanly would mean a
/// rune-store rewrite that buys little.

/// The top menubar menus. Used for the open/close toggle state.
export type MenuId = 'file' | 'edit' | 'view' | 'tools' | 'help';

/// App-level actions a global keystroke can request. App.svelte switches
/// on `kind` and performs the (component-coupled) effect — undo/redo on the
/// project store, opening a file via `file_ops`, flipping a dialog flag.
export type ShortcutAction =
  | 'undo'
  | 'redo'
  | 'open'
  | 'save'
  | 'escape'
  | 'add-text'
  | 'shortcut-help';

/// A resolved shortcut: the action plus whether the caller should
/// `preventDefault()`. Escape is the one action that does NOT prevent the
/// default (it clears selection / closes menus but leaves the keystroke
/// otherwise alone — e.g. a native input can still see it), matching the
/// pre-extraction behavior exactly.
export interface ShortcutResolution {
  action: ShortcutAction;
  preventDefault: boolean;
}

/// True when the event target is a text-entry control, so global
/// single-key shortcuts (undo, "t" for text, …) don't fire while the user
/// is typing into an input / textarea / select / contenteditable.
export function isTypingTarget(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  if (!el) return false;
  const tag = el.tagName ?? '';
  const editable = el.isContentEditable;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || !!editable;
}

/// Map a keydown to an app action, or `null` if the key isn't a shortcut
/// (or is a typing-guarded shortcut fired while the user is typing — in
/// which case the keystroke must pass through untouched). Pure: performs no
/// DOM mutation and never calls `preventDefault` itself; the caller does
/// that iff the returned `preventDefault` is true.
export function resolveShortcut(e: KeyboardEvent): ShortcutResolution | null {
  const mod = e.ctrlKey || e.metaKey;
  if (mod && !e.altKey) {
    const k = e.key.toLowerCase();
    // Ctrl/Cmd combos all defer to the typing guard so the browser's
    // native in-field undo / open / save still work while editing.
    if (k === 'z' && !e.shiftKey) {
      if (isTypingTarget(e.target)) return null;
      return { action: 'undo', preventDefault: true };
    }
    if ((k === 'y' && !e.shiftKey) || (k === 'z' && e.shiftKey)) {
      if (isTypingTarget(e.target)) return null;
      return { action: 'redo', preventDefault: true };
    }
    if (k === 'o' && !e.shiftKey) {
      if (isTypingTarget(e.target)) return null;
      return { action: 'open', preventDefault: true };
    }
    if (k === 's' && !e.shiftKey) {
      if (isTypingTarget(e.target)) return null;
      return { action: 'save', preventDefault: true };
    }
  }
  // Escape always fires (even inside inputs) and does NOT preventDefault.
  if (e.key === 'Escape') {
    return { action: 'escape', preventDefault: false };
  }
  // Bare single-key shortcuts — guarded so they don't steal keystrokes
  // while typing, and only when no modifier is held.
  if ((e.key === 't' || e.key === 'T') && !e.ctrlKey && !e.metaKey && !e.altKey) {
    if (isTypingTarget(e.target)) return null;
    return { action: 'add-text', preventDefault: true };
  }
  if ((e.key === '?' || e.key === 'F1') && !e.ctrlKey && !e.metaKey && !e.altKey) {
    if (isTypingTarget(e.target)) return null;
    return { action: 'shortcut-help', preventDefault: true };
  }
  return null;
}

/// Arrow / Home / End index math for menubar dropdown navigation. Given the
/// pressed key, the currently-focused item index (`-1` when focus is
/// outside the list), and the item count, return the next index to focus —
/// or `null` if the key isn't a navigation key (caller leaves focus alone).
/// Down/Up wrap around; Home/End jump to the ends. Mirrors the WAI-ARIA
/// `role="menu"` pattern. The DOM query + `.focus()` stay in App.svelte.
export function nextMenuItemIndex(key: string, currentIdx: number, count: number): number | null {
  if (count <= 0) return null;
  if (key === 'ArrowDown') return currentIdx < 0 ? 0 : (currentIdx + 1) % count;
  if (key === 'ArrowUp') return currentIdx <= 0 ? count - 1 : currentIdx - 1;
  if (key === 'Home') return 0;
  if (key === 'End') return count - 1;
  return null;
}

/// True when a drag event carries a `Files` payload — the gate for the
/// window-level drag-and-drop import overlay.
export function dragHasFiles(e: DragEvent): boolean {
  return !!e.dataTransfer && Array.from(e.dataTransfer.types).includes('Files');
}
