/// Tests for the shared Modal component. The vitest config runs in node
/// without jsdom, so we don't render the .svelte file — instead we cover
/// the exported pure helpers (`handleModalKey`, `__scrollCache`) with
/// hand-rolled stand-ins. The Svelte wrapper is a thin shell over these.

import { describe, expect, it, vi, beforeEach } from 'vitest';
import {
  handleModalKey,
  __scrollCache,
  __geomCache,
  centeredModalPosition,
  clampModalPosition,
  FOCUSABLE_SELECTOR,
} from './modal_behavior';

interface FakeButton {
  focus: ReturnType<typeof vi.fn>;
  matches: (sel: string) => boolean;
}

/// Build a minimal body stand-in that returns the supplied buttons from
/// `querySelectorAll(FOCUSABLE_SELECTOR)`. `ownerDocument.activeElement`
/// is settable so tests can simulate which button currently has focus.
/// `contains` defaults to "active is one of the supplied buttons" so the
/// trap can tell inside-from-outside without a real DOM. Pass
/// `containsActive: false` to simulate focus that's still on the trigger
/// (outside the dialog).
function makeBody(
  buttons: FakeButton[],
  active: FakeButton | null,
  containsActive = true,
) {
  return {
    querySelectorAll: (sel: string) => {
      if (sel !== FOCUSABLE_SELECTOR) return [] as unknown as NodeListOf<HTMLElement>;
      return buttons as unknown as NodeListOf<HTMLElement>;
    },
    contains: (n: unknown) => containsActive && n === active,
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

  it('Shift+Tab on the first focusable wraps focus to the last', () => {
    const first = makeButton();
    const last = makeButton();
    const body = makeBody([first, last], first);
    const ev = makeKey('Tab', true);
    handleModalKey(ev, body, vi.fn());
    expect(last.focus).toHaveBeenCalledOnce();
    expect(first.focus).not.toHaveBeenCalled();
    expect(ev.preventDefault).toHaveBeenCalledOnce();
  });

  it('Tab while focus is outside the modal pulls it to the first focusable', () => {
    // Simulates the just-opened-modal state: trigger still has focus,
    // user hits Tab; without the inside-check the trap would let Tab
    // walk to the next document element instead of entering the dialog.
    const triggerOutside = makeButton();
    const first = makeButton();
    const last = makeButton();
    const body = makeBody([first, last], triggerOutside, /* containsActive */ false);
    const ev = makeKey('Tab');
    handleModalKey(ev, body, vi.fn());
    expect(first.focus).toHaveBeenCalledOnce();
    expect(last.focus).not.toHaveBeenCalled();
    expect(ev.preventDefault).toHaveBeenCalledOnce();
  });

  it('Shift+Tab while focus is outside pulls it to the last focusable', () => {
    const triggerOutside = makeButton();
    const first = makeButton();
    const last = makeButton();
    const body = makeBody([first, last], triggerOutside, /* containsActive */ false);
    const ev = makeKey('Tab', true);
    handleModalKey(ev, body, vi.fn());
    expect(last.focus).toHaveBeenCalledOnce();
    expect(first.focus).not.toHaveBeenCalled();
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

describe('centeredModalPosition (zi6p)', () => {
  it('centers a modal that fits the viewport', () => {
    expect(centeredModalPosition(800, 600, 1920, 1080)).toEqual({ left: 560, top: 240 });
  });

  it('clamps to 0 when the modal is wider/taller than the viewport', () => {
    expect(centeredModalPosition(2000, 1200, 1280, 800)).toEqual({ left: 0, top: 0 });
  });
});

describe('clampModalPosition (zi6p)', () => {
  const VW = 1280;
  const VH = 800;
  const W = 600;
  const HH = 40;

  it('passes through a position well inside the viewport', () => {
    expect(clampModalPosition(300, 200, W, VW, VH, HH)).toEqual({ left: 300, top: 200 });
  });

  it('keeps the header from going above the top edge', () => {
    expect(clampModalPosition(300, -50, W, VW, VH, HH).top).toBe(0);
  });

  it('keeps the header reachable at the bottom edge', () => {
    // maxTop = vh - headerH
    expect(clampModalPosition(300, 9999, W, VW, VH, HH).top).toBe(VH - HH);
  });

  it('keeps edgeMargin px reachable on the right (cannot drag fully off-screen)', () => {
    // maxLeft = vw - margin (margin defaults to 60)
    expect(clampModalPosition(99999, 100, W, VW, VH, HH).left).toBe(VW - 60);
  });

  it('keeps edgeMargin px reachable on the left (negative left allowed but bounded)', () => {
    // minLeft = margin - modalW = 60 - 600 = -540
    expect(clampModalPosition(-99999, 100, W, VW, VH, HH).left).toBe(60 - W);
  });
});

describe('Modal geometry cache (zi6p)', () => {
  beforeEach(() => __geomCache.clear());

  it('stores and restores geometry by persistKey', () => {
    __geomCache.set('machine', { left: 100, top: 50, width: 700, height: 500 });
    expect(__geomCache.get('machine')).toEqual({ left: 100, top: 50, width: 700, height: 500 });
    expect(__geomCache.get('tools')).toBeUndefined();
  });
});
