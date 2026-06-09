import { describe, it, expect } from 'vitest';
import type { OpEntry } from './op_types';
import { migrateLegacyOpFields } from './op-migration';

describe('migrateLegacyOpFields', () => {
  it('renames tabMode.auto_count → autoCount on pre-0euf saves', () => {
    const legacy = {
      id: 1,
      kind: 'profile',
      tabMode: { kind: 'mixed', auto_count: 5 },
    } as unknown as OpEntry;
    const out = migrateLegacyOpFields(legacy) as { tabMode?: unknown };
    expect(out.tabMode).toEqual({ kind: 'mixed', autoCount: 5 });
  });

  it('prefers the camel key when a mid-migration file carries both', () => {
    const both = {
      id: 1,
      kind: 'profile',
      tabMode: { kind: 'mixed', auto_count: 5, autoCount: 7 },
    } as unknown as OpEntry;
    const out = migrateLegacyOpFields(both) as { tabMode?: unknown };
    expect(out.tabMode).toEqual({ kind: 'mixed', autoCount: 7 });
  });

  it('renames legacy PatternConfig fields forward', () => {
    const legacy = {
      id: 1,
      kind: 'drill',
      pattern: { kind: 'polar', count: 6, center_x: 1, center_y: 2, angle_step_deg: 60 },
    } as unknown as OpEntry;
    const out = migrateLegacyOpFields(legacy) as { pattern?: unknown };
    expect(out.pattern).toEqual({
      kind: 'polar',
      count: 6,
      centerX: 1,
      centerY: 2,
      angleStepDeg: 60,
    });
    const grid = {
      id: 2,
      kind: 'drill',
      pattern: { kind: 'grid', count_x: 2, count_y: 3, dx: 10, dy: 10 },
    } as unknown as OpEntry;
    expect((migrateLegacyOpFields(grid) as { pattern?: unknown }).pattern).toEqual({
      kind: 'grid',
      countX: 2,
      countY: 3,
      dx: 10,
      dy: 10,
    });
  });

  it('passes through current-format and non-mixed ops unchanged', () => {
    const current = {
      id: 1,
      kind: 'profile',
      tabMode: { kind: 'mixed', autoCount: 3 },
    } as unknown as OpEntry;
    expect(migrateLegacyOpFields(current)).toBe(current);
    const auto = {
      id: 2,
      kind: 'pocket',
      tabMode: { kind: 'auto', count: 4 },
    } as unknown as OpEntry;
    expect(migrateLegacyOpFields(auto)).toBe(auto);
    const bare = { id: 3, kind: 'drill' } as unknown as OpEntry;
    expect(migrateLegacyOpFields(bare)).toBe(bare);
  });
});
