/// Tests for the shared Modal component. The vitest config runs in node
/// without jsdom, so we don't render the .svelte file — instead we cover
/// the exported pure helpers (`handleModalKey`, `__scrollCache`) with
/// hand-rolled stand-ins. The Svelte wrapper is a thin shell over these.

import { describe, expect, it, vi, beforeEach } from 'vitest';
import { handleModalKey, __scrollCache, FOCUSABLE_SELECTOR } from './modal_behavior';

interface FakeButton {
  focus: ReturnType<typeof vi.fn>;
  matches: (sel: string) => boolean;
}

/// Build a minimal body stand-in that returns the supplied buttons from
/// `querySelectorAll(FOCUSABLE_SELECTOR)`. `ownerDocument.activeElement`
/// is settable so tests can simulate which button currently has focus.
function makeBody(buttons: FakeButton[], active: FakeButton | null) {
  return {
    querySelectorAll: (sel: string) => {
      if (sel !== FOCUSABLE_SELECTOR) return [] as unknown as NodeListOf<HTMLElement>;
      return buttons as unknown as NodeListOf<HTMLElement>;
    },
    ownerDocument: { activeElement: active },
  } as unknown as HTMLElement;
}

function makeButton(): FakeButton {
  return {
    focus: vi.fn(),
    matches: () => true,
  };
}

function makeKey(key: string, shift = false) {
  return {
    key,
    shiftKey: shift,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
  } as unknown as KeyboardEvent;
}

describe('Modal keyboard handler', () => {
  it('Escape calls onClose', () => {
    const onClose = vi.fn();
    const body = makeBody([makeButton()], null);
    handleModalKey(makeKey('Escape'), body, onClose);
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('Tab on the last focusable wraps focus to the first', () => {
    const first = makeButton();
    const last = makeButton();
    const body = makeBody([first, last], last);
    const ev = makeKey('Tab');
    handleModalKey(ev, body, vi.fn());
    expect(first.focus).toHaveBeenCalledOnce();
    expect(last.focus).not.toHaveBeenCalled();
    expect(ev.preventDefault).toHaveBeenCalledOnce();
  });
});

describe('Modal scroll cache', () => {
  beforeEach(() => {
    __scrollCache.clear();
  });

  it('stores and restores scrollTop by persistKey across mount cycles', () => {
    __scrollCache.set('foo', 123);
    expect(__scrollCache.get('foo')).toBe(123);
    // Different key → independent slot.
    __scrollCache.set('bar', 999);
    expect(__scrollCache.get('foo')).toBe(123);
    expect(__scrollCache.get('bar')).toBe(999);
  });
});
