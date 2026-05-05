<script lang="ts">
  import FileUpload from './lib/components/FileUpload.svelte';
  import EntityCanvas2D from './lib/components/EntityCanvas2D.svelte';
  import Scene3D from './lib/components/Scene3D.svelte';
  import LayerList from './lib/components/LayerList.svelte';
  import SetupPanel from './lib/components/SetupPanel.svelte';
  import GenerateBar from './lib/components/GenerateBar.svelte';
  import PlaybackBar from './lib/components/PlaybackBar.svelte';
  import { project } from './lib/state/project.svelte';

  let activePane = $state<'2d' | '3d'>('2d');

  // Auto-switch to 3D when /generate returns; people want to see the toolpath.
  $effect(() => {
    if (project.generated) activePane = '3d';
  });

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === 'Escape' && project.selectedEntities.size > 0) {
      project.selectedEntities = new Set();
    }
  }
</script>

<svelte:window onkeydown={onKeyDown} />

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
      <div class="canvas-area">
        {#if activePane === '2d'}
          <EntityCanvas2D />
        {:else}
          <Scene3D />
        {/if}
      </div>
      {#if activePane === '3d' && project.generated}
        <PlaybackBar />
      {/if}
    </section>
    <aside class="sidebar">
      <div class="layers-host">
        <LayerList />
      </div>
      <div class="setup-host">
        <SetupPanel />
      </div>
    </aside>
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
    background: var(--bg-panel);
    border-bottom: 1px solid var(--border);
  }
  h1 {
    font-size: 1rem;
    margin: 0;
    color: var(--text-strong);
    font-weight: 600;
  }
  .tagline {
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .spacer {
    flex: 1;
  }
  .pane-toggle {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 4px;
    overflow: hidden;
  }
  .pane-toggle button {
    background: var(--bg-elevated);
    color: var(--text-muted);
    border: 0;
    padding: 0.3rem 0.7rem;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .pane-toggle button.active {
    background: var(--accent);
    color: white;
  }
  main {
    display: grid;
    grid-template-columns: 1fr 320px;
    overflow: hidden;
    min-height: 0;
  }
  .viewport {
    position: relative;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  .canvas-area {
    flex: 1;
    min-height: 0;
    position: relative;
  }
  .sidebar {
    display: grid;
    grid-template-rows: 200px 1fr;
    min-height: 0;
    overflow: hidden;
  }
  .layers-host,
  .setup-host {
    min-height: 0;
    overflow: hidden;
  }
  footer {
    background: var(--bg-panel);
    border-top: 1px solid var(--border);
    padding: 0.35rem 0.9rem;
    font-size: 0.75rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
</style>
