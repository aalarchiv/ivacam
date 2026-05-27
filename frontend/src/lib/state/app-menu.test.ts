import { describe, expect, it } from 'vitest';
import {
  isTypingTarget,
  resolveShortcut,
  nextMenuItemIndex,
  dragHasFiles,
  type ShortcutResolution,
} from './app-menu';

// The vitest env is 'node' (no DOM), but these functions only read a few
// properties off the event / target, so we duck-type minimal fakes and
// cast. This keeps the pure decision logic testable without jsdom.
function key(
  k: string,
  mods: Partial<{ ctrl: boolean; meta: boolean; alt: boolean; shift: boolean }> = {},
  target: unknown = null,
): KeyboardEvent {
  return {
    key: k,
    ctrlKey: mods.ctrl ?? false,
    metaKey: mods.meta ?? false,
    altKey: mods.alt ?? false,
    shiftKey: mods.shift ?? false,
    target,
  } as unknown as KeyboardEvent;
}
const INPUT = { tagName: 'INPUT', isContentEditable: false };
const DIV = { tagName: 'DIV', isContentEditable: false };

describe('isTypingTarget', () => {
  it('flags inputs / textareas / selects / contenteditable', () => {
    expect(isTypingTarget({ tagName: 'INPUT' } as unknown as EventTarget)).toBe(true);
    expect(isTypingTarget({ tagName: 'TEXTAREA' } as unknown as EventTarget)).toBe(true);
    expect(isTypingTarget({ tagName: 'SELECT' } as unknown as EventTarget)).toBe(true);
    expect(
      isTypingTarget({ tagName: 'DIV', isContentEditable: true } as unknown as EventTarget),
    ).toBe(true);
  });
  it('is false for null and non-editable elements', () => {
    expect(isTypingTarget(null)).toBe(false);
    expect(isTypingTarget({ tagName: 'BUTTON' } as unknown as EventTarget)).toBe(false);
  });
});

describe('resolveShortcut', () => {
  const res = (k: string, mods = {}, target: unknown = DIV): ShortcutResolution | null =>
    resolveShortcut(key(k, mods, target));

  it('maps ctrl/cmd combos to undo/redo/open/save and preventsDefault', () => {
    expect(res('z', { ctrl: true })).toEqual({ action: 'undo', preventDefault: true });
    expect(res('z', { meta: true })).toEqual({ action: 'undo', preventDefault: true });
    expect(res('y', { ctrl: true })).toEqual({ action: 'redo', preventDefault: true });
    expect(res('z', { ctrl: true, shift: true })).toEqual({ action: 'redo', preventDefault: true });
    expect(res('o', { ctrl: true })).toEqual({ action: 'open', preventDefault: true });
    expect(res('s', { ctrl: true })).toEqual({ action: 'save', preventDefault: true });
  });

  it('is case-insensitive on the combo key', () => {
    expect(res('Z', { ctrl: true })).toEqual({ action: 'undo', preventDefault: true });
    expect(res('S', { meta: true })).toEqual({ action: 'save', preventDefault: true });
  });

  it('suppresses ctrl combos while typing so the field keeps native undo/save', () => {
    expect(res('z', { ctrl: true }, INPUT)).toBeNull();
    expect(res('s', { ctrl: true }, INPUT)).toBeNull();
  });

  it('ignores ctrl combos when Alt is also held (reserved for other bindings)', () => {
    expect(res('z', { ctrl: true, alt: true })).toBeNull();
  });

  it('Escape always fires and does NOT preventDefault, even inside inputs', () => {
    expect(res('Escape')).toEqual({ action: 'escape', preventDefault: false });
    expect(res('Escape', {}, INPUT)).toEqual({ action: 'escape', preventDefault: false });
  });

  it('maps bare "t" / "?" / F1 to dialogs, guarded against typing and modifiers', () => {
    expect(res('t')).toEqual({ action: 'add-text', preventDefault: true });
    expect(res('T')).toEqual({ action: 'add-text', preventDefault: true });
    expect(res('?')).toEqual({ action: 'shortcut-help', preventDefault: true });
    expect(res('F1')).toEqual({ action: 'shortcut-help', preventDefault: true });
    expect(res('t', {}, INPUT)).toBeNull();
    expect(res('t', { ctrl: true })).toBeNull();
  });

  it('returns null for unmapped keys', () => {
    expect(res('a')).toBeNull();
    expect(res('Enter')).toBeNull();
  });
});

describe('nextMenuItemIndex', () => {
  it('ArrowDown advances and wraps; starts at 0 when focus is outside', () => {
    expect(nextMenuItemIndex('ArrowDown', -1, 3)).toBe(0);
    expect(nextMenuItemIndex('ArrowDown', 0, 3)).toBe(1);
    expect(nextMenuItemIndex('ArrowDown', 2, 3)).toBe(0);
  });
  it('ArrowUp retreats and wraps; from outside goes to last', () => {
    expect(nextMenuItemIndex('ArrowUp', -1, 3)).toBe(2);
    expect(nextMenuItemIndex('ArrowUp', 0, 3)).toBe(2);
    expect(nextMenuItemIndex('ArrowUp', 2, 3)).toBe(1);
  });
  it('Home / End jump to the ends', () => {
    expect(nextMenuItemIndex('Home', 2, 3)).toBe(0);
    expect(nextMenuItemIndex('End', 0, 3)).toBe(2);
  });
  it('returns null for non-nav keys and empty lists', () => {
    expect(nextMenuItemIndex('Enter', 0, 3)).toBeNull();
    expect(nextMenuItemIndex('ArrowDown', 0, 0)).toBeNull();
  });
});

describe('dragHasFiles', () => {
  const drag = (types: string[] | null): DragEvent =>
    ({ dataTransfer: types === null ? null : { types } }) as unknown as DragEvent;
  it('is true only when the payload includes Files', () => {
    expect(dragHasFiles(drag(['Files']))).toBe(true);
    expect(dragHasFiles(drag(['text/plain', 'Files']))).toBe(true);
    expect(dragHasFiles(drag(['text/plain']))).toBe(false);
    expect(dragHasFiles(drag(null))).toBe(false);
  });
});
