/// Open / Save / Sample I/O extracted from FileUpload so the toolbar
/// (and any other UI surface) can invoke the same flows without
/// reaching through DOM query selectors. Each function mutates
/// `project` directly — loading flag, error toast, the
/// imported / generated payload, and the active project path / recent
/// list.

import { project } from '../state/project.svelte';
import { workspace } from '../state/workspace.svelte';
import { confirmStore } from '../state/confirm.svelte';
import { isTauri } from '../api/env';
import { defaultClient } from '../api/http';
import { tryParseStructuredError } from '../api/client';
import { migrateLegacyToolTerms } from '../state/tool-migration';
// `pushRecent` (from ../recent) was a parallel store the UI never read
// — the File menu draws Recent Projects from workspace.recent_projects.
// The two stores could diverge silently (audit zxee). Dropped; the
// `ivac.recent` localStorage key is harmlessly orphaned.
import type { ImportResponse } from '../api/types';
import type { MachineSettings, ToolEntry } from '../state/project.svelte';
import { migrateMachineSettings } from '../state/project-types';

/// eu2b: clear the Rust-side process-global pipeline cache when a
/// project replace flow runs (open file, open project, load sample,
/// load project from disk). The cache key already encodes the
/// machine + tool fingerprints, so this is purely a hygiene step —
/// it bounds the working set to ops in the CURRENT project rather
/// than letting LRU eviction reclaim the previous project's entries
/// at its own pace. Browser/HTTP builds skip the invoke; the cache
/// lives in the same wasm address space and is bounded by the same
/// LRU there.
async function clearPipelineCacheOnReplace(): Promise<void> {
  if (!isTauri()) return;
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    await invoke('clear_pipeline_cache_cmd');
  } catch {
    // Cache invalidation is best-effort — a failed invoke shouldn't
    // block the load flow. The next Generate will compute fresh
    // entries either way.
  }
}

export function reportError(input: unknown) {
  const raw = input instanceof Error ? input.message : String(input);
  const structured = tryParseStructuredError(raw);
  project.setError(structured ?? raw);
}

/// Friendly progress message based on the file's extension. Used by
/// the loading overlay so the user knows what's happening for the
/// 100–500 ms a big DXF takes to parse.
export function pathToLoadingMessage(path: string): string {
  const ext = path.toLowerCase().split('.').pop() ?? '';
  switch (ext) {
    case 'dxf':
      return 'Parsing DXF…';
    case 'svg':
      return 'Parsing SVG…';
    case 'hpgl':
    case 'plt':
      return 'Parsing HPGL…';
    case 'ngc':
      return 'Parsing G-code…';
    case 'stl':
      return 'Parsing STL…';
    default:
      return 'Loading file…';
  }
}

/// Same-origin samples bundled in `public/samples/`. The labels
/// surface in the File ▸ Samples submenu.
// ujs2: the `*-rust.json` variants are gitignored (no generator ships
// them) and the py/rs split is vestigial now that the backend is
// Rust-only — both now come from the same importer. Keep just the two
// tracked, selectable fixtures so a clean checkout / static deploy never
// 404s a sample.
export const SAMPLES: { label: string; url: string }[] = [
  { label: 'simple', url: '/samples/simple.json' },
  { label: 'all', url: '/samples/all.json' },
];

/// FileUpload.svelte stashes its hidden `<input type=file>` elements
/// on `window` so the browser fallbacks can find them. This is a no-op
/// in Tauri where the native picker covers everything.
function hiddenFileInput(): HTMLInputElement | null {
  return (window as unknown as { __ivacFileInput?: HTMLInputElement }).__ivacFileInput ?? null;
}
function hiddenProjectInput(): HTMLInputElement | null {
  return (
    (window as unknown as { __ivacProjectInput?: HTMLInputElement }).__ivacProjectInput ?? null
  );
}

/// Desktop: native open dialog. Browser: programmatically click the
/// hidden `<input type=file>` rendered by FileUpload so the picker
/// fires inside the user-gesture window.
export async function openFile() {
  if (!(await confirmDiscardIfDirty('open another drawing'))) return;
  if (isTauri()) {
    const { open } = await import('@tauri-apps/plugin-dialog');
    const selected = await open({
      multiple: false,
      filters: [{ name: 'CAD/CAM input', extensions: ['dxf', 'svg'] }],
    });
    if (typeof selected === 'string') await loadFromPath(selected);
    return;
  }
  hiddenFileInput()?.click();
}

/// If the project has unsaved changes, prompt the user before a
/// destructive load (open file, open project, recent). Returns
/// `true` to proceed, `false` to bail.
///
/// Uses `confirmStore` for an in-app styled prompt — the previous
/// `window.confirm` regressed against the Tauri C10 rule (WebKitGTK
/// blocks the renderer and never returns). Exported so every
/// destructive-load entry point (including App.svelte's Recent click)
/// shares ONE confirmation dialog instead of a second native
/// `window.confirm` (npig).
export async function confirmDiscardIfDirty(action: string): Promise<boolean> {
  if (!project.dirty) return true;
  if (typeof window === 'undefined') return true;
  return confirmStore.ask({
    title: 'Unsaved changes',
    body: `Your project has unsaved changes. Continue and ${action}? Your unsaved work will be lost.`,
    primaryLabel: 'Discard & continue',
    cancelLabel: 'Keep editing',
    danger: true,
  });
}

/// Desktop: native open dialog for `.ivac-project.json`. Browser: same
/// hidden-input trick as openFile, but for project files.
export async function openProject() {
  if (!(await confirmDiscardIfDirty('open another project'))) return;
  if (isTauri()) {
    const { open } = await import('@tauri-apps/plugin-dialog');
    const { readTextFile } = await import('@tauri-apps/plugin-fs');
    const selected = await open({
      multiple: false,
      filters: [
        {
          name: 'ivaCAM project',
          extensions: ['ivac-project.json', 'vc-project.json', 'json'],
        },
      ],
    });
    if (typeof selected !== 'string') return;
    project.loading = true;
    project.loadingMessage = 'Loading project…';
    project.error = null;
    try {
      const text = await readTextFile(selected);
      project.clearProject();
      await clearPipelineCacheOnReplace();
      project.restore(JSON.parse(text));
      const filename = selected.split(/[\\/]/).pop() ?? selected;
      workspace.addRecentProject(selected, filename);
      project.setActiveProjectPath(selected);
      project.dirty = false;
    } catch (e) {
      project.setError(`load project: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      project.loading = false;
      project.loadingMessage = null;
    }
    return;
  }
  hiddenProjectInput()?.click();
}

/// Tauri-only path-based load — used by the menu, the reopen banner,
/// the recent-projects submenu, and OS file-association launches.
/// REPLACE semantics: clears the project (ops, fixtures, textLayers,
/// stock) before importing the new drawing so leftover ops from a
/// prior project don't silently re-target unrelated objects.
/// Callers that want ADD semantics (overlay a second drawing on the
/// current project) should use `addDrawingPath` instead.
export async function loadFromPath(path: string) {
  project.loading = true;
  project.loadingMessage = pathToLoadingMessage(path);
  project.error = null;
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    const result = await invoke<ImportResponse>('import_path', { path });
    project.clearProject();
    await clearPipelineCacheOnReplace();
    project.setImported(result, path);
    await project.convertImportedTextEntities();
    const filename = path.split(/[\\/]/).pop() ?? path;
    workspace.addRecentProject(path, filename);
    project.setActiveProjectPath(path);
    // setImported flips dirty=true; reset because the freshly-loaded
    // file matches what's on disk and the user hasn't edited yet.
    project.dirty = false;
  } catch (e) {
    reportError(e);
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Desktop-only ADD path — overlays another drawing onto the
/// current project without resetting ops/fixtures/etc. Used by the
/// "+ Add drawing" affordance for genuine multi-drawing workflows.
/// Not exposed in a menu today; reserved for future UI.
export async function addDrawingPath(path: string) {
  project.loading = true;
  project.loadingMessage = pathToLoadingMessage(path);
  project.error = null;
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    const result = await invoke<ImportResponse>('import_path', { path });
    project.addImported(result, path);
    await project.convertImportedTextEntities();
  } catch (e) {
    reportError(e);
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Tauri-only path-based project load — same flow as `loadFromPath`
/// but dispatches to the project-restore path. Callers (Recent
/// menu, OS file-association launch) should have already vetted
/// the dirty state via `confirmDiscardIfDirty`.
export async function loadProjectPath(path: string) {
  project.loading = true;
  project.loadingMessage = 'Loading project…';
  project.error = null;
  try {
    const { readTextFile } = await import('@tauri-apps/plugin-fs');
    const text = await readTextFile(path);
    project.clearProject();
    await clearPipelineCacheOnReplace();
    project.restore(JSON.parse(text));
    const filename = path.split(/[\\/]/).pop() ?? path;
    workspace.addRecentProject(path, filename);
    project.setActiveProjectPath(path);
    project.dirty = false;
  } catch (e) {
    project.setError(`load project: ${e instanceof Error ? e.message : String(e)}`);
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Browser-path import via the FastAPI-shaped /import endpoint (or its
/// WASM equivalent in Tauri). Used by drag-and-drop + the hidden
/// `<input type=file>` change handler. REPLACE semantics — see
/// `loadFromPath` for the desktop counterpart.
export async function loadFile(file: File) {
  const client = defaultClient();
  project.loading = true;
  project.loadingMessage = pathToLoadingMessage(file.name);
  project.error = null;
  try {
    const result = await client.importFile(file);
    project.clearProject();
    await clearPipelineCacheOnReplace();
    project.setImported(result);
    await project.convertImportedTextEntities();
    project.dirty = false;
  } catch (e) {
    reportError(e);
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Browser project load — paired with the hidden project input.
/// REPLACE semantics, mirroring the desktop `loadProjectPath`.
export async function loadProjectFile(file: File) {
  project.loading = true;
  project.loadingMessage = 'Loading project…';
  project.error = null;
  try {
    const text = await file.text();
    project.clearProject();
    await clearPipelineCacheOnReplace();
    project.restore(JSON.parse(text));
    project.dirty = false;
  } catch (e) {
    project.setError(`load project: ${e instanceof Error ? e.message : String(e)}`);
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Save the current project state. Desktop = native save dialog;
/// browser = anchor-tag download trick.
export async function saveProject() {
  const snapshot = JSON.stringify(project.snapshot(), null, 2);
  const base = project.transformedImport?.filename?.replace(/\.[^.]+$/, '') ?? 'project';
  const filename = `${base}.ivac-project.json`;
  if (isTauri()) {
    const { save } = await import('@tauri-apps/plugin-dialog');
    const { writeTextFile } = await import('@tauri-apps/plugin-fs');
    const path = await save({
      defaultPath: filename,
      filters: [
        {
          name: 'ivaCAM project',
          extensions: ['ivac-project.json', 'vc-project.json', 'json'],
        },
      ],
    });
    if (typeof path === 'string') {
      try {
        await writeTextFile(path, snapshot);
        // amwo: the file now matches disk — clear dirty to match the
        // contract every load path upholds. Otherwise the quit-confirm
        // dialog, the confirmDiscardIfDirty prompt on a later Open, and
        // the stale-gcode indicators all fire right after a save.
        project.dirty = false;
      } catch (e) {
        project.setError(`save: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    return;
  }
  const blob = new Blob([snapshot], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
  // amwo: the browser download IS the save; mirror the desktop branch and
  // clear dirty so the snapshot just written to disk isn't reported unsaved.
  project.dirty = false;
}

/// Save a project report as a Markdown file (vh6e). Desktop = native
/// save dialog (.md); browser = anchor-tag download. Mirrors
/// `saveProject`'s transport split.
export async function saveReportMarkdown(markdown: string, baseName: string) {
  const filename = `${baseName}.md`;
  if (isTauri()) {
    const { save } = await import('@tauri-apps/plugin-dialog');
    const { writeTextFile } = await import('@tauri-apps/plugin-fs');
    const path = await save({
      defaultPath: filename,
      filters: [{ name: 'Markdown', extensions: ['md', 'markdown'] }],
    });
    if (typeof path === 'string') {
      try {
        await writeTextFile(path, markdown);
      } catch (e) {
        project.setError(`save report: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    return;
  }
  const blob = new Blob([markdown], { type: 'text/markdown' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

/// Fetch + import one of the bundled `public/samples/<x>.json` files.
/// REPLACE semantics: loading a sample drops the current project to
/// start fresh.
export async function loadSample(url: string) {
  if (!(await confirmDiscardIfDirty('load a sample'))) return;
  project.loading = true;
  project.loadingMessage = 'Loading sample…';
  project.error = null;
  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`fetch ${url}: ${res.status}`);
    const data = (await res.json()) as ImportResponse;
    project.clearProject();
    await clearPipelineCacheOnReplace();
    project.setImported(data);
    project.dirty = false;
  } catch (e) {
    project.setError(e instanceof Error ? e.message : String(e));
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Export the current `project.generated.gcode` to disk. Mirrors
/// `saveProject` — native save dialog on Tauri, anchor-tag download in
/// the browser. Filename suffix is .plt for HPGL output, .ngc otherwise.
/// `postProcessor` controls the suffix only; the gcode buffer is
/// already post-processed by the time it lands in `project.generated`.
export async function exportGeneratedGcode(
  postProcessor: 'linuxcnc' | 'grbl' | 'hpgl',
): Promise<void> {
  if (!project.generated) return;
  const base = project.transformedImport?.filename?.replace(/\.[^.]+$/, '') ?? 'output';
  const ext = postProcessor === 'hpgl' ? 'plt' : 'ngc';
  const filename = `${base}.${ext}`;
  if (isTauri()) {
    const { save } = await import('@tauri-apps/plugin-dialog');
    const { writeTextFile } = await import('@tauri-apps/plugin-fs');
    const path = await save({
      defaultPath: filename,
      filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
    });
    if (typeof path === 'string') {
      try {
        await writeTextFile(path, project.generated.gcode);
      } catch (e) {
        project.setError(`save: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    return;
  }
  const blob = new Blob([project.generated.gcode], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

/// 9c34: export the live simulated stock as a binary STL — exactly the
/// carved heightfield the 3D scene is rendering, serialized to a mesh
/// you can open in any STL viewer or diff against a reference. Walls
/// drop to the stock's underside (top minus thickness) for a watertight
/// mesh. No-op when there's no live sim (Generate hasn't run yet).
export async function exportSimulatedStockStl(): Promise<void> {
  const { getCurrentDriver } = await import('../sim/driver');
  const driver = getCurrentDriver();
  if (!driver) {
    project.setError('No simulated stock to export — run Generate first.');
    return;
  }
  const stock = project.stock;
  const topZ = 0; // stock top sits at WCS Z=0 by the project convention
  const stockBottomZ = topZ - Math.max(stock.thickness, 0);
  const bytes = driver.exportStl(stockBottomZ);
  if (!bytes) {
    project.setError('No simulated stock to export — run Generate first.');
    return;
  }
  const base = project.transformedImport?.filename?.replace(/\.[^.]+$/, '') ?? 'stock';
  const filename = `${base}.stl`;
  if (isTauri()) {
    const { save } = await import('@tauri-apps/plugin-dialog');
    const { writeFile } = await import('@tauri-apps/plugin-fs');
    const path = await save({
      defaultPath: filename,
      filters: [{ name: 'STL', extensions: ['stl'] }],
    });
    if (typeof path === 'string') {
      try {
        await writeFile(path, bytes);
      } catch (e) {
        project.setError(`stl export: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    return;
  }
  const blob = new Blob([bytes as BlobPart], { type: 'model/stl' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

/// Combined sample + pre-generated gcode load. Driven by query string
/// `?sample=X&gen=Y` at startup so demo links can land users on a
/// fully-loaded project.
export async function loadSampleWithGenerate(sampleUrl: string, generatedUrl: string) {
  project.loading = true;
  project.loadingMessage = 'Loading sample…';
  try {
    const [imp, gen] = await Promise.all([
      fetch(sampleUrl).then((r) => r.json()),
      fetch(generatedUrl).then((r) => r.json()),
    ]);
    project.setImported(imp);
    project.setGenerated(gen);
  } catch (e) {
    project.setError(e instanceof Error ? e.message : String(e));
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

// ───────────────────────────────────────────────────────────────────
// h0tx: toolset + machine save/load files.
//
// Two side-files independent of the .ivac-project.json: a toolset
// snapshot the user can share across projects, and a machine config
// snapshot the user can share across shop floors. Both wrap the
// payload in a small envelope:
//
//   {
//     kind: 'toolset' | 'machine',
//     format_version: 1,
//     updated_at: ISO timestamp at save,
//     payload: <data>,
//   }
//
// `updated_at` is a monotonic-ish identifier — when two snapshots
// disagree, the newer ISO timestamp wins. Used by the planned
// project-load merge prompt (deferred to a follow-up issue).
// ───────────────────────────────────────────────────────────────────

interface SnapshotEnvelope<K extends 'toolset' | 'machine', P> {
  kind: K;
  format_version: number;
  updated_at: string;
  payload: P;
}

const TOOLSET_FORMAT_VERSION = 1;
const MACHINE_FORMAT_VERSION = 1;

async function pickAndReadJson(
  filters: Array<{ name: string; extensions: string[] }>,
): Promise<string | null> {
  if (isTauri()) {
    const { open } = await import('@tauri-apps/plugin-dialog');
    const { readTextFile } = await import('@tauri-apps/plugin-fs');
    const selected = await open({ filters, multiple: false });
    if (typeof selected !== 'string') return null;
    return readTextFile(selected);
  }
  return new Promise<string | null>((resolve) => {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = filters.flatMap((f) => f.extensions.map((e) => `.${e}`)).join(',');
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) {
        resolve(null);
        return;
      }
      resolve(await file.text());
    };
    input.click();
  });
}

async function writeJson(
  defaultName: string,
  body: string,
  filters: Array<{ name: string; extensions: string[] }>,
) {
  if (isTauri()) {
    const { save } = await import('@tauri-apps/plugin-dialog');
    const { writeTextFile } = await import('@tauri-apps/plugin-fs');
    const path = await save({ defaultPath: defaultName, filters });
    if (typeof path === 'string') {
      await writeTextFile(path, body);
    }
    return;
  }
  const blob = new Blob([body], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = defaultName;
  a.click();
  URL.revokeObjectURL(url);
}

/// Export the current tool library to a `.ivac-toolset.json` file.
/// The user's set of tools, no machine, no project state. Reusable
/// across projects.
export async function saveToolset() {
  const envelope: SnapshotEnvelope<'toolset', ToolEntry[]> = {
    kind: 'toolset',
    format_version: TOOLSET_FORMAT_VERSION,
    updated_at: new Date().toISOString(),
    payload: project.tools.map((t) => ({ ...t })),
  };
  await writeJson('toolset.ivac-toolset.json', JSON.stringify(envelope, null, 2), [
    { name: 'ivaCAM toolset', extensions: ['ivac-toolset.json', 'json'] },
  ]);
}

/// Import a `.ivac-toolset.json`. `mode` controls how the file's
/// tools merge into the current set:
///   * `'replace'` — drop the current tools, use the file's
///   * `'add'`     — append; tools whose `name` already exists are
///                   skipped (the user's existing entries win).
export async function loadToolset(mode: 'replace' | 'add') {
  let text: string | null;
  try {
    text = await pickAndReadJson([
      { name: 'ivaCAM toolset', extensions: ['ivac-toolset.json', 'json'] },
    ]);
  } catch (e) {
    project.setError(`toolset load: ${e instanceof Error ? e.message : String(e)}`);
    return;
  }
  if (!text) return;
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    project.setError(`toolset parse: ${e instanceof Error ? e.message : String(e)}`);
    return;
  }
  const env = parsed as SnapshotEnvelope<'toolset', ToolEntry[]>;
  if (
    env == null ||
    typeof env !== 'object' ||
    env.kind !== 'toolset' ||
    !Array.isArray(env.payload)
  ) {
    project.setError('toolset load: not a .ivac-toolset.json file');
    return;
  }
  const incoming = env.payload.map(migrateLegacyToolTerms);
  if (mode === 'replace') {
    // Re-number ids 1..N so the new tools have a clean monotonic
    // sequence the project file can reference.
    const next = incoming.map((t, idx) => ({ ...t, id: idx + 1 }));
    // eu2b: the cache key folds in every ToolEntry field, so swapping
    // the library mid-session would force a miss-and-recompute on every
    // op anyway. Clear up front so the old entries stop occupying LRU
    // slots that will never hit again.
    await clearPipelineCacheOnReplace();
    project.replaceTools(next);
    return;
  }
  // Add: append everything not already present by name (case-insensitive).
  const existingNames = new Set(project.tools.map((t) => t.name.toLowerCase()));
  let nextId = project.tools.reduce((m, t) => Math.max(m, t.id), 0);
  const additions: ToolEntry[] = [];
  for (const t of incoming) {
    if (existingNames.has(t.name.toLowerCase())) continue;
    nextId += 1;
    additions.push({ ...t, id: nextId });
  }
  if (additions.length > 0) {
    project.replaceTools([...project.tools, ...additions]);
  }
}

/// Export the current machine config to a `.ivac-machine.json` file.
export async function saveMachine() {
  const envelope: SnapshotEnvelope<'machine', MachineSettings> = {
    kind: 'machine',
    format_version: MACHINE_FORMAT_VERSION,
    updated_at: new Date().toISOString(),
    payload: { ...project.machine },
  };
  const fileBase = (project.machine.name && project.machine.name.trim()) || 'machine';
  await writeJson(`${fileBase}.ivac-machine.json`, JSON.stringify(envelope, null, 2), [
    { name: 'ivaCAM machine', extensions: ['ivac-machine.json', 'json'] },
  ]);
}

/// Import a `.ivac-machine.json`. Replaces the active machine
/// config wholesale.
export async function loadMachine() {
  let text: string | null;
  try {
    text = await pickAndReadJson([
      { name: 'ivaCAM machine', extensions: ['ivac-machine.json', 'json'] },
    ]);
  } catch (e) {
    project.setError(`machine load: ${e instanceof Error ? e.message : String(e)}`);
    return;
  }
  if (!text) return;
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    project.setError(`machine parse: ${e instanceof Error ? e.message : String(e)}`);
    return;
  }
  const env = parsed as SnapshotEnvelope<'machine', MachineSettings>;
  if (
    env == null ||
    typeof env !== 'object' ||
    env.kind !== 'machine' ||
    env.payload == null ||
    typeof env.payload !== 'object'
  ) {
    project.setError('machine load: not a .ivac-machine.json file');
    return;
  }
  // eu2b: machine swap invalidates the cache the same way as a tool
  // library swap — hash_machine folds every relevant field, so the
  // post-swap Generate will miss-and-recompute. Drop the old entries
  // proactively.
  await clearPipelineCacheOnReplace();
  // cb5y: an older .ivac-machine.json carries `supportsToolchange` instead
  // of `toolchangeStrategy` — migrate before applying.
  project.setMachine(migrateMachineSettings(env.payload));
}

/// Decide whether a dropped/picked file is a project vs. raw geometry
/// by extension, then dispatch.
export function importDroppedFile(file: File) {
  if (
    file.name.endsWith('.ivac-project.json') ||
    file.name.endsWith('.vc-project.json') ||
    file.name.endsWith('.json')
  ) {
    return loadProjectFile(file);
  }
  return loadFile(file);
}
