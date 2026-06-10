/// Pure-logic tests for the selection helpers. `SelectionState` itself
/// is Svelte-rune-backed; the helpers live in selection.svelte.ts but
/// are plain functions, so vitest can drive them without booting the
/// rune runtime.

import { describe, expect, it } from 'vitest';
import { computeSelectionUpdate, selectionsEqual } from './selection.svelte';

describe('selectionsEqual', () => {
  it('treats same-instance sets as equal', () => {
    const s = new Set([1, 2, 3]);
    expect(selectionsEqual(s, s)).toBe(true);
  });

  it('returns true for equal-by-contents sets', () => {
    expect(selectionsEqual(new Set([1, 2]), new Set([2, 1]))).toBe(true);
    expect(selectionsEqual(new Set(), new Set())).toBe(true);
  });

  it('returns false when sizes differ', () => {
    expect(selectionsEqual(new Set([1]), new Set([1, 2]))).toBe(false);
  });

  it('returns false when contents differ', () => {
    expect(selectionsEqual(new Set([1, 2]), new Set([1, 3]))).toBe(false);
  });
});

describe('computeSelectionUpdate', () => {
  it('replace mode lands the anchor on a single selected id', () => {
    const r = computeSelectionUpdate(new Set([3, 7]), 7, [5], 'replace');
    expect([...r.selected]).toEqual([5]);
    expect(r.anchor).toBe(5);
  });

  it('replace mode with multiple ids clears the anchor', () => {
    const r = computeSelectionUpdate(new Set([3]), 3, [4, 5, 6], 'replace');
    expect([...r.selected].sort()).toEqual([4, 5, 6]);
    expect(r.anchor).toBeNull();
  });

  it('add mode unions ids and preserves the prior anchor on multi-add', () => {
    const r = computeSelectionUpdate(new Set([1]), 1, [4, 5], 'add');
    expect([...r.selected].sort()).toEqual([1, 4, 5]);
    expect(r.anchor).toBe(1);
  });

  it('add mode with a single id updates the anchor', () => {
    const r = computeSelectionUpdate(new Set([1]), 1, [9], 'add');
    expect([...r.selected].sort()).toEqual([1, 9]);
    expect(r.anchor).toBe(9);
  });

  it('toggle mode XORs ids', () => {
    const r = computeSelectionUpdate(new Set([1, 2, 3]), 3, [2, 5], 'toggle');
    expect([...r.selected].sort()).toEqual([1, 3, 5]);
  });

  it('toggle of a single newly-added id sets the anchor', () => {
    const r = computeSelectionUpdate(new Set([1]), 1, [9], 'toggle');
    expect([...r.selected].sort()).toEqual([1, 9]);
    expect(r.anchor).toBe(9);
  });

  it('toggle that removes a single id leaves the anchor alone', () => {
    const r = computeSelectionUpdate(new Set([1, 2]), 2, [2], 'toggle');
    expect([...r.selected]).toEqual([1]);
    expect(r.anchor).toBe(2);
  });

  it('filters out non-positive ids (0 = unchained segment, -1 = sentinel)', () => {
    const r = computeSelectionUpdate(new Set(), null, [0, -1, 5], 'replace');
    expect([...r.selected]).toEqual([5]);
  });
});
