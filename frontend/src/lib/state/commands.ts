/// Command builders for every undoable project mutation. Each builder
/// returns a `Command { label, apply, revert, coalesce_key? }`. The
/// closure captures whatever it needs to undo (saved index, prior
/// value, etc.) lazily — apply() runs first and records the original
/// state, so revert() always has the right rollback target.
///
/// Used by the wrappers in project.svelte.ts. Components keep calling
/// `project.updateOperation(...)` etc.; the migration is internal.

import type { Command } from './history';
import type {
  AppSettings,
  Fixture,
  FixtureKind,
  MachineSettings,
  OpEntry,
  StockConfig,
  Tab,
  ToolEntry,
} from './project.svelte';
import type { ImportResponse } from '../api/types';

/// The shape of the project state Commands operate on. Kept narrow so
/// commands.ts doesn't pull in the Svelte-runtime class itself; only
/// the fields it touches.
export interface CommandTarget {
  imported: ImportResponse | null;
  operations: OpEntry[];
  tools: ToolEntry[];
  fixtures: Fixture[];
  tabs: Record<number, Tab[]>;
  machine: MachineSettings;
  stock: StockConfig;
  settings: AppSettings;
  dirty: boolean;
}

// ── operations ───────────────────────────────────────────────────────

export function addOperationCommand(op: OpEntry): Command {
  return {
    label: 'Add operation',
    apply: (s) => {
      const t = s as CommandTarget;
      t.operations = [...t.operations, structuredClone(op)];
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.operations = t.operations.filter((o) => o.id !== op.id);
      t.dirty = true;
    },
  };
}

export function deleteOperationCommand(opId: number): Command {
  let savedIdx = -1;
  let savedOp: OpEntry | undefined;
  return {
    label: 'Delete operation',
    apply: (s) => {
      const t = s as CommandTarget;
      savedIdx = t.operations.findIndex((o) => o.id === opId);
      if (savedIdx >= 0) {
        savedOp = structuredClone(t.operations[savedIdx]);
        t.operations = t.operations.filter((o) => o.id !== opId);
        t.dirty = true;
      }
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (savedIdx >= 0 && savedOp) {
        const next = [...t.operations];
        next.splice(savedIdx, 0, structuredClone(savedOp));
        t.operations = next;
        t.dirty = true;
      }
    },
  };
}

export function updateOperationCommand(opId: number, patch: Partial<OpEntry>): Command {
  let prevPatch: Partial<OpEntry> = {};
  return {
    label: 'Update operation',
    apply: (s) => {
      const t = s as CommandTarget;
      const cur = t.operations.find((o) => o.id === opId);
      if (!cur) return;
      prevPatch = {};
      for (const k of Object.keys(patch) as (keyof OpEntry)[]) {
        (prevPatch as Record<string, unknown>)[k as string] = cur[k];
      }
      t.operations = t.operations.map((o) =>
        o.id === opId ? { ...o, ...patch } : o,
      );
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.operations = t.operations.map((o) =>
        o.id === opId ? { ...o, ...prevPatch } : o,
      );
      t.dirty = true;
    },
    coalesce_key: coalesceKeyForPatch(opId, patch),
  };
}

/// Single-field op set with a coalesce key tied to (opId, key) so rapid
/// slider drags / number-field typing collapse into one undo step.
export function setOpFieldCommand<K extends keyof OpEntry>(
  opId: number,
  key: K,
  value: OpEntry[K],
): Command {
  let prev: OpEntry[K] | undefined;
  return {
    label: `Set ${String(key)}`,
    apply: (s) => {
      const t = s as CommandTarget;
      const cur = t.operations.find((o) => o.id === opId);
      if (!cur) return;
      prev = cur[key];
      t.operations = t.operations.map((o) =>
        o.id === opId ? { ...o, [key]: value } : o,
      );
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (prev === undefined) return;
      const restore = prev;
      t.operations = t.operations.map((o) =>
        o.id === opId ? { ...o, [key]: restore } : o,
      );
      t.dirty = true;
    },
    coalesce_key: `setOpField:${opId}:${String(key)}`,
  };
}

export function reorderOperationCommand(id: number, toIndex: number): Command {
  let fromIdx = -1;
  let actualTo = -1;
  return {
    label: 'Reorder operation',
    apply: (s) => {
      const t = s as CommandTarget;
      fromIdx = t.operations.findIndex((o) => o.id === id);
      if (fromIdx < 0) return;
      actualTo = Math.max(0, Math.min(toIndex, t.operations.length - 1));
      if (actualTo === fromIdx) return;
      const next = [...t.operations];
      const [op] = next.splice(fromIdx, 1);
      next.splice(actualTo, 0, op);
      t.operations = next;
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (fromIdx < 0 || actualTo < 0 || actualTo === fromIdx) return;
      const next = [...t.operations];
      const [op] = next.splice(actualTo, 1);
      next.splice(fromIdx, 0, op);
      t.operations = next;
      t.dirty = true;
    },
  };
}

// ── tools ────────────────────────────────────────────────────────────

export function addToolCommand(tool: ToolEntry): Command {
  return {
    label: 'Add tool',
    apply: (s) => {
      const t = s as CommandTarget;
      t.tools = [...t.tools, structuredClone(tool)];
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.tools = t.tools.filter((x) => x.id !== tool.id);
      t.dirty = true;
    },
  };
}

export function deleteToolCommand(toolId: number): Command {
  let savedIdx = -1;
  let savedTool: ToolEntry | undefined;
  return {
    label: 'Delete tool',
    apply: (s) => {
      const t = s as CommandTarget;
      savedIdx = t.tools.findIndex((x) => x.id === toolId);
      if (savedIdx >= 0) {
        savedTool = structuredClone(t.tools[savedIdx]);
        t.tools = t.tools.filter((x) => x.id !== toolId);
        t.dirty = true;
      }
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (savedIdx >= 0 && savedTool) {
        const next = [...t.tools];
        next.splice(savedIdx, 0, structuredClone(savedTool));
        t.tools = next;
        t.dirty = true;
      }
    },
  };
}

export function replaceToolsCommand(nextTools: ToolEntry[]): Command {
  let prev: ToolEntry[] = [];
  return {
    label: 'Update tool library',
    apply: (s) => {
      const t = s as CommandTarget;
      prev = t.tools.map((x) => structuredClone(x));
      t.tools = nextTools.map((x) => structuredClone(x));
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.tools = prev.map((x) => structuredClone(x));
      t.dirty = true;
    },
  };
}

// ── fixtures ─────────────────────────────────────────────────────────

export function addFixtureCommand(f: Fixture): Command {
  return {
    label: 'Add fixture',
    apply: (s) => {
      const t = s as CommandTarget;
      t.fixtures = [...t.fixtures, structuredClone(f)];
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.fixtures = t.fixtures.filter((x) => x.id !== f.id);
      t.dirty = true;
    },
  };
}

export function removeFixtureCommand(id: number): Command {
  let savedIdx = -1;
  let savedFixture: Fixture | undefined;
  return {
    label: 'Remove fixture',
    apply: (s) => {
      const t = s as CommandTarget;
      savedIdx = t.fixtures.findIndex((x) => x.id === id);
      if (savedIdx >= 0) {
        savedFixture = structuredClone(t.fixtures[savedIdx]);
        t.fixtures = t.fixtures.filter((x) => x.id !== id);
        t.dirty = true;
      }
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (savedIdx >= 0 && savedFixture) {
        const next = [...t.fixtures];
        next.splice(savedIdx, 0, structuredClone(savedFixture));
        t.fixtures = next;
        t.dirty = true;
      }
    },
  };
}

export function updateFixtureCommand(id: number, patch: Partial<Fixture>): Command {
  let prevPatch: Partial<Fixture> = {};
  return {
    label: 'Update fixture',
    apply: (s) => {
      const t = s as CommandTarget;
      const cur = t.fixtures.find((x) => x.id === id);
      if (!cur) return;
      prevPatch = {};
      for (const k of Object.keys(patch) as (keyof Fixture)[]) {
        (prevPatch as Record<string, unknown>)[k as string] = structuredClone(cur[k]);
      }
      t.fixtures = t.fixtures.map((x) => (x.id === id ? { ...x, ...patch } : x));
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.fixtures = t.fixtures.map((x) => (x.id === id ? { ...x, ...prevPatch } : x));
      t.dirty = true;
    },
    coalesce_key: coalesceKeyForFixturePatch(id, patch),
  };
}

// ── tabs ─────────────────────────────────────────────────────────────

export function addTabCommand(segmentIdx: number, tab: Tab): Command {
  return {
    label: 'Add tab',
    apply: (s) => {
      const t = s as CommandTarget;
      const next = { ...t.tabs };
      next[segmentIdx] = [...(next[segmentIdx] ?? []), { x: tab.x, y: tab.y }];
      t.tabs = next;
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      const list = t.tabs[segmentIdx];
      if (!list || list.length === 0) return;
      const next = { ...t.tabs };
      next[segmentIdx] = list.slice(0, -1);
      if (next[segmentIdx].length === 0) delete next[segmentIdx];
      t.tabs = next;
      t.dirty = true;
    },
  };
}

export function removeTabCommand(segmentIdx: number, tabIdx: number): Command {
  let saved: Tab | undefined;
  return {
    label: 'Remove tab',
    apply: (s) => {
      const t = s as CommandTarget;
      const list = t.tabs[segmentIdx];
      if (!list || tabIdx < 0 || tabIdx >= list.length) return;
      saved = { ...list[tabIdx] };
      const next = { ...t.tabs };
      next[segmentIdx] = list.filter((_, i) => i !== tabIdx);
      if (next[segmentIdx].length === 0) delete next[segmentIdx];
      t.tabs = next;
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (!saved) return;
      const cur = t.tabs[segmentIdx] ?? [];
      const restored = [...cur];
      restored.splice(Math.min(tabIdx, restored.length), 0, { ...saved });
      const next = { ...t.tabs };
      next[segmentIdx] = restored;
      t.tabs = next;
      t.dirty = true;
    },
  };
}

export function clearTabsCommand(): Command {
  let saved: Record<number, Tab[]> = {};
  return {
    label: 'Clear tabs',
    apply: (s) => {
      const t = s as CommandTarget;
      saved = {};
      for (const k of Object.keys(t.tabs)) {
        saved[Number(k)] = t.tabs[Number(k)].map((x) => ({ ...x }));
      }
      t.tabs = {};
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      const restored: Record<number, Tab[]> = {};
      for (const k of Object.keys(saved)) {
        restored[Number(k)] = saved[Number(k)].map((x) => ({ ...x }));
      }
      t.tabs = restored;
      t.dirty = true;
    },
  };
}

// ── machine / stock ──────────────────────────────────────────────────

export function setMachineCommand(next: MachineSettings): Command {
  let prev: MachineSettings | null = null;
  return {
    label: 'Update machine',
    apply: (s) => {
      const t = s as CommandTarget;
      prev = structuredClone(t.machine);
      t.machine = structuredClone(next);
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (prev) t.machine = structuredClone(prev);
      t.dirty = true;
    },
  };
}

export function setStockCommand(patch: Partial<StockConfig>): Command {
  let prevPatch: Partial<StockConfig> = {};
  return {
    label: 'Update stock',
    apply: (s) => {
      const t = s as CommandTarget;
      prevPatch = {};
      for (const k of Object.keys(patch) as (keyof StockConfig)[]) {
        (prevPatch as Record<string, unknown>)[k as string] = t.stock[k];
      }
      t.stock = { ...t.stock, ...patch };
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.stock = { ...t.stock, ...prevPatch };
      t.dirty = true;
    },
    coalesce_key: coalesceKeyForStockPatch(patch),
  };
}

// ── imported geometry ────────────────────────────────────────────────

export interface AppendImportedSegmentsPayload {
  before: ImportResponse | null;
  after: ImportResponse;
}

/// Append-imported-segments is the only `imported` mutation that should
/// be undoable in the normal authoring flow (Add Text). `setImported`
/// (file load) and `restore` (project load) instead clear history.
export function appendImportedCommand(p: AppendImportedSegmentsPayload): Command {
  return {
    label: 'Add geometry',
    apply: (s) => {
      const t = s as CommandTarget;
      t.imported = structuredClone(p.after);
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.imported = p.before ? structuredClone(p.before) : null;
      t.dirty = true;
    },
  };
}

// ── helpers ──────────────────────────────────────────────────────────

function coalesceKeyForPatch(opId: number, patch: Partial<OpEntry>): string | undefined {
  const keys = Object.keys(patch);
  if (keys.length !== 1) return undefined;
  return `setOpField:${opId}:${keys[0]}`;
}

function coalesceKeyForFixturePatch(id: number, patch: Partial<Fixture>): string | undefined {
  const keys = Object.keys(patch);
  if (keys.length !== 1) return undefined;
  return `setFixture:${id}:${keys[0]}`;
}

function coalesceKeyForStockPatch(patch: Partial<StockConfig>): string | undefined {
  const keys = Object.keys(patch);
  if (keys.length !== 1) return undefined;
  return `setStock:${keys[0]}`;
}

export type FixtureKindForBuilder = FixtureKind;
