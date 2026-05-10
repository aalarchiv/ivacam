// Plain-TS core of the workspace persistence layer. See the doc comment
// on `./workspace.svelte.ts` for the bigger picture; this file is split
// from the `.svelte.ts` so vitest can import it without the Svelte rune
// compiler. Reactivity is bolted on by the `.svelte.ts` wrapper via a
// subscriber callback.

import { isTauri } from '../api/env';

const STORAGE_KEY = 'wiac-workspace';
const SCHEMA_VERSION = 1;
const MAX_RECENT = 10;
const SAVE_DEBOUNCE_MS = 500;

export interface RecentProject {
  path: string;
  filename: string;
  openedAt: number;
}

export interface CameraState {
  px: number;
  py: number;
  pz: number;
  tx: number;
  ty: number;
  tz: number;
}

export interface PanelLayout {
  left_width: number;
  right_width: number;
  bottom_height: number;
}

export interface PerProjectState {
  visible_layers: string[];
  selected_op_id: number | null;
  playhead: number;
}

export interface WorkspaceState {
  workspace_schema_version: number;
  last_project: string | null;
  recent_projects: RecentProject[];
  camera: CameraState | null;
  panels: PanelLayout;
  per_project: Record<string, PerProjectState>;
}

export const DEFAULT_WORKSPACE: WorkspaceState = {
  workspace_schema_version: SCHEMA_VERSION,
  last_project: null,
  recent_projects: [],
  camera: null,
  panels: { left_width: 0, right_width: 360, bottom_height: 240 },
  per_project: {},
};

function defaultsClone(): WorkspaceState {
  return {
    ...DEFAULT_WORKSPACE,
    recent_projects: [],
    panels: { ...DEFAULT_WORKSPACE.panels },
    per_project: {},
  };
}

/// Defensive parser. Any structural surprise (wrong type, missing key,
/// future schema version) falls back to defaults rather than throwing.
export function parseWorkspace(raw: string | null | undefined): WorkspaceState {
  if (!raw) return defaultsClone();
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return defaultsClone();
  }
  if (!parsed || typeof parsed !== 'object') return defaultsClone();
  const obj = parsed as Record<string, unknown>;
  const ver = obj.workspace_schema_version;
  if (typeof ver !== 'number' || ver !== SCHEMA_VERSION) {
    return defaultsClone();
  }
  const out = defaultsClone();
  if (typeof obj.last_project === 'string' || obj.last_project === null) {
    out.last_project = obj.last_project as string | null;
  }
  if (Array.isArray(obj.recent_projects)) {
    const recents: RecentProject[] = [];
    for (const e of obj.recent_projects) {
      if (
        e &&
        typeof e === 'object' &&
        typeof (e as RecentProject).path === 'string' &&
        typeof (e as RecentProject).filename === 'string' &&
        typeof (e as RecentProject).openedAt === 'number'
      ) {
        recents.push({
          path: (e as RecentProject).path,
          filename: (e as RecentProject).filename,
          openedAt: (e as RecentProject).openedAt,
        });
      }
    }
    out.recent_projects = recents.slice(0, MAX_RECENT);
  }
  if (obj.camera && typeof obj.camera === 'object') {
    const c = obj.camera as Record<string, unknown>;
    if (
      typeof c.px === 'number' && typeof c.py === 'number' && typeof c.pz === 'number' &&
      typeof c.tx === 'number' && typeof c.ty === 'number' && typeof c.tz === 'number'
    ) {
      out.camera = { px: c.px, py: c.py, pz: c.pz, tx: c.tx, ty: c.ty, tz: c.tz };
    }
  }
  if (obj.panels && typeof obj.panels === 'object') {
    const p = obj.panels as Record<string, unknown>;
    out.panels = {
      left_width: typeof p.left_width === 'number' ? p.left_width : DEFAULT_WORKSPACE.panels.left_width,
      right_width: typeof p.right_width === 'number' ? p.right_width : DEFAULT_WORKSPACE.panels.right_width,
      bottom_height: typeof p.bottom_height === 'number' ? p.bottom_height : DEFAULT_WORKSPACE.panels.bottom_height,
    };
  }
  if (obj.per_project && typeof obj.per_project === 'object') {
    const pp = obj.per_project as Record<string, unknown>;
    for (const [k, v] of Object.entries(pp)) {
      if (!v || typeof v !== 'object') continue;
      const e = v as Record<string, unknown>;
      out.per_project[k] = {
        visible_layers: Array.isArray(e.visible_layers)
          ? (e.visible_layers as unknown[]).filter((s): s is string => typeof s === 'string')
          : [],
        selected_op_id:
          typeof e.selected_op_id === 'number' ? e.selected_op_id : null,
        playhead: typeof e.playhead === 'number' ? e.playhead : 1.0,
      };
    }
  }
  return out;
}

/// Storage transport. Split into an interface so tests can inject an
/// in-memory store without touching `localStorage` or Tauri.
export interface WorkspaceTransport {
  read(): Promise<string | null>;
  write(json: string): Promise<void>;
}

class LocalStorageTransport implements WorkspaceTransport {
  async read(): Promise<string | null> {
    if (typeof window === 'undefined') return null;
    try {
      return window.localStorage.getItem(STORAGE_KEY);
    } catch {
      return null;
    }
  }
  async write(json: string): Promise<void> {
    if (typeof window === 'undefined') return;
    try {
      window.localStorage.setItem(STORAGE_KEY, json);
    } catch {
      // ignore — quota / disabled storage are non-fatal here.
    }
  }
}

class TauriTransport implements WorkspaceTransport {
  async read(): Promise<string | null> {
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      const v = await invoke<string | null>('read_workspace_file');
      return typeof v === 'string' ? v : null;
    } catch {
      return null;
    }
  }
  async write(json: string): Promise<void> {
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      await invoke('write_workspace_file', { json });
    } catch {
      // ignore — best-effort.
    }
  }
}

function defaultTransport(): WorkspaceTransport {
  return isTauri() ? new TauriTransport() : new LocalStorageTransport();
}

export class WorkspaceStore {
  private state: WorkspaceState = defaultsClone();
  private transport: WorkspaceTransport;
  private saveTimer: ReturnType<typeof setTimeout> | null = null;
  private loaded = false;
  private subscribers: Array<() => void> = [];

  constructor(transport?: WorkspaceTransport) {
    this.transport = transport ?? defaultTransport();
  }

  get(): WorkspaceState {
    return this.state;
  }

  /// Subscribe to mutations. Returns an unsubscribe function. The
  /// reactive wrapper in `workspace.svelte.ts` uses this to bump a
  /// `$state` version counter so component effects re-run.
  subscribe(fn: () => void): () => void {
    this.subscribers.push(fn);
    return () => {
      const i = this.subscribers.indexOf(fn);
      if (i >= 0) this.subscribers.splice(i, 1);
    };
  }

  private notify() {
    for (const fn of this.subscribers) fn();
  }

  update(patch: Partial<WorkspaceState>) {
    this.state = { ...this.state, ...patch };
    this.notify();
    this.scheduleSave();
  }

  addRecentProject(path: string, filename: string) {
    const now = Date.now();
    const filtered = this.state.recent_projects.filter((e) => e.path !== path);
    filtered.unshift({ path, filename, openedAt: now });
    this.state = {
      ...this.state,
      recent_projects: filtered.slice(0, MAX_RECENT),
      last_project: path,
    };
    this.notify();
    this.scheduleSave();
  }

  setPerProject(path: string, patch: Partial<PerProjectState>) {
    const cur = this.state.per_project[path] ?? {
      visible_layers: [],
      selected_op_id: null,
      playhead: 1.0,
    };
    const next = { ...cur, ...patch };
    this.state = {
      ...this.state,
      per_project: { ...this.state.per_project, [path]: next },
    };
    this.notify();
    this.scheduleSave();
  }

  clearRecentProjects() {
    this.state = { ...this.state, recent_projects: [], last_project: null };
    this.notify();
    this.scheduleSave();
  }

  /// Drop per-project entries and recent-projects entries whose paths
  /// no longer exist on disk (Tauri only — we have no fs check on the
  /// web). Keeps the JSON from accumulating stale state.
  async pruneMissingProjects() {
    if (!isTauri()) return;
    let exists: (path: string) => Promise<boolean>;
    try {
      const fs = await import('@tauri-apps/plugin-fs');
      exists = (path: string) => fs.exists(path);
    } catch {
      return;
    }
    const keep: Record<string, PerProjectState> = {};
    for (const [path, val] of Object.entries(this.state.per_project)) {
      try {
        if (await exists(path)) keep[path] = val;
      } catch {
        keep[path] = val;
      }
    }
    const recents: RecentProject[] = [];
    for (const r of this.state.recent_projects) {
      try {
        if (await exists(r.path)) recents.push(r);
      } catch {
        recents.push(r);
      }
    }
    let lastProject = this.state.last_project;
    if (lastProject) {
      try {
        if (!(await exists(lastProject))) lastProject = null;
      } catch {
        // leave as-is on probe failure.
      }
    }
    this.state = {
      ...this.state,
      per_project: keep,
      recent_projects: recents,
      last_project: lastProject,
    };
    this.notify();
    this.scheduleSave();
  }

  async load(): Promise<void> {
    const raw = await this.transport.read();
    this.state = parseWorkspace(raw);
    this.loaded = true;
    this.notify();
  }

  /// Force the pending debounced save to flush now. Call from
  /// `beforeunload` if you need belt-and-braces durability — the regular
  /// debounce handles steady-state.
  async save(): Promise<void> {
    if (this.saveTimer) {
      clearTimeout(this.saveTimer);
      this.saveTimer = null;
    }
    const json = JSON.stringify(this.state);
    await this.transport.write(json);
  }

  private scheduleSave() {
    if (!this.loaded) {
      // Defer first save until load() completes — otherwise we'd
      // overwrite a real file with defaults during startup.
      return;
    }
    if (this.saveTimer) clearTimeout(this.saveTimer);
    this.saveTimer = setTimeout(() => {
      this.saveTimer = null;
      void this.save();
    }, SAVE_DEBOUNCE_MS);
  }

  /// Test helper: pretend `load()` already ran so `update()` calls are
  /// flushed eagerly. Production code should call `load()` instead.
  markLoadedForTests() {
    this.loaded = true;
  }
}
