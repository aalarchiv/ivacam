// Global project state, Svelte 5 runes.
// Holds the most recently imported geometry plus UI flags.

import type {
  GenerateResponse,
  ImportResponse,
  ImportedObject,
  Point2,
  Segment,
  SimDiagnostics,
  SimWarning,
  SimSeverity,
  WiacError,
} from '../api/types';
import { History } from './history';
import { workspace } from './workspace.svelte';
import { isTauri as isTauriEnv } from '../api/env';

function isAbsolutePath(p: string): boolean {
  return p.startsWith('/') || /^[a-zA-Z]:[\\/]/.test(p);
}

import {
  addFixtureCommand,
  addOperationCommand,
  addTabCommand,
  addToolCommand,
  appendImportedCommand,
  clearTabsCommand,
  deleteOperationCommand,
  deleteToolCommand,
  duplicateOperationCommand,
  removeFixtureCommand,
  removeTabCommand,
  reorderOperationCommand,
  replaceToolsCommand,
  setMachineCommand,
  setStockCommand,
  updateFixtureCommand,
  updateOperationCommand,
  type CommandTarget,
} from './commands';

const SETTINGS_KEY = 'wiac.settings';
const LEGACY_THEME_KEY = 'wiac.theme';
const LEGACY_LOCALE_KEY = 'wiac.locale';

export interface AppSettings {
  theme: 'auto' | 'light' | 'dark';
  language: 'en' | 'de';
  previewMode: 'wireframe' | 'solid' | 'both';
  solidColor: string;
  solidOpacity: number;
  edgeColor: string;
  edgeOpacity: number;
  cellResolutionMode: 'auto' | 'manual';
  cellResolutionMm: number;
  solidPreviewByDefault: boolean;
  maxSimulationCells: number;
  /// When true, GenerateBar refuses to emit gcode while the most recent
  /// sim run reported critical warnings (collisions, rapid-through-stock).
  blockOnCriticalSimWarnings: boolean;
  /// When true, the sim driver keeps the playhead replayed to 1.0 after
  /// every project save / regenerate so warnings surface immediately.
  autoRunSimOnSave: boolean;
  /// Tauri-only: when true, source DXF/SVG/image files are reloaded
  /// automatically when their on-disk content changes (e.g. the user
  /// hits Ctrl+S in their CAD app). When false the user gets a
  /// "Reload?" toast instead.
  autoReloadSources: boolean;
  /// When true, Scene3D draws a translucent stock-envelope box at all
  /// times (not only when the sim heightfield is active). Combined with
  /// the per-project `stock.visible` toggle.
  showStockBox: boolean;
}

export const DEFAULT_SETTINGS: AppSettings = {
  theme: 'auto',
  language: 'en',
  previewMode: 'wireframe',
  solidColor: '#c8b48a',
  solidOpacity: 0.5,
  edgeColor: '#1a1a1a',
  edgeOpacity: 1.0,
  cellResolutionMode: 'auto',
  cellResolutionMm: 0.2,
  solidPreviewByDefault: false,
  maxSimulationCells: 4_000_000,
  blockOnCriticalSimWarnings: false,
  autoRunSimOnSave: true,
  autoReloadSources: true,
  showStockBox: true,
};

/// Load persisted settings, deep-merging stored values over defaults so
/// adding new keys later doesn't break old payloads. Falls back to the
/// legacy `wiac.theme` / `wiac.locale` keys when no `wiac.settings` blob
/// exists yet (first run after the dialog ships).
function loadSettings(): AppSettings {
  if (typeof window === 'undefined') return { ...DEFAULT_SETTINGS };
  let merged: AppSettings = { ...DEFAULT_SETTINGS };
  try {
    const raw = window.localStorage.getItem(SETTINGS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<AppSettings> | null;
      if (parsed && typeof parsed === 'object') {
        merged = { ...merged, ...parsed };
      }
      return merged;
    }
    // Migration path: seed from legacy single-purpose keys.
    const legacyTheme = window.localStorage.getItem(LEGACY_THEME_KEY);
    if (legacyTheme === 'auto' || legacyTheme === 'light' || legacyTheme === 'dark') {
      merged.theme = legacyTheme;
    }
    const legacyLocale = window.localStorage.getItem(LEGACY_LOCALE_KEY);
    if (legacyLocale === 'en' || legacyLocale === 'de') {
      merged.language = legacyLocale;
    }
  } catch {
    // localStorage unavailable / quota / parse failure → defaults are fine.
  }
  return merged;
}

class ProjectState {
  imported = $state<ImportResponse | null>(null);
  generated = $state<GenerateResponse | null>(null);
  loading = $state(false);
  generating = $state(false);
  /// Last error surfaced to the user. `string` for legacy paths (file
  /// upload, save dialogs, etc.); `WiacError` for backend pipeline /
  /// import errors so the toast can render recovery hints + auto-fix.
  error = $state<string | WiacError | null>(null);
  visibleLayers = $state<Set<string>>(new Set());

  /// Streaming pipeline state. `idle` between runs; `running` while the
  /// pipeline is actively emitting per-op events; `cancelling` after
  /// the user clicked Cancel and we're waiting for the worker to bail;
  /// `completed` for a brief beat after success so the UI can flash
  /// the success state before reverting to idle.
  pipelineState = $state<'idle' | 'running' | 'cancelling' | 'completed'>('idle');
  /// Last per-op progress event for the GenerateProgress UI. Reset to
  /// null when `pipelineState` returns to idle.
  pipelineProgress = $state<{
    opIdx: number;
    opTotal: number;
    opFraction: number;
    opName: string;
  } | null>(null);
  /// Stats from the most recent generate run: how many ops were served
  /// from the per-op result cache vs recomputed. Surfaced in the
  /// GenerateBar as "N of M cached" so the user can see when re-Generate
  /// hit cache instead of recomputing. Reset on each new generate.
  lastGenerateCachedCount = $state<number>(0);
  lastGenerateOpCount = $state<number>(0);

  /// Per-segment hover indicator (single segment, not the chain).
  hoverSegment = $state<number | null>(null);
  /// Object-level selection. Each id is a 1-based chain id from
  /// imported.objects (0 = unchained segment). Used by the operations
  /// list when the user clicks "Set source from selection".
  selectedObjects = $state<Set<number>>(new Set());
  /// Legacy entity-level selection (per-segment); kept for the project
  /// file but no longer drives the UI.
  selectedEntities = $state<Set<number>>(new Set());

  /// Toolpath scrub position in [0, 1]. Read by Scene3D for the tool-tip
  /// indicator and by PlaybackBar for the slider. Interpreted as a
  /// fraction of total ARC LENGTH (not segment count), so cutter speed
  /// stays consistent across short connectors and long edges. The
  /// playhead → segment mapping uses `toolpathCumLen` below.
  playhead = $state(1.0);

  /// Cumulative arc length per toolpath segment, computed when
  /// `setGenerated` is called. Index `i` holds the length-up-through
  /// segment `i` (so cumLen[total-1] = total arc length, cumLen[0] =
  /// length of segment 0). Used by `playheadToSegment` to map
  /// playhead → segment index by arc length so playback feels uniform
  /// across segment densities (a 50 mm boundary edge and a 1.5 mm
  /// zigzag connector each take time proportional to length, instead
  /// of both consuming 1/total_segments of playback).
  ///
  /// Arcs (MoveKind::Arc) are approximated as their straight-line
  /// chord here — slight underestimate but fine for visual playback
  /// since we don't have I/J center data on the frontend.
  toolpathCumLen = $state<Float64Array | null>(null);
  toolpathTotalLen = $state(0.0);

  /// Tab placements per imported segment index. Each tab is a position
  /// where the cutter lifts to clear the workpiece. The CAM core honors
  /// these via `setup.tabs.data` once gas.6 lands; until then they are
  /// purely visual + persisted in .vc-project.json.
  tabs = $state<Record<number, Tab[]>>({});

  /// Project fixtures (clamps, dogs, vise jaws). Threaded into the
  /// sim's collision check so the cutter can't run them over.
  fixtures = $state<Fixture[]>([]);
  /// 2D / 3D selection of the currently-edited fixture (id). Drives
  /// the highlight + the sidebar's edit form.
  selectedFixtureId = $state<number | null>(null);

  /// UI mode for placing tabs by clicking in the 2D canvas.
  tabMode = $state(false);

  /// Stock visualization in the 3D view. `auto` (default) derives the
  /// rectangular extent from the imported bbox plus a small margin and
  /// uses setup.mill.depth for the thickness; explicit values override.
  /// `visible` toggles the translucent box without losing the dimensions.
  stock = $state<StockConfig>({
    visible: true,
    mode: 'auto',
    margin: 5,
    thickness: 5,
    customX: 100,
    customY: 100,
  });

  /// Project-scoped tool library. Replaces the single `setup.tool`
  /// configured via SetupPanel; ops will reference an entry by id once
  /// the operations list lands. Today these don't drive Generate yet
  /// (the legacy setup path is still wired) but they're persisted via
  /// .vc-project so the user can curate a stable set across sessions.
  tools = $state<ToolEntry[]>([
    {
      id: 1,
      name: '3 mm endmill',
      kind: 'endmill',
      diameter: 3,
      flutes: 2,
      speed: 18000,
      plungeRate: 100,
      feedRate: 800,
      coolant: 'off',
    },
  ]);

  /// Project-scoped machine settings. Same story as tools — duplicates
  /// `setup.machine` until the rewire lands but is the source of truth
  /// going forward.
  machine = $state<MachineSettings>({
    unit: 'mm',
    mode: 'mill',
    comments: true,
    arcs: true,
    supportsToolchange: false,
    fastMoveZ: 5,
    accel: { x: 250, y: 250, z: 250 },
    toolchangeS: 5,
    rapidSpeed: 5000,
  });

  /// Ordered list of operations the program runs. Each op has a kind, a
  /// tool reference (id into project.tools), a source (which geometry it
  /// consumes), and per-kind parameters. Reordering = changing run
  /// order. Disabling = excluding from the final program without
  /// losing config.
  operations = $state<OpEntry[]>([]);
  /// id of the currently-selected op (drives OpPropertiesPanel).
  selectedOpId = $state<number | null>(null);
  /// True when the in-memory project differs from the gcode currently
  /// shown in `generated`. Set by op edits/reorders/enable toggles;
  /// cleared by setGenerated. The status badge in the ops list reads
  /// this so the user knows "re-Generate to refresh".
  dirty = $state(false);

  /// Whether the 2D canvas paints the filled-region preview for Pocket
  /// ops on top of the wireframe. Default on — it's the answer to
  /// "what will this op actually machine?".
  regionsVisible = $state(true);

  /// Per-installation user preferences. Persisted to localStorage under
  /// `wiac.settings`; not part of .vc-project (those are per-project).
  /// The SettingsDialog owns the UX; consumers (theme application, i18n
  /// init, future cutting-preview rendering) read from here.
  settings = $state<AppSettings>(loadSettings());

  /// Most recent sim diagnostics, written through by the sim driver
  /// after each forward advance(). Null = no sim run yet (or the
  /// preview is in pure wireframe mode and no driver is built).
  simDiagnostics = $state<SimDiagnostics | null>(null);

  /// Undo / redo. Per-session only; not serialized to .vc-project.json.
  /// View-state (selection, playhead, layer visibility, settings) is
  /// excluded — see history.ts for the full list.
  history = new History();

  /// Reactive mirror of `history.version` so $derived expressions in the
  /// UI re-run when the stacks change (the History class is plain TS so
  /// it can't be a $state itself).
  historyVersion = $state(0);

  /// Absolute path of the source file backing the current `imported`
  /// payload, when it was loaded from disk via `loadFromPath`. Drives
  /// the auto-reload watcher and the "Reload?" toast.
  lastImportPath = $state<string | null>(null);

  /// Absolute path of the currently-open project, or null if the user
  /// hasn't loaded one yet. Drives both the per-project workspace state
  /// look-up (eb8.6) and the watch set for source-change events (eb8.4).
  /// Set explicitly via `setActiveProjectPath` from the open-project
  /// flows. Not part of `snapshot()` — workspace state follows the
  /// path, the path is per-machine.
  activeProjectPath = $state<string | null>(null);

  /// Source-file change indicator. Populated when the watcher fires and
  /// the user has `autoReloadSources` disabled. SourceStaleToast renders
  /// from this and clears it on Reload / Ignore. Auto-reloads bypass it.
  sourceFileStaleNotice = $state<{ path: string; auto_reload: boolean } | null>(null);

  /// Drives the Tool library dialog. When non-null, App.svelte opens the
  /// dialog and the dialog scrolls/highlights the row whose id matches.
  /// Set via the "edit this tool" link in OpPropertiesPanel; cleared by
  /// the dialog on close. Per-session view state, not undoable.
  toolsDialogFocusId = $state<number | null>(null);


  constructor() {
    this.history.subscribe(() => {
      this.historyVersion = this.history.version;
    });
  }

  /// Switch the active project path and apply the persisted per-project
  /// workspace state (visible_layers / selected_op_id / playhead). Call
  /// AFTER `setImported` / `restore` so the layer set is already populated
  /// — we filter the saved layer names against what the import actually
  /// contains.
  setActiveProjectPath(path: string | null) {
    this.activeProjectPath = path;
    void this.refreshSourceWatch();
    if (path == null) return;
    const saved = workspace.get().per_project[path];
    if (!saved) return;
    if (this.imported && saved.visible_layers.length > 0) {
      const valid = new Set(this.imported.layers.map((l) => l.name));
      const restored = saved.visible_layers.filter((n) => valid.has(n));
      if (restored.length > 0) this.visibleLayers = new Set(restored);
    }
    if (saved.selected_op_id != null && this.operations.some((o) => o.id === saved.selected_op_id)) {
      this.selectedOpId = saved.selected_op_id;
    }
    if (typeof saved.playhead === 'number') {
      this.playhead = Math.max(0, Math.min(1, saved.playhead));
    }
  }

  /// Persist the current per-project view state. Called from $effects in
  /// App.svelte when `visibleLayers` / `selectedOpId` / `playhead` change.
  /// No-op when no path is active (browser uploads, samples, etc.).
  persistPerProjectState() {
    const path = this.activeProjectPath;
    if (!path) return;
    workspace.setPerProject(path, {
      visible_layers: [...this.visibleLayers],
      selected_op_id: this.selectedOpId,
      playhead: this.playhead,
    });
  }

  /// Cast to `CommandTarget` for command builders. Single helper so the
  /// `as unknown as` dance lives in one place.
  private target(): CommandTarget {
    return this as unknown as CommandTarget;
  }

  undo(): boolean {
    return this.history.undo(this.target());
  }
  redo(): boolean {
    return this.history.redo(this.target());
  }
  canUndo(): boolean {
    void this.historyVersion;
    return this.history.undoSize > 0;
  }
  canRedo(): boolean {
    void this.historyVersion;
    return this.history.redoSize > 0;
  }
  undoLabel(): string | null {
    void this.historyVersion;
    return this.history.undoLabel();
  }
  redoLabel(): string | null {
    void this.historyVersion;
    return this.history.redoLabel();
  }

  setSimDiagnostics(d: SimDiagnostics | null) {
    this.simDiagnostics = d;
  }

  /// Persist `settings` to localStorage. Cheap (one JSON.stringify on a
  /// tiny object) so we just call it on every mutation rather than
  /// debouncing — the dialog won't fire updates fast enough to matter.
  saveSettings() {
    if (typeof window === 'undefined') return;
    try {
      window.localStorage.setItem(SETTINGS_KEY, JSON.stringify(this.settings));
    } catch {
      // ignore — quota / disabled storage are non-fatal here.
    }
  }

  updateSettings(patch: Partial<AppSettings>) {
    this.settings = { ...this.settings, ...patch };
    this.saveSettings();
  }

  addTab(segmentIdx: number, position: Point2) {
    this.history.exec(addTabCommand(segmentIdx, { x: position.x, y: position.y }), this.target());
  }

  removeTab(segmentIdx: number, tabIdx: number) {
    const list = this.tabs[segmentIdx];
    if (!list || tabIdx < 0 || tabIdx >= list.length) return;
    this.history.exec(removeTabCommand(segmentIdx, tabIdx), this.target());
  }

  clearTabs() {
    if (Object.keys(this.tabs).length === 0) return;
    this.history.exec(clearTabsCommand(), this.target());
  }

  // ── fixtures ─────────────────────────────────────────────────────────

  addFixture(
    kind: FixtureKind,
    origin: [number, number],
    z_bottom: number,
    z_top: number,
    name?: string,
  ): Fixture {
    const nextId = this.fixtures.reduce((m, f) => Math.max(m, f.id), 0) + 1;
    const f: Fixture = {
      id: nextId,
      name: name ?? defaultFixtureName(kind, nextId),
      kind,
      origin,
      z_bottom,
      z_top,
      color: DEFAULT_FIXTURE_COLOR,
    };
    this.history.exec(addFixtureCommand(f), this.target());
    this.selectedFixtureId = f.id;
    return f;
  }

  updateFixture(id: number, patch: Partial<Fixture>) {
    this.history.exec(updateFixtureCommand(id, patch), this.target());
  }

  removeFixture(id: number) {
    if (!this.fixtures.some((f) => f.id === id)) return;
    this.history.exec(removeFixtureCommand(id), this.target());
    if (this.selectedFixtureId === id) this.selectedFixtureId = null;
  }

  selectFixture(id: number | null) {
    this.selectedFixtureId = id;
  }

  setImported(r: ImportResponse, sourcePath?: string | null) {
    this.imported = r;
    this.generated = null;
    this.toolpathCumLen = null;
    this.toolpathTotalLen = 0;
    this.dirty = true;
    this.error = null;
    this.visibleLayers = new Set(r.layers.map((l) => l.name));
    this.selectedEntities = new Set();
    this.selectedObjects = new Set();
    this.hoverSegment = null;
    this.tabs = {};
    if (sourcePath !== undefined) this.lastImportPath = sourcePath;
    this.sourceFileStaleNotice = null;
    // Imports cross a project boundary; undoing back across that boundary
    // would mix incompatible geometry/op state, so drop history here.
    this.history.clear();
    void this.refreshSourceWatch();
  }


  /// Refresh the desktop file-system watcher to track every absolute
  /// source path the project depends on. No-op outside Tauri; failure
  /// surfaces as a console warning so the rest of the app isn't blocked
  /// when the watcher backend is unavailable (e.g. inotify quota hit).
  async refreshSourceWatch(): Promise<void> {
    if (typeof window === 'undefined') return;
    if (!isTauriEnv()) return;
    const paths = new Set<string>();
    if (this.lastImportPath && isAbsolutePath(this.lastImportPath)) {
      paths.add(this.lastImportPath);
    }
    if (this.activeProjectPath && isAbsolutePath(this.activeProjectPath)) {
      paths.add(this.activeProjectPath);
    }
    try {
      const mod = await import('../api/tauri');
      await mod.watchSourcePaths(Array.from(paths));
    } catch (e) {
      console.warn('source watch:', e);
    }
  }

  /// Drop every active watch slot. Called when the project closes.
  async stopSourceWatch(): Promise<void> {
    if (typeof window === 'undefined') return;
    if (!isTauriEnv()) return;
    try {
      const mod = await import('../api/tauri');
      await mod.unwatchAll();
    } catch (e) {
      console.warn('source watch stop:', e);
    }
  }

  /// Re-import the named source path and swap it in. Wraps the swap as a
  /// single-step undoable transaction so Ctrl+Z reverts to the prior
  /// geometry. Used by both the auto-reload effect and the manual
  /// "Reload" button on SourceStaleToast.
  ///
  /// After the swap, ops whose `sourceObjects` reference object ids no
  /// longer present in the new geometry are flagged via console.warn —
  /// richer recovery is a follow-up. Returns true on success.
  async reimportFromPath(path: string): Promise<boolean> {
    if (typeof window === 'undefined') return false;
    if (!isTauriEnv()) return false;
    const before = this.imported ? structuredClone(this.imported) : null;
    let after: ImportResponse;
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      after = await invoke<ImportResponse>('import_path', { path });
    } catch (e) {
      this.setError(`reload: ${e instanceof Error ? e.message : String(e)}`);
      return false;
    }
    this.history.beginTransaction('Reload source');
    try {
      this.history.exec(appendImportedCommand({ before, after }), this.target());
    } finally {
      this.history.commitTransaction();
    }
    this.lastImportPath = path;
    this.sourceFileStaleNotice = null;
    const presentIds = new Set(after.objects ?? []);
    for (const op of this.operations) {
      if (!Array.isArray(op.sourceObjects) || op.sourceObjects.length === 0) continue;
      const orphans = op.sourceObjects.filter((id) => !presentIds.has(id));
      if (orphans.length > 0) {
        console.warn(
          `op "${op.name}" (#${op.id}): source geometry missing for ids ${orphans.join(', ')}`,
        );
      }
    }
    return true;
  }

  toggleObject(id: number, additive = false) {
    if (id <= 0) return;
    const next = additive ? new Set(this.selectedObjects) : new Set<number>();
    if (additive && next.has(id)) next.delete(id);
    else next.add(id);
    this.selectedObjects = next;
  }
  clearSelection() {
    this.selectedObjects = new Set();
  }

  setGenerated(r: GenerateResponse) {
    this.generated = r;
    // Pre-compute cumulative arc length over the toolpath so playback
    // can advance by physical distance instead of segment count. See
    // `playheadToSegment` for the inverse lookup.
    const tp = r.toolpath;
    if (tp.length > 0) {
      const cum = new Float64Array(tp.length);
      let acc = 0;
      for (let i = 0; i < tp.length; i++) {
        const s = tp[i];
        const dx = s.to.x - s.from.x;
        const dy = s.to.y - s.from.y;
        const dz = s.to.z - s.from.z;
        acc += Math.hypot(dx, dy, dz);
        cum[i] = acc;
      }
      this.toolpathCumLen = cum;
      this.toolpathTotalLen = acc;
    } else {
      this.toolpathCumLen = null;
      this.toolpathTotalLen = 0;
    }
    this.dirty = false;
    this.error = null;
    this.playhead = 1.0;
  }

  setError(err: string | WiacError) {
    this.error = err;
  }

  clearError() {
    this.error = null;
  }

  toggleLayer(name: string) {
    const next = new Set(this.visibleLayers);
    if (next.has(name)) next.delete(name);
    else next.add(name);
    this.visibleLayers = next;
  }

  /// Snapshot for project save.
  snapshot(): ProjectFile {
    return {
      kind: 'wiac-project',
      version: 1,
      imported: this.imported,
      visibleLayers: [...this.visibleLayers],
      selectedEntities: [...this.selectedEntities],
      tabs: this.tabs,
      stock: this.stock,
      tools: this.tools,
      machine: this.machine,
      operations: this.operations,
      fixtures: this.fixtures,
    };
  }

  restore(file: ProjectFile) {
    if (file.kind !== 'wiac-project') {
      throw new Error('not a wiaConstructor project file');
    }
    if (file.imported) this.setImported(file.imported, null);
    this.visibleLayers = new Set(file.visibleLayers ?? []);
    this.selectedEntities = new Set(file.selectedEntities ?? []);
    this.tabs = file.tabs ?? {};
    if (file.stock) this.stock = { ...this.stock, ...file.stock };
    if (Array.isArray(file.tools) && file.tools.length > 0) this.tools = file.tools;
    if (file.machine) this.machine = { ...this.machine, ...file.machine };
    if (Array.isArray(file.operations)) this.operations = file.operations;
    this.fixtures = Array.isArray(file.fixtures) ? file.fixtures : [];
    this.selectedFixtureId = null;
    this.selectedOpId = null;
    // Loading a project resets to a clean undo baseline.
    this.history.clear();
  }

  /// Append the rendered segments from AddTextDialog to the imported
  /// geometry layer and return the 1-based object ids the chaining pass
  /// produced for them. The chaining pass owns object id assignment, so
  /// after appending we re-run the lightweight client-side approximation:
  /// each closed contour gets a fresh contiguous id higher than any
  /// existing one. This keeps the dialog's "use these objects as the op's
  /// source" wiring correct without round-tripping through /import.
  ///
  /// `singleLine` — when true, segments are open polylines (engraving
  /// strokes) and should NOT be treated as closed objects; they go in as
  /// id 0 (unchained), but we still return an array of ids so callers
  /// can use the same flow.
  appendImportedSegments(segments: Segment[], layerName: string, singleLine: boolean): number[] {
    const before: ImportResponse | null = this.imported ? structuredClone(this.imported) : null;
    if (!this.imported) {
      const empty: ImportResponse = {
        filename: 'text',
        format: 'text',
        bbox: { min_x: 0, min_y: 0, max_x: 0, max_y: 0 },
        layers: [],
        segments: [],
        unit_scale: 1,
        warnings: [],
        objects: [],
        object_meta: [],
      };
      this.imported = empty;
    }
    const cur = this.imported!;
    const baseObjId = (cur.objects ?? []).reduce((m, o) => Math.max(m, o), 0);

    // Group consecutive segments by closed contour heuristic: each chain
    // of head→tail-touching segments becomes one object. Open polylines
    // (single_line) get id 0 (unchained).
    const newObjects: number[] = [];
    const newMeta: ImportedObject[] = [];
    if (singleLine) {
      newObjects.push(...new Array(segments.length).fill(0));
    } else {
      let nextId = baseObjId;
      let curId: number | null = null;
      let prevEnd: { x: number; y: number } | null = null;
      const close = 1e-6;
      const eq = (a: { x: number; y: number }, b: { x: number; y: number }) =>
        Math.abs(a.x - b.x) < close && Math.abs(a.y - b.y) < close;
      for (const s of segments) {
        if (curId == null || prevEnd == null || !eq(prevEnd, s.start)) {
          nextId += 1;
          curId = nextId;
        }
        newObjects.push(curId);
        prevEnd = s.end;
      }
      // Build minimal object metadata. closed=true as a hint; the
      // backend will reclassify on next /generate. This is enough for
      // the OperationsList / canvas selection wiring to recognize the
      // ids without a round trip.
      const ids = Array.from(new Set(newObjects.filter((i) => i > baseObjId)));
      for (const id of ids) {
        const owned = segments.filter((_, i) => newObjects[i] === id);
        let minX = Infinity,
          minY = Infinity,
          maxX = -Infinity,
          maxY = -Infinity;
        for (const s of owned) {
          minX = Math.min(minX, s.start.x, s.end.x);
          minY = Math.min(minY, s.start.y, s.end.y);
          maxX = Math.max(maxX, s.start.x, s.end.x);
          maxY = Math.max(maxY, s.start.y, s.end.y);
        }
        newMeta.push({
          id,
          closed: true,
          layer: layerName,
          color: owned[0]?.color ?? 7,
          bbox: { min_x: minX, min_y: minY, max_x: maxX, max_y: maxY },
        });
      }
    }

    // Recompute layer summary.
    const layers = [...cur.layers];
    let layerEntry = layers.find((l) => l.name === layerName);
    if (!layerEntry) {
      layerEntry = { name: layerName, color: segments[0]?.color ?? 7, segment_count: 0 };
      layers.push(layerEntry);
    }
    layerEntry.segment_count += segments.length;

    // Expand bbox to enclose appended geometry.
    let bbox = { ...cur.bbox };
    for (const s of segments) {
      bbox.min_x = Math.min(bbox.min_x, s.start.x, s.end.x);
      bbox.min_y = Math.min(bbox.min_y, s.start.y, s.end.y);
      bbox.max_x = Math.max(bbox.max_x, s.start.x, s.end.x);
      bbox.max_y = Math.max(bbox.max_y, s.start.y, s.end.y);
    }
    if (cur.segments.length === 0) {
      // First import — bbox starts from the appended geometry only.
      bbox = {
        min_x: Math.min(...segments.flatMap((s) => [s.start.x, s.end.x])),
        min_y: Math.min(...segments.flatMap((s) => [s.start.y, s.end.y])),
        max_x: Math.max(...segments.flatMap((s) => [s.start.x, s.end.x])),
        max_y: Math.max(...segments.flatMap((s) => [s.start.y, s.end.y])),
      };
    }

    const after: ImportResponse = {
      ...cur,
      segments: [...cur.segments, ...segments],
      objects: [...(cur.objects ?? []), ...newObjects],
      object_meta: [...(cur.object_meta ?? []), ...newMeta],
      layers,
      bbox,
    };
    // If we just synthesized an empty import above, fold that into the
    // command's `before` so a single undo wipes the whole "Add Text" run.
    this.history.exec(appendImportedCommand({ before, after }), this.target());
    this.visibleLayers = new Set([...this.visibleLayers, layerName]);

    // Return the de-duplicated set of new object ids (in insertion order).
    const distinct: number[] = [];
    const seen = new Set<number>();
    for (const id of newObjects) {
      if (id > 0 && !seen.has(id)) {
        seen.add(id);
        distinct.push(id);
      }
    }
    return distinct;
  }

  // ── operation helpers ────────────────────────────────────────────────

  addOperation(kind: OpKind): OpEntry {
    const nextId = this.operations.reduce((m, o) => Math.max(m, o.id), 0) + 1;
    const tool = this.tools[0];
    const op: OpEntry = {
      id: nextId,
      name: prettyOpKind(kind),
      enabled: true,
      kind,
      toolId: tool?.id ?? 1,
      sourceCombine: 'auto',
      sourceLayers: null,
      depth: -2,
      startDepth: 0,
      step: -1,
      offset: kind === 'engrave' || kind === 'drag_knife' ? 'on' : 'outside',
      pocketStrategy: kind === 'pocket' ? 'cascade' : null,
      ...(kind === 'drill' ? { drillCycle: { kind: 'simple', dwell_sec: 0 } as DrillCycle } : {}),
      cutDirection: 'conventional',
      finishCutDirection: 'conventional',
      plunge: { kind: 'direct' },
      xyOverlap: 0.5,
      ...(kind === 'vcarve' ? { multiPassRefine: false } : {}),
    };
    this.history.exec(addOperationCommand(op), this.target());
    this.selectedOpId = op.id;
    return op;
  }

  removeOperation(id: number) {
    if (!this.operations.some((o) => o.id === id)) return;
    this.history.exec(deleteOperationCommand(id), this.target());
    if (this.selectedOpId === id) this.selectedOpId = null;
  }

  /// Deep-clone the op and insert it immediately after the original.
  /// Returns the new op or null if `id` is unknown.
  duplicateOperation(id: number): OpEntry | null {
    const src = this.operations.find((o) => o.id === id);
    if (!src) return null;
    const nextId = this.operations.reduce((m, o) => Math.max(m, o.id), 0) + 1;
    const copy: OpEntry = {
      ...structuredClone(src),
      id: nextId,
      name: `${src.name} (copy)`,
    };
    this.history.exec(duplicateOperationCommand(id, copy, id), this.target());
    this.selectedOpId = copy.id;
    return copy;
  }

  updateOperation(id: number, patch: Partial<OpEntry>) {
    if (!this.operations.some((o) => o.id === id)) return;
    this.history.exec(updateOperationCommand(id, patch), this.target());
  }

  /// Reorder. Skipped when source and target index are the same so a
  /// stray drag-and-drop with no actual move doesn't dirty the project.
  /// (A real reorder still flips dirty so the status badge surfaces it,
  /// but the previously-generated gcode stays on screen until the user
  /// clicks Generate again.)
  reorderOperation(id: number, toIndex: number) {
    const cur = this.operations.findIndex((o) => o.id === id);
    if (cur < 0) return;
    const clamped = Math.max(0, Math.min(toIndex, this.operations.length - 1));
    if (clamped === cur) return;
    this.history.exec(reorderOperationCommand(id, clamped), this.target());
  }

  // ── tool library ─────────────────────────────────────────────────────

  /// Replace the entire tool library in one undoable step. Used by the
  /// Tool library dialog's commit button.
  replaceTools(nextTools: ToolEntry[]) {
    if (nextTools.length === 0) return;
    this.history.exec(replaceToolsCommand(nextTools), this.target());
  }

  addTool(tool: ToolEntry) {
    this.history.exec(addToolCommand(tool), this.target());
  }

  removeTool(id: number) {
    if (!this.tools.some((t) => t.id === id)) return;
    this.history.exec(deleteToolCommand(id), this.target());
  }

  // ── machine / stock ──────────────────────────────────────────────────

  setMachine(next: MachineSettings) {
    this.history.exec(setMachineCommand(next), this.target());
  }

  setStock(patch: Partial<StockConfig>) {
    if (Object.keys(patch).length === 0) return;
    this.history.exec(setStockCommand(patch), this.target());
  }
}

function prettyOpKind(kind: OpKind): string {
  switch (kind) {
    case 'profile':
      return 'Profile';
    case 'pocket':
      return 'Pocket';
    case 'drill':
      return 'Drill';
    case 'thread':
      return 'Thread';
    case 'chamfer':
      return 'Chamfer';
    case 'engrave':
      return 'Engraving';
    case 'drag_knife':
      return 'Drag-knife';
    case 'helix':
      return 'Helix';
    case 'vcarve':
      return 'V-Carve';
  }
}

export interface Tab {
  x: number;
  y: number;
}

/// Mirrors `wiac_core::project::FixtureKind`. The `shape` discriminator
/// is the wire-side serde tag; vertex coords for `polygon` are local
/// (origin-relative) so the fixture can be moved by editing `origin`.
export type FixtureKind =
  | { shape: 'box'; width: number; depth: number }
  | { shape: 'cylinder'; radius: number }
  | { shape: 'polygon'; vertices: [number, number][] };

export interface Fixture {
  id: number;
  name: string;
  kind: FixtureKind;
  origin: [number, number];
  z_bottom: number;
  z_top: number;
  color: number;
}

/// Default packed RGBA color: amber, ~75% alpha.
export const DEFAULT_FIXTURE_COLOR = 0xffa050c0;

function defaultFixtureName(kind: FixtureKind, id: number): string {
  switch (kind.shape) {
    case 'box':
      return `Clamp ${id}`;
    case 'cylinder':
      return `Dog ${id}`;
    case 'polygon':
      return `Fixture ${id}`;
  }
}

export interface StockConfig {
  visible: boolean;
  mode: 'auto' | 'manual';
  margin: number;
  thickness: number;
  customX: number;
  customY: number;
}

export type { ToolKind, OpKind, ProfileOffset, SourceCombine, FrameShape } from './op_types';
import type { FrameShape, OpKind, ProfileOffset, SourceCombine, ToolKind } from './op_types';

export type CoolantMode = 'off' | 'mist' | 'flood';

export interface ToolEntry {
  id: number;
  name: string;
  kind: ToolKind;
  diameter: number;
  tipDiameter?: number;
  /// V-bit full apex angle in degrees. Drives the V-Carve depth math
  /// (`z = -R / tan(tipAngleDeg / 2)`); ignored for non-V tools.
  /// Optional in TS for back-compat with old project files; the wire
  /// payload omits it when undefined and the Rust side defaults to 60°.
  tipAngleDeg?: number;
  dragoff?: number;
  flutes: number;
  speed: number;
  plungeRate: number;
  feedRate: number;
  coolant: CoolantMode;
  /// Default depth-per-pass (negative, mm). Operations using this tool
  /// inherit this when their own `step` is unset.
  defaultStep?: number;
  /// Length of cutting flutes in mm. Undefined = treat the entire tool
  /// as cutting (legacy behavior — no holder collision check is done).
  fluteLengthMm?: number;
  /// Shank diameter in mm. Undefined = same as `diameter`
  /// (parallel-shank bit). Drives the holder/shank collision sweep.
  shankDiameterMm?: number;
  /// Holder geometry above the shank. Undefined = no holder check.
  holder?: HolderShape;
}

/// Tool holder geometry above the shank. Mirrors
/// `wiac_core::project::HolderShape`. v1 treats every holder as
/// cylindrically symmetric — set-screw flats and asymmetric ER nuts
/// are bounded by their enclosing cylinder/cone.
export type HolderShape =
  | { kind: 'cylinder'; diameter_mm: number; length_mm: number }
  | { kind: 'cone'; bottom_diameter_mm: number; top_diameter_mm: number; length_mm: number }
  | {
      kind: 'stepped';
      cylinder_diameter_mm: number;
      cylinder_length_mm: number;
      cone_top_diameter_mm: number;
      cone_length_mm: number;
    };

export interface AxisLimits {
  x: number;
  y: number;
  z: number;
}

export interface MachineSettings {
  unit: 'mm' | 'inch';
  mode: 'mill' | 'laser' | 'drag';
  comments: boolean;
  arcs: boolean;
  supportsToolchange: boolean;
  fastMoveZ: number;
  /// Per-axis acceleration (mm/s²). Optional — empty means defaults
  /// (250 mm/s² per axis, LinuxCNC convention).
  accel?: AxisLimits;
  /// Per-axis jerk (mm/s³). Optional — empty means trapezoidal-only
  /// profiling (S-curve is Phase 2).
  jerk?: AxisLimits;
  /// Tool-change time in seconds (default 5).
  toolchangeS?: number;
  /// Rapid (G0) speed in mm/min (default 5000).
  rapidSpeed?: number;
  /// Maximum chord-to-arc deviation (mm) when collapsing line runs into
  /// G2/G3 on emit. Only consulted when `arcs == true`. undefined ⇒
  /// 0.01 mm (the backend default).
  arcFitToleranceMm?: number;
}

export type PocketStrategy = 'cascade' | 'zigzag' | 'spiral' | 'trochoidal';
/// Cut direction for milling. `conventional` is the safer default —
/// cutter rotation opposes the feed at the contact point so chip starts
/// thin and grows; works on machines with backlash. `climb` is rotation
/// with feed → better surface finish but needs a rigid stiff machine.
/// See wiac_core::project::CutDirection for the winding rules.
export type CutDirection = 'conventional' | 'climb';

/// Plunge entry strategy. `direct` is a straight Z dive (current
/// behavior); `ramp` walks forward along the path while descending Z so
/// the cutter takes a chip in both directions simultaneously — required
/// for non-center-cutting bits and for harder materials. `helix` is a
/// start-of-cut spiral descent on a small circle inside the closed
/// pocket boundary — the standard for non-center-cutting endmills and
/// harder materials. Angles are in degrees, conservative default 3°.
/// Helix `radius_mm` is the spiral radius; pick something larger than
/// the tool radius so the helix carves a small clearance hole inside
/// the pocket. Sane default: 1.5 × tool radius. Set to null to auto-fit
/// the helix to the largest inscribed circle of the pocket boundary.
export type PlungeStrategy =
  | { kind: 'direct' }
  | { kind: 'ramp'; angle_deg: number }
  | { kind: 'helix'; angle_deg: number; radius_mm: number | null };
/// Drill cycle for an OperationKind::Drill op. Mirrors wiac_core::project::DrillCycle.
/// `simple` → G81; `peck` → G83 (full retract between pecks); `chip_break` → G73
/// (small partial retract between pecks). `dwell_sec` is the dwell at bottom in
/// seconds (0 = no dwell). `peck_step_mm` is the per-peck Z step.
export type DrillCycle =
  | { kind: 'simple'; dwell_sec?: number }
  | { kind: 'peck'; peck_step_mm: number; dwell_sec?: number }
  | { kind: 'chip_break'; peck_step_mm: number; dwell_sec?: number };

/// Thin frontend mirror of wiac_core::project::Operation. Tracks just
/// what the UI needs to show + edit; the wire format expands to the
/// full Operation when Generate ships.
export interface OpEntry {
  id: number;
  name: string;
  enabled: boolean;
  kind: OpKind;
  toolId: number;
  /// Source kind:
  ///   null              → all imported geometry (the default)
  ///   string[]          → run only on chains whose layer name is listed
  ///   { objects: [...]} → run only on the listed object ids (1-based)
  sourceLayers: string[] | null;
  sourceObjects?: number[];
  /// Combine mode for multi-object selections. Default 'auto' is the
  /// containment-aware behavior (outer + inner = annulus pocket); other
  /// modes drive clipper2 boolean ops on the selected closed polygons.
  /// Persisted in .vc-project so the user's choice survives reloads.
  sourceCombine?: SourceCombine;
  depth: number;
  startDepth: number;
  /// Per-pass Z step in mm (negative). null = inherit from the assigned
  /// tool's `defaultStep`; if that's also unset the backend warns
  /// `step_unspecified`.
  step: number | null;
  offset: ProfileOffset;
  pocketStrategy: PocketStrategy | null;
  /// Drill cycle for OperationKind::Drill. Honored only when `kind === 'drill'`.
  /// Default { kind: 'simple', dwell_sec: 0 } via addOperation.
  drillCycle?: DrillCycle;
  /// Main / roughing cut direction. Default 'conventional'.
  cutDirection?: CutDirection;
  /// Direction for the wall-defining finishing pass. Default
  /// 'conventional' regardless of cutDirection — surface quality on
  /// hobby machines is almost always best with conventional milling
  /// even when the roughing passes use climb.
  finishCutDirection?: CutDirection;
  /// How the cutter descends into material at the start of each Z
  /// pass. Default { kind: 'direct' }.
  plunge?: PlungeStrategy;
  /// XY overlap fraction in (0.05, 0.95) — drives the cascade step
  /// (= tool_diameter * (1 - overlap)) and zigzag stride. Default 0.5
  /// = 50% overlap. Higher = tighter cascade = better fill on small
  /// pockets. Honored only by Pocket ops.
  xyOverlap?: number;
  /// Trochoidal engagement angle in degrees. Drives the centerline
  /// pitch (step_main = tool_d * sin(eng/2)). Default 30°.
  engagementAngleDeg?: number;
  /// Trochoidal loop radius as a fraction of tool radius. Default 0.6.
  loopRadiusFactor?: number;
  /// Tab geometry. `tabType=rectangle` (default) is a straight Z lift
  /// over each tab; `tabType=ramp` runs a sloped ramp up to the tab top
  /// at `tabRampAngleDeg` (default 30°), holds the flat top, then ramps
  /// back down. The actual tab placements live on `project.tabs`; this
  /// just controls the shape of the cut over each tab. Per-op so a
  /// user can mix Rectangle pockets with Ramp profiles in one project.
  tabType?: 'rectangle' | 'ramp';
  tabRampAngleDeg?: number;
  /// Per-op feedrate override in mm/min. When set, replaces the tool's
  /// `feedRate` for this op (cutting feed). Useful for finishing passes
  /// or hard materials where you don't want to edit the tool entry.
  /// Undefined = use the tool's default.
  feedRateOverride?: number;
  /// Per-op plunge-rate override in mm/min. Replaces the tool's
  /// `plungeRate` for Z descents in this op only. Undefined = use the
  /// tool's default.
  plungeRateOverride?: number;
  /// When > 0, slow the feed at sharp Line→Line corners by this
  /// fraction. 0.0 (default) = no reduction. 0.5 = half feed at
  /// corners. Most useful for zigzag pocket fills.
  cornerFeedReduction?: number;
  /// Lead-in / lead-out shape for Profile (and other contour) ops.
  /// Default Off — straight rapid + plunge to the contour start.
  /// `straight` adds a perpendicular hop into the contour by `leadIn`
  /// mm; `arc` rolls onto the contour with a tangent quarter-arc of
  /// `leadIn` mm RADIUS so the cutter eases into the cut without
  /// dwelling at the start point. `leadOut` is the symmetric size for
  /// the roll-off motion at the end of the path.
  leadInKind?: 'off' | 'straight' | 'arc';
  leadOutKind?: 'off' | 'straight' | 'arc';
  /// Lead-in size in mm. Length when `leadInKind=straight`, arc radius
  /// when `leadInKind=arc`. Ignored when `leadInKind=off`.
  leadIn?: number;
  /// Lead-out size in mm. Same per-kind interpretation as `leadIn`.
  leadOut?: number;
  /// Optional smaller step for the FINAL Z pass (cleaner bottom). Same
  /// sign convention as `step` (negative). Undefined = use `step` for
  /// every pass.
  finishStep?: number;
  /// Cut past `depth` by this many mm (positive). Useful for
  /// through-cuts on edge-clamped sheet.
  throughDepth?: number;
  /// Explicit ordered list of Z depths (negative numbers). When
  /// non-empty, overrides `step`/`finishStep`/`throughDepth`.
  depthList?: number[];
  /// V-Carve cap on the inscribed-circle radius (mm). Undefined =
  /// no cap; the V-bit reaches the geometric medial axis. Useful for
  /// keeping the carve narrower than the bit's usable shoulder.
  carveMaxWidthMm?: number;
  /// V-Carve refinement pass toggle. Default false.
  multiPassRefine?: boolean;
  /// Pocket-Outside (rt1.3): when set, the op carves the area between a
  /// synthetic frame and the source selection. The frame is computed in
  /// the pipeline from these params — not persisted as project geometry.
  /// Set by the "Pocket Outside" entry in OperationsList.
  frameShape?: FrameShape;
  /// Padding (mm) added on every side of the selection bbox to size the
  /// frame. Auto-defaulted to 3 × tool diameter when the wrapper creates
  /// the op; once the user types a value it stays manual.
  framePaddingMm?: number;
  /// Corner radius (mm) for `frameShape === 'rounded_rectangle'`. Ignored
  /// otherwise. Undefined ⇒ backend defaults to `framePaddingMm`.
  frameCornerRadiusMm?: number;
}

export interface ProjectFile {
  kind: 'wiac-project';
  version: 1;
  imported: ImportResponse | null;
  visibleLayers: string[];
  selectedEntities: number[];
  tabs?: Record<number, Tab[]>;
  stock?: StockConfig;
  tools?: ToolEntry[];
  machine?: MachineSettings;
  operations?: OpEntry[];
  fixtures?: Fixture[];
}

export const project = new ProjectState();

/// Severity mapping for a sim warning. Mirrors
/// `wiac_core::sim::diagnostics::severity` so the UI can color-code
/// without a round-trip.
export function simWarningSeverity(w: SimWarning): SimSeverity {
  switch (w.kind) {
    case 'rapid_through_material':
    case 'fixture_collision':
    case 'holder_collision':
      return 'critical';
    case 'engagement_overload':
    case 'dragging_rapids':
      return 'warning';
  }
}

/// Segment index a warning attaches to. `dragging_rapids` reports a
/// run; we anchor it at the first segment in the run for marker
/// placement.
export function simWarningSegmentIdx(w: SimWarning): number {
  if (w.kind === 'dragging_rapids') return w.first_segment_idx;
  return w.segment_idx;
}

/// Short human-readable line for tooltips / list rows.
export function simWarningSummary(w: SimWarning): string {
  switch (w.kind) {
    case 'rapid_through_material':
      return `Rapid through material at segment ${w.segment_idx}, x=${w.worst_x.toFixed(1)} y=${w.worst_y.toFixed(1)}`;
    case 'fixture_collision':
      return `Fixture #${w.fixture_id} collision at segment ${w.segment_idx}`;
    case 'holder_collision':
      return `Tool holder hits wall at segment ${w.segment_idx} (clearance ${w.required_clearance_mm.toFixed(2)} mm)`;
    case 'engagement_overload':
      return `Engagement ${w.engagement_pct.toFixed(0)}% at segment ${w.segment_idx}`;
    case 'dragging_rapids':
      return `Dragging rapids: ${w.count} consecutive rapids from segment ${w.first_segment_idx}`;
  }
}

/// Map `playhead ∈ [0,1]` (fraction of total arc length) to a segment
/// index + parametric position within that segment. Returns
/// `{ segIdx, segT }` where `segT ∈ [0,1]` is the fractional distance
/// along segment `segIdx`. Returns `{ segIdx: -1, segT: 0 }` when the
/// toolpath is empty or there is no length to traverse.
///
/// Arc-length-based mapping is what makes playback feel uniform: a
/// 50 mm boundary edge takes ~33× longer than a 1.5 mm zigzag connector
/// at the same `speed`, instead of both consuming `1/total_segments`
/// of playback time.
export function playheadToSegment(
  playhead: number,
  cumLen: Float64Array | null,
  totalLen: number,
): { segIdx: number; segT: number } {
  if (!cumLen || cumLen.length === 0 || totalLen <= 0) {
    return { segIdx: -1, segT: 0 };
  }
  const clamped = Math.max(0, Math.min(1, playhead));
  const target = clamped * totalLen;
  // Binary search for the smallest i where cumLen[i] >= target.
  let lo = 0;
  let hi = cumLen.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (cumLen[mid] < target) lo = mid + 1;
    else hi = mid;
  }
  const segEndLen = cumLen[lo];
  const segStartLen = lo === 0 ? 0 : cumLen[lo - 1];
  const segLen = segEndLen - segStartLen;
  const segT = segLen > 1e-12 ? (target - segStartLen) / segLen : 0;
  return { segIdx: lo, segT };
}
