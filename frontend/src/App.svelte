<script lang="ts">
  import FileUpload from './lib/components/FileUpload.svelte';
  import EntityCanvas2D from './lib/components/EntityCanvas2D.svelte';
  // Scene3D pulls in the entire three.js graph (~600 KB pre-min) — keep
  // it out of the initial bundle by dynamic-importing on first 3D switch.
  type Scene3DComp = typeof import('./lib/components/Scene3D.svelte').default;
  let Scene3D = $state<Scene3DComp | null>(null);
  let scene3dLoading = $state(false);
  import LayerList from './lib/components/LayerList.svelte';
  import OperationsList from './lib/components/OperationsList.svelte';
  import StockPanel from './lib/components/StockPanel.svelte';
  import GenerateBar from './lib/components/GenerateBar.svelte';
  import PlaybackBar from './lib/components/PlaybackBar.svelte';
  import GcodePanel from './lib/components/GcodePanel.svelte';
  import MachineDialog from './lib/components/MachineDialog.svelte';
  import ToolLibraryDialog from './lib/components/ToolLibraryDialog.svelte';
  import SettingsDialog from './lib/components/SettingsDialog.svelte';
  import AddTextDialog from './lib/components/AddTextDialog.svelte';

  let machineOpen = $state(false);
  let toolsOpen = $state(false);
  let settingsOpen = $state(false);
  let addTextOpen = $state(false);

  // G-code panel visibility. The playback bar always sits below the
  // 3D canvas; the gcode panel opens as an extra row beneath it so
  // the user sees the toolpath, the playhead, and the program text
  // simultaneously and can drive each from the others.
  let gcodeOpen = $state(false);
  import { project } from './lib/state/project.svelte';
  import { onMount } from 'svelte';
  import { _ } from 'svelte-i18n';
  import { setLocale, locale } from './lib/i18n';
  import { isTauri } from './lib/api/env';

  // Keep the i18n locale in sync with the persisted setting on first
  // load. Subsequent changes go through SettingsDialog which calls
  // setLocale itself.
  $effect(() => {
    const cur = $locale;
    if ((cur === 'en' || cur === 'de') && cur !== project.settings.language) {
      project.updateSettings({ language: cur });
    }
  });

  onMount(() => {
    document.documentElement.dataset.theme = project.settings.theme;

    if (isTauri()) {
      void wireMenuEvents();
    }
  });

  /**
   * Bridge native menu actions emitted from crates/wiac-tauri/src/menu.rs.
   * Each menu item's id (e.g. 'file:open', 'view:2d') maps to a UI action
   * that already exists elsewhere in the app — we mostly dispatch synthetic
   * clicks against the visible buttons so behavior stays in one place.
   */
  async function wireMenuEvents() {
    const { listen } = await import('@tauri-apps/api/event');
    await listen<string>('app:menu', (event) => {
      const id = event.payload;
      switch (id) {
        case 'file:open':
          (document.querySelector('button.open-file') as HTMLButtonElement | null)?.click();
          break;
        case 'file:open_project':
          (document.querySelector('button.open-project') as HTMLButtonElement | null)?.click();
          break;
        case 'file:save_project':
          (document.querySelector('button.save-project') as HTMLButtonElement | null)?.click();
          break;
        case 'file:export_gcode':
          (document.querySelector('button.download') as HTMLButtonElement | null)?.click();
          break;
        case 'view:2d':
          activePane = '2d';
          break;
        case 'view:3d':
          activePane = '3d';
          break;
        case 'view:toggle_tabs':
          if (project.imported) project.tabMode = !project.tabMode;
          break;
        case 'help:check_update':
          void runUpdateCheck();
          break;
      }
    });
  }

  /**
   * Manual auto-update check. Pulls the latest manifest from the configured
   * endpoint, prompts the user via the plugin's built-in dialog, downloads
   * + installs + relaunches if accepted. Failures surface as a toast in
   * project.error so they don't crash silently.
   */
  async function runUpdateCheck() {
    try {
      const { check } = await import('@tauri-apps/plugin-updater');
      const update = await check();
      if (!update) {
        return;
      }
      // The plugin has a built-in dialog when configured in tauri.conf.json,
      // so we just trigger downloadAndInstall on confirmation.
      await update.downloadAndInstall();
      const { relaunch } = await import('@tauri-apps/plugin-process');
      await relaunch();
    } catch (e) {
      project.setError(`update: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  // Reactively apply the current theme. Persistence is handled inside
  // project.updateSettings() so we just mirror the current value into
  // the document dataset for the CSS [data-theme] selectors.
  $effect(() => {
    document.documentElement.dataset.theme = project.settings.theme;
  });

  let activePane = $state<'2d' | '3d'>('2d');

  // Auto-switch to 3D when /generate returns; people want to see the toolpath.
  $effect(() => {
    if (project.generated) activePane = '3d';
  });

  // Pull Scene3D in on first activation. The dynamic import becomes its
  // own Vite chunk, so the initial main chunk doesn't carry three.js.
  $effect(() => {
    if (activePane === '3d' && !Scene3D && !scene3dLoading) {
      scene3dLoading = true;
      void import('./lib/components/Scene3D.svelte').then((m) => {
        Scene3D = m.default;
        scene3dLoading = false;
      });
    }
  });

  const tabCount = $derived(
    Object.values(project.tabs).reduce((n, list) => n + list.length, 0),
  );

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      if (project.tabMode) project.tabMode = false;
      else if (project.selectedEntities.size > 0) project.selectedEntities = new Set();
      return;
    }
    // Keyboard shortcut: T = Add Text. Skip when typing in any text input
    // and skip when modifier keys are held so it doesn't shadow Ctrl-T etc.
    if ((e.key === 't' || e.key === 'T') && !e.ctrlKey && !e.metaKey && !e.altKey) {
      const t = e.target as HTMLElement | null;
      const tag = t?.tagName ?? '';
      const editable = (t as HTMLElement | null)?.isContentEditable;
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || editable) return;
      addTextOpen = true;
      e.preventDefault();
    }
  }
</script>

<svelte:window onkeydown={onKeyDown} />

<div class="app">
  <header>
    <h1>{$_('app.title')}</h1>
    <span class="tagline">{$_('app.tagline')}</span>
    <div class="spacer"></div>
    <button
      class="config-btn icon"
      onclick={() => (addTextOpen = true)}
      title="Add Text (T)"
      aria-label="Add Text"
    >
      <svg
        viewBox="0 0 24 24"
        width="14"
        height="14"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="M12 19l7-7 3 3-7 7-3-3z"></path>
        <path d="M18 13l-1.5-7.5L2 2l3.5 14.5L13 18l5-5z"></path>
        <path d="M2 2l7.586 7.586"></path>
        <circle cx="11" cy="11" r="2"></circle>
      </svg>
      <span>Text</span>
    </button>
    <button class="config-btn" onclick={() => (toolsOpen = true)} title="Tool library">
      Tools…
    </button>
    <button class="config-btn" onclick={() => (machineOpen = true)} title="Machine settings">
      Machine…
    </button>
    <button
      class="config-btn icon"
      onclick={() => (settingsOpen = true)}
      title="Settings"
      aria-label="Settings"
    >
      <svg
        viewBox="0 0 24 24"
        width="14"
        height="14"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <circle cx="12" cy="12" r="3"></circle>
        <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"></path>
      </svg>
    </button>
    <button
      class="tool-toggle"
      class:active={project.tabMode}
      onclick={() => (project.tabMode = !project.tabMode)}
      disabled={!project.imported}
      title={$_('header.tabs_hint')}
    >
      {$_('header.tabs', { values: { count: tabCount } })}
    </button>
    <div class="pane-toggle">
      <button
        class:active={activePane === '2d'}
        onclick={() => (activePane = '2d')}
      >
        {$_('header.pane.2d')}
      </button>
      <button
        class:active={activePane === '3d'}
        onclick={() => (activePane = '3d')}
      >
        {$_('header.pane.3d')}
      </button>
    </div>
  </header>

  <FileUpload />
  <GenerateBar />

  <main>
    <section class="viewport">
      <div class="canvas-area">
        <!-- Keep both panes mounted so the 3D camera angle and the
             heightfield mesh state survive 2D ↔ 3D toggles. The hidden
             pane has display:none, so its IntersectionObserver reports
             non-intersecting and Scene3D's RAF pauses (see 9js). The
             user only pays the Scene3D mount cost once, on first 3D
             activation. -->
        <div class:pane-hidden={activePane !== '2d'} class="pane">
          <EntityCanvas2D />
        </div>
        {#if Scene3D}
          {@const C = Scene3D}
          <div class:pane-hidden={activePane !== '3d'} class="pane">
            <C />
          </div>
        {:else if activePane === '3d'}
          <p class="loading-3d">Loading 3D…</p>
        {/if}
      </div>
      {#if project.generated}
        <PlaybackBar />
        <div class="gcode-toggle">
          <button
            class:active={gcodeOpen}
            onclick={() => (gcodeOpen = !gcodeOpen)}
            title="Show / hide the gcode text panel. Click a line to scrub the playhead; the playhead's current line scrolls into view."
          >
            {gcodeOpen ? '▼' : '▶'} {$_('bottom.gcode') ?? 'G-code'}
            <span class="hint">{project.generated.gcode.split('\n').length} lines</span>
          </button>
        </div>
        {#if gcodeOpen}
          <div class="gcode-row">
            <GcodePanel />
          </div>
        {/if}
      {/if}
    </section>
    <aside class="sidebar">
      <div class="layers-host">
        <LayerList />
      </div>
      <div class="ops-host">
        <OperationsList />
      </div>
      <div class="stock-host">
        <details>
          <summary>Stock</summary>
          <StockPanel />
        </details>
        {#if project.generated && project.generated.regions && project.generated.regions.length > 0}
          <label class="region-toggle" title="Show / hide the translucent fill that marks each pocket op's machined region.">
            <input
              type="checkbox"
              checked={project.regionsVisible}
              onchange={(e) => (project.regionsVisible = (e.currentTarget as HTMLInputElement).checked)}
            />
            <span>Show machined regions</span>
          </label>
        {/if}
        <div class="preview-mode" title="Wireframe = toolpath lines only. Solid = simulated stock with material removed (semi-transparent + edge lines). Both = solid underneath, toolpath on top.">
          <span class="preview-mode-label">3D preview</span>
          <div class="pill-group" role="radiogroup" aria-label="3D preview mode">
            {#each ['wireframe', 'solid', 'both'] as mode (mode)}
              <button
                type="button"
                role="radio"
                aria-checked={project.settings.previewMode === mode}
                class:active={project.settings.previewMode === mode}
                onclick={() =>
                  project.updateSettings({ previewMode: mode as 'wireframe' | 'solid' | 'both' })}
              >
                {mode}
              </button>
            {/each}
          </div>
        </div>
      </div>
    </aside>
  </main>

  <MachineDialog open={machineOpen} onClose={() => (machineOpen = false)} />
  <ToolLibraryDialog open={toolsOpen} onClose={() => (toolsOpen = false)} />
  <SettingsDialog open={settingsOpen} onClose={() => (settingsOpen = false)} />
  <AddTextDialog open={addTextOpen} onClose={() => (addTextOpen = false)} />

  <footer>
    {#if project.imported}
      {$_('footer.bbox', {
        values: {
          minX: project.imported.bbox.min_x.toFixed(2),
          minY: project.imported.bbox.min_y.toFixed(2),
          maxX: project.imported.bbox.max_x.toFixed(2),
          maxY: project.imported.bbox.max_y.toFixed(2),
          count: project.imported.segments.length,
          unit: project.imported.unit_scale,
        },
      })}
    {:else}
      {$_('footer.ready')}
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
  .config-btn {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    padding: 0.3rem 0.7rem;
    border-radius: 4px;
    font-size: 0.78rem;
    cursor: pointer;
  }
  .config-btn:hover {
    color: var(--text-strong);
    border-color: var(--accent);
  }
  .config-btn.icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 0.35rem;
    padding: 0.3rem 0.55rem;
  }
  .tool-toggle {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    padding: 0.3rem 0.7rem;
    border-radius: 4px;
    font-size: 0.78rem;
    cursor: pointer;
  }
  .tool-toggle.active {
    background: var(--tab-marker);
    color: #1a1a1a;
    border-color: var(--tab-marker);
  }
  .tool-toggle:disabled {
    opacity: 0.4;
    cursor: not-allowed;
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
    grid-template-columns: 1fr 360px;
    overflow: hidden;
    min-height: 0;
  }
  @media (max-width: 1100px) {
    main {
      grid-template-columns: 1fr 320px;
    }
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
  .pane {
    width: 100%;
    height: 100%;
  }
  .pane-hidden {
    display: none;
  }
  .loading-3d {
    display: flex;
    height: 100%;
    align-items: center;
    justify-content: center;
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  .gcode-toggle {
    display: flex;
    border-top: 1px solid var(--border);
    background: var(--bg-panel);
  }
  .gcode-toggle button {
    background: transparent;
    color: var(--text-muted);
    border: 0;
    padding: 0.2rem 0.7rem;
    font-size: 0.72rem;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
  }
  .gcode-toggle button.active {
    color: var(--text-strong);
  }
  .gcode-toggle .hint {
    color: var(--text-faint);
    font-size: 0.7rem;
  }
  .gcode-row {
    border-top: 1px solid var(--border);
    background: var(--bg-input);
    /* Vertical split: capped so the canvas stays usable on small screens. */
    height: clamp(180px, 35vh, 480px);
    overflow: hidden;
    min-height: 0;
  }
  .sidebar {
    display: grid;
    grid-template-rows: minmax(80px, 130px) minmax(0, 1fr) auto;
    min-height: 0;
    min-width: 0;
    overflow: hidden;
  }
  .layers-host,
  .ops-host,
  .stock-host {
    min-height: 0;
    min-width: 0;
    overflow: hidden;
  }
  .stock-host {
    border-top: 1px solid var(--border);
    background: var(--bg-panel);
    padding: 0.3rem 0.55rem 0.4rem;
    max-height: 30vh;
    overflow: auto;
  }
  .stock-host summary {
    font-size: 0.7rem;
    color: var(--text-muted);
    text-transform: uppercase;
    cursor: pointer;
    padding: 0.15rem 0;
  }
  .region-toggle {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin-top: 0.4rem;
    font-size: 0.72rem;
    color: var(--text-muted);
    cursor: pointer;
  }
  .preview-mode {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin-top: 0.5rem;
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .preview-mode-label {
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-size: 0.65rem;
  }
  .pill-group {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 999px;
    overflow: hidden;
    background: var(--bg-elevated);
  }
  .pill-group button {
    flex: 1;
    background: transparent;
    color: var(--text-muted);
    border: 0;
    padding: 0.2rem 0.6rem;
    font-size: 0.7rem;
    cursor: pointer;
    text-transform: capitalize;
  }
  .pill-group button.active {
    background: var(--accent);
    color: white;
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
