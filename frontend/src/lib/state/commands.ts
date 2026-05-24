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
  ImportEntry,
  MachineSettings,
  OpEntry,
  StockConfig,
  TabPlacement,
  TextLayer,
  ToolEntry,
  WorkOffset,
} from './project.svelte';
import type { WiacAutoFix } from '../api/types';
import type { ContourFields, ProfileOffset } from './op_types';
import { isContourOp } from './op_types';

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
  imports: ImportEntry[];
  operations: OpEntry[];
  tools: ToolEntry[];
  fixtures: Fixture[];
  machine: MachineSettings;
  stock: StockConfig;
  settings: AppSettings;
  textLayers: TextLayer[];
  workOffset: WorkOffset;
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
      // Spread-merge widens the type — `{ ...o, ...patch }` loses its
      // OpEntry variant tag because TS can't prove patch.kind matches
      // o.kind. Patches are constructed against a specific op id, so the
      // assertion is sound by callsite construction.
      t.operations = t.operations.map((o) =>
        o.id === opId ? ({ ...o, ...patch } as OpEntry) : o,
      );
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.operations = t.operations.map((o) =>
        o.id === opId ? ({ ...o, ...prevPatch } as OpEntry) : o,
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
    // Bulk replace from ToolLibraryDialog — successive edits within a
    // single dialog session collapse into one undo step.
    coalesce_key: 'replaceTools',
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
      // Tabs only apply to contour ops (Profile / Pocket). Bail out
      // silently if the caller targeted a non-contour op — the UI
      // shouldn't surface tab affordances on those, but guard at the
      // command boundary so a misrouted call doesn't poison the op.
      if (!isContourOp(op)) return;
      const current: ContourFields['tabPlacements'] = op.tabPlacements ?? [];
      saved = current.map((p) => ({ ...p }));
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
      const op = t.operations[opIdx];
      if (!isContourOp(op)) return;
      const nextOps = [...t.operations];
      nextOps[opIdx] = {
        ...op,
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
    // Rapid edits in MachineDialog (each spin / keystroke flushes via
    // commit() on close, but multi-field edits should collapse into
    // one undo step too).
    coalesce_key: 'setMachine',
  };
}

/// Apply a partial WorkOffset patch (audit abdk). The X/Y/Z spinners
/// + WCS picker in StockPanel route through this so each edit is
/// undoable and the coalesce key collapses rapid spinner mashing into
/// one history entry — same UX as the stock-dim spinners.
export function setWorkOffsetCommand(patch: Partial<WorkOffset>): Command {
  let prevPatch: Partial<WorkOffset> = {};
  return {
    label: 'Update WCS / work offset',
    apply: (s) => {
      const t = s as CommandTarget;
      prevPatch = {};
      for (const k of Object.keys(patch) as (keyof WorkOffset)[]) {
        (prevPatch as Record<string, unknown>)[k as string] = t.workOffset[k];
      }
      t.workOffset = { ...t.workOffset, ...patch };
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.workOffset = { ...t.workOffset, ...prevPatch };
      t.dirty = true;
    },
    // Per-field coalesce: many small spin-button taps on X collapse to
    // one undo step; switching WCS dropdown is its own step; X then Y
    // edits stay separate (so undo undoes Y, then X — matching user
    // intuition).
    coalesce_key: `setWorkOffset:${Object.keys(patch).sort().join(',')}`,
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

/// Whole-imports-array swap (wrsu). Every undoable mutation of the
/// imports list — add/remove a drawing, edit a per-import fileTransform,
/// delete a layer across all imports, append text segments to imports[0]
/// — goes through this single command. The whole-array swap is
/// conceptually heavier than a per-field patch but the array is tiny
/// (1-handful of entries, each a shallow header around an ImportResponse
/// reference); clone() handles it fine.
///
/// `coalesceKey` lets per-entry transform spinner drags collapse into a
/// single undo entry (typical pattern: `xform:<importId>:<field>`).
export function setImportsCommand(
  before: ImportEntry[],
  after: ImportEntry[],
  label: string,
  coalesceKey?: string,
): Command {
  return {
    label,
    coalesce_key: coalesceKey ? `setImports:${coalesceKey}` : undefined,
    apply: (s) => {
      const t = s as CommandTarget;
      t.imports = clone(after);
      t.dirty = true;
    },
    revert: (s) => {
      const t = s as CommandTarget;
      t.imports = clone(before);
      t.dirty = true;
    },
  };
}

// ── selection (80gv: view-only, marksDirty=false) ────────────────────

/// Minimum view of a SelectionState the selection commands touch.
/// Carving this out lets commands.ts stay decoupled from the
/// Svelte-runtime class declaration in selection.svelte.ts.
export interface SelectionTarget {
  selectedObjects: Set<number>;
  selectionAnchorObjectId: number | null;
}

/// Selection-update command (80gv). Captures the BEFORE selection
/// + anchor and the AFTER selection + anchor; apply restores AFTER,
/// revert restores BEFORE. `marksDirty: false` so undo-able selection
/// changes don't flag the project file as edited. `coalesce_key`
/// merges rapid consecutive clicks into one undo step.
export function selectObjectsCommand(
  sel: SelectionTarget,
  prev: { selected: Set<number>; anchor: number | null },
  next: { selected: Set<number>; anchor: number | null },
): Command {
  return {
    label: 'Change selection',
    coalesce_key: 'selection',
    marksDirty: false,
    apply() {
      sel.selectedObjects = new Set(next.selected);
      sel.selectionAnchorObjectId = next.anchor;
    },
    revert() {
      sel.selectedObjects = new Set(prev.selected);
      sel.selectionAnchorObjectId = prev.anchor;
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
      // `offset` lives on Profile / Engrave / DragKnife only; the
      // auto-fix shouldn't have routed a Pocket / Drill / Chamfer
      // / VCarve / Thread id to this command. Guard at the boundary.
      if (!cur || !('offset' in cur)) return;
      prev = cur.offset;
      t.operations = t.operations.map((o) =>
        o.id === opId ? ({ ...o, offset } as OpEntry) : o,
      );
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
///
/// Note: `settings` is per-installation (wiac.settings localStorage,
/// outside the project snapshot). Don't flip `t.dirty` — accepting the
/// auto-fix shouldn't mark the project file as having unsaved changes
/// (audit zxee). The setting still persists via `project.saveSettings()`
/// which the ErrorToast call site fires explicitly.
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
