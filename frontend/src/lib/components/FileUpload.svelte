<script lang="ts">
  import { onMount } from 'svelte';
  import { defaultClient } from '../api/http';
  import { tryParseStructuredError } from '../api/client';
  import { isTauri } from '../api/env';
  import { project } from '../state/project.svelte';
  import { workspace } from '../state/workspace.svelte';
  import type { ImportResponse } from '../api/types';
  import { pushRecent, readRecent, type RecentEntry } from '../recent';
  import ErrorToast from './ErrorToast.svelte';

  /// Bridge: catch handlers across this file may receive a stringified
  /// structured error from Tauri/WASM. Delegate the parse so the toast
  /// can render it in full fidelity instead of as raw JSON text.
  function reportError(input: unknown) {
    const raw = input instanceof Error ? input.message : String(input);
    const structured = tryParseStructuredError(raw);
    project.setError(structured ?? raw);
  }

  const client = defaultClient();
  let dragOver = $state(false);
  let inputEl: HTMLInputElement;
  let recent = $state<RecentEntry[]>([]);

  async function refreshRecent() {
    recent = await readRecent();
  }
  async function recordRecent(path: string, filename: string) {
    recent = await pushRecent({ path, filename, lastOpened: new Date().toISOString() });
  }

  // Same-origin samples for the demo. Bypasses cross-origin HSTS issues
  // when the frontend is opened from a host that has cached HSTS for the
  // server's IP. Drop into public/samples/ as JSON dumps of /import.
  const SAMPLES = [
    { label: 'simple (py)', url: '/samples/simple.json' },
    { label: 'simple (rs)', url: '/samples/simple-rust.json' },
    { label: 'all (py)', url: '/samples/all.json' },
    { label: 'all (rs)', url: '/samples/all-rust.json' },
  ];

  async function saveProject() {
    const snapshot = JSON.stringify(project.snapshot(), null, 2);
    const base = project.imported?.filename?.replace(/\.[^.]+$/, '') ?? 'project';
    // New saves use .wiac-project.json. Loads still accept the legacy
    // .vc-project.json (inherited from viaConstructor, the Python
    // predecessor) for backwards compat.
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

  async function loadProjectFile(file: File) {
    project.loading = true;
    project.loadingMessage = 'Loading project…';
    project.error = null;
    try {
      const text = await file.text();
      const data = JSON.parse(text);
      project.restore(data);
    } catch (e) {
      project.setError(`load project: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      project.loading = false;
      project.loadingMessage = null;
    }
  }

  let projectInput: HTMLInputElement;
  function onProjectPick(e: Event) {
    const target = e.target as HTMLInputElement;
    if (target.files?.[0]) loadProjectFile(target.files[0]);
  }

  /**
   * Native open dialog for desktop. Routes through tauri-plugin-dialog so
   * the user gets the OS picker and we receive an absolute path back —
   * which the Rust import_path command can use directly without the
   * write-to-temp dance the web client needs.
   */
  async function openFileNative() {
    const { open } = await import('@tauri-apps/plugin-dialog');
    const selected = await open({
      multiple: false,
      // Only formats the wiac-core importer actually supports today
      // (input.rs dispatch lists DXF + SVG). HPGL / PLT / NGC / STL
      // are tracked as separate follow-up issues (byd, rt1.32). When
      // those land, extend this filter so the picker stays honest.
      filters: [{ name: 'CAD/CAM input', extensions: ['dxf', 'svg'] }],
    });
    if (typeof selected !== 'string') return;
    await loadFromPath(selected);
  }

  async function loadFromPath(path: string) {
    project.loading = true;
    project.loadingMessage = pathToLoadingMessage(path);
    project.error = null;
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      const result = await invoke<ImportResponse>('import_path', { path });
      project.setImported(result, path);
      const filename = path.split(/[\\/]/).pop() ?? path;
      await recordRecent(path, filename);
      workspace.addRecentProject(path, filename);
      project.setActiveProjectPath(path);
    } catch (e) {
      reportError(e);
    } finally {
      project.loading = false;
      project.loadingMessage = null;
    }
  }

  function pathToLoadingMessage(path: string): string {
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

  async function openProjectNative() {
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
      await recordRecent(selected, filename);
      workspace.addRecentProject(selected, filename);
      project.setActiveProjectPath(selected);
    } catch (e) {
      project.setError(`load project: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      project.loading = false;
      project.loadingMessage = null;
    }
  }

  async function load(file: File) {
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

  async function loadSample(url: string) {
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

  function onPick(e: Event) {
    const target = e.target as HTMLInputElement;
    if (target.files?.[0]) load(target.files[0]);
  }

  function onDrop(e: DragEvent) {
    e.preventDefault();
    dragOver = false;
    const file = e.dataTransfer?.files[0];
    if (!file) return;
    if (
      file.name.endsWith('.wiac-project.json') ||
      file.name.endsWith('.vc-project.json') ||
      file.name.endsWith('.json')
    ) {
      loadProjectFile(file);
    } else {
      load(file);
    }
  }

  async function loadSampleWithGenerate(sampleUrl: string, generatedUrl: string) {
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

  onMount(async () => {
    void refreshRecent();
    if (isTauri()) {
      const { listen } = await import('@tauri-apps/api/event');
      // OS file-association launches: main.rs forwards the path here.
      void listen<string>('app:open_path', (event) => {
        if (typeof event.payload === 'string') void loadFromPath(event.payload);
      });
    }
    const params = new URLSearchParams(window.location.search);
    const sample = params.get('sample');
    const gen = params.get('gen');
    if (sample && gen) {
      await loadSampleWithGenerate(`/samples/${sample}.json`, `/samples/${gen}.json`);
    } else if (sample) {
      await loadSample(`/samples/${sample}.json`);
    }
    // (legacy ?tabs= sample loader dropped with rt1.10 — tabs are per-op now)
  });
</script>

<div
  class="upload"
  class:drag-over={dragOver}
  ondragover={(e) => {
    e.preventDefault();
    dragOver = true;
  }}
  ondragleave={() => (dragOver = false)}
  ondrop={onDrop}
  role="region"
  aria-label="File upload"
>
  <input bind:this={inputEl} type="file" accept=".dxf,.svg" onchange={onPick} hidden />
  <input
    bind:this={projectInput}
    type="file"
    accept=".wiac-project.json,.vc-project.json,.json"
    onchange={onProjectPick}
    hidden
  />
  <button
    type="button"
    class="open-file"
    onclick={() => (isTauri() ? openFileNative() : inputEl.click())}
    disabled={project.loading}
  >
    {project.loading ? 'Loading…' : 'Open file'}
  </button>
  <button
    type="button"
    class="secondary open-project"
    onclick={() => (isTauri() ? openProjectNative() : projectInput.click())}
    disabled={project.loading}
    title="Open a saved project (.wiac-project.json or legacy .vc-project.json)"
  >
    Open project
  </button>
  <button
    type="button"
    class="secondary save-project"
    onclick={saveProject}
    disabled={!project.imported}
    title="Save current geometry + setup to a .wiac-project.json file"
  >
    Save project
  </button>
  <span class="hint">or drop a .dxf or .svg here</span>
  {#if isTauri() && recent.length > 0}
    <span class="recent-host">
      <select
        class="recent"
        title="Recent files"
        onchange={(e) => {
          const path = (e.currentTarget as HTMLSelectElement).value;
          (e.currentTarget as HTMLSelectElement).value = '';
          if (path) void loadFromPath(path);
        }}
      >
        <option value="">recent…</option>
        {#each recent as r (r.path)}
          <option value={r.path} title={r.path}>{r.filename}</option>
        {/each}
      </select>
    </span>
  {/if}
  <span class="samples">
    samples:
    {#each SAMPLES as s (s.url)}
      <button type="button" class="sample" onclick={() => loadSample(s.url)}>
        {s.label}
      </button>
    {/each}
  </span>
  {#if project.imported}
    <span class="loaded">
      Loaded {project.imported.filename} — {project.imported.segments.length} segments,
      {project.imported.layers.length} layers
    </span>
  {/if}
  {#if project.error}
    <ErrorToast error={project.error} />
  {/if}
</div>

<style>
  .upload {
    display: flex;
    align-items: center;
    gap: 0.5rem 0.75rem;
    padding: 0.4rem 0.75rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text);
    flex-wrap: wrap;
  }
  .loaded {
    flex: 1;
    min-width: 12rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .upload.drag-over {
    background: color-mix(in srgb, var(--accent) 24%, var(--bg-elevated));
  }
  button {
    background: var(--accent);
    color: white;
    border: none;
    padding: 0.4rem 0.9rem;
    border-radius: 4px;
    font-size: 0.85rem;
    cursor: pointer;
  }
  button.secondary {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
  }
  button.secondary:hover {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  button:disabled {
    opacity: 0.6;
    cursor: progress;
  }
  .hint {
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .samples {
    font-size: 0.75rem;
    color: var(--text-muted);
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
  }
  .samples .sample {
    background: transparent;
    color: var(--accent-strong);
    border: 1px solid var(--border);
    padding: 0.15rem 0.45rem;
    border-radius: 3px;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .samples .sample:hover {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
  }
  .recent-host {
    display: inline-flex;
  }
  .recent {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.4rem;
    font-size: 0.78rem;
    max-width: 14rem;
  }
  .loaded {
    font-size: 0.8rem;
    color: var(--success);
  }
</style>
