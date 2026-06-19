/// Open / Save / Sample I/O extracted from FileUpload so the toolbar
/// (and any other UI surface) can invoke the same flows without
/// reaching through DOM query selectors. Each function mutates
/// `project` directly — loading flag, error toast, the
/// imported / generated payload, and the active project path / recent
/// list.

import { project } from '../state/project.svelte';
import { workspace } from '../state/workspace.svelte';
import { confirmStore } from '../state/confirm.svelte';
import { isTauri, isAndroid } from '../api/env';
import { isProjectPath } from './file-kind';
export { isProjectPath } from './file-kind';
import { defaultClient } from '../api/http';
import { tryParseStructuredError } from '../api/client';
import { migrateLegacyToolTerms } from '../state/tool-migration';
// `pushRecent` (from ../recent) was a parallel store the UI never read
// — the File menu draws Recent Projects from workspace.recent_projects.
// The two stores could diverge silently. Dropped; the
// `ivac.recent` localStorage key is harmlessly orphaned.
import type { ImportResponse } from '../api/types';
import type { MachineSettings, ToolEntry } from '../state/project.svelte';
import { migrateMachineSettings } from '../state/project-types';

/// Clear the Rust-side process-global pipeline cache when a
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

/// Collapse multi-dot "extensions" (e.g. `ivac-machine.json`) to their
/// final segment (`json`) and dedupe. Android's SAF picker maps the
/// dialog's extension filters to MIME types and fails on multi-dot
/// values; desktop is unaffected by the broader filter.
function simpleExtensions(exts: string[]): string[] {
  const out = new Set<string>();
  for (const e of exts) {
    const seg = e.split('.').pop();
    if (seg) out.add(seg);
  }
  return [...out];
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
// The `*-rust.json` variants are gitignored (no generator ships
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
function hiddenOpenInput(): HTMLInputElement | null {
  return (window as unknown as { __ivacOpenInput?: HTMLInputElement }).__ivacOpenInput ?? null;
}

/// Unified "Open" (7jug.14): one picker for drawings AND projects, routed
/// by extension — replaces the separate "Open file" / "Open project".
/// Desktop opens a single native dialog spanning both filter sets; browser
/// clicks one combined hidden input whose change handler routes the same
/// way (see FileUpload).
/// Android SAF: the dialog returns a `content://` URI, not a filesystem
/// path, so the Rust `import_path` command can't open it. Read the bytes
/// via plugin-fs (which understands SAF URIs), content-sniff the format
/// (the URI carries no usable extension), and route through the
/// content-based loaders, which write to a real temp file before
/// importing. Used for both drawing and project opens.
async function contentUriToFile(
  uri: string,
  allowProject: boolean,
): Promise<{ file: File; isProject: boolean }> {
  const { readFile } = await import('@tauri-apps/plugin-fs');
  const bytes = await readFile(uri);
  const head = new TextDecoder('utf-8', { fatal: false }).decode(bytes.slice(0, 512)).trimStart();
  const isProject = allowProject && head.startsWith('{');
  const isSvg = /<svg|<\?xml/i.test(head);
  const name = isProject ? 'import.json' : isSvg ? 'import.svg' : 'import.dxf';
  return { file: new File([bytes], name), isProject };
}

/// Android SAF ADD path — read a `content://` drawing and APPEND it as a
/// new layer (drawings only; projects never ADD).
async function addDrawingFromContentUri(uri: string) {
  const { file } = await contentUriToFile(uri, false);
  await addDrawingFile(file);
}

/// True for an Android SAF document URI returned by the file dialog.
function isContentUri(p: string): boolean {
  return p.startsWith('content://');
}

/// Extension filters for the native open dialog. Desktop gets the real filter
/// set so the picker scopes to the right file types. Android gets an EMPTY
/// filter (all files selectable): its SAF picker maps extension filters to
/// MIME types and silently drops any extension with no registered MIME
/// (.dxf, .ngc, .plt, the multi-dot project names), which otherwise greys
/// those files out / shows "no items". We content-sniff the bytes after
/// picking, so showing everything is safe. Empty array (not omitted) — the
/// dialog plugin's Kotlin `filters` field is non-null and parseArgs throws if
/// it's missing.
function openFilters(
  desktop: { name: string; extensions: string[] }[],
): { name: string; extensions: string[] }[] {
  return isAndroid() ? [] : desktop;
}

/// True when the project already holds something a new drawing could
/// either replace or join — a drawing, text, or ops. Drives the
/// New/Add prompt: with an empty project the question is moot.
function projectHasContent(): boolean {
  return (
    project.data.imports.length > 0 ||
    project.data.textLayers.length > 0 ||
    project.data.operations.length > 0
  );
}

/// The general "Open" entry (File menu / toolbar) is intent-ambiguous for a
/// DRAWING: the user might want a fresh project or to add the drawing to
/// what's already there. When the project has content, ask; otherwise it's
/// unambiguously a fresh open. ('new' replaces, 'add' appends, 'cancel'
/// aborts.) The layer panel's "+ Add" skips this — it always adds.
async function chooseDrawingOpenMode(): Promise<'new' | 'add' | 'cancel'> {
  if (!projectHasContent()) return 'new';
  const choice = await confirmStore.askChoice({
    title: 'Open drawing',
    body: 'Open this drawing as a new project, or add it to the current layers?',
    primaryLabel: 'New project',
    extraLabel: 'Add to layers',
    cancelLabel: 'Cancel',
    danger: false,
  });
  return choice === 'primary' ? 'new' : choice === 'extra' ? 'add' : 'cancel';
}

/// Shared post-pick router for a File opened via the general "Open"
/// (browser hidden input + Android SAF, which both yield a File). Projects
/// always replace; drawings ask New/Add when the project has content. The
/// 'new' branch still runs the unsaved-work guard before discarding.
async function routeOpenedFile(file: File, isProject: boolean): Promise<void> {
  if (isProject) {
    if (!(await confirmDiscardIfDirty('open another file'))) return;
    await loadProjectFile(file);
    return;
  }
  const mode = await chooseDrawingOpenMode();
  if (mode === 'cancel') return;
  if (mode === 'new') {
    if (!(await confirmDiscardIfDirty('open another file'))) return;
    await loadFile(file);
  } else {
    await addDrawingFile(file);
  }
}

/// Browser hidden-input change handler for the unified Open input — called
/// by FileUpload so the New/Add decision lives here, not in the component.
export async function handleOpenPick(file: File): Promise<void> {
  await routeOpenedFile(file, isProjectPath(file.name));
}

export async function openAny() {
  if (isTauri()) {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      // Single-segment extensions only: Android's SAF maps extensions to
      // MIME types and chokes on multi-dot "extensions" like
      // `ivac-project.json` (project files are plain `.json`, covered here).
      const selected = await open({
        multiple: false,
        filters: openFilters([{ name: 'Drawing or project', extensions: ['dxf', 'svg', 'json'] }]),
      });
      if (typeof selected !== 'string') return;
      // Android SAF: sniff bytes to tell project from drawing, then route
      // through the shared File path (replace project / ask New-or-Add).
      if (isContentUri(selected)) {
        const { file, isProject } = await contentUriToFile(selected, true);
        await routeOpenedFile(file, isProject);
        return;
      }
      // Desktop: path-based loaders (import_path), so route by extension.
      if (isProjectPath(selected)) {
        if (!(await confirmDiscardIfDirty('open another file'))) return;
        await loadProjectPath(selected);
        return;
      }
      const mode = await chooseDrawingOpenMode();
      if (mode === 'cancel') return;
      if (mode === 'new') {
        if (!(await confirmDiscardIfDirty('open another file'))) return;
        await loadFromPath(selected);
      } else {
        await addDrawingPath(selected);
      }
    } catch (e) {
      reportError(e);
    }
    return;
  }
  hiddenOpenInput()?.click();
}

/// ADD a drawing to the current project — the layer panel's "+ Add ▸ Open
/// drawing file" and its empty-state CTA. Always additive
/// (project.addImported / addDrawingPath): overlays another drawing as a
/// new layer WITHOUT clearing existing drawings, text, ops or stock, so
/// text-then-drawing and multi-drawing workflows work. No discard prompt —
/// nothing is destroyed. The first drawing into an empty project still
/// auto-places/fits, because addImported routes the first import through
/// setImported. (The File menu's Open is the intent-ambiguous entry that
/// asks New-or-Add; this one's intent is explicit.)
export async function addDrawing() {
  if (isTauri()) {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        multiple: false,
        filters: openFilters([{ name: 'CAD/CAM input', extensions: ['dxf', 'svg'] }]),
      });
      if (typeof selected !== 'string') return;
      if (isContentUri(selected)) await addDrawingFromContentUri(selected);
      else await addDrawingPath(selected);
    } catch (e) {
      reportError(e);
    }
    return;
  }
  hiddenFileInput()?.click();
}

/// If the project has unsaved changes, prompt the user before a
/// destructive load (open file, open project, recent, drag-drop).
/// Returns `true` to proceed with the load, `false` to bail.
///
/// Three-way prompt: Save & continue / Don't save / Cancel — so
/// the user can keep their work without having to cancel, dig out Save,
/// and retry. Picking Save runs `saveProject()` first and only proceeds
/// if the save actually lands (see below).
///
/// Uses `confirmStore` for an in-app styled prompt — the previous
/// `window.confirm` regressed against the Tauri C10 rule (WebKitGTK
/// blocks the renderer and never returns). Exported so every
/// destructive-load entry point (including App.svelte's Recent click)
/// shares ONE confirmation dialog instead of a second native
/// `window.confirm`.
export async function confirmDiscardIfDirty(action: string): Promise<boolean> {
  // hasUnsavedWork (not just `dirty`) so a freshly imported drawing that
  // was never saved as a project still prompts — `dirty` is reset to
  // false right after every load, so it alone misses the "has not been
  // saved" case the user can lose by opening another file.
  if (!project.hasUnsavedWork) return true;
  if (typeof window === 'undefined') return true;
  const choice = await confirmStore.askChoice({
    title: 'Unsaved changes',
    body: `Your project has unsaved changes. Save before you ${action}?`,
    primaryLabel: 'Save & continue',
    extraLabel: "Don't save",
    cancelLabel: 'Cancel',
    danger: false,
    extraDanger: true,
  });
  if (choice === 'cancel') return false;
  if (choice === 'extra') return true; // discard unsaved changes, proceed
  // 'primary' → save first, then proceed only if the save actually
  // landed. saveProject() clears `project.data.dirty` on a successful write;
  // if the user cancels the native save dialog (desktop) or the write
  // errors, dirty stays true — abort the load rather than silently
  // discarding the work the user just asked to keep.
  await saveProject();
  // Proceed only if the save actually landed and cleared the unsaved
  // state. A cancelled native save dialog or a failed write leaves
  // hasUnsavedWork true — abort rather than discard the kept work.
  return !project.hasUnsavedWork;
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
      filters: openFilters([
        {
          name: 'ivaCAM project',
          extensions: ['ivac-project.json', 'vc-project.json', 'json'],
        },
      ]),
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
      project.data.dirty = false;
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
    project.data.dirty = false;
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
    project.data.dirty = false;
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
    project.data.dirty = false;
  } catch (e) {
    reportError(e);
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Browser/wasm ADD path — import a drawing File and APPEND it as a new
/// layer (`project.addImported`) instead of replacing. Mirrors `loadFile`
/// minus the `clearProject`; no dirty reset because adding IS an edit.
/// The first import into an empty project still auto-places (addImported
/// delegates to setImported there).
export async function addDrawingFile(file: File) {
  const client = defaultClient();
  project.loading = true;
  project.loadingMessage = pathToLoadingMessage(file.name);
  project.error = null;
  try {
    const result = await client.importFile(file);
    project.addImported(result);
    await project.convertImportedTextEntities();
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
    project.data.dirty = false;
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
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const { writeTextFile } = await import('@tauri-apps/plugin-fs');
      const path = await save({
        defaultPath: filename,
        filters: [{ name: 'ivaCAM project', extensions: ['json'] }],
      });
      if (typeof path === 'string') {
        await writeTextFile(path, snapshot);
        // The file now matches disk — clear dirty to match the
        // contract every load path upholds. Otherwise the quit-confirm
        // dialog, the confirmDiscardIfDirty prompt on a later Open, and
        // the stale-gcode indicators all fire right after a save.
        project.data.dirty = false;
        // The project now lives in a saved file — clears hasUnsavedWork
        // so a later Open doesn't prompt on a just-saved project.
        project.savedToProject = true;
      }
    } catch (e) {
      project.setError(`save: ${e instanceof Error ? e.message : String(e)}`);
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
  // The browser download IS the save; mirror the desktop branch and
  // clear dirty so the snapshot just written to disk isn't reported unsaved.
  project.data.dirty = false;
  project.savedToProject = true;
}

/// Save a project report as a Markdown file. Desktop = native
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
    project.data.dirty = false;
  } catch (e) {
    project.setError(e instanceof Error ? e.message : String(e));
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Export the current `project.gen.generated.gcode` to disk. Mirrors
/// `saveProject` — native save dialog on Tauri, anchor-tag download in
/// the browser. Filename suffix is .plt for HPGL output, .ngc otherwise.
/// `postProcessor` controls the suffix only; the gcode buffer is
/// already post-processed by the time it lands in `project.gen.generated`.
export async function exportGeneratedGcode(
  postProcessor: 'linuxcnc' | 'grbl' | 'hpgl',
): Promise<void> {
  if (!project.gen.generated) return;
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
        await writeTextFile(path, project.gen.generated.gcode);
      } catch (e) {
        project.setError(`save: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    return;
  }
  const blob = new Blob([project.gen.generated.gcode], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

/// Export the live simulated stock as a binary STL — exactly the
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
  const stock = project.data.stock;
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
// Toolset + machine save/load files.
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
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const { readTextFile } = await import('@tauri-apps/plugin-fs');
      // Flatten to single-segment extensions for Android SAF compatibility.
      const safe = filters.map((f) => ({ ...f, extensions: simpleExtensions(f.extensions) }));
      const selected = await open({ filters: safe, multiple: false });
      if (typeof selected !== 'string') return null;
      return await readTextFile(selected);
    } catch (e) {
      reportError(e);
      return null;
    }
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
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const { writeTextFile } = await import('@tauri-apps/plugin-fs');
      const safe = filters.map((f) => ({ ...f, extensions: simpleExtensions(f.extensions) }));
      const path = await save({ defaultPath: defaultName, filters: safe });
      if (typeof path === 'string') {
        await writeTextFile(path, body);
      }
    } catch (e) {
      reportError(e);
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

/// Export an arbitrary tool list to a `.ivac-toolset.json` file —
/// shared by the project tool set and the shop inventory.
export async function exportToolset(tools: readonly ToolEntry[]) {
  const envelope: SnapshotEnvelope<'toolset', ToolEntry[]> = {
    kind: 'toolset',
    format_version: TOOLSET_FORMAT_VERSION,
    updated_at: new Date().toISOString(),
    payload: tools.map((t) => ({ ...t })),
  };
  await writeJson('toolset.ivac-toolset.json', JSON.stringify(envelope, null, 2), [
    { name: 'ivaCAM toolset', extensions: ['ivac-toolset.json', 'json'] },
  ]);
}

/// Export the current tool library to a `.ivac-toolset.json` file.
/// The user's set of tools, no machine, no project state. Reusable
/// across projects.
export async function saveToolset() {
  await exportToolset(project.data.tools);
}

/// Pick a `.ivac-toolset.json` and merge it into `current`, returning
/// the merged list (or null on cancel / parse failure — errors surface
/// via the toast). Pure of any store: the caller decides whether the
/// result lands in the project tools or the shop inventory.
///   * `'replace'` — the file's tools, re-numbered 1..N
///   * `'add'`     — append; tools whose `name` already exists in
///                   `current` are skipped (existing entries win)
export async function importToolset(
  mode: 'replace' | 'add',
  current: readonly ToolEntry[],
): Promise<ToolEntry[] | null> {
  let text: string | null;
  try {
    text = await pickAndReadJson([
      { name: 'ivaCAM toolset', extensions: ['ivac-toolset.json', 'json'] },
    ]);
  } catch (e) {
    project.setError(`toolset load: ${e instanceof Error ? e.message : String(e)}`);
    return null;
  }
  if (!text) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    project.setError(`toolset parse: ${e instanceof Error ? e.message : String(e)}`);
    return null;
  }
  const env = parsed as SnapshotEnvelope<'toolset', ToolEntry[]>;
  if (
    env == null ||
    typeof env !== 'object' ||
    env.kind !== 'toolset' ||
    !Array.isArray(env.payload)
  ) {
    project.setError('toolset load: not a .ivac-toolset.json file');
    return null;
  }
  const incoming = env.payload.map(migrateLegacyToolTerms);
  if (mode === 'replace') {
    return incoming.map((t, idx) => ({ ...t, id: idx + 1 }));
  }
  const existingNames = new Set(current.map((t) => t.name.toLowerCase()));
  let nextId = current.reduce((m, t) => Math.max(m, t.id), 0);
  const additions: ToolEntry[] = [];
  for (const t of incoming) {
    if (existingNames.has(t.name.toLowerCase())) continue;
    nextId += 1;
    additions.push({ ...t, id: nextId });
  }
  return additions.length > 0 ? [...current, ...additions] : [...current];
}

/// Import a `.ivac-toolset.json`. `mode` controls how the file's
/// tools merge into the current set:
///   * `'replace'` — drop the current tools, use the file's
///   * `'add'`     — append; tools whose `name` already exists are
///                   skipped (the user's existing entries win).
export async function loadToolset(mode: 'replace' | 'add') {
  const merged = await importToolset(mode, project.data.tools);
  if (merged == null) return;
  if (mode === 'add' && merged.length === project.data.tools.length) return;
  if (mode === 'replace') {
    // The cache key folds in every ToolEntry field, so swapping
    // the library mid-session would force a miss-and-recompute on every
    // op anyway. Clear up front so the old entries stop occupying LRU
    // slots that will never hit again.
    await clearPipelineCacheOnReplace();
  }
  project.replaceTools(merged);
}

/// Export the current machine config to a `.ivac-machine.json` file.
export async function saveMachine() {
  const envelope: SnapshotEnvelope<'machine', MachineSettings> = {
    kind: 'machine',
    format_version: MACHINE_FORMAT_VERSION,
    updated_at: new Date().toISOString(),
    payload: { ...project.data.machine },
  };
  const fileBase = (project.data.machine.name && project.data.machine.name.trim()) || 'machine';
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
  // Machine swap invalidates the cache the same way as a tool
  // library swap — hash_machine folds every relevant field, so the
  // post-swap Generate will miss-and-recompute. Drop the old entries
  // proactively.
  await clearPipelineCacheOnReplace();
  // An older .ivac-machine.json carries `supportsToolchange` instead
  // of `toolchangeStrategy` — migrate before applying.
  project.setMachine(migrateMachineSettings(env.payload));
}
