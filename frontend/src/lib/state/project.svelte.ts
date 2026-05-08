// Global project state, Svelte 5 runes.
// Holds the most recently imported geometry plus UI flags.

import type { GenerateResponse, ImportResponse, Point2 } from '../api/types';

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
  error = $state<string | null>(null);
  visibleLayers = $state<Set<string>>(new Set());

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
    { id: 1, name: '3 mm endmill', kind: 'endmill', diameter: 3, flutes: 2,
      speed: 18000, plungeRate: 100, feedRate: 800, coolant: 'off' },
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
    const next = { ...this.tabs };
    next[segmentIdx] = [...(next[segmentIdx] ?? []), { x: position.x, y: position.y }];
    this.tabs = next;
    this.dirty = true;
  }

  removeTab(segmentIdx: number, tabIdx: number) {
    const list = this.tabs[segmentIdx];
    if (!list) return;
    const next = { ...this.tabs };
    next[segmentIdx] = list.filter((_, i) => i !== tabIdx);
    if (next[segmentIdx].length === 0) delete next[segmentIdx];
    this.tabs = next;
    this.dirty = true;
  }

  clearTabs() {
    this.tabs = {};
    this.dirty = true;
  }

  setImported(r: ImportResponse) {
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

  setError(msg: string) {
    this.error = msg;
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
    };
  }

  restore(file: ProjectFile) {
    if (file.kind !== 'wiac-project') {
      throw new Error('not a wiaConstructor project file');
    }
    if (file.imported) this.setImported(file.imported);
    this.visibleLayers = new Set(file.visibleLayers ?? []);
    this.selectedEntities = new Set(file.selectedEntities ?? []);
    this.tabs = file.tabs ?? {};
    if (file.stock) this.stock = { ...this.stock, ...file.stock };
    if (Array.isArray(file.tools) && file.tools.length > 0) this.tools = file.tools;
    if (file.machine) this.machine = { ...this.machine, ...file.machine };
    if (Array.isArray(file.operations)) this.operations = file.operations;
    this.selectedOpId = null;
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
      ...(kind === 'drill'
        ? { drillCycle: { kind: 'simple', dwell_sec: 0 } as DrillCycle }
        : {}),
      cutDirection: 'conventional',
      finishCutDirection: 'conventional',
      plunge: { kind: 'direct' },
      xyOverlap: 0.5,
    };
    this.operations = [...this.operations, op];
    this.selectedOpId = op.id;
    this.dirty = true;
    return op;
  }

  removeOperation(id: number) {
    this.operations = this.operations.filter((o) => o.id !== id);
    if (this.selectedOpId === id) this.selectedOpId = null;
    this.dirty = true;
  }

  updateOperation(id: number, patch: Partial<OpEntry>) {
    this.operations = this.operations.map((o) => (o.id === id ? { ...o, ...patch } : o));
    this.dirty = true;
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
    const next = [...this.operations];
    const [op] = next.splice(cur, 1);
    next.splice(clamped, 0, op);
    this.operations = next;
    this.dirty = true;
  }
}

function prettyOpKind(kind: OpKind): string {
  switch (kind) {
    case 'profile': return 'Profile';
    case 'pocket': return 'Pocket';
    case 'drill': return 'Drill';
    case 'thread': return 'Thread';
    case 'chamfer': return 'Chamfer';
    case 'engrave': return 'Engraving';
    case 'drag_knife': return 'Drag-knife';
    case 'helix': return 'Helix';
  }
}

export interface Tab {
  x: number;
  y: number;
}

export interface StockConfig {
  visible: boolean;
  mode: 'auto' | 'manual';
  margin: number;
  thickness: number;
  customX: number;
  customY: number;
}

export type ToolKind = 'endmill' | 'ball_nose' | 'v_bit' | 'engraver' | 'drag_knife' | 'drill' | 'laser_beam';
export type CoolantMode = 'off' | 'mist' | 'flood';

export interface ToolEntry {
  id: number;
  name: string;
  kind: ToolKind;
  diameter: number;
  tipDiameter?: number;
  dragoff?: number;
  flutes: number;
  speed: number;
  plungeRate: number;
  feedRate: number;
  coolant: CoolantMode;
}

export interface MachineSettings {
  unit: 'mm' | 'inch';
  mode: 'mill' | 'laser' | 'drag';
  comments: boolean;
  arcs: boolean;
  supportsToolchange: boolean;
  fastMoveZ: number;
}

export type OpKind =
  | 'profile'
  | 'pocket'
  | 'drill'
  | 'thread'
  | 'chamfer'
  | 'engrave'
  | 'drag_knife'
  | 'helix';

export type ProfileOffset = 'outside' | 'inside' | 'on';
export type PocketStrategy = 'cascade' | 'zigzag' | 'spiral';
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
/// the pocket. Sane default: 1.5 × tool radius.
export type PlungeStrategy =
  | { kind: 'direct' }
  | { kind: 'ramp'; angle_deg: number }
  | { kind: 'helix'; angle_deg: number; radius_mm: number };
/// Drill cycle for an OperationKind::Drill op. Mirrors wiac_core::project::DrillCycle.
/// `simple` → G81; `peck` → G83 (full retract between pecks); `chip_break` → G73
/// (small partial retract between pecks). `dwell_sec` is the dwell at bottom in
/// seconds (0 = no dwell). `peck_step_mm` is the per-peck Z step.
export type DrillCycle =
  | { kind: 'simple'; dwell_sec?: number }
  | { kind: 'peck'; peck_step_mm: number; dwell_sec?: number }
  | { kind: 'chip_break'; peck_step_mm: number; dwell_sec?: number };
/// How a multi-object source selection is combined into the region(s)
/// the operation actually consumes. Mirrors wiac_core::project::SourceCombine.
/// Default 'auto' is containment-aware (outer + inner = annulus).
export type SourceCombine =
  | 'auto'
  | 'union'
  | 'difference'
  | 'intersection'
  | 'xor'
  | 'none';

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
  step: number;
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
}

export const project = new ProjectState();

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
