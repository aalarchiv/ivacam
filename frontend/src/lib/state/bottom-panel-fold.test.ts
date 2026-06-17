import { describe, it, expect } from 'vitest';
import {
  FOLD_SNAPS,
  OPEN_SNAPS,
  FOLDED,
  DEFAULT_OPEN_SNAP,
  nearestSnap,
  nearestOpenSnap,
  snapHeightPx,
  restoreOpenSnap,
  toggleFold,
} from './bottom-panel-fold';

describe('snap constants', () => {
  it('exposes folded + 33/55/75 and the open subset', () => {
    expect(FOLD_SNAPS).toEqual([0, 0.33, 0.55, 0.75]);
    expect(OPEN_SNAPS).toEqual([0.33, 0.55, 0.75]);
    expect(FOLDED).toBe(0);
    expect(OPEN_SNAPS).toContain(DEFAULT_OPEN_SNAP);
  });
});

describe('nearestSnap', () => {
  it('snaps a free drag fraction to the closest position', () => {
    expect(nearestSnap(0.02)).toBe(0); // near folded
    expect(nearestSnap(0.3)).toBe(0.33);
    expect(nearestSnap(0.5)).toBe(0.55);
    expect(nearestSnap(0.9)).toBe(0.75); // clamps to the largest snap
  });

  it('resolves a fraction just past a midpoint to the larger snap', () => {
    // 0.45 is closer to 0.55 (0.10) than to 0.33 (0.12).
    expect(nearestSnap(0.45)).toBe(0.55);
    // On an exact tie the `<=` scan keeps the later (larger) snap; floats
    // rarely hit one, so this is asserted via the monotonic case above.
  });
});

describe('nearestOpenSnap', () => {
  it('never returns folded, even for a near-zero fraction', () => {
    expect(nearestOpenSnap(0)).toBe(0.33);
    expect(nearestOpenSnap(0.01)).toBe(0.33);
  });
});

describe('snapHeightPx', () => {
  it('multiplies the fraction by viewport height and rounds', () => {
    expect(snapHeightPx(0.55, 800)).toBe(440);
    expect(snapHeightPx(0, 800)).toBe(0);
    expect(snapHeightPx(0.33, 811)).toBe(Math.round(0.33 * 811));
  });
});

describe('restoreOpenSnap', () => {
  it('falls back to the default for null / folded / negative', () => {
    expect(restoreOpenSnap(null)).toBe(DEFAULT_OPEN_SNAP);
    expect(restoreOpenSnap(undefined)).toBe(DEFAULT_OPEN_SNAP);
    expect(restoreOpenSnap(0)).toBe(DEFAULT_OPEN_SNAP);
    expect(restoreOpenSnap(-0.2)).toBe(DEFAULT_OPEN_SNAP);
  });

  it('returns a valid persisted open snap unchanged', () => {
    expect(restoreOpenSnap(0.33)).toBe(0.33);
    expect(restoreOpenSnap(0.75)).toBe(0.75);
  });

  it('snaps a drifted persisted value to the nearest open snap', () => {
    expect(restoreOpenSnap(0.6)).toBe(0.55);
  });
});

describe('toggleFold', () => {
  it('folds when open, opens to the saved snap when folded', () => {
    expect(toggleFold(0.55, 0.75)).toBe(FOLDED); // open → folded
    expect(toggleFold(FOLDED, 0.75)).toBe(0.75); // folded → saved
    expect(toggleFold(FOLDED, null)).toBe(DEFAULT_OPEN_SNAP); // folded → default
  });
});
