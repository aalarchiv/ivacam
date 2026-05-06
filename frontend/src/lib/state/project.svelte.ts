// Global project state, Svelte 5 runes.
// Holds the most recently imported geometry plus UI flags.

import type { DefaultsResponse, JsonSchema } from '../api/client';
import type { GenerateResponse, ImportResponse, Point2 } from '../api/types';

class ProjectState {
  imported = $state<ImportResponse | null>(null);
  generated = $state<GenerateResponse | null>(null);
  loading = $state(false);
  generating = $state(false);
  error = $state<string | null>(null);
  visibleLayers = $state<Set<string>>(new Set());

  /// Setup tree (machine/tool/mill/pockets/tabs/leads). Hydrated from
  /// /defaults; user edits replace the in-memory copy.
  setup = $state<Record<string, unknown>>({});
  setupSchema = $state<JsonSchema | null>(null);
  setupDefinitions = $state<Record<string, JsonSchema>>({});

  /// Extra app-level UI state we want round-tripped to .vc-project.
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

  addTab(segmentIdx: number, position: Point2) {
    const next = { ...this.tabs };
    next[segmentIdx] = [...(next[segmentIdx] ?? []), { x: position.x, y: position.y }];
    this.tabs = next;
    this.generated = null; // invalidate gcode — needs re-Generate
  }

  removeTab(segmentIdx: number, tabIdx: number) {
    const list = this.tabs[segmentIdx];
    if (!list) return;
    const next = { ...this.tabs };
    next[segmentIdx] = list.filter((_, i) => i !== tabIdx);
    if (next[segmentIdx].length === 0) delete next[segmentIdx];
    this.tabs = next;
    this.generated = null;
  }

  clearTabs() {
    this.tabs = {};
    this.generated = null;
  }

  setImported(r: ImportResponse) {
    this.imported = r;
    this.generated = null;
    this.error = null;
    this.visibleLayers = new Set(r.layers.map((l) => l.name));
    this.selectedEntities = new Set();
    this.tabs = {};
  }

  setGenerated(r: GenerateResponse) {
    this.generated = r;
    this.error = null;
    this.playhead = 1.0;
  }

  setDefaults(d: DefaultsResponse) {
    this.setup = d.setup;
    this.setupSchema = d.schema;
    this.setupDefinitions = d.definitions;
  }

  setSetup(next: Record<string, unknown>) {
    this.setup = next;
    // Discard any prior toolpath — the setup change invalidates it.
    this.generated = null;
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
      version: 1,
      kind: 'wiac-project',
      imported: this.imported,
      setup: this.setup,
      visibleLayers: [...this.visibleLayers],
      selectedEntities: [...this.selectedEntities],
      tabs: this.tabs,
      stock: this.stock,
      tools: this.tools,
      machine: this.machine,
    };
  }

  restore(file: ProjectFile) {
    if (file.kind !== 'wiac-project') {
      throw new Error('not a wiaConstructor project file');
    }
    if (file.imported) this.setImported(file.imported);
    this.setup = file.setup ?? this.setup;
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
    this.generated = null;
    return op;
  }

  removeOperation(id: number) {
    this.operations = this.operations.filter((o) => o.id !== id);
    if (this.selectedOpId === id) this.selectedOpId = null;
    this.generated = null;
  }

  updateOperation(id: number, patch: Partial<OpEntry>) {
    this.operations = this.operations.map((o) => (o.id === id ? { ...o, ...patch } : o));
    this.generated = null;
  }

  reorderOperation(id: number, toIndex: number) {
    const cur = this.operations.findIndex((o) => o.id === id);
    if (cur < 0) return;
    const next = [...this.operations];
    const [op] = next.splice(cur, 1);
    next.splice(Math.max(0, Math.min(toIndex, next.length)), 0, op);
    this.operations = next;
    this.generated = null;
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
  /// If set, the op runs only on chains whose layer name is in the list.
  /// null means "every chain in the imported geometry".
  sourceLayers: string[] | null;
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
  setup: Record<string, unknown>;
  visibleLayers: string[];
  selectedEntities: number[];
  tabs?: Record<number, Tab[]>;
  stock?: StockConfig;
  tools?: ToolEntry[];
  machine?: MachineSettings;
  operations?: OpEntry[];
}

export const project = new ProjectState();
