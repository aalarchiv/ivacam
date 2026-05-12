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
  import SourceStaleToast from './lib/components/SourceStaleToast.svelte';
  import ShortcutHelp from './lib/components/ShortcutHelp.svelte';
  import LoadingOverlay from './lib/components/LoadingOverlay.svelte';

  let machineOpen = $state(false);
  let toolsOpen = $state(false);
  let settingsOpen = $state(false);
  let addTextOpen = $state(false);
  let shortcutHelpOpen = $state(false);
  /// Startup banner: when set, the user was previously editing a
  /// project and we offer to reopen it. Styled in-app instead of a
  /// native window.confirm so the first impression of the app isn't
  /// an unstyled OS dialog (audit C10).
  let reopenPrompt = $state<{ path: string; filename: string } | null>(null);

  // Open the Tool library dialog when OpPropertiesPanel's "edit this
  // tool" icon requests focus on a specific tool row. The dialog reads
  // project.toolsDialogFocusId and handles scroll/highlight.
  $effect(() => {
    if (project.toolsDialogFocusId != null) {
      toolsOpen = true;
    }
  });

  // G-code panel visibility. The playback bar always sits below the
  // 3D canvas; the gcode panel opens as an extra row beneath it so
  // the user sees the toolpath, the playhead, and the program text
  // simultaneously and can drive each from the others.
  let gcodeOpen = $state(false);
  import { project } from './lib/state/project.svelte';
  import { workspace } from './lib/state/workspace.svelte';
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

    // Global error capture. Silent throws inside Svelte 5 $effect bodies
    // can abort the reactivity scheduler — every button still fires its
    // onclick, but visible state stops updating. Surfacing these to the
    // console (and to project.error for severity) makes the failure mode
    // visible instead of "the whole UI just stopped working".
    window.addEventListener('error', (ev) => {
      console.error('uncaught error:', ev.error ?? ev.message);
    });
    window.addEventListener('unhandledrejection', (ev) => {
      console.error('unhandled promise rejection:', ev.reason);
    });

    if (isTauri()) {
      void wireMenuEvents();
      void wireSourceWatch();
    }
    void loadWorkspaceAndMaybeReopen();
  });

  /// Pull persisted workspace state at startup. After load completes,
  /// prune any per-project / recent entries pointing at files that have
  /// disappeared (Tauri only — browser localStorage has no fs probe).
  /// On Tauri, optionally prompt the user to reopen the last project so
  /// they don't have to navigate the file picker every launch. Browser
  /// builds skip the prompt because we have no path-based load there.
  async function loadWorkspaceAndMaybeReopen() {
    try {
      await workspace.load();
    } catch {
      // ignore — defaults are fine.
    }
    if (isTauri()) {
      void workspace.pruneMissingProjects();
      const last = workspace.get().last_project;
      if (last) {
        const filename = last.split(/[\\/]/).pop() ?? last;
        reopenPrompt = { path: last, filename };
      }
    }
  }

  async function acceptReopen() {
    if (!reopenPrompt) return;
    const path = reopenPrompt.path;
    reopenPrompt = null;
    await openProjectPath(path);
  }
  function dismissReopen() {
    reopenPrompt = null;
  }

  // Auto-dismiss the reopen banner once a project / drawing is loaded by
  // any path (the user clicked Open, dragged a file, or accepted the
  // banner). The banner only makes sense as a startup affordance —
  // keeping it visible after the user is already working is noise.
  $effect(() => {
    void project.imported;
    void project.activeProjectPath;
    if (reopenPrompt && (project.imported || project.activeProjectPath)) {
      reopenPrompt = null;
    }
  });

  /// Load a project by absolute path. Picks the import path vs.
  /// .vc-project loader by the file extension. Mirrors what
  /// FileUpload.svelte's loadFromPath / openProjectNative flows do —
  /// kept here so the recent-projects submenu and the reopen prompt
  /// can drive loads without poking at FileUpload's internals.
  async function openProjectPath(path: string) {
    if (!isTauri()) return;
    const isProjectFile = /\.(wiac|vc)-project\.json$|\.json$/i.test(path);
    project.loading = true;
    project.loadingMessage = isProjectFile ? 'Loading project…' : 'Parsing file…';
    project.error = null;
    try {
      if (isProjectFile) {
        const { readTextFile } = await import('@tauri-apps/plugin-fs');
        const text = await readTextFile(path);
        project.restore(JSON.parse(text));
      } else {
        const { invoke } = await import('@tauri-apps/api/core');
        const result = await invoke('import_path', { path });
        project.setImported(result as Parameters<typeof project.setImported>[0]);
      }
      const filename = path.split(/[\\/]/).pop() ?? path;
      workspace.addRecentProject(path, filename);
      project.setActiveProjectPath(path);
    } catch (e) {
      project.setError(e instanceof Error ? e.message : String(e));
    } finally {
      project.loading = false;
      project.loadingMessage = null;
    }
  }

  /// Persist per-project workspace state when the user adjusts visible
  /// layers / selected op / playhead. Skipped when no project path is
  /// known (samples, browser drag-and-drop) — there's no key to store
  /// against. The store debounces writes, so this fires often.
  $effect(() => {
    void project.visibleLayers;
    void project.selectedOpId;
    void project.playhead;
    if (project.activeProjectPath) {
      project.persistPerProjectState();
    }
  });

  let fileMenuOpen = $state(false);
  function toggleFileMenu() { fileMenuOpen = !fileMenuOpen; }
  function closeFileMenu() { fileMenuOpen = false; }
  /// Reactive view of the workspace recent list. `void workspace.version`
  /// subscribes the derived to the store's mutation counter.
  const recentProjects = $derived.by(() => {
    void workspace.version;
    return workspace.get().recent_projects;
  });
  function clickRecent(path: string) {
    closeFileMenu();
    void openProjectPath(path);
  }
  function clickClearRecents() {
    closeFileMenu();
    workspace.clearRecentProjects();
  }

  /// Subscribe to backend `source-file-changed` events emitted by the
  /// project watcher. With auto-reload enabled, swap the geometry in
  /// silently as a single undoable transaction; otherwise surface the
  /// "Reload?" toast. The unlisten fn is intentionally not stored —
  /// the watch lives for the lifetime of the window.
  async function wireSourceWatch() {
    const { onSourceFileChanged } = await import('./lib/api/tauri');
    await onSourceFileChanged(async ({ path }) => {
      if (path !== project.lastImportPath) return;
      if (project.settings.autoReloadSources) {
        await project.reimportFromPath(path);
      } else {
        project.sourceFileStaleNotice = { path, auto_reload: false };
      }
    });
  }

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
          // rt1.10: tab-mode is now per-op via OpPropertiesPanel. The
          // menu item lands the user on the Tabs fieldset of the
          // selected op (no-op if no op is selected).
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
    project.operations.reduce(
      (n, op) => n + (op.tabPlacements?.length ?? 0),
      0,
    ),
  );

  /// Bumped to `performance.now()` whenever an undo/redo is attempted on
  /// an empty stack — drives the shake animation on the Edit-menu items.
  let undoShakeAt = $state(0);
  let redoShakeAt = $state(0);
  function shake(which: 'undo' | 'redo') {
    if (which === 'undo') undoShakeAt = performance.now();
    else redoShakeAt = performance.now();
  }

  function isTypingTarget(t: EventTarget | null): boolean {
    const el = t as HTMLElement | null;
    if (!el) return false;
    const tag = el.tagName ?? '';
    const editable = el.isContentEditable;
    return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || !!editable;
  }

  function onKeyDown(e: KeyboardEvent) {
    // Ctrl/Cmd+Z = undo, Ctrl+Y / Ctrl+Shift+Z / Cmd+Shift+Z = redo.
    // Skip when a text input is focused so the browser's native field-
    // level undo still works.
    const mod = e.ctrlKey || e.metaKey;
    if (mod && !e.altKey) {
      const k = e.key.toLowerCase();
      if (k === 'z' && !e.shiftKey) {
        if (isTypingTarget(e.target)) return;
        e.preventDefault();
        if (!project.undo()) shake('undo');
        return;
      }
      if ((k === 'y' && !e.shiftKey) || (k === 'z' && e.shiftKey)) {
        if (isTypingTarget(e.target)) return;
        e.preventDefault();
        if (!project.redo()) shake('redo');
        return;
      }
    }
    if (e.key === 'Escape') {
      if (project.selectedEntities.size > 0) project.selectedEntities = new Set();
      return;
    }
    // Keyboard shortcut: T = Add Text. Skip when typing in any text input
    // and skip when modifier keys are held so it doesn't shadow Ctrl-T etc.
    if ((e.key === 't' || e.key === 'T') && !e.ctrlKey && !e.metaKey && !e.altKey) {
      if (isTypingTarget(e.target)) return;
      addTextOpen = true;
      e.preventDefault();
    }
    // Shortcut cheatsheet: '?' (Shift+/) or F1. Skip when typing so the
    // user can still type '?' into text fields.
    if ((e.key === '?' || e.key === 'F1') && !e.ctrlKey && !e.metaKey && !e.altKey) {
      if (isTypingTarget(e.target)) return;
      shortcutHelpOpen = true;
      e.preventDefault();
    }
  }

  const undoLabel = $derived(project.undoLabel());
  const redoLabel = $derived(project.redoLabel());
  const canUndo = $derived(project.canUndo());
  const canRedo = $derived(project.canRedo());
  let editMenuOpen = $state(false);
  function toggleEditMenu() { editMenuOpen = !editMenuOpen; }
  function closeEditMenu() { editMenuOpen = false; }
  function doUndo() {
    closeEditMenu();
    if (!project.undo()) shake('undo');
  }
  function doRedo() {
    closeEditMenu();
    if (!project.redo()) shake('redo');
  }
</script>

<svelte:window onkeydown={onKeyDown} />

<div class="app">
  <header>
    <h1>{$_('app.title')}</h1>
    <span class="tagline">{$_('app.tagline')}</span>
    <div class="spacer"></div>
    <div class="edit-menu" class:open={fileMenuOpen}>
      <button
        type="button"
        class="config-btn"
        onclick={toggleFileMenu}
        title="File menu (Recent projects)"
        aria-haspopup="menu"
        aria-expanded={fileMenuOpen}
      >File ▾</button>
      {#if fileMenuOpen}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="edit-dropdown"
          role="menu"
          tabindex="-1"
          onmouseleave={closeFileMenu}
        >
          <div class="recent-heading">Recent Projects</div>
          {#if recentProjects.length === 0}
            <div class="recent-empty">No recent projects</div>
          {:else}
            {#each recentProjects as r (r.path)}
              <button
                type="button"
                role="menuitem"
                class="edit-item"
                onclick={() => clickRecent(r.path)}
                title={r.path}
              >
                <span class="edit-label">{r.filename}</span>
              </button>
            {/each}
          {/if}
          <div class="menu-divider"></div>
          <button
            type="button"
            role="menuitem"
            class="edit-item"
            disabled={recentProjects.length === 0}
            onclick={clickClearRecents}
            title="Clear the recent projects list"
          >
            <span class="edit-label">Clear recent projects</span>
          </button>
        </div>
      {/if}
    </div>
    <div class="edit-menu" class:open={editMenuOpen}>
      <button
        type="button"
        class="config-btn"
        onclick={toggleEditMenu}
        title="Edit menu (Undo / Redo)"
        aria-haspopup="menu"
        aria-expanded={editMenuOpen}
      >Edit ▾</button>
      {#if editMenuOpen}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="edit-dropdown"
          role="menu"
          tabindex="-1"
          onmouseleave={closeEditMenu}
        >
          <button
            type="button"
            role="menuitem"
            class="edit-item"
            class:shake={undoShakeAt > 0}
            onanimationend={() => (undoShakeAt = 0)}
            disabled={!canUndo}
            onclick={doUndo}
            title="Ctrl+Z"
          >
            <span class="edit-label">Undo{undoLabel ? `: ${undoLabel}` : ''}</span>
            <span class="edit-kbd">Ctrl+Z</span>
          </button>
          <button
            type="button"
            role="menuitem"
            class="edit-item"
            class:shake={redoShakeAt > 0}
            onanimationend={() => (redoShakeAt = 0)}
            disabled={!canRedo}
            onclick={doRedo}
            title="Ctrl+Y / Ctrl+Shift+Z"
          >
            <span class="edit-label">Redo{redoLabel ? `: ${redoLabel}` : ''}</span>
            <span class="edit-kbd">Ctrl+Y</span>
          </button>
        </div>
      {/if}
    </div>
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
    <!-- Tab count chip — read-only summary. Click-to-place lives on
         the 2D canvas and per-op Tabs section (audit C11). -->
    <span
      class="tab-count-chip"
      title="Total tabs across all operations. Select an op and click on its contour in the 2D canvas to add or remove a tab."
      aria-label={$_('header.tabs', { values: { count: tabCount } })}
    >
      {$_('header.tabs', { values: { count: tabCount } })}
    </span>
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

  {#if reopenPrompt}
    <div class="reopen-banner" role="alert">
      <span class="reopen-text">
        Reopen <strong>{reopenPrompt.filename}</strong>?
      </span>
      <button class="reopen-accept" type="button" onclick={acceptReopen}>Open</button>
      <button class="reopen-dismiss" type="button" onclick={dismissReopen}>Dismiss</button>
    </div>
  {/if}

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
          <EntityCanvas2D onShowHelp={() => (shortcutHelpOpen = true)} />
        </div>
        {#if Scene3D}
          {@const C = Scene3D}
          <div class:pane-hidden={activePane !== '3d'} class="pane">
            <C />
          </div>
        {:else if activePane === '3d'}
          <p class="loading-3d">Loading 3D…</p>
        {/if}
        <LoadingOverlay visible={project.loading} message={project.loadingMessage} />
      </div>
      {#if project.generated}
        <PlaybackBar />
        <div class="gcode-toggle">
          <button
            class:active={gcodeOpen}
            onclick={() => (gcodeOpen = !gcodeOpen)}
            title="Show / hide the G-code text panel. Click a line to scrub the playhead; the playhead's current line scrolls into view."
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
        <LayerList onOpenFileClick={() => (document.querySelector('button.open-file') as HTMLButtonElement | null)?.click()} />
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
          <label class="region-toggle" title="Show / hide the translucent fill that marks each pocket operation's machined region.">
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
  <ToolLibraryDialog
    open={toolsOpen}
    onClose={() => {
      toolsOpen = false;
      project.toolsDialogFocusId = null;
    }}
  />
  <SettingsDialog open={settingsOpen} onClose={() => (settingsOpen = false)} />
  <AddTextDialog open={addTextOpen} onClose={() => (addTextOpen = false)} />
  {#if shortcutHelpOpen}
    <ShortcutHelp onClose={() => (shortcutHelpOpen = false)} />
  {/if}
  <SourceStaleToast onReload={async (p) => { await project.reimportFromPath(p); }} />

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
  .reopen-banner {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.35rem 0.75rem;
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    border-bottom: 1px solid var(--border);
    font-size: 0.85rem;
    color: var(--text);
  }
  .reopen-text {
    flex: 1;
  }
  .reopen-accept,
  .reopen-dismiss {
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text);
    padding: 0.2rem 0.6rem;
    border-radius: 3px;
    cursor: pointer;
    font-size: 0.82rem;
  }
  .reopen-accept {
    background: var(--accent);
    color: #fff;
    border-color: var(--accent);
  }
  .reopen-accept:hover {
    background: var(--accent-strong, var(--accent));
  }
  .reopen-dismiss:hover {
    background: var(--bg);
  }
  .tab-count-chip {
    display: inline-flex;
    align-items: center;
    padding: 0.18rem 0.55rem;
    border: 1px solid var(--border);
    border-radius: 12px;
    background: var(--bg-elevated);
    color: var(--text-muted);
    font-size: 0.78rem;
    line-height: 1.2;
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
  .edit-menu {
    position: relative;
    display: inline-block;
  }
  .edit-dropdown {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    min-width: 240px;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.3);
    padding: 0.2rem;
    z-index: 60;
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }
  .edit-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.6rem;
    background: transparent;
    color: var(--text);
    border: 0;
    padding: 0.3rem 0.55rem;
    font-size: 0.78rem;
    border-radius: 3px;
    cursor: pointer;
    text-align: left;
  }
  .edit-item:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .edit-item:disabled {
    color: var(--text-faint);
    cursor: not-allowed;
  }
  .edit-kbd {
    font-size: 0.7rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
  .edit-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 220px;
  }
  /* 100ms shake when undo/redo is invoked on an empty stack — surfaces
     the "no-op" without throwing an error popup at the user. */
  @keyframes wiac-undo-shake {
    0% { transform: translateX(0); }
    25% { transform: translateX(-3px); }
    50% { transform: translateX(3px); }
    75% { transform: translateX(-2px); }
    100% { transform: translateX(0); }
  }
  .edit-item.shake {
    animation: wiac-undo-shake 100ms ease-in-out;
  }
  .recent-heading {
    padding: 0.25rem 0.55rem 0.1rem;
    font-size: 0.65rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .recent-empty {
    padding: 0.25rem 0.55rem;
    font-size: 0.75rem;
    color: var(--text-faint);
    font-style: italic;
  }
  .menu-divider {
    height: 1px;
    background: var(--border);
    margin: 0.2rem 0.1rem;
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
    /* Layers row sizes to its content (capped); ops fills the rest;
       stock sticks to the bottom. min-content stops a near-empty
       layer list from reserving the legacy 80-130 px gap that used
       to show as whitespace above operations. */
    grid-template-rows: minmax(min-content, 22vh) minmax(0, 1fr) auto;
    min-height: 0;
    min-width: 0;
    overflow: hidden;
  }
  .layers-host {
    min-height: 0;
    min-width: 0;
    overflow: auto;
  }
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
