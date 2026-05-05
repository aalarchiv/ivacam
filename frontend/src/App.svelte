<script lang="ts">
  import FileUpload from './lib/components/FileUpload.svelte';
  import EntityCanvas2D from './lib/components/EntityCanvas2D.svelte';
  import Scene3D from './lib/components/Scene3D.svelte';
  import LayerList from './lib/components/LayerList.svelte';
  import GenerateBar from './lib/components/GenerateBar.svelte';
  import { project } from './lib/state/project.svelte';

  let activePane = $state<'2d' | '3d'>('2d');

  // Auto-switch to 3D when /generate returns; people want to see the toolpath.
  $effect(() => {
    if (project.generated) activePane = '3d';
  });
</script>

<div class="app">
  <header>
    <h1>wiaConstructor</h1>
    <span class="tagline">DXF / SVG → G-code · Stage-1 web preview</span>
    <div class="spacer"></div>
    <div class="pane-toggle">
      <button
        class:active={activePane === '2d'}
        onclick={() => (activePane = '2d')}
      >
        2D
      </button>
      <button
        class:active={activePane === '3d'}
        onclick={() => (activePane = '3d')}
      >
        3D
      </button>
    </div>
  </header>

  <FileUpload />
  <GenerateBar />

  <main>
    <section class="viewport">
      {#if activePane === '2d'}
        <EntityCanvas2D />
      {:else}
        <Scene3D />
      {/if}
    </section>
    <LayerList />
  </main>

  <footer>
    {#if project.imported}
      bbox=({project.imported.bbox.min_x.toFixed(2)},
      {project.imported.bbox.min_y.toFixed(2)})–({project.imported.bbox.max_x.toFixed(2)},
      {project.imported.bbox.max_y.toFixed(2)}) · {project.imported.segments.length} segments ·
      unit_scale={project.imported.unit_scale}
    {:else}
      Ready
    {/if}
  </footer>
</div>

<style>
  :global(html),
  :global(body) {
    height: 100%;
    margin: 0;
    background: #0d0d0d;
    color: #d6d6d6;
    font-family:
      system-ui,
      -apple-system,
      Segoe UI,
      Roboto,
      sans-serif;
  }
  :global(#app) {
    height: 100%;
  }
  .app {
    display: grid;
    grid-template-rows: auto auto auto 1fr auto;
    height: 100vh;
    width: 100vw;
  }
  header {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.5rem 0.9rem;
    background: #161616;
    border-bottom: 1px solid #2b2b2b;
  }
  h1 {
    font-size: 1rem;
    margin: 0;
    color: #f0f0f0;
    font-weight: 600;
  }
  .tagline {
    font-size: 0.75rem;
    color: #888;
  }
  .spacer {
    flex: 1;
  }
  .pane-toggle {
    display: inline-flex;
    border: 1px solid #2b2b2b;
    border-radius: 4px;
    overflow: hidden;
  }
  .pane-toggle button {
    background: #1a1a1a;
    color: #aaa;
    border: 0;
    padding: 0.3rem 0.7rem;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .pane-toggle button.active {
    background: #2d6cdf;
    color: white;
  }
  main {
    display: grid;
    grid-template-columns: 1fr 240px;
    overflow: hidden;
    min-height: 0;
  }
  .viewport {
    position: relative;
    overflow: hidden;
  }
  footer {
    background: #161616;
    border-top: 1px solid #2b2b2b;
    padding: 0.35rem 0.9rem;
    font-size: 0.75rem;
    color: #888;
    font-variant-numeric: tabular-nums;
  }
</style>
