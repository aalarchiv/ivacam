/// Open / Save / Sample I/O extracted from FileUpload so the toolbar
/// (and any other UI surface) can invoke the same flows without
/// reaching through DOM query selectors. Each function mutates
/// `project` directly — loading flag, error toast, the
/// imported / generated payload, and the active project path / recent
/// list.

import { project } from './project.svelte';
import { workspace } from './workspace.svelte';
import { isTauri } from '../api/env';
import { defaultClient } from '../api/http';
import { tryParseStructuredError } from '../api/client';
import { pushRecent } from '../recent';
import type { ImportResponse } from '../api/types';

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
export const SAMPLES: { label: string; url: string }[] = [
  { label: 'simple (py)', url: '/samples/simple.json' },
  { label: 'simple (rs)', url: '/samples/simple-rust.json' },
  { label: 'all (py)', url: '/samples/all.json' },
  { label: 'all (rs)', url: '/samples/all-rust.json' },
];

/// FileUpload.svelte stashes its hidden `<input type=file>` elements
/// on `window` so the browser fallbacks can find them. This is a no-op
/// in Tauri where the native picker covers everything.
function hiddenFileInput(): HTMLInputElement | null {
  return (window as unknown as { __wiacFileInput?: HTMLInputElement }).__wiacFileInput ?? null;
}
function hiddenProjectInput(): HTMLInputElement | null {
  return (
    (window as unknown as { __wiacProjectInput?: HTMLInputElement }).__wiacProjectInput ?? null
  );
}

/// Desktop: native open dialog. Browser: programmatically click the
/// hidden `<input type=file>` rendered by FileUpload so the picker
/// fires inside the user-gesture window.
export async function openFile() {
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

/// Desktop: native open dialog for `.wiac-project.json`. Browser: same
/// hidden-input trick as openFile, but for project files.
export async function openProject() {
  if (isTauri()) {
    const { open } = await import('@tauri-apps/plugin-dialog');
    const { readTextFile } = await import('@tauri-apps/plugin-fs');
    const selected = await open({
      multiple: false,
      filters: [
        {
          name: 'wiaConstructor project',
          extensions: ['wiac-project.json', 'vc-project.json', 'json'],
        },
      ],
    });
    if (typeof selected !== 'string') return;
    project.loading = true;
    project.loadingMessage = 'Loading project…';
    project.error = null;
    try {
      const text = await readTextFile(selected);
      project.restore(JSON.parse(text));
      const filename = selected.split(/[\\/]/).pop() ?? selected;
      await pushRecent({ path: selected, filename, lastOpened: new Date().toISOString() });
      workspace.addRecentProject(selected, filename);
      project.setActiveProjectPath(selected);
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
export async function loadFromPath(path: string) {
  project.loading = true;
  project.loadingMessage = pathToLoadingMessage(path);
  project.error = null;
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    const result = await invoke<ImportResponse>('import_path', { path });
    project.setImported(result, path);
    const filename = path.split(/[\\/]/).pop() ?? path;
    await pushRecent({ path, filename, lastOpened: new Date().toISOString() });
    workspace.addRecentProject(path, filename);
    project.setActiveProjectPath(path);
  } catch (e) {
    reportError(e);
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Tauri-only path-based project load — same flow as `loadFromPath`
/// but dispatches to the project-restore path.
export async function loadProjectPath(path: string) {
  project.loading = true;
  project.loadingMessage = 'Loading project…';
  project.error = null;
  try {
    const { readTextFile } = await import('@tauri-apps/plugin-fs');
    const text = await readTextFile(path);
    project.restore(JSON.parse(text));
    const filename = path.split(/[\\/]/).pop() ?? path;
    await pushRecent({ path, filename, lastOpened: new Date().toISOString() });
    workspace.addRecentProject(path, filename);
    project.setActiveProjectPath(path);
  } catch (e) {
    project.setError(`load project: ${e instanceof Error ? e.message : String(e)}`);
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Browser-path import via the FastAPI-shaped /import endpoint (or its
/// WASM equivalent in Tauri). Used by drag-and-drop + the hidden
/// `<input type=file>` change handler.
export async function loadFile(file: File) {
  const client = defaultClient();
  project.loading = true;
  project.loadingMessage = pathToLoadingMessage(file.name);
  project.error = null;
  try {
    const result = await client.importFile(file);
    project.setImported(result);
  } catch (e) {
    reportError(e);
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
}

/// Browser project load — paired with the hidden project input.
export async function loadProjectFile(file: File) {
  project.loading = true;
  project.loadingMessage = 'Loading project…';
  project.error = null;
  try {
    const text = await file.text();
    project.restore(JSON.parse(text));
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
  const base = project.imported?.filename?.replace(/\.[^.]+$/, '') ?? 'project';
  const filename = `${base}.wiac-project.json`;
  if (isTauri()) {
    const { save } = await import('@tauri-apps/plugin-dialog');
    const { writeTextFile } = await import('@tauri-apps/plugin-fs');
    const path = await save({
      defaultPath: filename,
      filters: [
        {
          name: 'wiaConstructor project',
          extensions: ['wiac-project.json', 'vc-project.json', 'json'],
        },
      ],
    });
    if (typeof path === 'string') {
      try {
        await writeTextFile(path, snapshot);
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
}

/// Fetch + import one of the bundled `public/samples/<x>.json` files.
export async function loadSample(url: string) {
  project.loading = true;
  project.loadingMessage = 'Loading sample…';
  project.error = null;
  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`fetch ${url}: ${res.status}`);
    const data = (await res.json()) as ImportResponse;
    project.setImported(data);
  } catch (e) {
    project.setError(e instanceof Error ? e.message : String(e));
  } finally {
    project.loading = false;
    project.loadingMessage = null;
  }
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

/// Decide whether a dropped/picked file is a project vs. raw geometry
/// by extension, then dispatch.
export function importDroppedFile(file: File) {
  if (
    file.name.endsWith('.wiac-project.json') ||
    file.name.endsWith('.vc-project.json') ||
    file.name.endsWith('.json')
  ) {
    return loadProjectFile(file);
  }
  return loadFile(file);
}
