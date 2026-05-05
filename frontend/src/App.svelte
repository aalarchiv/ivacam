<script lang="ts">
  import FileUpload from './lib/components/FileUpload.svelte';
  import EntityCanvas2D from './lib/components/EntityCanvas2D.svelte';
  import Scene3D from './lib/components/Scene3D.svelte';
  import LayerList from './lib/components/LayerList.svelte';
  import SetupPanel from './lib/components/SetupPanel.svelte';
  import GenerateBar from './lib/components/GenerateBar.svelte';
  import PlaybackBar from './lib/components/PlaybackBar.svelte';
  import { project } from './lib/state/project.svelte';
  import { onMount } from 'svelte';
  import { _ } from 'svelte-i18n';
  import { setLocale, locale } from './lib/i18n';
  import { isTauri } from './lib/api/tauri';

  type LocalePref = 'en' | 'de';
  let lang = $state<LocalePref>('en');
  $effect(() => {
    const cur = $locale;
    if (cur === 'en' || cur === 'de') lang = cur;
  });
  function pickLocale(code: LocalePref) {
    setLocale(code);
  }

  type ThemePref = 'auto' | 'light' | 'dark';
  let theme = $state<ThemePref>('auto');
  const THEME_KEY = 'wiac.theme';

  onMount(() => {
    const stored = localStorage.getItem(THEME_KEY) as ThemePref | null;
    if (stored === 'auto' || stored === 'light' || stored === 'dark') {
      theme = stored;
    }
    document.documentElement.dataset.theme = theme;

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
      }
    });
  }

  $effect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      localStorage.setItem(THEME_KEY, theme);
    } catch {}
  });

  let activePane = $state<'2d' | '3d'>('2d');

  // Auto-switch to 3D when /generate returns; people want to see the toolpath.
  $effect(() => {
    if (project.generated) activePane = '3d';
  });

  const tabCount = $derived(
    Object.values(project.tabs).reduce((n, list) => n + list.length, 0),
  );

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      if (project.tabMode) project.tabMode = false;
      else if (project.selectedEntities.size > 0) project.selectedEntities = new Set();
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
    <div class="theme-toggle" role="group" aria-label="Theme">
      <button
        class:active={theme === 'auto'}
        onclick={() => (theme = 'auto')}
        title={$_('header.theme.auto_hint')}
      >{$_('header.theme.auto')}</button>
      <button
        class:active={theme === 'light'}
        onclick={() => (theme = 'light')}
        title={$_('header.theme.light_hint')}
      >{$_('header.theme.light')}</button>
      <button
        class:active={theme === 'dark'}
        onclick={() => (theme = 'dark')}
        title={$_('header.theme.dark_hint')}
      >{$_('header.theme.dark')}</button>
    </div>
    <div class="lang-toggle" role="group" aria-label={$_('header.lang.title')}>
      <button class:active={lang === 'en'} onclick={() => pickLocale('en')}>EN</button>
      <button class:active={lang === 'de'} onclick={() => pickLocale('de')}>DE</button>
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
  .theme-toggle {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 4px;
    overflow: hidden;
  }
  .theme-toggle button {
    background: var(--bg-elevated);
    color: var(--text-muted);
    border: 0;
    padding: 0.3rem 0.55rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .theme-toggle button.active {
    background: var(--accent);
    color: white;
  }
  .lang-toggle {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 4px;
    overflow: hidden;
  }
  .lang-toggle button {
    background: var(--bg-elevated);
    color: var(--text-muted);
    border: 0;
    padding: 0.3rem 0.55rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .lang-toggle button.active {
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
  .sidebar {
    display: grid;
    grid-template-rows: minmax(120px, 220px) minmax(0, 1fr);
    min-height: 0;
    min-width: 0;
    overflow: hidden;
  }
  .layers-host,
  .setup-host {
    min-height: 0;
    min-width: 0;
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
