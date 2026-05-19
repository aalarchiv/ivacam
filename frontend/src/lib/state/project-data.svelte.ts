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

import type { ImportResponse } from '../api/types';
import {
  identityFileTransform,
  type FileTransform,
  type Fixture,
  type MachineSettings,
  type StockConfig,
  type TextLayer,
  type ToolEntry,
} from './project-types';
import type { OpEntry } from './op_types';

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
  /// 75op: when true, GenerateBar debounces project.dirty changes and
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
  // Stepped voxel mesh is ~280 bytes / cell (positions + normals +
  // indices). 1M cells is ~280 MB of GPU memory — comfortable on
  // integrated GPUs and most laptops. Users on discrete-GPU desktops
  // can raise this in Settings → Performance. (audit-auim)
  maxSimulationCells: 1_000_000,
  blockOnCriticalSimWarnings: false,
  autoRegenerate: false,
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
  imported = $state<ImportResponse | null>(null);

  /// Non-destructive file-level transform (bww). Translates / rotates /
  /// scales / mirrors the entire imported drawing as a layout convenience
  /// — lets the user reposition the part on stock without re-exporting
  /// from CAD. Applied lazily by `project.transformedImport`; the raw
  /// `imported` is unchanged. Identity = no-op short-circuit.
  fileTransform = $state<FileTransform>(identityFileTransform());

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
    supportsToolchange: false,
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

  /// Persistent text entities — phase 1 of the text-engraving rework.
  /// Each entry holds the editable inputs (text content, font, size,
  /// transform, spacing) that the pipeline turns into segments at
  /// generate time. Distinct from baked TEXT segments in `imported`:
  /// editing a TextLayer field re-runs the renderer, and a future
  /// `text_engrave` op references one by id.
  textLayers = $state<TextLayer[]>([]);

  /// True when the in-memory project differs from the gcode currently
  /// shown in `generated`. Set by op edits/reorders/enable toggles;
  /// cleared by setGenerated. The status badge in the ops list reads
  /// this so the user knows "re-Generate to refresh".
  dirty = $state(false);

  /// Per-installation user preferences. Persisted to localStorage under
  /// `wiac.settings`; not part of .vc-project (those are per-project).
  /// The SettingsDialog owns the UX; consumers (theme application, i18n
  /// init, future cutting-preview rendering) read from here.
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
