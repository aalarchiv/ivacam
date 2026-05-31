// Global project state, Svelte 5 runes.
// Holds the most recently imported geometry plus UI flags.

import type {
  GenerateResponse,
  ImportResponse,
  ImportedObject,
  Segment,
  SimDiagnostics,
  WiacError,
} from '../api/types';
import { History } from './history';
import { workspace } from './workspace.svelte';
import { invalidatePreview, resetPreviewCache } from './text_preview.svelte';
import { isTauri as isTauriEnv } from '../api/env';
import {
  GeneratedState,
  type PipelineNoteEvent,
  type PipelinePhase,
  type PipelineProgress,
} from './generated.svelte';
import { SelectionState, type PickMode, type SelectionMode } from './selection.svelte';

export type { PickMode };
import {
  ProjectDataState,
  DEFAULT_SETTINGS,
  saveSettings,
  type AppSettings,
} from './project-data.svelte';

export { DEFAULT_SETTINGS };
export type { AppSettings };
// Bring the union types into scope locally; project-types and op_types
// re-export them through this module for back-compat callers.
import { isContourOp, type OpEntry, type OpKind, type OpPatch } from './op_types';
import { migrateLegacyToolTerms } from './tool-migration';

// Pure-TypeScript data shapes live in project-types.ts so vitest specs
// and non-Svelte helpers can import them without booting the rune
// runtime (audit 6cpl). They're re-exported below for backwards-compat
// with the 40+ call sites that already import from this module.
import {
  DEFAULT_FIXTURE_COLOR,
  defaultAxesConfig,
  defaultFixtureName,
  identityFileTransform,
  isIdentityFileTransform,
  prettyOpKind,
} from './project-types';
import type {
  AxesConfig,
  AxisFormat,
  AxisLimits,
  CoolantMode,
  CutDirection,
  DrillCycle,
  FileTransform,
  Fixture,
  FixtureKind,
  FormProfileSample,
  HalfpipeProfile,
  HolderShape,
  ImportEntry,
  MachineSettings,
  PatternConfig,
  PlungeStrategy,
  PocketStrategy,
  PostProfile,
  ProjectFile,
  ReliefSource,
  SpindleDirection,
  StockConfig,
  TabPlacement,
  TabPlacementMode,
  TextAlignment,
  TextFontSource,
  TextLayer,
  TextLayerKind,
  ToolEntry,
  Wcs,
  WorkOffset,
} from './project-types';
import { defaultWorkOffset, inferDefaultWorkOffset, isDefaultWorkOffset } from './project-types';
import { migrateMachineSettings } from './project-types';
import {
  applyFileTransformToPoint,
  combineImports,
  invertFileTransformPoint,
} from './file-transform';

export {
  DEFAULT_FIXTURE_COLOR,
  defaultAxesConfig,
  defaultFixtureName,
  defaultWorkOffset,
  identityFileTransform,
  isDefaultWorkOffset,
  isIdentityFileTransform,
  prettyOpKind,
};
export type {
  AxesConfig,
  AxisFormat,
  AxisLimits,
  CoolantMode,
  CutDirection,
  DrillCycle,
  FileTransform,
  Fixture,
  FixtureKind,
  FormProfileSample,
  HalfpipeProfile,
  HolderShape,
  ImportEntry,
  MachineSettings,
  PatternConfig,
  PlungeStrategy,
  PocketStrategy,
  PostProfile,
  ProjectFile,
  ReliefSource,
  SpindleDirection,
  StockConfig,
  TabPlacement,
  TabPlacementMode,
  TextAlignment,
  TextFontSource,
  TextLayer,
  TextLayerKind,
  ToolEntry,
  Wcs,
  WorkOffset,
};
// OpEntry union + variant types live in op_types.ts; re-export from
// here for backwards-compat with call sites that imported them from
// state/project.svelte.
export type {
  ChamferOp,
  ContourFields,
  DragKnifeOp,
  DrillOp,
  EngraveOp,
  FrameShape,
  LeadKind,
  OpBase,
  OpEntry,
  OpField,
  OpFieldValue,
  OpKind,
  OpOfKind,
  OpPatch,
  PocketOp,
  ProfileOffset,
  ProfileOp,
  ReliefMillOp,
  ScanDirection,
  SourceCombine,
  TabType,
  ThreadOp,
  ToolKind,
  VCarveOp,
} from './op_types';
export { isContourOp, isPathOp } from './op_types';

// Pure 2D geometry primitives extracted to `lib/canvas/selection-geometry.ts`
// so vitest specs can exercise them without mounting the canvas (audit y0ez).
import { bboxOfSegments, lineCrossesBBox } from '../canvas/selection-geometry';
import { computeFootprint } from '../sim/driver';
import { augmentWithStockOutline } from './stock-outline';
import { pickBestToolForOp } from './tool_picker';

function isAbsolutePath(p: string): boolean {
  return p.startsWith('/') || /^[a-zA-Z]:[\\/]/.test(p);
}

/// Memoised bundled-font fetch — the DejaVu Sans bytes used as the
/// default font for imported DXF TEXT/MTEXT entities. Resolved once
/// per session and shared across every TextLayer created from
/// `imported.text_entities`. Returns base64 because that's the form
/// TextFontSource carries.
let _defaultFontBytesB64: Promise<string | null> | null = null;
function loadDefaultFontBytesB64(): Promise<string | null> {
  if (_defaultFontBytesB64) return _defaultFontBytesB64;
  _defaultFontBytesB64 = (async () => {
    try {
      const res = await fetch('/fonts/DejaVuSans.ttf');
      if (!res.ok) return null;
      const buf = new Uint8Array(await res.arrayBuffer());
      let binary = '';
      const chunk = 0x8000;
      for (let i = 0; i < buf.length; i += chunk) {
        binary += String.fromCharCode(...buf.subarray(i, i + chunk));
      }
      return btoa(binary);
    } catch {
      return null;
    }
  })();
  return _defaultFontBytesB64;
}

import {
  addFixtureCommand,
  addOperationCommand,
  addReliefSourceCommand,
  addTextLayerCommand,
  addToolCommand,
  deleteOperationCommand,
  deleteReliefSourceCommand,
  deleteTextLayerCommand,
  deleteToolCommand,
  duplicateOperationCommand,
  removeFixtureCommand,
  reorderOperationCommand,
  replaceToolsCommand,
  selectObjectsCommand,
  setImportsCommand,
  setMachineCommand,
  setStockCommand,
  setWorkOffsetCommand,
  toggleTabPlacementCommand,
  updateFixtureCommand,
  updateOperationCommand,
  updateReliefSourceCommand,
  updateTextLayerCommand,
  type CommandTarget,
} from './commands';
import { computeSelectionUpdate, selectionsEqual } from './selection.svelte';

class ProjectState {
  /// Project-data slice (audit 6cpl step 4 / n5v5). Owns `imported`,
  /// `operations`, `tools`, `machine`, `stock`, `fixtures`,
  /// `textLayers`, `dirty`, `visibleLayers`, `regionsVisible`, and
  /// `settings` — i.e. every field the undo/redo command bus mutates.
  /// The proxy getters/setters below forward `project.imported` etc.
  /// to `this.data.…` so existing call sites stay unchanged.
  data = new ProjectDataState();

  /// Raw imports array (wrsu). Every external consumer reads this via
  /// `transformedImport` (combined view); per-entry mutations go through
  /// `addImported`, `removeImport`, `patchFileTransformForImport`, and
  /// `resetFileTransformForImport`.
  get imports(): ImportEntry[] {
    return this.data.imports;
  }
  set imports(v: ImportEntry[]) {
    this.data.imports = v;
  }

  /// All imports merged into one ImportResponse with each entry's
  /// fileTransform applied (wrsu Phase 2). Every visual consumer (canvas
  /// / 3D scene / OSnap / sim / build-project payload / footprint) reads
  /// this rather than `imported`, so the user sees N drawings on stock
  /// with independent layout transforms.
  ///
  /// Single-entry case short-circuits to `applyFileTransform(entry.source,
  /// entry.fileTransform)` — same identity-fast path as Phase 1. Multi-
  /// entry case namespaces object ids (entries[0] keeps ids 1..N, later
  /// entries get the next range) so existing op references stay valid.
  transformedImport = $derived.by<ImportResponse | null>(() => {
    return combineImports(this.data.imports);
  });

  /// 8jce: geometry the canvas selects + the wire payload sends —
  /// `transformedImport` plus a synthetic, selectable stock-outline
  /// object when the stock is shown, so an op (chamfer/profile/…) can
  /// target the workpiece edge. Returns the SAME object as
  /// `transformedImport` when the stock is hidden or the footprint is
  /// degenerate (referential identity), so the canvas / build path see
  /// no change in the common case. Auto-stock sizing keeps reading the
  /// raw `transformedImport` (see `computeFootprint` callers) — feeding
  /// this back would loop (outline ← footprint ← bbox ← outline).
  geometryView = $derived.by<ImportResponse | null>(() => {
    const base = this.transformedImport;
    if (!this.data.stock.visible) return base;
    const fp = computeFootprint(base, this.data.stock, this.data.machine.workArea);
    return augmentWithStockOutline(base, fp);
  });

  loading = $state(false);
  loadingMessage = $state<string | null>(null);
  /// Last error surfaced to the user. `string` for legacy paths (file
  /// upload, save dialogs, etc.); `WiacError` for backend pipeline /
  /// import errors so the toast can render recovery hints + auto-fix.
  error = $state<string | WiacError | null>(null);
  get visibleLayers(): Set<string> {
    return this.data.visibleLayers;
  }
  set visibleLayers(v: Set<string>) {
    this.data.visibleLayers = v;
  }

  /// Generate-pipeline slice (audit 6cpl step 2). Holds `generated`,
  /// `generating`, `pipelineState`/`pipelineProgress`,
  /// `lastGenerateOpCount` / `lastGenerateCachedCount`,
  /// `toolpathCumLen` / `toolpathTotalLen`, `simDiagnostics`, plus
  /// the lifecycle methods. The `get …` accessors below forward every
  /// `project.generated` / `project.pipelineState` etc. call site to
  /// `this.gen.…` so the existing API surface is unchanged.
  gen = new GeneratedState();

  get generated(): GenerateResponse | null {
    return this.gen.generated;
  }
  set generated(v: GenerateResponse | null) {
    this.gen.generated = v;
  }
  get generatedVersion(): number {
    return this.gen.generatedVersion;
  }
  set generatedVersion(v: number) {
    this.gen.generatedVersion = v;
  }
  get generating(): boolean {
    return this.gen.generating;
  }
  set generating(v: boolean) {
    this.gen.generating = v;
  }
  get pipelineState(): PipelinePhase {
    return this.gen.pipelineState;
  }
  set pipelineState(v: PipelinePhase) {
    this.gen.pipelineState = v;
  }
  get pipelineProgress(): PipelineProgress | null {
    return this.gen.pipelineProgress;
  }
  set pipelineProgress(v: PipelineProgress | null) {
    this.gen.pipelineProgress = v;
  }
  get lastGenerateCachedCount(): number {
    return this.gen.lastGenerateCachedCount;
  }
  set lastGenerateCachedCount(v: number) {
    this.gen.lastGenerateCachedCount = v;
  }
  get lastGenerateOpCount(): number {
    return this.gen.lastGenerateOpCount;
  }
  set lastGenerateOpCount(v: number) {
    this.gen.lastGenerateOpCount = v;
  }

  /// UI-selection slice (audit 6cpl). Holds hoverSegment, the
  /// selectedObjects / anchor / entities sets, plus the selectedOpId /
  /// selectedFixtureId / selectedTextLayerId / toolsDialogFocusId
  /// pointers. The proxy accessors below forward
  /// `project.selectedObjects` / `project.selectedOpId` etc. to
  /// `this.sel.…` so existing call sites stay unchanged.
  sel = new SelectionState();

  get hoverSegment(): number | null {
    return this.sel.hoverSegment;
  }
  set hoverSegment(v: number | null) {
    this.sel.hoverSegment = v;
  }
  get selectedObjects(): Set<number> {
    return this.sel.selectedObjects;
  }
  set selectedObjects(v: Set<number>) {
    this.sel.selectedObjects = v;
  }
  get selectionAnchorObjectId(): number | null {
    return this.sel.selectionAnchorObjectId;
  }
  set selectionAnchorObjectId(v: number | null) {
    this.sel.selectionAnchorObjectId = v;
  }
  get selectedEntities(): Set<number> {
    return this.sel.selectedEntities;
  }
  set selectedEntities(v: Set<number>) {
    this.sel.selectedEntities = v;
  }

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
  get toolpathCumLen(): Float64Array | null {
    return this.gen.toolpathCumLen;
  }
  set toolpathCumLen(v: Float64Array | null) {
    this.gen.toolpathCumLen = v;
  }
  get toolpathTotalLen(): number {
    return this.gen.toolpathTotalLen;
  }
  set toolpathTotalLen(v: number) {
    this.gen.toolpathTotalLen = v;
  }

  get fixtures(): Fixture[] {
    return this.data.fixtures;
  }
  set fixtures(v: Fixture[]) {
    this.data.fixtures = v;
  }
  get selectedFixtureId(): number | null {
    return this.sel.selectedFixtureId;
  }
  set selectedFixtureId(v: number | null) {
    this.sel.selectedFixtureId = v;
  }

  get stock(): StockConfig {
    return this.data.stock;
  }
  set stock(v: StockConfig) {
    this.data.stock = v;
  }

  /// i5g4: program-level WCS offset (j4tv wiring). Defaults to all-zero
  /// at G54, which serializes as "no work_offset field" on the wire so
  /// legacy projects round-trip unchanged.
  get workOffset(): WorkOffset {
    return this.data.workOffset;
  }
  set workOffset(v: WorkOffset) {
    this.data.workOffset = v;
  }

  get tools(): ToolEntry[] {
    return this.data.tools;
  }
  set tools(v: ToolEntry[]) {
    this.data.tools = v;
  }

  get machine(): MachineSettings {
    return this.data.machine;
  }
  set machine(v: MachineSettings) {
    this.data.machine = v;
  }

  get operations(): OpEntry[] {
    return this.data.operations;
  }
  set operations(v: OpEntry[]) {
    this.data.operations = v;
  }
  get selectedOpId(): number | null {
    return this.sel.selectedOpId;
  }
  set selectedOpId(v: number | null) {
    this.sel.selectedOpId = v;
  }

  get reliefSources(): ReliefSource[] {
    return this.data.reliefSources;
  }
  set reliefSources(v: ReliefSource[]) {
    this.data.reliefSources = v;
  }

  /// l8lk: opt-in tool-change-order optimization (group ops by tool).
  /// Changing it reorders the emitted program, so it invalidates the
  /// cached toolpath (the user has to re-Generate) the same way a
  /// machine edit does.
  get groupOpsByTool(): boolean {
    return this.data.groupOpsByTool;
  }
  set groupOpsByTool(v: boolean) {
    if (this.data.groupOpsByTool === v) return;
    this.data.groupOpsByTool = v;
    this.dirty = true;
    this.generated = null;
  }

  get textLayers(): TextLayer[] {
    return this.data.textLayers;
  }
  set textLayers(v: TextLayer[]) {
    this.data.textLayers = v;
  }
  get selectedTextLayerId(): number | null {
    return this.sel.selectedTextLayerId;
  }
  set selectedTextLayerId(v: number | null) {
    this.sel.selectedTextLayerId = v;
  }
  get dirty(): boolean {
    return this.data.dirty;
  }
  set dirty(v: boolean) {
    this.data.dirty = v;
  }

  get regionsVisible(): boolean {
    return this.data.regionsVisible;
  }
  set regionsVisible(v: boolean) {
    this.data.regionsVisible = v;
  }

  get settings(): AppSettings {
    return this.data.settings;
  }
  set settings(v: AppSettings) {
    this.data.settings = v;
  }

  /// Most recent sim diagnostics, written through by the sim driver
  /// after each forward advance(). Null = no sim run yet (or the
  /// preview is in pure wireframe mode and no driver is built).
  get simDiagnostics(): SimDiagnostics | null {
    return this.gen.simDiagnostics;
  }
  set simDiagnostics(v: SimDiagnostics | null) {
    this.gen.simDiagnostics = v;
  }

  /// Undo / redo. Per-session only; not serialized to .vc-project.json.
  /// View-state (selection, playhead, layer visibility, settings) is
  /// excluded — see history.ts for the full list.
  history = new History();

  /// Reactive mirror of `history.version` so `$derived` expressions in
  /// the UI re-run when the stacks change. We can't make `History` a
  /// `.svelte.ts` module today: vitest's test config
  /// (frontend/vitest.config.ts) skips the Svelte plugin to avoid the
  /// vite 5 / vite 8 plugin mismatch, and every History test would
  /// fail with "$state is not defined". jbz1 tracks dropping this
  /// mirror once the test runner can handle the runes (it's a vitest
  /// + plugin-svelte upgrade, not a code-level change).
  historyVersion = $state(0);

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

  get toolsDialogFocusId(): number | null {
    return this.sel.toolsDialogFocusId;
  }
  set toolsDialogFocusId(v: number | null) {
    this.sel.toolsDialogFocusId = v;
  }
  get pickMode(): PickMode | null {
    return this.sel.pickMode;
  }
  set pickMode(v: PickMode | null) {
    this.sel.pickMode = v;
  }

  constructor() {
    this.history.subscribe(() => {
      this.historyVersion = this.history.version;
    });
  }

  /// Reset every project-scoped field to its empty / default state.
  /// Preserves `tools` (per-user library) and `machine` (per-shop
  /// config) — those persist across project boundaries by design.
  /// Drops imports, ops, fixtures, textLayers, stock, generated
  /// state, selections, dirty flag, history.
  ///
  /// Called by the open-file / open-recent flows before loading a
  /// new drawing so leftover ops from the previous project don't
  /// silently re-target unrelated objects in the new geometry.
  clearProject() {
    this.data.imports = [];
    this.operations = [];
    this.fixtures = [];
    this.textLayers = [];
    this.reliefSources = [];
    this.groupOpsByTool = false;
    this.stock = { ...this.stock };
    // j4tv: workOffset is per-project (the user pre-zeros their machine
    // at a different point per drawing), so reset to default like ops.
    this.workOffset = defaultWorkOffset();
    this.generated = null;
    this.toolpathCumLen = null;
    this.toolpathTotalLen = 0;
    this.selectedEntities = new Set();
    this.selectedObjects = new Set();
    this.selectedOpId = null;
    this.selectedFixtureId = null;
    this.selectedTextLayerId = null;
    this.hoverSegment = null;
    this.visibleLayers = new Set();
    this.activeProjectPath = null;
    this.sourceFileStaleNotice = null;
    this.error = null;
    this.dirty = false;
    resetPreviewCache();
    this.history.clear();
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
    const view = this.transformedImport;
    if (view && saved.visible_layers.length > 0) {
      const valid = new Set(view.layers.map((l) => l.name));
      const restored = saved.visible_layers.filter((n) => valid.has(n));
      if (restored.length > 0) this.visibleLayers = new Set(restored);
    }
    if (
      saved.selected_op_id != null &&
      this.operations.some((o) => o.id === saved.selected_op_id)
    ) {
      this.selectedOpId = saved.selected_op_id;
    }
    if (typeof saved.playhead === 'number') {
      this.playhead = Math.max(0, Math.min(1, saved.playhead));
    }
  }

  /// Persist the current per-project view state. Called from $effects in
  /// App.svelte when `visibleLayers` / `selectedOpId` / `playhead` change.
  /// No-op when no path is active (browser uploads, samples, etc.).
  ///
  /// Defers the workspace write off the synchronous Svelte 5 effect flush
  /// via queueMicrotask. The write would otherwise mutate
  /// `workspace.version` ($state) inside the effect body — when the
  /// dispatch chain landed on top of the eb8.6 commit, this caused the
  /// entire reactivity scheduler to abort silently after the first DXF
  /// import (toolbar buttons stopped responding, file picker opened but
  /// imports didn't propagate, etc.). The try/catch guards against the
  /// throw still leaking past the microtask boundary.
  persistPerProjectState() {
    const path = this.activeProjectPath;
    if (!path) return;
    const snapshot = {
      visible_layers: [...this.visibleLayers],
      selected_op_id: this.selectedOpId,
      playhead: this.playhead,
    };
    queueMicrotask(() => {
      try {
        workspace.setPerProject(path, snapshot);
      } catch (e) {
        console.warn('persist per-project state:', e);
      }
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
  /// Public façade for `history.cancelTransaction` that hides the
  /// `CommandTarget` cast — call sites in the UI can stay free of
  /// `as unknown as never` workarounds (audit-jbz1).
  cancelTransaction(): void {
    this.history.cancelTransaction(this.target());
  }
  // The four accessors below touch `this.historyVersion` to subscribe
  // the rune scheduler. The mirror lives on this class (which is
  // already rune-aware) so `History.subscribe` can bump it from plain
  // TS — see the class doc on `historyVersion`.
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
    saveSettings(this.settings);
  }

  updateSettings(patch: Partial<AppSettings>) {
    this.settings = { ...this.settings, ...patch };
    this.saveSettings();
  }

  /// rt1.10: click-toggle a tab placement on an op. `toleranceT` is
  /// the parameter-space distance under which a click on an existing
  /// nearby tab removes it (Estlcam-style toggle). Single undoable
  /// history entry per click.
  toggleTabPlacement(opId: number, placement: { objectId: number; t: number }, toleranceT: number) {
    this.history.exec(toggleTabPlacementCommand(opId, placement, toleranceT), this.target());
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
    if (Object.keys(patch).length === 0) return;
    if (!this.fixtures.some((f) => f.id === id)) return;
    this.history.exec(updateFixtureCommand(id, patch), this.target());
  }

  removeFixture(id: number) {
    if (!this.fixtures.some((f) => f.id === id)) return;
    this.history.exec(removeFixtureCommand(id), this.target());
    if (this.selectedFixtureId === id) this.selectedFixtureId = null;
  }

  selectFixture(id: number | null) {
    this.sel.selectFixture(id);
  }

  /// Append another drawing to the project as its own ImportEntry
  /// (wrsu Phase 2). Each entry keeps its own fileTransform so the user
  /// can position drawings independently on stock. Layer visibility
  /// opens for newly-arrived names so the user sees the new drawing.
  ///
  /// Object id namespacing is handled at view time by `combineImports`
  /// — each entry occupies a contiguous id range starting after the
  /// previous entries. Existing op references stay valid because
  /// imports[0]'s id range is unchanged.
  ///
  /// Undo: not history-tracked in Phase 2A — adding a drawing crosses
  /// a project boundary similar to setImported. Phase 2B is filed to
  /// thread the add through a proper command if users complain.
  addImported(r: ImportResponse, sourcePath?: string | null) {
    if (this.data.imports.length === 0) {
      // First import: identical to setImported for back-compat with
      // the open-file flows that always call addImported.
      this.setImported(r, sourcePath);
      return;
    }
    const nextId = this.data.imports.reduce((m, e) => (e.id > m ? e.id : m), 0) + 1;
    const before = this.data.imports;
    const after: ImportEntry[] = [
      ...before,
      {
        id: nextId,
        source: r,
        fileTransform: identityFileTransform(),
        lastImportPath: sourcePath ?? null,
      },
    ];
    const label = `Add ${sourcePath?.split(/[\\/]/).pop() ?? r.filename ?? 'drawing'}`;
    this.history.exec(setImportsCommand(before, after, label), this.target());
    // Visibility lives outside history (UI-only); reveal the new layers
    // now even though undo won't reverse the toggle.
    const nextVis = new Set(this.visibleLayers);
    for (const l of r.layers) nextVis.add(l.name);
    this.visibleLayers = nextVis;
    this.generated = null;
    this.toolpathCumLen = null;
    this.toolpathTotalLen = 0;
    this.error = null;
    void this.refreshSourceWatch();
  }

  /// Remove an import by its ImportEntry.id (wrsu Phase 2). Layer
  /// visibility entries that no longer have any backing import are
  /// pruned (visibility lives outside history). Undoable via the
  /// `setImportsCommand` shape.
  removeImport(id: number) {
    const before = this.data.imports;
    const after = before.filter((e) => e.id !== id);
    if (after.length === before.length) return;
    const removed = before.find((e) => e.id === id);
    const label = `Remove ${removed?.source.filename ?? 'drawing'}`;
    this.history.exec(setImportsCommand(before, after, label), this.target());
    const stillThere = new Set<string>();
    for (const e of after) for (const l of e.source.layers) stillThere.add(l.name);
    const filtered = new Set<string>();
    for (const l of this.visibleLayers) if (stillThere.has(l)) filtered.add(l);
    this.visibleLayers = filtered;
    this.generated = null;
    this.toolpathCumLen = null;
    this.toolpathTotalLen = 0;
    void this.refreshSourceWatch();
  }

  setImported(r: ImportResponse, sourcePath?: string | null) {
    // Replace imports[0] in place: inherit the previous entry's id when
    // there was one (so undo entries built against that id stay valid),
    // reset the per-import fileTransform to identity (a new source means
    // the old layout was for different geometry), and seed lastImportPath
    // from `sourcePath` when the caller provided one.
    const prev = this.data.imports[0];
    const nextPath = sourcePath !== undefined ? sourcePath : (prev?.lastImportPath ?? null);
    this.data.imports = [
      {
        id: prev?.id ?? 1,
        source: r,
        fileTransform: identityFileTransform(),
        lastImportPath: nextPath,
      },
    ];
    this.generated = null;
    this.toolpathCumLen = null;
    this.toolpathTotalLen = 0;
    this.dirty = true;
    this.error = null;
    this.visibleLayers = new Set(r.layers.map((l) => l.name));
    this.selectedEntities = new Set();
    this.selectedObjects = new Set();
    this.hoverSegment = null;
    this.sourceFileStaleNotice = null;
    // gldc: auto-default work_offset to the geometry bbox's bottom-left
    // when the drawing was authored off-origin in CAD and the user
    // hasn't explicitly set an offset. Suppresses the
    // `stock_origin_outside_geometry_bbox` pipeline warning at its
    // most common firing site (drawings centered around a non-zero
    // point in the source CAD), matching the canonical CNC workflow
    // (operator zeros at the bottom-left corner of the drawing).
    // No-op when the user has already moved away from default.
    this.workOffset = inferDefaultWorkOffset(r.bbox, this.workOffset);
    // Replacing the imported geometry implies a new project boundary —
    // drop any text-preview segments cached from the previous project
    // so we don't paint stale TextLayer glyphs over the new file.
    resetPreviewCache();
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
    // wrsu Phase 2: watch every import's source path, not just imports[0].
    for (const entry of this.data.imports) {
      if (entry.lastImportPath && isAbsolutePath(entry.lastImportPath)) {
        paths.add(entry.lastImportPath);
      }
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
  /// Source-file watcher callback (eb8.4 + wrsu Phase 2). The watcher
  /// fires per-path; we look up the matching ImportEntry and replace
  /// its source in place, preserving its fileTransform + id. If no
  /// entry matches the path (stale watch), bail rather than overwrite
  /// an unrelated import.
  async reimportFromPath(path: string): Promise<boolean> {
    if (typeof window === 'undefined') return false;
    if (!isTauriEnv()) return false;
    const idx = this.data.imports.findIndex((e) => e.lastImportPath === path);
    if (idx < 0) {
      this.setError(`reload: no import is watching ${path}`);
      return false;
    }
    let after: ImportResponse;
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      after = await invoke<ImportResponse>('import_path', { path });
    } catch (e) {
      this.setError(`reload: ${e instanceof Error ? e.message : String(e)}`);
      return false;
    }
    const next = [...this.data.imports];
    next[idx] = { ...next[idx], source: after };
    this.data.imports = next;
    this.dirty = true;
    this.sourceFileStaleNotice = null;
    // Orphan-source detection runs against the merged view (post-reload)
    // so ops keyed by ids from OTHER imports still see their objects.
    // eb8.7's inline Re-pick chip on OperationsList rows surfaces the
    // affected ops; this warn keeps the dev console signal too.
    // Use the augmented view so an op targeting the synthetic stock
    // outline (STOCK_OUTLINE_ID) isn't mistaken for an orphan (8jce).
    const presentIds = new Set(this.geometryView?.objects ?? []);
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
    // Route through the same command path as `selectObjects` so the
    // canvas-click toggle ends up in the undo/redo stack (80gv).
    this.selectObjects([id], additive ? 'toggle' : 'replace');
  }

  /// Bulk selection update — used by box-select and any other path
  /// that needs to commit a set of object ids with FreeCAD-style
  /// modifier semantics in one go. Pushes the change through the
  /// History so Ctrl+Z reverts the selection (80gv).
  selectObjects(ids: Iterable<number>, mode: SelectionMode) {
    const prevSelected = new Set(this.sel.selectedObjects);
    const prevAnchor = this.sel.selectionAnchorObjectId;
    const { selected: nextSelected, anchor: nextAnchor } = computeSelectionUpdate(
      prevSelected,
      prevAnchor,
      ids,
      mode,
    );
    this.pushSelectionChange(prevSelected, prevAnchor, nextSelected, nextAnchor);
  }

  /// Internal: emit a single selection-change command. Used by
  /// `selectObjects`, `clearSelection`, `seriesSelectTo`, and any
  /// future selection helper that needs to land in the undo stack.
  /// Skips the push when prev == next (no-op selection updates
  /// shouldn't waste an undo slot).
  private pushSelectionChange(
    prevSelected: Set<number>,
    prevAnchor: number | null,
    nextSelected: Set<number>,
    nextAnchor: number | null,
  ) {
    if (selectionsEqual(prevSelected, nextSelected) && prevAnchor === nextAnchor) return;
    this.history.exec(
      selectObjectsCommand(
        this.sel,
        { selected: prevSelected, anchor: prevAnchor },
        { selected: nextSelected, anchor: nextAnchor },
      ),
      this.target(),
    );
  }
  /// Series-select: extend the selection from the current anchor object
  /// to `targetId`, picking every visible object whose bbox is crossed
  /// by the straight line between the two bbox centroids. Falls back to
  /// a plain replace when no anchor exists. Honors visibleLayers so
  /// hidden chains can't be accidentally swept in. (audit-eqxd)
  seriesSelectTo(targetId: number) {
    if (targetId <= 0) return;
    const anchorId = this.selectionAnchorObjectId;
    const meta = this.transformedImport?.object_meta ?? [];
    if (anchorId == null || anchorId === targetId || meta.length === 0) {
      this.selectObjects([targetId], 'replace');
      return;
    }
    const visible = this.visibleLayers;
    const byId = new Map<number, (typeof meta)[number]>();
    for (const m of meta) byId.set(m.id, m);
    const a = byId.get(anchorId);
    const t = byId.get(targetId);
    if (!a || !t) {
      this.selectObjects([targetId], 'replace');
      return;
    }
    const p0 = { x: (a.bbox.min_x + a.bbox.max_x) * 0.5, y: (a.bbox.min_y + a.bbox.max_y) * 0.5 };
    const p1 = { x: (t.bbox.min_x + t.bbox.max_x) * 0.5, y: (t.bbox.min_y + t.bbox.max_y) * 0.5 };
    const picked: number[] = [anchorId, targetId];
    for (const m of meta) {
      if (m.id === anchorId || m.id === targetId) continue;
      if (!visible.has(m.layer)) continue;
      if (lineCrossesBBox(p0, p1, m.bbox)) picked.push(m.id);
    }
    // Compute the post-add selection + override the anchor to `targetId`
    // so consecutive Shift+clicks chain (anchor → click → click → click).
    // Single command so Ctrl+Z restores both selection and anchor in
    // one undo step (80gv).
    const prevSelected = new Set(this.sel.selectedObjects);
    const prevAnchor = this.sel.selectionAnchorObjectId;
    const { selected: nextSelected } = computeSelectionUpdate(
      prevSelected,
      prevAnchor,
      picked,
      'add',
    );
    this.pushSelectionChange(prevSelected, prevAnchor, nextSelected, targetId);
  }
  clearSelection() {
    const prevSelected = new Set(this.sel.selectedObjects);
    const prevAnchor = this.sel.selectionAnchorObjectId;
    this.pushSelectionChange(prevSelected, prevAnchor, new Set(), null);
  }

  setGenerated(r: GenerateResponse) {
    this.generated = r;
    this.generatedVersion += 1;
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

  /// Pipeline-state lifecycle helpers (audit-pgxb). Most delegate to
  /// the generated-state slice; `failGenerate` lives here because it
  /// crosses slices (error + pipelineState reset).
  beginGenerate() {
    this.error = null;
    this.gen.beginGenerate();
  }

  notePipelineEvent(ev: PipelineNoteEvent) {
    this.gen.notePipelineEvent(ev);
  }

  finishGenerate() {
    this.gen.finishGenerate();
  }

  cancelGenerate() {
    this.gen.cancelGenerate();
  }

  /// Pipeline failure path. Routes the error through setError and
  /// snaps the generate slice back to idle. Spans two slices, so
  /// stays on the parent rather than living on either.
  failGenerate(err: string | WiacError) {
    this.setError(err);
    this.gen.pipelineState = 'idle';
  }

  endGenerate() {
    this.gen.endGenerate();
  }

  toggleLayer(name: string) {
    const next = new Set(this.visibleLayers);
    if (next.has(name)) next.delete(name);
    else next.add(name);
    this.visibleLayers = next;
  }

  /// Delete every imported segment that belongs to `layerName`. Drops
  /// the layer entry, the visibleLayers entry, and (parallel-index)
  /// the `objects[]` per-segment mapping. `object_meta` is left intact
  /// — entries for deleted objects become orphaned but no remaining
  /// segment references them, so they're harmless until the next
  /// re-import. Bbox is recomputed from the surviving segments.
  /// Undoable via the imports-snapshot command pattern.
  ///
  /// Multi-file: removes the layer from EVERY import that carries it,
  /// matching what the user sees in the unioned LayerList count.
  removeImportedLayer(layerName: string) {
    const before = this.data.imports;
    if (before.length === 0) return;
    let touched = false;
    const after = before.map((entry) => {
      const src = entry.source;
      const keep = src.segments.map((s) => s.layer !== layerName);
      if (keep.every((k) => k)) return entry;
      touched = true;
      const newSegments = src.segments.filter((_, i) => keep[i]);
      const newObjects = (src.objects ?? []).filter((_, i) => keep[i]);
      const newLayers = src.layers.filter((l) => l.name !== layerName);
      const newBbox = bboxOfSegments(newSegments);
      return {
        ...entry,
        source: {
          ...src,
          segments: newSegments,
          layers: newLayers,
          bbox: newBbox,
          objects: newObjects,
        },
      };
    });
    if (!touched) return;
    this.history.beginTransaction(`Delete layer "${layerName}"`);
    this.history.exec(
      setImportsCommand(before, after, `Delete layer "${layerName}"`),
      this.target(),
    );
    // Drop visibility tracking for the gone layer too — visibleLayers
    // lives outside the command target, so this is a plain mutation.
    if (this.visibleLayers.has(layerName)) {
      const next = new Set(this.visibleLayers);
      next.delete(layerName);
      this.visibleLayers = next;
    }
    this.history.commitTransaction();
  }

  /// Snapshot for project save.
  ///
  /// View-state fields (`visibleLayers`, `selectedEntities`) are
  /// intentionally OMITTED — they're per-installation UI preferences
  /// owned by `workspace.per_project[path].visible_layers`. Including
  /// them in the .wiac-project save caused a two-source-of-truth
  /// conflict where workspace silently won on reopen, surprising
  /// users who expected their saved file to dictate visibility (audit
  /// vep). Old projects that still carry them load fine via the
  /// `?? []` fallback in restore().
  snapshot(): ProjectFile {
    return {
      kind: 'wiac-project',
      version: 1,
      imports: this.data.imports,
      visibleLayers: [],
      selectedEntities: [],
      stock: this.stock,
      tools: this.tools,
      machine: this.machine,
      operations: this.operations,
      fixtures: this.fixtures,
      textLayers: this.textLayers,
      ...(this.reliefSources.length > 0 ? { reliefSources: this.reliefSources } : {}),
      // i5g4 / j4tv: only persist work_offset when non-default so legacy
      // / unset projects keep their compact .wiac-project payloads. The
      // restore() side defaults to defaultWorkOffset() when absent.
      ...(isDefaultWorkOffset(this.workOffset) ? {} : { workOffset: this.workOffset }),
      // l8lk: persist the tool-grouping toggle only when on.
      ...(this.groupOpsByTool ? { groupOpsByTool: true } : {}),
    };
  }

  restore(file: ProjectFile) {
    if (file.kind !== 'wiac-project') {
      throw new Error('not a wiaConstructor project file');
    }
    // wrsu Phase 1: imports[] is the canonical shape. Pre-wrsu project
    // files (with bare `imported` / `fileTransform` / `lastImportPath`
    // fields) are no longer loadable — the user explicitly waived
    // backward compatibility for this migration.
    this.data.imports = Array.isArray(file.imports) ? file.imports : [];
    if (this.data.imports[0]) {
      this.setImported(this.data.imports[0].source, this.data.imports[0].lastImportPath ?? null);
    }
    // Layer visibility precedence (best wins):
    //   1. workspace.per_project[path].visible_layers (applied in
    //      setActiveProjectPath after restore returns).
    //   2. file.visibleLayers, when the saved project carries any —
    //      e.g. a shared .wiac-project file from another machine
    //      whose workspace we don't have.
    //   3. setImported defaults (all layers visible).
    // Empty `file.visibleLayers` is treated as "no opinion" and falls
    // through to setImported defaults — new saves OMIT these fields
    // (audit vep) so workspace can be the single source of truth.
    if (Array.isArray(file.visibleLayers) && file.visibleLayers.length > 0) {
      this.visibleLayers = new Set(file.visibleLayers);
    }
    if (Array.isArray(file.selectedEntities) && file.selectedEntities.length > 0) {
      this.selectedEntities = new Set(file.selectedEntities);
    }
    if (file.stock) this.stock = { ...this.stock, ...file.stock };
    if (Array.isArray(file.tools) && file.tools.length > 0)
      this.tools = file.tools.map(migrateLegacyToolTerms);
    if (file.machine) this.machine = { ...this.machine, ...migrateMachineSettings(file.machine) };
    if (Array.isArray(file.operations)) this.operations = file.operations;
    this.fixtures = Array.isArray(file.fixtures) ? file.fixtures : [];
    this.textLayers = Array.isArray(file.textLayers) ? file.textLayers : [];
    this.reliefSources = Array.isArray(file.reliefSources) ? file.reliefSources : [];
    // j4tv: restore the program-level WCS offset. Legacy files lack
    // this field — fall back to all-zero @ G54, which matches the
    // pre-i5g4 behavior (geometry origin = WCS origin).
    this.workOffset = file.workOffset
      ? { ...defaultWorkOffset(), ...file.workOffset }
      : defaultWorkOffset();
    // l8lk: restore the tool-grouping toggle (legacy files lack it → false).
    this.groupOpsByTool = file.groupOpsByTool === true;
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
    const before = this.data.imports;
    // imports[0] is the canonical Add-Text target. Synthesize an empty
    // entry when none exists so the user can author text in a fresh
    // project before importing geometry; this synthesis is captured in
    // the command's `before` snapshot so a single undo wipes the whole
    // Add-Text run including the synthetic seed.
    const seedEntry: ImportEntry = before[0] ?? {
      id: 1,
      source: {
        filename: 'text',
        format: 'text',
        bbox: { min_x: 0, min_y: 0, max_x: 0, max_y: 0 },
        layers: [],
        segments: [],
        unit_scale: 1,
        warnings: [],
        objects: [],
        object_meta: [],
      },
      fileTransform: identityFileTransform(),
      lastImportPath: null,
    };
    const cur = seedEntry.source;
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

    const afterSource: ImportResponse = {
      ...cur,
      segments: [...cur.segments, ...segments],
      objects: [...(cur.objects ?? []), ...newObjects],
      object_meta: [...(cur.object_meta ?? []), ...newMeta],
      layers,
      bbox,
    };
    const after: ImportEntry[] = [{ ...seedEntry, source: afterSource }, ...before.slice(1)];
    this.history.exec(setImportsCommand(before, after, 'Add geometry'), this.target());
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
    // rt1.34: Pause has no tool, no source, no geometry — only a
    // message. Skip the source-selection presets and the geometry-side
    // defaults that the variant types don't carry.
    if (kind === 'pause') {
      const pauseOp: OpEntry = {
        id: nextId,
        name: prettyOpKind(kind),
        enabled: true,
        kind: 'pause',
        toolId: 0,
        sourceCombine: 'auto',
        sourceLayers: null,
        message: '',
      } as OpEntry;
      this.history.exec(addOperationCommand(pauseOp), this.target());
      this.selectedOpId = pauseOp.id;
      return pauseOp;
    }
    // 8n4k: program-only building blocks. Same skeleton as Pause —
    // no tool, no geometry, no Z schedule — but each carries its
    // own kind-specific fields with sensible defaults.
    if (kind === 'homing') {
      const op: OpEntry = {
        id: nextId,
        name: prettyOpKind(kind),
        enabled: true,
        kind: 'homing',
        toolId: 0,
        sourceCombine: 'auto',
        sourceLayers: null,
        retractToSafeZ: true,
      } as OpEntry;
      this.history.exec(addOperationCommand(op), this.target());
      this.selectedOpId = op.id;
      return op;
    }
    if (kind === 'probe') {
      const op: OpEntry = {
        id: nextId,
        name: prettyOpKind(kind),
        enabled: true,
        kind: 'probe',
        toolId: 0,
        sourceCombine: 'auto',
        sourceLayers: null,
        axis: 'z',
        distanceMm: -10,
        feedMmMin: 100,
      } as OpEntry;
      this.history.exec(addOperationCommand(op), this.target());
      this.selectedOpId = op.id;
      return op;
    }
    if (kind === 'cycle_marker') {
      const op: OpEntry = {
        id: nextId,
        name: prettyOpKind(kind),
        enabled: true,
        kind: 'cycle_marker',
        toolId: 0,
        sourceCombine: 'auto',
        sourceLayers: null,
        label: '',
      } as OpEntry;
      this.history.exec(addOperationCommand(op), this.target());
      this.selectedOpId = op.id;
      return op;
    }
    // rxm9: external G-code include. Same program-only skeleton.
    // path + content default to empty — the user picks a file via
    // OpPropertiesPanel which reads the bytes and sets both fields.
    if (kind === 'gcode_include') {
      const op: OpEntry = {
        id: nextId,
        name: prettyOpKind(kind),
        enabled: true,
        kind: 'gcode_include',
        toolId: 0,
        sourceCombine: 'auto',
        sourceLayers: null,
        path: '',
        content: '',
        // xi2g: default off — the counted summary fires anyway when
        // unsim lines exist; the user opts into the per-line fan-out
        // via the OpPropertiesPanel checkbox.
        verboseUnsimWarnings: false,
      } as OpEntry;
      this.history.exec(addOperationCommand(op), this.target());
      this.selectedOpId = op.id;
      return op;
    }
    // f60x: relief surfacing follows a target Z-surface, not source
    // geometry — skip the offset/contour defaults. Prefer a ball-nose
    // tool; bind to the first loaded relief source (0 = none yet).
    if (kind === 'relief_mill') {
      const ball =
        this.tools.find((t) => t.kind === 'ball_nose' || t.kind === 'bull_nose') ?? this.tools[0];
      const reliefOp: OpEntry = {
        id: nextId,
        name: prettyOpKind(kind),
        enabled: true,
        kind: 'relief_mill',
        toolId: ball?.id ?? this.tools[0]?.id ?? 1,
        sourceCombine: 'auto',
        sourceLayers: null,
        depth: -2,
        startDepth: 0,
        step: -1,
        sourceId: this.reliefSources[0]?.id ?? 0,
        zMinMm: -2,
        zMaxMm: 0,
        invert: false,
        scallopHeightMm: 0.05,
        stepoverMm: null,
        scanDirection: 'along_x',
        alongStepMm: 0.5,
      } as OpEntry;
      this.history.exec(addOperationCommand(reliefOp), this.target());
      this.selectedOpId = reliefOp.id;
      return reliefOp;
    }
    // When the user has objects selected on the canvas, pin the new op
    // to that exact set. Most users select first, click "+ Pocket"
    // expecting the op to apply to what they highlighted — the
    // alternative (default to All) silently runs across every imported
    // chain. Empty selection ⇒ keep the All default (sourceObjects
    // undefined + sourceLayers: null).
    const selectionIds = [...this.selectedObjects];
    const presetSources = selectionIds.length > 0 ? { sourceObjects: selectionIds } : {};
    // dx8p: when adding a drill against a square-ish selection, pick
    // the library tool whose diameter best matches the inferred hole.
    // Falls through to the first-tool default for other kinds or when
    // the geometry signal is ambiguous.
    const tool = pickBestToolForOp(
      kind,
      selectionIds,
      this.transformedImport?.object_meta ?? [],
      this.tools,
    );
    // The literal builds a merged shape with conditionally-included
    // variant-specific fields (`offset` for profile/engrave/drag_knife,
    // `pocketStrategy` for pocket, `drillCycle` for drill, …) — TS
    // can't infer the discriminated union from the `kind` binding, so
    // assert the constructed shape at the boundary.
    const op = {
      id: nextId,
      name: prettyOpKind(kind),
      enabled: true,
      kind,
      toolId: tool?.id ?? 1,
      sourceCombine: 'auto',
      sourceLayers: null,
      ...presetSources,
      depth: -2,
      startDepth: 0,
      step: -1,
      offset:
        kind === 'engrave' || kind === 'drag_knife' || kind === 't_slot' || kind === 'dovetail'
          ? 'on'
          : 'outside',
      pocketStrategy: kind === 'pocket' ? 'cascade' : null,
      ...(kind === 'drill' ? { drillCycle: { kind: 'simple', dwell_sec: 0 } as DrillCycle } : {}),
      cutDirection: 'conventional',
      finishCutDirection: 'conventional',
      plunge: { kind: 'direct' },
      xyOverlap: 0.5,
      ...(kind === 'vcarve' ? { multiPassRefine: false } : {}),
    } as OpEntry;
    this.history.exec(addOperationCommand(op), this.target());
    this.selectedOpId = op.id;
    return op;
  }

  removeOperation(id: number) {
    if (!this.operations.some((o) => o.id === id)) return;
    this.history.exec(deleteOperationCommand(id), this.target());
    if (this.selectedOpId === id) this.selectedOpId = null;
  }

  /// Insert a text layer with the given configuration; `id` and the
  /// default `name` are filled in if absent. Returns the inserted
  /// layer (with the assigned id). Undoable.
  addTextLayer(
    seed: Omit<TextLayer, 'id' | 'name'> & Partial<Pick<TextLayer, 'id' | 'name'>>,
  ): TextLayer {
    const nextId = seed.id ?? this.textLayers.reduce((m, t) => Math.max(m, t.id), 0) + 1;
    const previewText = seed.text.split(/\r?\n/, 1)[0] ?? '';
    const truncated = previewText.length > 20 ? `${previewText.slice(0, 20)}…` : previewText;
    const defaultName = `${seed.kind} — "${truncated}"`;
    const layer: TextLayer = { ...seed, id: nextId, name: seed.name ?? defaultName };
    this.history.exec(addTextLayerCommand(layer), this.target());
    return layer;
  }

  /// f60x: insert a relief surface source (e.g. a decoded grayscale
  /// image). `id` is assigned if absent. Returns the inserted source.
  /// Undoable.
  addReliefSource(
    seed: Omit<ReliefSource, 'id'> & Partial<Pick<ReliefSource, 'id'>>,
  ): ReliefSource {
    const nextId = seed.id ?? this.reliefSources.reduce((m, s) => Math.max(m, s.id), 0) + 1;
    const source: ReliefSource = { ...seed, id: nextId };
    this.history.exec(addReliefSourceCommand(source), this.target());
    return source;
  }

  updateReliefSource(id: number, patch: Partial<ReliefSource>) {
    if (Object.keys(patch).length === 0) return;
    if (!this.reliefSources.some((s) => s.id === id)) return;
    this.history.exec(updateReliefSourceCommand(id, patch), this.target());
  }

  removeReliefSource(id: number) {
    if (!this.reliefSources.some((s) => s.id === id)) return;
    this.history.exec(deleteReliefSourceCommand(id), this.target());
  }

  updateTextLayer(id: number, patch: Partial<TextLayer>) {
    if (Object.keys(patch).length === 0) return;
    if (!this.textLayers.some((t) => t.id === id)) return;
    this.history.exec(updateTextLayerCommand(id, patch), this.target());
  }

  /// Convert any `imported.text_entities` from the most recent setImported
  /// call into editable `TextLayer` entries. Each entity gets the bundled
  /// DejaVu Sans by default so the user sees the text immediately; they
  /// can swap fonts later from the sidebar. No-op when nothing was
  /// imported or no TEXT/MTEXT entities were present.
  async convertImportedTextEntities(): Promise<void> {
    const entry = this.data.imports[0];
    if (!entry) return;
    const entities = entry.source.text_entities;
    if (!entities || entities.length === 0) return;
    const bytes_b64 = await loadDefaultFontBytesB64();
    if (!bytes_b64) return;
    this.history.beginTransaction('Import text entities');
    try {
      for (const e of entities) {
        const isMtext = e.kind === 'MTEXT';
        this.addTextLayer({
          kind: isMtext ? 'MTEXT' : 'TEXT',
          text: e.text,
          fontSource: { kind: 'bundled', path: '/fonts/DejaVuSans.ttf', bytes_b64 },
          sizeMm: e.size_mm,
          origin: { x: e.origin[0], y: e.origin[1] },
          rotationDeg: e.rotation_deg ?? 0,
          letterSpacingMm: 0,
          lineSpacingMm: 0,
          alignment: 'left',
          widthScale: 1.0,
          singleLine: false,
        });
      }
      this.history.commitTransaction();
    } catch (err) {
      this.history.cancelTransaction(this.target());
      throw err;
    }
    // Consume the queue so subsequent addImported() calls don't try
    // to convert the same entities again into duplicate TextLayers.
    // Plain mutation (not a command): this is bookkeeping after the
    // text-layer-add commands above, not user-undoable state.
    const cur = this.data.imports[0];
    if (cur) {
      this.data.imports = [
        { ...cur, source: { ...cur.source, text_entities: [] } },
        ...this.data.imports.slice(1),
      ];
    }
  }

  removeTextLayer(id: number) {
    if (!this.textLayers.some((t) => t.id === id)) return;
    const syntheticLayer = `__text_${id}`;
    // Drop the cached preview segments so the canvas doesn't keep
    // painting glyphs from a layer that no longer exists.
    invalidatePreview(id);
    // Cascade-delete any ops whose source targets the text layer's
    // synthetic geometry layer — leaving them around would make the
    // pipeline raise "no segments on layer __text_<id>".
    const dependentOps = this.operations.filter(
      (o) => Array.isArray(o.sourceLayers) && o.sourceLayers.includes(syntheticLayer),
    );
    if (dependentOps.length > 0) {
      this.history.beginTransaction('Delete text');
      for (const op of dependentOps) {
        this.history.exec(deleteOperationCommand(op.id), this.target());
      }
      this.history.exec(deleteTextLayerCommand(id), this.target());
      this.history.commitTransaction();
    } else {
      this.history.exec(deleteTextLayerCommand(id), this.target());
    }
    if (this.selectedTextLayerId === id) this.selectedTextLayerId = null;
  }

  /// Deep-clone the op and insert it immediately after the original.
  /// Returns the new op or null if `id` is unknown.
  duplicateOperation(id: number): OpEntry | null {
    const src = this.operations.find((o) => o.id === id);
    if (!src) return null;
    const nextId = this.operations.reduce((m, o) => Math.max(m, o.id), 0) + 1;
    // JSON-roundtrip clone: Svelte 5 `$state` proxies make
    // structuredClone throw DataCloneError in production builds — the
    // dup button would die with an uncaught exception and look dead.
    const copy: OpEntry = {
      ...(JSON.parse(JSON.stringify(src)) as OpEntry),
      id: nextId,
      name: `${src.name} (copy)`,
    };
    this.history.exec(duplicateOperationCommand(id, copy, id), this.target());
    this.selectedOpId = copy.id;
    return copy;
  }

  updateOperation(id: number, patch: Partial<OpEntry>) {
    if (Object.keys(patch).length === 0) return;
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

  // (op grouping removed — ops are a flat list)

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
    // Machine change invalidates the cached gcode: work area / units /
    // post-processor dialect / rapid feeds all feed into the run, so a
    // toolpath generated against the prior machine isn't safe to draw
    // against the new envelope or download into the new dialect's file.
    // The user has to regen; clearing here lets the GcodePanel + Scene3D
    // empty-state messaging show the stale-vs-fresh distinction
    // immediately instead of silently lying.
    this.generated = null;
  }

  setStock(patch: Partial<StockConfig>) {
    if (Object.keys(patch).length === 0) return;
    this.history.exec(setStockCommand(patch), this.target());
  }

  /// Undoable WorkOffset edit. Routes through the command bus so the
  /// X/Y/Z spinners + WCS picker in StockPanel + the warnings-panel
  /// Apply-Fix button all coalesce into history entries identical to
  /// the stock-dim flow (audit abdk).
  setWorkOffset(patch: Partial<WorkOffset>) {
    if (Object.keys(patch).length === 0) return;
    this.history.exec(setWorkOffsetCommand(patch), this.target());
  }

  /// Per-import variant of patchFileTransform (wrsu Phase 2). Undoable;
  /// the optional coalesceKey is per-import-per-field so two consecutive
  /// nudges of the X spinner on entry #3 collapse to one history step.
  ///
  /// 43l2: after swapping the transform, project every op's
  /// approachPoint and (mirror-sensitive) tabPlacements through the
  /// delta so they stay attached to the same geometry the user sees.
  /// Approach points round-trip via raw-import space; tab `t` values
  /// flip 1-t when the mirror parity changed since contour traversal
  /// reverses. Bundled into the same transaction as the imports swap
  /// so Ctrl+Z reverts the whole intent in one step.
  patchFileTransformForImport(
    importId: number,
    patch: Partial<Omit<FileTransform, 'translate'>> & {
      translate?: Partial<FileTransform['translate']>;
    },
    coalesceKey?: string,
  ) {
    const idx = this.data.imports.findIndex((e) => e.id === importId);
    if (idx < 0) return;
    const entry = this.data.imports[idx];
    const beforeXf = entry.fileTransform;
    const afterXf: FileTransform = {
      ...beforeXf,
      ...patch,
      translate: { ...beforeXf.translate, ...(patch.translate ?? {}) },
    };
    const before = this.data.imports;
    const after = [...before];
    after[idx] = { ...entry, fileTransform: afterXf };
    const label = 'Edit file transform';
    const opPatches = this.computeOpPatchesForXfDelta(before, idx, beforeXf, afterXf);
    if (opPatches.length === 0) {
      // Hot path: spinner drags with no affected ops stay as a single
      // command so the coalesce key still collapses streaks of nudges
      // into one undo entry.
      this.history.exec(
        setImportsCommand(
          before,
          after,
          label,
          coalesceKey ? `xform:${importId}:${coalesceKey}` : undefined,
        ),
        this.target(),
      );
      return;
    }
    this.history.beginTransaction(label);
    try {
      this.history.exec(setImportsCommand(before, after, label), this.target());
      for (const { opId, patch: opPatch } of opPatches) {
        this.history.exec(updateOperationCommand(opId, opPatch), this.target());
      }
      this.history.commitTransaction();
    } catch (e) {
      this.history.cancelTransaction(this.target());
      throw e;
    }
  }

  /// 43l2 helper: compute the per-op `approachPoint` + `tabPlacements`
  /// patches needed to keep the user's authored markers stuck to the
  /// geometry when the import at `idx`'s fileTransform changes. Returns
  /// only ops that actually need an update; ops whose source touches
  /// OTHER imports aren't moved. Pure — doesn't mutate.
  private computeOpPatchesForXfDelta(
    imports: readonly ImportEntry[],
    idx: number,
    beforeXf: FileTransform,
    afterXf: FileTransform,
  ): { opId: number; patch: OpPatch }[] {
    if (isIdentityFileTransform(beforeXf) && isIdentityFileTransform(afterXf)) {
      return [];
    }
    // Compute this entry's namespaced object-id range (matches
    // combineImports' offset arithmetic — entries[0] keeps 1..N0,
    // entries[1] gets N0+1..N0+N1, etc.).
    let idOffset = 0;
    for (let i = 0; i < idx; i++) {
      const m = (imports[i].source.objects ?? []).reduce((max, id) => (id > max ? id : max), 0);
      idOffset += m;
    }
    const localMax = (imports[idx].source.objects ?? []).reduce((m, id) => (id > m ? id : m), 0);
    const lo = idOffset + 1;
    const hi = idOffset + localMax;
    const ownsId = (id: number) => id >= lo && id <= hi;
    // The pivot for both forward + inverse is the RAW import bbox
    // centre, which doesn't change with the transform itself.
    const rawBbox = imports[idx].source.bbox;
    const mirrorParityChanged =
      Number(beforeXf.mirrorX) !== Number(afterXf.mirrorX) ||
      Number(beforeXf.mirrorY) !== Number(afterXf.mirrorY);
    const out: { opId: number; patch: OpPatch }[] = [];
    for (const op of this.operations) {
      // approachPoint + tabPlacements live on contour ops only.
      // Non-contour ops (Drill / VCarve / …) have no markers to keep
      // attached, so skip them — also avoids narrowing pain on the
      // OpEntry discriminated union.
      if (!isContourOp(op)) continue;
      // Empty sourceObjects = "all geometry" — ambiguous which import
      // it belongs to. Skip; the user re-positions if needed (no worse
      // than today for that case).
      if (!Array.isArray(op.sourceObjects) || op.sourceObjects.length === 0) continue;
      const ownedIds = op.sourceObjects.filter(ownsId);
      if (ownedIds.length === 0) continue;
      // The op is narrowed to ProfileOp | PocketOp by isContourOp
      // above, so its patch can carry the contour-only fields directly.
      const patch: Partial<typeof op> = {};
      // Approach point: world(before) → raw → world(after).
      if (op.approachPoint) {
        const [ax, ay] = op.approachPoint;
        const raw = invertFileTransformPoint({ x: ax, y: ay }, beforeXf, rawBbox);
        const next = applyFileTransformToPoint(raw, afterXf, rawBbox);
        // Skip the write when the result is identical (no-op transforms
        // that still survived the identity guard above).
        if (Math.abs(next.x - ax) > 1e-9 || Math.abs(next.y - ay) > 1e-9) {
          patch.approachPoint = [next.x, next.y];
        }
      }
      // Tab placements: mirror parity flip reverses contour traversal,
      // so t → 1-t per placement on this import's objects.
      const tabs = op.tabPlacements;
      if (mirrorParityChanged && Array.isArray(tabs) && tabs.length > 0) {
        const flipped = tabs.map((tp) => (ownsId(tp.objectId) ? { ...tp, t: 1 - tp.t } : tp));
        // Only emit when at least one placement actually flipped.
        if (flipped.some((tp, i) => tp.t !== tabs[i].t)) {
          patch.tabPlacements = flipped;
        }
      }
      if (Object.keys(patch).length > 0) out.push({ opId: op.id, patch });
    }
    return out;
  }

  resetFileTransformForImport(importId: number) {
    const idx = this.data.imports.findIndex((e) => e.id === importId);
    if (idx < 0) return;
    if (isIdentityFileTransform(this.data.imports[idx].fileTransform)) return;
    const before = this.data.imports;
    const after = [...before];
    after[idx] = { ...after[idx], fileTransform: identityFileTransform() };
    this.history.exec(setImportsCommand(before, after, 'Reset file transform'), this.target());
  }
}

export const project = new ProjectState();

// These helpers used to live in this module; they were moved to
// `sim/warnings.ts` and `sim/playhead.ts` so vitest can import them
// without booting the Svelte rune runtime. Re-exported here for
// backwards-compat with existing call sites.
export { simWarningSeverity, simWarningSegmentIdx, simWarningSummary } from '../sim/warnings';
export { playheadToSegment } from '../sim/playhead';
