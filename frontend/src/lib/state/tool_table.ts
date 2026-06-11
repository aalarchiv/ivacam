/// Tool-table view model — sorting / filtering / pagination for the
/// tool library + inventory table. Pure logic so vitest covers it
/// without the rune runtime.
///
/// The view NEVER reorders or mutates the stored library: callers wrap
/// each draft entry as `{ tool, i }` (i = original draft index) before
/// applying the view, so row edits keep indexing the draft correctly
/// no matter how the table is sorted or filtered.

import type { MachineMode, ToolKind } from './op_types';
import type { ToolEntry } from './project-types';
import { KIND_DISPLAY_LABELS, TOOL_COMPATIBLE_MODES } from './tool_family';

export type ToolSortKey =
  | 'id'
  | 'name'
  | 'kind'
  | 'diameter'
  | 'flutes'
  | 'speed'
  | 'feedRate'
  | 'plungeRate';

export interface ToolTableView {
  /// Case-insensitive substring match against name, comment, and the
  /// kind's display label. Empty = no text filter.
  query: string;
  /// Exact kind filter. '' = all kinds.
  kind: ToolKind | '';
  /// Machine-capability filter: keep tools that can run on this mode.
  /// '' = all machines.
  mode: MachineMode | '';
  /// null = natural library order (no sort applied).
  sortKey: ToolSortKey | null;
  sortDir: 'asc' | 'desc';
}

export const EMPTY_TOOL_VIEW: ToolTableView = {
  query: '',
  kind: '',
  mode: '',
  sortKey: null,
  sortDir: 'asc',
};

export interface ToolRow {
  tool: ToolEntry;
  /// Index into the ORIGINAL draft array — what row edits mutate.
  i: number;
}

export function matchesToolFilter(tool: ToolEntry, view: ToolTableView): boolean {
  if (view.kind !== '' && tool.kind !== view.kind) return false;
  if (view.mode !== '' && !TOOL_COMPATIBLE_MODES[tool.kind].includes(view.mode)) return false;
  const q = view.query.trim().toLowerCase();
  if (q !== '') {
    const hay =
      `${tool.name} ${tool.comment ?? ''} ${KIND_DISPLAY_LABELS[tool.kind]}`.toLowerCase();
    if (!hay.includes(q)) return false;
  }
  return true;
}

function sortValue(tool: ToolEntry, key: ToolSortKey): string | number {
  switch (key) {
    case 'id':
      return tool.id;
    case 'name':
      return tool.name;
    case 'kind':
      return KIND_DISPLAY_LABELS[tool.kind];
    case 'diameter':
      return tool.diameter;
    case 'flutes':
      return tool.flutes;
    case 'speed':
      return tool.speed;
    case 'feedRate':
      return tool.feedRate;
    case 'plungeRate':
      return tool.plungeRate;
  }
}

/// Filter + sort the wrapped rows per the view. Stable: equal keys keep
/// library order (ties broken by original index), so toggling a sort
/// never shuffles equal rows.
export function applyToolTableView(rows: readonly ToolRow[], view: ToolTableView): ToolRow[] {
  const filtered = rows.filter((r) => matchesToolFilter(r.tool, view));
  const key = view.sortKey;
  if (key == null) return filtered;
  const dir = view.sortDir === 'asc' ? 1 : -1;
  return [...filtered].sort((a, b) => {
    const va = sortValue(a.tool, key);
    const vb = sortValue(b.tool, key);
    let cmp: number;
    if (typeof va === 'string' || typeof vb === 'string') {
      cmp = String(va).localeCompare(String(vb), undefined, { numeric: true, sensitivity: 'base' });
    } else {
      cmp = va - vb;
    }
    if (cmp !== 0) return cmp * dir;
    return a.i - b.i;
  });
}

/// Tri-state header click: natural → asc → desc → natural.
export function nextSortState(
  view: Pick<ToolTableView, 'sortKey' | 'sortDir'>,
  key: ToolSortKey,
): Pick<ToolTableView, 'sortKey' | 'sortDir'> {
  if (view.sortKey !== key) return { sortKey: key, sortDir: 'asc' };
  if (view.sortDir === 'asc') return { sortKey: key, sortDir: 'desc' };
  return { sortKey: null, sortDir: 'asc' };
}

export const TOOL_PAGE_SIZE = 50;

export interface ToolPage {
  rows: ToolRow[];
  /// 0-based current page, clamped into range.
  page: number;
  pageCount: number;
  total: number;
}

/// Slice one page out of the (already filtered/sorted) rows. The pager
/// UI only appears when `pageCount > 1`.
export function paginateToolRows(
  rows: readonly ToolRow[],
  page: number,
  pageSize: number = TOOL_PAGE_SIZE,
): ToolPage {
  const total = rows.length;
  const pageCount = Math.max(1, Math.ceil(total / pageSize));
  const clamped = Math.min(Math.max(0, page), pageCount - 1);
  return {
    rows: rows.slice(clamped * pageSize, (clamped + 1) * pageSize),
    page: clamped,
    pageCount,
    total,
  };
}

/// Which page (0-based) a given tool id lands on under the current
/// view — used by the "edit this tool" focus flow so the target row is
/// actually on screen. null when the view filters it out entirely.
export function pageOfTool(
  rows: readonly ToolRow[],
  toolId: number,
  pageSize: number = TOOL_PAGE_SIZE,
): number | null {
  const idx = rows.findIndex((r) => r.tool.id === toolId);
  if (idx < 0) return null;
  return Math.floor(idx / pageSize);
}
