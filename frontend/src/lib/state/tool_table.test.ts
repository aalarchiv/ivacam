import { describe, expect, it } from 'vitest';
import {
  applyToolTableView,
  EMPTY_TOOL_VIEW,
  matchesToolFilter,
  nextSortState,
  pageOfTool,
  paginateToolRows,
  type ToolRow,
} from './tool_table';
import type { ToolEntry } from './project-types';
import type { ToolKind } from './op_types';

const tool = (id: number, name: string, kind: ToolKind = 'endmill', diameter = 3): ToolEntry => ({
  id,
  name,
  kind,
  diameter,
  flutes: 2,
  speed: 18000,
  plungeRate: 100,
  feedRate: 800,
  coolant: 'off',
});

const rows = (...tools: ToolEntry[]): ToolRow[] => tools.map((t, i) => ({ tool: t, i }));

describe('matchesToolFilter', () => {
  const torch = tool(2, 'Plasma torch', 'plasma_torch');
  it('text query matches name, comment, and kind label, case-insensitive', () => {
    expect(matchesToolFilter(tool(1, 'Fine 2mm'), { ...EMPTY_TOOL_VIEW, query: 'fine' })).toBe(
      true,
    );
    expect(matchesToolFilter(torch, { ...EMPTY_TOOL_VIEW, query: 'PLASMA' })).toBe(true);
    expect(
      matchesToolFilter(
        { ...tool(3, 'x'), comment: 'for aluminium' },
        { ...EMPTY_TOOL_VIEW, query: 'alu' },
      ),
    ).toBe(true);
    expect(matchesToolFilter(tool(1, 'Fine 2mm'), { ...EMPTY_TOOL_VIEW, query: 'torch' })).toBe(
      false,
    );
  });
  it('kind and machine-capability filters', () => {
    expect(matchesToolFilter(torch, { ...EMPTY_TOOL_VIEW, kind: 'plasma_torch' })).toBe(true);
    expect(matchesToolFilter(torch, { ...EMPTY_TOOL_VIEW, kind: 'endmill' })).toBe(false);
    expect(matchesToolFilter(torch, { ...EMPTY_TOOL_VIEW, mode: 'plasma' })).toBe(true);
    expect(matchesToolFilter(torch, { ...EMPTY_TOOL_VIEW, mode: 'mill' })).toBe(false);
    // The engraver's dual compatibility shows under both machines.
    const engraver = tool(4, 'engraver', 'engraver');
    expect(matchesToolFilter(engraver, { ...EMPTY_TOOL_VIEW, mode: 'mill' })).toBe(true);
    expect(matchesToolFilter(engraver, { ...EMPTY_TOOL_VIEW, mode: 'drag' })).toBe(true);
  });
});

describe('applyToolTableView', () => {
  const data = rows(
    tool(1, 'b-mill', 'endmill', 6),
    tool(2, 'a-mill', 'endmill', 3),
    tool(3, 'torch', 'plasma_torch', 1.5),
  );
  it('null sortKey keeps natural library order', () => {
    expect(applyToolTableView(data, EMPTY_TOOL_VIEW).map((r) => r.tool.id)).toEqual([1, 2, 3]);
  });
  it('sorts by name asc/desc without touching original indices', () => {
    const asc = applyToolTableView(data, { ...EMPTY_TOOL_VIEW, sortKey: 'name', sortDir: 'asc' });
    expect(asc.map((r) => r.tool.name)).toEqual(['a-mill', 'b-mill', 'torch']);
    expect(asc.map((r) => r.i)).toEqual([1, 0, 2]); // original draft indices preserved
    const desc = applyToolTableView(data, { ...EMPTY_TOOL_VIEW, sortKey: 'name', sortDir: 'desc' });
    expect(desc.map((r) => r.tool.name)).toEqual(['torch', 'b-mill', 'a-mill']);
  });
  it('numeric sort by diameter; ties keep library order', () => {
    const tied = rows(tool(1, 'x', 'endmill', 3), tool(2, 'y', 'endmill', 3));
    const out = applyToolTableView(tied, {
      ...EMPTY_TOOL_VIEW,
      sortKey: 'diameter',
      sortDir: 'desc',
    });
    expect(out.map((r) => r.tool.id)).toEqual([1, 2]);
  });
  it('filter composes with sort', () => {
    const out = applyToolTableView(data, {
      ...EMPTY_TOOL_VIEW,
      query: 'mill',
      sortKey: 'diameter',
      sortDir: 'asc',
    });
    expect(out.map((r) => r.tool.name)).toEqual(['a-mill', 'b-mill']);
  });
});

describe('nextSortState', () => {
  it('cycles natural → asc → desc → natural per column', () => {
    let s: { sortKey: 'name' | null; sortDir: 'asc' | 'desc' } = { sortKey: null, sortDir: 'asc' };
    s = nextSortState(s, 'name') as never;
    expect(s).toEqual({ sortKey: 'name', sortDir: 'asc' });
    s = nextSortState(s, 'name') as never;
    expect(s).toEqual({ sortKey: 'name', sortDir: 'desc' });
    s = nextSortState(s, 'name') as never;
    expect(s).toEqual({ sortKey: null, sortDir: 'asc' });
  });
  it('switching columns starts asc', () => {
    expect(nextSortState({ sortKey: 'name', sortDir: 'desc' }, 'diameter')).toEqual({
      sortKey: 'diameter',
      sortDir: 'asc',
    });
  });
});

describe('paginateToolRows / pageOfTool', () => {
  const many = rows(...Array.from({ length: 120 }, (_, i) => tool(i + 1, `t${i + 1}`)));
  it('slices pages and clamps out-of-range pages', () => {
    const p0 = paginateToolRows(many, 0);
    expect(p0.rows).toHaveLength(50);
    expect(p0.pageCount).toBe(3);
    expect(p0.total).toBe(120);
    const last = paginateToolRows(many, 99);
    expect(last.page).toBe(2);
    expect(last.rows).toHaveLength(20);
  });
  it('small sets are one page (pager hidden)', () => {
    expect(paginateToolRows(rows(tool(1, 'a')), 0).pageCount).toBe(1);
  });
  it('pageOfTool finds the page under the current view; null when filtered out', () => {
    expect(pageOfTool(many, 1)).toBe(0);
    expect(pageOfTool(many, 51)).toBe(1);
    expect(pageOfTool(many, 120)).toBe(2);
    expect(pageOfTool(many, 999)).toBeNull();
  });
});
