/// Project-data slice of ProjectState (audit 6cpl step 4 / n5v5). Owns
/// every "what does this project contain" field that survives across
/// sessions: the imported geometry, the ops list, the tool library,
/// machine + stock settings, fixtures, text layers, plus the dirty flag
/// and the per-installation user preferences (`settings`).
///
/// This is the slice the undo/redo command bus mutates. The
/// `CommandTarget` interface in `commands.ts` lists exactly the fields
/// owned here; commands never see the parent class, only this surface
/// (the parent's proxy getter/setters forward through).
///
/// Like the other slices, no mutation methods live here — the parent
/// `ProjectState` still owns operations like `addOperation` /
/// `updateMachine` that wrap the right command in the history bus.

import {
  defaultWorkOffset,
  type Fixture,
  type ImportEntry,
  type MachineSettings,
  type ReliefSource,
  type StockConfig,
  type TextLayer,
  type ToolEntry,
  type WorkOffset,
} from './project-types';
import type { OpEntry } from './op_types';
import { DEFAULT_OSNAP_SETTINGS, type OSnapSettings } from '../canvas/osnap';

const SETTINGS_KEY = 'ivac.settings';
const LEGACY_THEME_KEY = 'ivac.theme';

export interface AppSettings {
  theme: 'auto' | 'light' | 'dark';
  previewMode: 'wireframe' | 'solid' | 'both';
  solidColor: string;
  solidOpacity: number;
  edgeColor: string;
  edgeOpacity: number;
  cellResolutionMode: 'auto' | 'manual';
  cellResolutionMm: number;
  solidPreviewByDefault: boolean;
  maxSimulationCells: number;
  /// 9tba: ceiling on render triangle count for the 3D-sim heightfield.
  /// When the active LOD level's mesh would exceed this, the renderer
  /// drops to a coarser pyramid level. Decouples sim accuracy
  /// (`maxSimulationCells`) from GPU budget so projects with high
  /// `maxSimulationCells` still render smoothly on integrated GPUs.
  maxRenderTriangles: number;
  /// When true, GenerateBar refuses to emit gcode while the most recent
  /// sim run reported critical warnings (collisions, rapid-through-stock).
  blockOnCriticalSimWarnings: boolean;
  /// Tier-4 safety: when true, GenerateBar refuses to EXPORT gcode while
  /// the last Generate reported `out_of_work_area` moves (the path leaves
  /// the machine envelope — soft-limit fault or a gantry crash). Opt-IN
  /// (default false) because the work-area default is often a placeholder
  /// that doesn't match the real machine; operators who've set their true
  /// envelope turn this on for a hard pre-send gate. Generate/preview stay
  /// open so the violation can be seen and fixed.
  blockOnWorkAreaViolation: boolean;
  /// 75op: when true, GenerateBar debounces project.data.dirty changes and
  /// auto-runs Generate after a brief idle. Off by default; power
  /// users on big projects keep manual control.
  autoRegenerate: boolean;
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
  /// li0m: 2D-canvas object-snap toggles. Per-kind booleans + grid
  /// step. Persisted in localStorage so the user's snap preferences
  /// survive across sessions / projects.
  osnap: OSnapSettings;
  /// Stroke width (px) for preview lines — the 2D canvas geometry and
  /// the 3D toolpath / wireframe (the latter via fat Line2 lines, since
  /// WebGL ignores plain line width). 1.5 ≈ the previous fixed look.
  previewLineWidth: number;
  /// 3D tool-move direction-arrow density. Scales the cumulative-path
  /// spacing between arrows (higher = more arrows). 1 = default (~3 mm
  /// spacing); 0 disables arrows.
  toolMoveArrowDensity: number;
  /// 27ng / rpas: when true (the default), scrubbing the playhead
  /// BACKWARD triggers a full sim reset followed by a forward
  /// replay from t=0 to the new position so the heightfield
  /// exactly reflects the carve state at the new playhead. When
  /// false, backstep is a no-op for the sim — cells retain
  /// whatever the deepest cut at each XY was the last time the
  /// playhead reached that segment. Combined with the post-
  /// Generate `playhead = 1.0` hop (so warnings surface
  /// immediately), the false case shows the END-OF-PROGRAM
  /// state regardless of where the user drags the scrubber back
  /// to — which is the rpas regression. Default true tracks the
  /// playhead at the cost of a replay per backstep; users on
  /// programs with tens of thousands of segments can flip it off
  /// to keep scrubbing responsive at the price of time-accurate
  /// rewind.
  exactSimRewind: boolean;
}

export const DEFAULT_SETTINGS: AppSettings = {
  theme: 'auto',
  previewMode: 'wireframe',
  solidColor: '#c8b48a',
  solidOpacity: 0.5,
  edgeColor: '#1a1a1a',
  edgeOpacity: 1.0,
  cellResolutionMode: 'auto',
  cellResolutionMm: 0.2,
  solidPreviewByDefault: false,
  // Stepped voxel mesh is ~280 bytes / cell (positions + normals +
  // indices). 1M cells is ~280 MB of GPU memory — comfortable on
  // integrated GPUs and most laptops. Users on discrete-GPU desktops
  // can raise this in Settings → Performance. (audit-auim)
  maxSimulationCells: 1_000_000,
  // Stepped voxel mesh emits ~6 triangles / cell. 2M triangles maps to
  // ~333k cells active — a comfortable mid-range integrated-GPU
  // budget (audit-9tba). Above this the renderer drops to a coarser
  // LOD level. Independent from `maxSimulationCells` so high sim
  // accuracy doesn't force a GPU stall.
  maxRenderTriangles: 2_000_000,
  // v0ez: default the safety gate ON for the beta. Out of the box a
  // program that exits the work-area / stock envelope (or trips a
  // collision / rapid-through-stock sim warning) is blocked from
  // generate + download until the user fixes it or explicitly disables
  // the gate in Settings. Safer default for people running real
  // machines; opt-out rather than opt-in.
  blockOnCriticalSimWarnings: true,
  // Tier-4: opt-IN (the work-area envelope is often a placeholder, so a
  // default-on gate would be noise). Operators with an accurate envelope
  // enable it for a hard pre-send block on out-of-work-area moves.
  blockOnWorkAreaViolation: false,
  autoRegenerate: false,
  autoRunSimOnSave: true,
  autoReloadSources: true,
  showStockBox: true,
  osnap: { ...DEFAULT_OSNAP_SETTINGS },
  previewLineWidth: 1.5,
  toolMoveArrowDensity: 1,
  // rpas: default ON so the 3D heightfield tracks the playhead
  // exactly — the only sane interaction with the post-Generate
  // `playhead = 1.0` hop. Users on long programs who'd rather
  // have responsive scrubbing than time-accurate terrain flip
  // this off in Settings → Performance.
  exactSimRewind: true,
};

/// Load persisted settings, deep-merging stored values over defaults so
/// adding new keys later doesn't break old payloads. Falls back to the
/// legacy `ivac.theme` key when no `ivac.settings` blob exists yet
/// (first run after the dialog ships).
function loadSettings(): AppSettings {
  if (typeof window === 'undefined') return { ...DEFAULT_SETTINGS };
  let merged: AppSettings = { ...DEFAULT_SETTINGS };
  try {
    const raw = window.localStorage.getItem(SETTINGS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<AppSettings> | null;
      if (parsed && typeof parsed === 'object') {
        // rpas: one-shot migration — the 27ng default of
        // `exactSimRewind: false` interacted badly with the
        // post-Generate `playhead = 1.0` hop (terrain stuck at
        // end-of-program). Any persisted `false` we see now is
        // almost certainly the 27ng default, not a user choice
        // (the toggle shipped same-day with the buggy default).
        // Drop the field so the new default kicks in. Users on
        // huge programs who legitimately want the off semantic
        // re-flip the toggle once.
        if (parsed.exactSimRewind === false) {
          delete (parsed as Record<string, unknown>).exactSimRewind;
        }
        merged = { ...merged, ...parsed };
        // Deep-merge `osnap` so a future-added knob falls back to its
        // DEFAULT instead of being undefined when the user's stored
        // blob predates the new key (li0m). Same care needed for any
        // future nested object setting.
        if (parsed.osnap && typeof parsed.osnap === 'object') {
          merged.osnap = { ...DEFAULT_OSNAP_SETTINGS, ...parsed.osnap };
        }
      }
      return merged;
    }
    // Migration path: seed from legacy single-purpose keys.
    const legacyTheme = window.localStorage.getItem(LEGACY_THEME_KEY);
    if (legacyTheme === 'auto' || legacyTheme === 'light' || legacyTheme === 'dark') {
      merged.theme = legacyTheme;
    }
  } catch {
    // localStorage unavailable / quota / parse failure → defaults are fine.
  }
  return merged;
}

/// Persist the current settings blob to localStorage. Cheap (one
/// JSON.stringify on a tiny object) so we just call it on every
/// mutation rather than debouncing — the SettingsDialog won't fire
/// updates fast enough to matter.
export function saveSettings(s: AppSettings): void {
  if (typeof window === 'undefined') return;
  try {
    window.localStorage.setItem(SETTINGS_KEY, JSON.stringify(s));
  } catch {
    // ignore — quota / disabled storage are non-fatal here.
  }
}

export class ProjectDataState {
  /// Imported drawings (wrsu). Each entry owns its own ImportResponse,
  /// non-destructive layout transform (bww — fileTransform), and source
  /// file path. Multi-file workflows append entries; common-case projects
  /// have 0 or 1. Phase 1 keeps existing single-import consumers reading
  /// imports[0] via the proxy accessors on `ProjectState` (project.imported,
  /// project.fileTransform, etc.). Phases 2+ migrate consumers to iterate
  /// or address by id.
  imports = $state<ImportEntry[]>([]);

  /// Ordered list of operations the program runs. Each op has a kind, a
  /// tool reference (id into `tools`), a source (which geometry it
  /// consumes), and per-kind parameters. Reordering = changing run
  /// order. Disabling = excluding from the final program without
  /// losing config.
  operations = $state<OpEntry[]>([]);

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
    toolchangeStrategy: 'manual_m0_pause',
    fastMoveZ: 5,
    accel: { x: 250, y: 250, z: 250 },
    toolchangeS: 5,
    rapidSpeed: 5000,
    workArea: { x: 200, y: 300, z: 50 },
  });

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

  /// Project fixtures (clamps, dogs, vise jaws). Threaded into the
  /// sim's collision check so the cutter can't run them over.
  fixtures = $state<Fixture[]>([]);

  /// i5g4: program-level WCS offset. All-zero @ G54 is the legacy
  /// default ("geometry origin = WCS origin"); set when the user
  /// zeros the spindle somewhere different from the drawing origin
  /// so the sim aligns the heightmap to the WCS frame. Persisted in
  /// the project file; full UI editor lands as P2 (abdk).
  workOffset = $state<WorkOffset>(defaultWorkOffset());

  /// Persistent text entities — phase 1 of the text-engraving rework.
  /// Each entry holds the editable inputs (text content, font, size,
  /// transform, spacing) that the pipeline turns into segments at
  /// generate time. Distinct from baked TEXT segments in `imported`:
  /// editing a TextLayer field re-runs the renderer, and a future
  /// `text_engrave` op references one by id.
  textLayers = $state<TextLayer[]>([]);

  /// f60x: relief / 3-axis surfacing sources (target Z(x,y) surfaces a
  /// `relief_mill` op finishes), referenced by op `sourceId`. Each holds a
  /// normalized-brightness grid decoded from a grayscale image.
  reliefSources = $state<ReliefSource[]>([]);

  /// l8lk: opt-in tool-change-order optimization. When true, the pipeline
  /// groups consecutive same-tool ops so a T1/T2/T1 program emits one
  /// tool change instead of two. Barrier-aware (program-only ops + ops
  /// with `pinOrder` stay put). Default false = declared order.
  groupOpsByTool = $state(false);

  /// True when the in-memory project differs from the gcode currently
  /// shown in `generated`. Set by op edits/reorders/enable toggles;
  /// cleared by setGenerated. The status badge in the ops list reads
  /// this so the user knows "re-Generate to refresh".
  dirty = $state(false);

  /// Per-installation user preferences. Persisted to localStorage under
  /// `ivac.settings`; not part of .vc-project (those are per-project).
  /// The SettingsDialog owns the UX; consumers (theme application,
  /// preview-mode toggles, performance caps) read from here.
  settings = $state<AppSettings>(loadSettings());

  /// Per-layer visibility. Mutated as a single Set replacement so Svelte
  /// reactivity fires (in-place .add()/.delete() don't trigger re-renders).
  /// Persisted per-project via `workspace.per_project`; not part of the
  /// undo bus.
  visibleLayers = $state<Set<string>>(new Set());

  /// Whether the 2D canvas paints the filled-region preview for Pocket
  /// ops on top of the wireframe. Default on — it's the answer to
  /// "what will this op actually machine?".
  regionsVisible = $state(true);
}
