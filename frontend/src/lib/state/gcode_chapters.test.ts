import { describe, it, expect } from 'vitest';
import {
  parseGcodeChapters,
  firstSegmentInRange,
  NO_SEGMENT,
  type GcodeChapter,
} from './gcode_chapters';
import type { OpEntry } from './project.svelte';

function op(id: number, name: string, enabled = true): OpEntry {
  return {
    id,
    name,
    enabled,
    kind: 'profile',
    toolId: 1,
    sourceLayers: null,
    depth: 0,
    startDepth: 0,
    step: null,
    offset: 'outside',
    pocketStrategy: null,
  } as unknown as OpEntry;
}

describe('parseGcodeChapters', () => {
  it('returns a single header chapter when there are no op markers', () => {
    const lines = ['G21', 'G90', 'M3 S1000'];
    const chapters = parseGcodeChapters(lines, []);
    expect(chapters).toHaveLength(1);
    expect(chapters[0]).toEqual<GcodeChapter>({
      opId: 0,
      name: 'Program header',
      startLine: 1,
      endLine: 3,
      disabled: false,
    });
  });

  it('splits at `; OP N` markers and labels using the matching op', () => {
    const lines = [
      'G21', //                 1   header
      '; OP 1', //              2   op 1 starts
      'G0 X0 Y0', //            3
      '; OP 2', //              4   op 2 starts
      'G1 Z-1 F300', //         5
      'M30', //                 6
    ];
    const ops = [op(1, 'Profile'), op(2, 'Pocket')];
    const chapters = parseGcodeChapters(lines, ops);
    expect(chapters.map((c) => [c.opId, c.startLine, c.endLine])).toEqual([
      [0, 1, 1],
      [1, 2, 3],
      [2, 4, 6],
    ]);
    expect(chapters[1].name).toBe('#1 Profile');
    expect(chapters[2].name).toBe('#2 Pocket');
  });

  it('flags disabled ops via the operation list', () => {
    const lines = ['; OP 7', 'G1 X1 F100'];
    const chapters = parseGcodeChapters(lines, [op(7, 'Silenced', false)]);
    expect(chapters[0].disabled).toBe(true);
  });

  it('accepts the alternate `(OP N)` paren-comment form', () => {
    const lines = ['(OP 3)', 'G0 X0'];
    const chapters = parseGcodeChapters(lines, [op(3, 'A')]);
    expect(chapters[0].opId).toBe(3);
  });

  it('synthesizes a name when the op id is unknown', () => {
    const chapters = parseGcodeChapters(['; OP 99'], []);
    expect(chapters[0].name).toBe('Op #99');
  });
});

describe('firstSegmentInRange', () => {
  it('skips NO_SEGMENT entries and returns the first real index', () => {
    const map = [NO_SEGMENT, NO_SEGMENT, 5, 6];
    expect(firstSegmentInRange(map, 1, 4)).toBe(5);
  });

  it('returns null when the range is pure comments', () => {
    const map = [NO_SEGMENT, NO_SEGMENT];
    expect(firstSegmentInRange(map, 1, 2)).toBeNull();
  });

  it('clamps end to the array length', () => {
    const map = [NO_SEGMENT, 4];
    expect(firstSegmentInRange(map, 1, 999)).toBe(4);
  });
});
