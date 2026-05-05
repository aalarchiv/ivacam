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
    if (file) load(file);
  }

  onMount(() => {
    const params = new URLSearchParams(window.location.search);
    const sample = params.get('sample');
    if (sample) loadSample(`/samples/${sample}.json`);
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
  <button type="button" onclick={() => inputEl.click()} disabled={project.loading}>
    {project.loading ? 'Loading…' : 'Open file'}
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
    border-bottom: 1px solid #2b2b2b;
    background: #1a1a1a;
    color: #e6e6e6;
    flex-wrap: wrap;
  }
  .upload.drag-over {
    background: #243049;
  }
  button {
    background: #2d6cdf;
    color: white;
    border: none;
    padding: 0.4rem 0.9rem;
    border-radius: 4px;
    font-size: 0.85rem;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.6;
    cursor: progress;
  }
  .hint {
    font-size: 0.8rem;
    color: #888;
  }
  .samples {
    font-size: 0.75rem;
    color: #888;
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
  }
  .samples .sample {
    background: transparent;
    color: #6e9ce6;
    border: 1px solid #2b2b2b;
    padding: 0.15rem 0.45rem;
    border-radius: 3px;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .samples .sample:hover {
    background: #1f2c44;
  }
  .loaded {
    font-size: 0.8rem;
    color: #6ec068;
  }
  .error {
    font-size: 0.8rem;
    color: #df6c6c;
  }
</style>
