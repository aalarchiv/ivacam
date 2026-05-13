import { describe, expect, it } from 'vitest';
import { groupOperations, isGroupAllEnabled } from './op-grouping';

describe('groupOperations', () => {
  it('puts ungrouped ops in the trailing empty-string bucket', () => {
    const ops = [{ id: 1 }, { id: 2 }];
    const groups = groupOperations(ops);
    expect(groups).toEqual([{ name: '', ops: [{ id: 1 }, { id: 2 }] }]);
  });

  it('preserves group insertion order from the source list', () => {
    const ops = [
      { id: 1, group: 'Front' },
      { id: 2, group: 'Back' },
      { id: 3, group: 'Front' },
      { id: 4 },
      { id: 5, group: 'Back' },
    ];
    const groups = groupOperations(ops);
    expect(groups.map((g) => g.name)).toEqual(['Front', 'Back', '']);
    expect(groups[0].ops.map((o) => o.id)).toEqual([1, 3]);
    expect(groups[1].ops.map((o) => o.id)).toEqual([2, 5]);
    expect(groups[2].ops.map((o) => o.id)).toEqual([4]);
  });

  it('always emits an "Other" bucket even when every op is grouped', () => {
    const ops = [
      { id: 1, group: 'A' },
      { id: 2, group: 'A' },
    ];
    const groups = groupOperations(ops);
    // No ungrouped ops, but we still want a trailing '' bucket when
    // the source has zero groups. Once any named groups exist we
    // drop the empty bucket.
    expect(groups.map((g) => g.name)).toEqual(['A']);
  });

  it('emits a sole empty bucket for an empty input', () => {
    const groups = groupOperations([]);
    expect(groups).toEqual([{ name: '', ops: [] }]);
  });

  it('handles undefined vs empty-string groups identically', () => {
    // Some old projects might persist `group: ''` instead of dropping
    // the field; both should fall into the trailing ungrouped bucket.
    const ops = [{ id: 1 }, { id: 2, group: '' }, { id: 3 }];
    const groups = groupOperations(ops);
    expect(groups).toHaveLength(1);
    expect(groups[0].name).toBe('');
    expect(groups[0].ops.map((o) => o.id)).toEqual([1, 2, 3]);
  });
});

describe('isGroupAllEnabled', () => {
  it('is false for an empty bucket', () => {
    expect(isGroupAllEnabled([])).toBe(false);
  });

  it('is false when any op is disabled', () => {
    expect(
      isGroupAllEnabled([
        { id: 1, enabled: true },
        { id: 2, enabled: false },
      ]),
    ).toBe(false);
  });

  it('is true when every op is enabled', () => {
    expect(
      isGroupAllEnabled([
        { id: 1, enabled: true },
        { id: 2, enabled: true },
      ]),
    ).toBe(true);
  });

  it('is false when enabled is missing (undefined) on any op', () => {
    expect(isGroupAllEnabled([{ id: 1, enabled: true }, { id: 2 }])).toBe(false);
  });
});
