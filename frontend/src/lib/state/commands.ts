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
  TabPlacement,
  TextLayer,
  ToolEntry,
} from './project.svelte';
import type { ImportResponse, WiacAutoFix } from '../api/types';
import type { ProfileOffset } from './op_types';

/// Deep-clone via JSON round-trip. Svelte 5's `$state` proxies carry
/// reactivity metadata that `structuredClone` chokes on in production
/// builds — the symbol-keyed internals leak through Reflect.ownKeys
/// and `structuredClone` raises `DataCloneError`. The button click
/// that triggered the command then dies with an uncaught exception
/// and the user sees nothing happen.
///
/// JSON round-trip works because every command target field is plain
/// data (no Map/Set/Date/Function). Used everywhere a command needs
/// to snapshot or copy state-tracked data. Functionally equivalent
/// to `structuredClone` for our shapes.
function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T;
}

/// The shape of the project state Commands operate on. Kept narrow so
/// commands.ts doesn't pull in the Svelte-runtime class itself; only
/// the fields it touches.
export interface CommandTarget {
  imported: ImportResponse | null;
  operations: OpEntry[];
  tools: ToolEntry[];
  fixtures: Fixture[];
  machine: MachineSettings;
  stock: StockConfig;
  settings: AppSettings;
  textLayers: TextLayer[];
  dirty: boolean;
}

// ── operations ───────────────────────────────────────────────────────

export function addOperationCommand(op: OpEntry): Command {
  return {
    label: 'Add operation',
    apply: (s) => {
      const t = s as CommandTarget;
      t.operations = [...t.operations, clone(op)];
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.operations = t.operations.filter((o) => o.id !== op.id);
      t.dirty = true;
    },
  };
}

/// Insert a deep-cloned op immediately after the source row. `insertAfter`
/// is an op id; the new op lands at index(sourceOp) + 1 so the user sees
/// the copy adjacent to the original. Undo removes the inserted op.
export function duplicateOperationCommand(
  srcId: number,
  newOp: OpEntry,
  insertAfter: number,
): Command {
  return {
    label: 'Duplicate operation',
    apply: (s) => {
      const t = s as CommandTarget;
      const idx = t.operations.findIndex((o) => o.id === insertAfter);
      const next = [...t.operations];
      const pos = idx < 0 ? next.length : idx + 1;
      next.splice(pos, 0, clone(newOp));
      t.operations = next;
      t.dirty = true;
      void srcId;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.operations = t.operations.filter((o) => o.id !== newOp.id);
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
        savedOp = clone(t.operations[savedIdx]);
        t.operations = t.operations.filter((o) => o.id !== opId);
        t.dirty = true;
      }
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (savedIdx >= 0 && savedOp) {
        const next = [...t.operations];
        next.splice(savedIdx, 0, clone(savedOp));
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
      t.operations = t.operations.map((o) => (o.id === opId ? { ...o, ...patch } : o));
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.operations = t.operations.map((o) => (o.id === opId ? { ...o, ...prevPatch } : o));
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
      t.operations = t.operations.map((o) => (o.id === opId ? { ...o, [key]: value } : o));
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (prev === undefined) return;
      const restore = prev;
      t.operations = t.operations.map((o) => (o.id === opId ? { ...o, [key]: restore } : o));
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
      t.tools = [...t.tools, clone(tool)];
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
        savedTool = clone(t.tools[savedIdx]);
        t.tools = t.tools.filter((x) => x.id !== toolId);
        t.dirty = true;
      }
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (savedIdx >= 0 && savedTool) {
        const next = [...t.tools];
        next.splice(savedIdx, 0, clone(savedTool));
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
      prev = t.tools.map((x) => clone(x));
      t.tools = nextTools.map((x) => clone(x));
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.tools = prev.map((x) => clone(x));
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
      t.fixtures = [...t.fixtures, clone(f)];
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
        savedFixture = clone(t.fixtures[savedIdx]);
        t.fixtures = t.fixtures.filter((x) => x.id !== id);
        t.dirty = true;
      }
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (savedIdx >= 0 && savedFixture) {
        const next = [...t.fixtures];
        next.splice(savedIdx, 0, clone(savedFixture));
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
        (prevPatch as Record<string, unknown>)[k as string] = clone(cur[k]);
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

// ── text layers ──────────────────────────────────────────────────────

export function addTextLayerCommand(layer: TextLayer): Command {
  return {
    label: 'Add text',
    apply: (s) => {
      const t = s as CommandTarget;
      t.textLayers = [...t.textLayers, clone(layer)];
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.textLayers = t.textLayers.filter((tl) => tl.id !== layer.id);
      t.dirty = true;
    },
  };
}

export function deleteTextLayerCommand(id: number): Command {
  let savedIdx = -1;
  let savedLayer: TextLayer | undefined;
  return {
    label: 'Delete text',
    apply: (s) => {
      const t = s as CommandTarget;
      savedIdx = t.textLayers.findIndex((tl) => tl.id === id);
      if (savedIdx >= 0) {
        savedLayer = clone(t.textLayers[savedIdx]);
        t.textLayers = t.textLayers.filter((tl) => tl.id !== id);
        t.dirty = true;
      }
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (savedIdx >= 0 && savedLayer) {
        const next = [...t.textLayers];
        next.splice(savedIdx, 0, clone(savedLayer));
        t.textLayers = next;
        t.dirty = true;
      }
    },
  };
}

export function updateTextLayerCommand(id: number, patch: Partial<TextLayer>): Command {
  let prevPatch: Partial<TextLayer> = {};
  return {
    label: 'Edit text',
    apply: (s) => {
      const t = s as CommandTarget;
      const cur = t.textLayers.find((tl) => tl.id === id);
      if (!cur) return;
      prevPatch = {};
      for (const k of Object.keys(patch) as (keyof TextLayer)[]) {
        (prevPatch as Record<string, unknown>)[k as string] = cur[k];
      }
      t.textLayers = t.textLayers.map((tl) => (tl.id === id ? { ...tl, ...patch } : tl));
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.textLayers = t.textLayers.map((tl) => (tl.id === id ? { ...tl, ...prevPatch } : tl));
      t.dirty = true;
    },
    /// Slider / number-field drags collapse into one undo step per (id, field).
    coalesce_key: coalesceKeyForTextPatch(id, patch),
  };
}

function coalesceKeyForTextPatch(id: number, patch: Partial<TextLayer>): string | undefined {
  const keys = Object.keys(patch);
  if (keys.length !== 1) return undefined;
  return `text:${id}:${keys[0]}`;
}

// ── tabs (rt1.10) ─────────────────────────────────────────────────────
//
// Tab placements are now per-op fields (`op.tabMode`, `op.tabPlacements`).
// Add / remove / toggle go through `updateOperationCommand` with a new
// placements array. No bespoke commands needed; the
// `toggleTabPlacementCommand` helper below stages the array transition
// as a single undoable history entry.

export function toggleTabPlacementCommand(
  opId: number,
  placement: TabPlacement,
  toleranceT: number,
): Command {
  let saved: TabPlacement[] | undefined;
  return {
    label: 'Toggle tab',
    apply: (s) => {
      const t = s as CommandTarget;
      const opIdx = t.operations.findIndex((o) => o.id === opId);
      if (opIdx < 0) return;
      const op = t.operations[opIdx];
      saved = op.tabPlacements ? op.tabPlacements.map((p) => ({ ...p })) : [];
      const current = saved;
      const matchIdx = current.findIndex(
        (p) =>
          p.objectId === placement.objectId &&
          Math.min(Math.abs(p.t - placement.t), 1 - Math.abs(p.t - placement.t)) < toleranceT,
      );
      const next =
        matchIdx >= 0 ? current.filter((_, i) => i !== matchIdx) : [...current, { ...placement }];
      // Produce a NEW operations array (and a new op object) so $derived /
      // $effect blocks that depend on operations' reference identity
      // (the 2D ghost tab, 3D tab markers) refire. The previous
      // in-place mutation left stale markers until something else
      // dirtied operations.
      const nextOps = [...t.operations];
      nextOps[opIdx] = { ...op, tabPlacements: next };
      t.operations = nextOps;
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      const opIdx = t.operations.findIndex((o) => o.id === opId);
      if (opIdx < 0 || saved === undefined) return;
      const nextOps = [...t.operations];
      nextOps[opIdx] = {
        ...t.operations[opIdx],
        tabPlacements: saved.map((p) => ({ ...p })),
      };
      t.operations = nextOps;
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
      prev = clone(t.machine);
      t.machine = clone(next);
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (prev) t.machine = clone(prev);
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
      t.imported = clone(p.after);
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.imported = p.before ? clone(p.before) : null;
      t.dirty = true;
    },
  };
}

/// Generic imported-swap with a custom label. Used by per-layer delete
/// (and any future imported-geometry mutation that isn't a simple
/// append). Same before/after snapshot pattern as appendImportedCommand.
export function replaceImportedCommand(
  before: ImportResponse | null,
  after: ImportResponse | null,
  label: string,
): Command {
  return {
    label,
    apply: (s) => {
      const t = s as CommandTarget;
      t.imported = after ? clone(after) : null;
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.imported = before ? clone(before) : null;
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

// ── auto-fix commands (from structured backend errors) ───────────────

/// Reassign an op's tool. Used by the AssignTool auto-fix when the user
/// clicks "Apply fix" on a misconfigured-tool error toast.
export function assignToolCommand(opId: number, toolId: number): Command {
  let prev: number | undefined;
  return {
    label: 'Assign tool',
    apply: (s) => {
      const t = s as CommandTarget;
      const cur = t.operations.find((o) => o.id === opId);
      if (!cur) return;
      prev = cur.toolId;
      t.operations = t.operations.map((o) => (o.id === opId ? { ...o, toolId } : o));
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (prev === undefined) return;
      const restore = prev;
      t.operations = t.operations.map((o) => (o.id === opId ? { ...o, toolId: restore } : o));
      t.dirty = true;
    },
  };
}

/// Disable an op (sets enabled=false). The DisableOp auto-fix.
export function disableOpCommand(opId: number): Command {
  let prev: boolean | undefined;
  return {
    label: 'Disable operation',
    apply: (s) => {
      const t = s as CommandTarget;
      const cur = t.operations.find((o) => o.id === opId);
      if (!cur) return;
      prev = cur.enabled;
      t.operations = t.operations.map((o) => (o.id === opId ? { ...o, enabled: false } : o));
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (prev === undefined) return;
      const restore = prev;
      t.operations = t.operations.map((o) => (o.id === opId ? { ...o, enabled: restore } : o));
      t.dirty = true;
    },
  };
}

/// Change a Profile op's offset. The ChangeProfileOffset auto-fix.
export function changeProfileOffsetCommand(opId: number, offset: ProfileOffset): Command {
  let prev: ProfileOffset | undefined;
  return {
    label: 'Change profile offset',
    apply: (s) => {
      const t = s as CommandTarget;
      const cur = t.operations.find((o) => o.id === opId);
      if (!cur) return;
      prev = cur.offset;
      t.operations = t.operations.map((o) => (o.id === opId ? { ...o, offset } : o));
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (prev === undefined) return;
      const restore = prev;
      t.operations = t.operations.map((o) => (o.id === opId ? { ...o, offset: restore } : o));
      t.dirty = true;
    },
  };
}

/// Lower the simulation cell resolution to bring the cell count back
/// under the configured cap. The LowerSimResolution auto-fix.
export function lowerSimResolutionCommand(suggestedCellMm: number): Command {
  let prev: { mode: 'auto' | 'manual'; cellMm: number } | null = null;
  return {
    label: 'Lower simulation resolution',
    apply: (s) => {
      const t = s as CommandTarget;
      prev = {
        mode: t.settings.cellResolutionMode,
        cellMm: t.settings.cellResolutionMm,
      };
      t.settings = {
        ...t.settings,
        cellResolutionMode: 'manual',
        cellResolutionMm: suggestedCellMm,
      };
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      if (!prev) return;
      const restore = prev;
      t.settings = {
        ...t.settings,
        cellResolutionMode: restore.mode,
        cellResolutionMm: restore.cellMm,
      };
      t.dirty = true;
    },
  };
}

/// Map a structured AutoFix value (from `WiacError.auto_fix`) to the
/// matching Command. The frontend's ErrorToast calls this and pipes the
/// result into `project.history.exec(cmd, project)` so the fix participates
/// in undo/redo like any other edit.
export function autoFixToCommand(fix: WiacAutoFix): Command {
  switch (fix.kind) {
    case 'assign_tool':
      return assignToolCommand(fix.op_id, fix.suggested_tool_id);
    case 'disable_op':
      return disableOpCommand(fix.op_id);
    case 'change_profile_offset':
      return changeProfileOffsetCommand(fix.op_id, fix.suggested as ProfileOffset);
    case 'lower_sim_resolution':
      return lowerSimResolutionCommand(fix.suggested_cell_mm);
  }
}
