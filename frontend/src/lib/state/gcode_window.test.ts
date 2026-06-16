import { describe, it, expect } from 'vitest';
import { buildRowOffsets, computeWindow } from './gcode_window';

describe('buildRowOffsets', () => {
  it('accumulates uniform row heights', () => {
    const offsets = buildRowOffsets(new Uint8Array(4), 10, 30);
    expect(Array.from(offsets)).toEqual([0, 10, 20, 30, 40]);
  });

  it('adds the header height to chapter-start rows', () => {
    // lines 0 and 2 start a chapter → each carries +chapterH.
    const starts = Uint8Array.from([1, 0, 1, 0]);
    const offsets = buildRowOffsets(starts, 10, 30);
    // 0; +40 (row+header); +10; +40; +10
    expect(Array.from(offsets)).toEqual([0, 40, 50, 90, 100]);
  });

  it('empty input yields a single zero offset', () => {
    expect(Array.from(buildRowOffsets(new Uint8Array(0), 10, 30))).toEqual([0]);
  });
});

describe('computeWindow', () => {
  // 100 uniform 10px rows → offsets 0,10,...,1000.
  const offsets = buildRowOffsets(new Uint8Array(100), 10, 30);
  const count = 100;

  it('returns an empty window for no items', () => {
    const w = computeWindow(buildRowOffsets(new Uint8Array(0), 10, 30), 0, 0, 200, 5);
    expect(w).toEqual({ first: 0, last: -1, padTop: 0, padBottom: 0 });
  });

  it('windows the viewport at the top with overscan', () => {
    // viewport [0, 100) covers rows 0..9; +0 overscan up, +overscan down.
    const w = computeWindow(offsets, count, 0, 100, 3);
    expect(w.first).toBe(0); // can't overscan past the start
    expect(w.last).toBe(9 + 3);
    expect(w.padTop).toBe(0);
    expect(w.padBottom).toBe(offsets[count] - offsets[w.last + 1]);
  });

  it('windows a mid-list scroll position symmetrically', () => {
    // scrollTop 500, height 100 → rows 50..59 visible.
    const w = computeWindow(offsets, count, 500, 100, 4);
    expect(w.first).toBe(50 - 4);
    expect(w.last).toBe(59 + 4);
    // Spacers exactly reconstruct the missing extent.
    expect(w.padTop).toBe(offsets[w.first]);
    expect(w.padBottom).toBe(offsets[count] - offsets[w.last + 1]);
    // Total height is conserved: padTop + rendered + padBottom = total.
    const rendered = offsets[w.last + 1] - offsets[w.first];
    expect(w.padTop + rendered + w.padBottom).toBe(offsets[count]);
  });

  it('clamps the window to the end of the list', () => {
    const w = computeWindow(offsets, count, 100_000, 100, 5);
    expect(w.last).toBe(count - 1);
    expect(w.padBottom).toBe(0);
  });

  it('never blanks out when the viewport height is unmeasured (0)', () => {
    const w = computeWindow(offsets, count, 250, 0, 0);
    // Renders at least the landing row even with a zero-height viewport.
    expect(w.first).toBeLessThanOrEqual(w.last);
    expect(w.first).toBe(25);
  });

  it('handles variable-height rows (chapter headers shift the mapping)', () => {
    // Row 0 is a chapter start (40px), rest are 10px:
    // offsets = 0,40,50,60,...  scrollTop 45 lands inside row 1.
    const starts = new Uint8Array(10);
    starts[0] = 1;
    const off = buildRowOffsets(starts, 10, 30);
    const w = computeWindow(off, 10, 45, 10, 0);
    expect(w.first).toBe(1); // row 1 spans [40,50)
    expect(w.padTop).toBe(off[1]); // 40px of header+row above
  });
});
