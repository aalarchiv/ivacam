import { describe, it, expect } from 'vitest';
import { clampPanelRect, initialPanelPosition } from './floating-panel';

const MIN_W = 320;
const MIN_H = 220;

describe('clampPanelRect', () => {
  it('leaves an in-bounds rect untouched', () => {
    const r = clampPanelRect({ x: 100, y: 100, w: 480, h: 400 }, 1920, 1080, MIN_W, MIN_H);
    expect(r).toEqual({ x: 100, y: 100, w: 480, h: 400 });
  });

  it('preserves null position (uncomputed → first-open default applies later)', () => {
    const r = clampPanelRect({ x: null, y: null, w: 480, h: 400 }, 1920, 1080, MIN_W, MIN_H);
    expect(r.x).toBeNull();
    expect(r.y).toBeNull();
  });

  it('enforces the minimum size', () => {
    const r = clampPanelRect({ x: 10, y: 10, w: 50, h: 50 }, 1920, 1080, MIN_W, MIN_H);
    expect(r.w).toBe(MIN_W);
    expect(r.h).toBe(MIN_H);
  });

  it('caps size to viewport minus the 16px inset', () => {
    const r = clampPanelRect({ x: 10, y: 10, w: 5000, h: 5000 }, 1000, 800, MIN_W, MIN_H);
    expect(r.w).toBe(1000 - 16);
    expect(r.h).toBe(800 - 16);
  });

  it('keeps the panel at least 8px inside each edge', () => {
    const r = clampPanelRect({ x: -50, y: -50, w: 480, h: 400 }, 1920, 1080, MIN_W, MIN_H);
    expect(r.x).toBe(8);
    expect(r.y).toBe(8);
    const r2 = clampPanelRect({ x: 99999, y: 99999, w: 480, h: 400 }, 1920, 1080, MIN_W, MIN_H);
    expect(r2.x).toBe(1920 - 480 - 8);
    expect(r2.y).toBe(1080 - 400 - 8);
  });

  it('clamps position using the POST-clamp size after a viewport shrink', () => {
    // Panel was sized for a big window; window shrank. Size must shrink
    // first so the position bound (vw - w - 8) isn't computed against
    // the stale oversized width (which would pin x to the left margin
    // even when the resized panel fits).
    const r = clampPanelRect({ x: 600, y: 500, w: 900, h: 700 }, 800, 600, MIN_W, MIN_H);
    expect(r.w).toBe(800 - 16);
    expect(r.h).toBe(600 - 16);
    expect(r.x).toBe(800 - (800 - 16) - 8); // = 8
    expect(r.y).toBe(600 - (600 - 16) - 8); // = 8
  });

  it('pins to the 8px margin when the panel exactly fills the clamped viewport', () => {
    // vw - w - 8 < 8 here; Math.max(8, …) wins, matching the original
    // inline clamp's bias toward the top-left margin.
    const r = clampPanelRect({ x: 100, y: 100, w: 790, h: 590 }, 800, 600, MIN_W, MIN_H);
    expect(r.x).toBe(8);
    expect(r.y).toBe(8);
  });
});

describe('initialPanelPosition', () => {
  it('places the panel top-right with 16px margin, 56px below the top', () => {
    expect(initialPanelPosition(1920, 480)).toEqual({ x: 1920 - 480 - 16, y: 56 });
  });

  it('never goes left of the margin on a narrow viewport', () => {
    expect(initialPanelPosition(300, 480)).toEqual({ x: 16, y: 56 });
  });
});
