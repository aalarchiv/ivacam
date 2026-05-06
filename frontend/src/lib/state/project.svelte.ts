// Global project state, Svelte 5 runes.
// Holds the most recently imported geometry plus UI flags.

import type { GenerateResponse, ImportResponse, Point2 } from '../api/types';

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
  /// indicator and by PlaybackBar for the slider.
  playhead = $state(1.0);

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
      sourceLayers: null,
      depth: -2,
      startDepth: 0,
      step: -1,
      offset: kind === 'engrave' || kind === 'drag_knife' ? 'on' : 'outside',
      pocketStrategy: kind === 'pocket' ? 'cascade' : null,
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
  depth: number;
  startDepth: number;
  step: number;
  offset: ProfileOffset;
  pocketStrategy: PocketStrategy | null;
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
