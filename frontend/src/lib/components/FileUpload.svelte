<script lang="ts">
  import { onMount } from 'svelte';
  import { defaultClient } from '../api/http';
  import { project } from '../state/project.svelte';
  import type { ImportResponse } from '../api/types';

  const client = defaultClient();
  let dragOver = $state(false);
  let inputEl: HTMLInputElement;

  // Same-origin samples for the demo. Bypasses cross-origin HSTS issues
  // when the frontend is opened from a host that has cached HSTS for the
  // server's IP. Drop into public/samples/ as JSON dumps of /import.
  const SAMPLES = [
    { label: 'simple (py)', url: '/samples/simple.json' },
    { label: 'simple (rs)', url: '/samples/simple-rust.json' },
    { label: 'all (py)', url: '/samples/all.json' },
    { label: 'all (rs)', url: '/samples/all-rust.json' },
  ];

  function saveProject() {
    const blob = new Blob([JSON.stringify(project.snapshot(), null, 2)], {
      type: 'application/json',
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    const base = project.imported?.filename?.replace(/\.[^.]+$/, '') ?? 'project';
    a.href = url;
    a.download = `${base}.vc-project.json`;
    a.click();
    URL.revokeObjectURL(url);
  }

  async function loadProjectFile(file: File) {
    project.loading = true;
    project.error = null;
    try {
      const text = await file.text();
      const data = JSON.parse(text);
      project.restore(data);
    } catch (e) {
      project.setError(`load project: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      project.loading = false;
    }
  }

  let projectInput: HTMLInputElement;
  function onProjectPick(e: Event) {
    const target = e.target as HTMLInputElement;
    if (target.files?.[0]) loadProjectFile(target.files[0]);
  }

  async function load(file: File) {
    project.loading = true;
    project.error = null;
    try {
      const result = await client.importFile(file);
      project.setImported(result);
    } catch (e) {
      project.setError(e instanceof Error ? e.message : String(e));
    } finally {
      project.loading = false;
    }
  }

  async function loadSample(url: string) {
    project.loading = true;
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
    if (file.name.endsWith('.vc-project.json') || file.name.endsWith('.json')) {
      loadProjectFile(file);
    } else {
      load(file);
    }
  }

  async function loadSampleWithGenerate(sampleUrl: string, generatedUrl: string) {
    project.loading = true;
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
    }
  }

  onMount(() => {
    const params = new URLSearchParams(window.location.search);
    const sample = params.get('sample');
    const gen = params.get('gen');
    if (sample && gen) {
      loadSampleWithGenerate(`/samples/${sample}.json`, `/samples/${gen}.json`);
    } else if (sample) {
      loadSample(`/samples/${sample}.json`);
    }
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
  <input
    bind:this={inputEl}
    type="file"
    accept=".dxf,.svg,.hpgl,.plt,.ngc,.stl"
    onchange={onPick}
    hidden
  />
  <input
    bind:this={projectInput}
    type="file"
    accept=".vc-project.json,.json"
    onchange={onProjectPick}
    hidden
  />
  <button type="button" onclick={() => inputEl.click()} disabled={project.loading}>
    {project.loading ? 'Loading…' : 'Open file'}
  </button>
  <button
    type="button"
    class="secondary"
    onclick={() => projectInput.click()}
    disabled={project.loading}
    title="Open a saved project (.vc-project.json)"
  >
    Open project
  </button>
  <button
    type="button"
    class="secondary"
    onclick={saveProject}
    disabled={!project.imported}
    title="Save current geometry + setup to a .vc-project.json file"
  >
    Save project
  </button>
  <span class="hint">or drop a .dxf / .svg / .hpgl / .ngc / .stl here</span>
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
    <span class="error">{project.error}</span>
  {/if}
</div>

<style>
  .upload {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text);
    flex-wrap: wrap;
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
  .loaded {
    font-size: 0.8rem;
    color: var(--success);
  }
  .error {
    font-size: 0.8rem;
    color: var(--error);
  }
</style>
