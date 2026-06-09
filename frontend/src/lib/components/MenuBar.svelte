<script lang="ts">
  /// Top menubar (File / Edit / View / Tools / Help) extracted from
  /// App.svelte (oytm pt 2). Owns the open/close/hover state machine and
  /// the dropdown markup + CSS; the pure decision logic (arrow-key index
  /// math, shortcut table) stays in `lib/state/app-menu.ts` where it is
  /// unit-tested. File actions call straight into `lib/services/file_ops`
  /// (whose charter is exactly "any UI surface can invoke the same
  /// flows"); dialog opens are App-owned action callbacks and the two
  /// read+write view toggles (`activePane`, `gcodeOpen`) are `$bindable`
  /// props, since the dialogs/panes themselves render in App's tree. App
  /// keeps the global keyboard dispatch and pokes `shake()` /
  /// `closeAllMenus()` through a `bind:this` ref so keyboard
  /// undo/redo/Escape still animate / close the menu items.
  import { project } from '../state/project.svelte';
  import { workspace } from '../state/workspace.svelte';
  import {
    openFile,
    openProject,
    saveProject,
    loadSample,
    exportGeneratedGcode,
    exportSimulatedStockStl,
    SAMPLES,
  } from '../services/file_ops';
  import { nextMenuItemIndex, type MenuId } from '../state/app-menu';

  interface Props {
    activePane: '2d' | '3d';
    gcodeOpen: boolean;
    onOpenMachine: () => void;
    onOpenTools: () => void;
    onOpenSettings: () => void;
    onOpenShortcutHelp: () => void;
    onOpenReport: () => void;
    onOpenAbout: () => void;
    /// Recent-project click. The dirty-check + path-vs-project routing
    /// lives in `lib/services/workspace-session`; the menubar only closes
    /// itself and forwards the path.
    onOpenRecent: (path: string) => void;
  }
  let {
    activePane = $bindable(),
    gcodeOpen = $bindable(),
    onOpenMachine,
    onOpenTools,
    onOpenSettings,
    onOpenShortcutHelp,
    onOpenReport,
    onOpenAbout,
    onOpenRecent,
  }: Props = $props();

  let openMenu = $state<MenuId | null>(null);
  function toggleMenu(id: MenuId) {
    openMenu = openMenu === id ? null : id;
  }
  export function closeAllMenus() {
    openMenu = null;
  }
  function onWindowClick(e: MouseEvent) {
    if (openMenu == null) return;
    const target = e.target as HTMLElement | null;
    // Clicks inside the menu (button or dropdown) keep it open — the
    // item handlers themselves call closeAllMenus when they should.
    if (target?.closest('.menu')) return;
    closeAllMenus();
  }

  /// Arrow-key / Home / End nav inside an open menubar dropdown. Wired
  /// to the dropdown div's onkeydown — keeps focus inside the menu and
  /// matches the WAI-ARIA pattern for `role="menu"`. ESC is handled at
  /// the window level (App's keyboard dispatch calls closeAllMenus).
  function onMenuKey(e: KeyboardEvent) {
    const dropdown = (e.currentTarget as HTMLElement) ?? null;
    if (!dropdown) return;
    const items = Array.from(
      dropdown.querySelectorAll<HTMLElement>('button[role="menuitem"]:not(:disabled)'),
    );
    const active = document.activeElement as HTMLElement | null;
    const idx = active ? items.indexOf(active) : -1;
    const next = nextMenuItemIndex(e.key, idx, items.length);
    if (next === null) return;
    e.preventDefault();
    items[next]?.focus();
  }
  /// Svelte action that auto-focuses the first menuitem inside the
  /// dropdown on mount. Without it, keyboard users opening the File menu
  /// would have to Tab past every preceding control to reach the first
  /// item — combined with `onMenuKey` above, arrow keys then walk items.
  function focusFirstMenuItemAction(node: HTMLElement) {
    queueMicrotask(() => {
      const first = node.querySelector<HTMLElement>('button[role="menuitem"]:not(:disabled)');
      first?.focus();
    });
  }

  function pickMenu<T>(fn: () => T): T {
    closeAllMenus();
    return fn();
  }

  /// Bumped to `performance.now()` whenever an undo/redo is attempted on
  /// an empty stack — drives the shake animation on the Edit-menu items.
  /// Exported so App's keyboard dispatch can trigger the same feedback.
  let undoShakeAt = $state(0);
  let redoShakeAt = $state(0);
  export function shake(which: 'undo' | 'redo') {
    if (which === 'undo') undoShakeAt = performance.now();
    else redoShakeAt = performance.now();
  }
  function doUndo() {
    closeAllMenus();
    if (!project.undo()) shake('undo');
  }
  function doRedo() {
    closeAllMenus();
    if (!project.redo()) shake('redo');
  }

  const undoLabel = $derived(project.undoLabel());
  const redoLabel = $derived(project.redoLabel());
  const canUndo = $derived(project.canUndo());
  const canRedo = $derived(project.canRedo());

  /// Reactive view of the workspace recent list. `void workspace.version`
  /// subscribes the derived to the store's mutation counter.
  const recentProjects = $derived.by(() => {
    void workspace.version;
    return workspace.get().recent_projects;
  });

  function clickRecent(path: string) {
    closeAllMenus();
    onOpenRecent(path);
  }
  function clickClearRecents() {
    closeAllMenus();
    workspace.clearRecentProjects();
  }

  async function exportGcode() {
    // Read the last-used post processor from the workspace store so the
    // File-menu export matches the toolbar's Download button without
    // having to reach across the DOM (was querySelector('button.download')
    // .click() — a 'a40m' audit item).
    const raw = workspace.get().last_post_processor;
    const post: 'linuxcnc' | 'grbl' | 'hpgl' = raw === 'grbl' || raw === 'hpgl' ? raw : 'linuxcnc';
    await exportGeneratedGcode(post);
  }
</script>

<svelte:window onclick={onWindowClick} />

<nav class="menubar" aria-label="Main menu">
  <div class="menu" class:open={openMenu === 'file'}>
    <button
      type="button"
      class="menu-btn"
      onclick={() => toggleMenu('file')}
      aria-haspopup="menu"
      aria-expanded={openMenu === 'file'}>File</button
    >
    {#if openMenu === 'file'}
      <div
        class="dropdown"
        role="menu"
        tabindex="-1"
        onmouseleave={closeAllMenus}
        onkeydown={onMenuKey}
        use:focusFirstMenuItemAction
      >
        <button role="menuitem" class="item" onclick={() => pickMenu(openFile)}>
          <span class="label">Open file…</span><span class="kbd">Ctrl+O</span>
        </button>
        <button role="menuitem" class="item" onclick={() => pickMenu(openProject)}>
          <span class="label">Open project…</span>
        </button>
        <button
          role="menuitem"
          class="item"
          disabled={!project.transformedImport}
          onclick={() => pickMenu(saveProject)}
        >
          <span class="label">Save project…</span><span class="kbd">Ctrl+S</span>
        </button>
        <button
          role="menuitem"
          class="item"
          disabled={!project.generated}
          onclick={() => pickMenu(exportGcode)}
        >
          <span class="label">Export G-code…</span>
        </button>
        <button
          role="menuitem"
          class="item"
          disabled={!project.generated}
          onclick={() => pickMenu(exportSimulatedStockStl)}
          title="Save the carved simulated stock as a binary STL. Run Generate first so the heightfield reflects the planned cuts."
        >
          <span class="label">Export simulated stock as STL…</span>
        </button>
        <button
          role="menuitem"
          class="item"
          onclick={() => pickMenu(onOpenReport)}
          title="Printable project summary — toolpath stats, time estimate, tools, ops, warnings."
        >
          <span class="label">Report…</span>
        </button>
        <div class="divider"></div>
        <div class="submenu">
          <div class="sub-head">Samples</div>
          {#each SAMPLES as s (s.url)}
            <button role="menuitem" class="item" onclick={() => pickMenu(() => loadSample(s.url))}>
              <span class="label">{s.label}</span>
            </button>
          {/each}
        </div>
        <div class="divider"></div>
        <div class="submenu">
          <div class="sub-head">Recent projects</div>
          {#if recentProjects.length === 0}
            <div class="item empty">No recent projects</div>
          {:else}
            {#each recentProjects as r (r.path)}
              <button
                role="menuitem"
                class="item"
                title={r.path}
                onclick={() => clickRecent(r.path)}
              >
                <span class="label">{r.filename}</span>
              </button>
            {/each}
            <button
              role="menuitem"
              class="item subtle"
              onclick={clickClearRecents}
              title="Clear the recent projects list"
            >
              <span class="label">Clear recent projects</span>
            </button>
          {/if}
        </div>
      </div>
    {/if}
  </div>

  <div class="menu" class:open={openMenu === 'edit'}>
    <button
      type="button"
      class="menu-btn"
      onclick={() => toggleMenu('edit')}
      aria-haspopup="menu"
      aria-expanded={openMenu === 'edit'}>Edit</button
    >
    {#if openMenu === 'edit'}
      <div
        class="dropdown"
        role="menu"
        tabindex="-1"
        onmouseleave={closeAllMenus}
        onkeydown={onMenuKey}
        use:focusFirstMenuItemAction
      >
        <button
          role="menuitem"
          class="item"
          class:shake={undoShakeAt > 0}
          onanimationend={() => (undoShakeAt = 0)}
          disabled={!canUndo}
          onclick={doUndo}
        >
          <span class="label">Undo{undoLabel ? `: ${undoLabel}` : ''}</span>
          <span class="kbd">Ctrl+Z</span>
        </button>
        <button
          role="menuitem"
          class="item"
          class:shake={redoShakeAt > 0}
          onanimationend={() => (redoShakeAt = 0)}
          disabled={!canRedo}
          onclick={doRedo}
        >
          <span class="label">Redo{redoLabel ? `: ${redoLabel}` : ''}</span>
          <span class="kbd">Ctrl+Y</span>
        </button>
      </div>
    {/if}
  </div>

  <div class="menu" class:open={openMenu === 'view'}>
    <button
      type="button"
      class="menu-btn"
      onclick={() => toggleMenu('view')}
      aria-haspopup="menu"
      aria-expanded={openMenu === 'view'}>View</button
    >
    {#if openMenu === 'view'}
      <div
        class="dropdown"
        role="menu"
        tabindex="-1"
        onmouseleave={closeAllMenus}
        onkeydown={onMenuKey}
        use:focusFirstMenuItemAction
      >
        <button
          role="menuitem"
          class="item"
          class:checked={activePane === '2d'}
          onclick={() => pickMenu(() => (activePane = '2d'))}
        >
          <span class="label">2D view</span>
        </button>
        <button
          role="menuitem"
          class="item"
          class:checked={activePane === '3d'}
          onclick={() => pickMenu(() => (activePane = '3d'))}
        >
          <span class="label">3D view</span>
        </button>
        <div class="divider"></div>
        <button
          role="menuitem"
          class="item"
          class:checked={gcodeOpen}
          disabled={!project.generated}
          onclick={() => pickMenu(() => (gcodeOpen = !gcodeOpen))}
        >
          <span class="label">G-code panel</span>
        </button>
      </div>
    {/if}
  </div>

  <div class="menu" class:open={openMenu === 'tools'}>
    <button
      type="button"
      class="menu-btn"
      onclick={() => toggleMenu('tools')}
      aria-haspopup="menu"
      aria-expanded={openMenu === 'tools'}>Tools</button
    >
    {#if openMenu === 'tools'}
      <div
        class="dropdown"
        role="menu"
        tabindex="-1"
        onmouseleave={closeAllMenus}
        onkeydown={onMenuKey}
        use:focusFirstMenuItemAction
      >
        <button role="menuitem" class="item" onclick={() => pickMenu(onOpenTools)}>
          <span class="label">Tool library…</span>
        </button>
        <button role="menuitem" class="item" onclick={() => pickMenu(onOpenMachine)}>
          <span class="label">Machine…</span>
        </button>
        <button role="menuitem" class="item" onclick={() => pickMenu(onOpenSettings)}>
          <span class="label">Settings…</span>
        </button>
      </div>
    {/if}
  </div>

  <div class="menu" class:open={openMenu === 'help'}>
    <button
      type="button"
      class="menu-btn"
      onclick={() => toggleMenu('help')}
      aria-haspopup="menu"
      aria-expanded={openMenu === 'help'}>Help</button
    >
    {#if openMenu === 'help'}
      <div
        class="dropdown"
        role="menu"
        tabindex="-1"
        onmouseleave={closeAllMenus}
        onkeydown={onMenuKey}
        use:focusFirstMenuItemAction
      >
        <button role="menuitem" class="item" onclick={() => pickMenu(onOpenShortcutHelp)}>
          <span class="label">Keyboard shortcuts…</span><span class="kbd">?</span>
        </button>
        <button role="menuitem" class="item" onclick={() => pickMenu(onOpenAbout)}>
          <span class="label">About ivaCAM…</span>
        </button>
      </div>
    {/if}
  </div>
</nav>

<style>
  .menubar {
    display: flex;
    align-items: stretch;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--border);
    padding: 0 0.25rem;
    min-height: 1.85rem;
  }
  .menu {
    position: relative;
  }
  .menu-btn {
    background: transparent;
    color: var(--text);
    border: 0;
    padding: 0.25rem 0.7rem;
    font-size: 0.82rem;
    cursor: pointer;
    border-radius: 3px;
    line-height: 1.3;
  }
  .menu-btn:hover {
    background: var(--bg-elevated);
  }
  .menu.open .menu-btn {
    background: var(--bg-elevated);
    color: var(--text-strong);
  }
  .dropdown {
    position: absolute;
    top: 100%;
    left: 0;
    min-width: 240px;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px var(--shadow-modal);
    padding: 0.2rem;
    z-index: var(--z-dropdown);
    display: flex;
    flex-direction: column;
    gap: 0.05rem;
  }
  .dropdown .item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.7rem;
    background: transparent;
    color: var(--text);
    border: 0;
    padding: 0.3rem 0.55rem;
    font-size: 0.78rem;
    border-radius: 3px;
    cursor: pointer;
    text-align: left;
    width: 100%;
  }
  .dropdown .item:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .dropdown .item:disabled {
    color: var(--text-faint);
    cursor: not-allowed;
  }
  .dropdown .item.checked .label::before {
    content: '✓ ';
    color: var(--accent);
  }
  .dropdown .item.empty {
    color: var(--text-faint);
    font-style: italic;
    cursor: default;
  }
  .dropdown .item.subtle {
    color: var(--text-muted);
    font-size: 0.72rem;
  }
  .dropdown .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    /* Cap relative to viewport so wide windows can show longer filenames
       (Recent Projects in particular); narrow windows still ellipsis. */
    max-width: min(420px, 80vw);
  }
  .dropdown .kbd {
    color: var(--text-muted);
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
  }
  .dropdown .divider {
    height: 1px;
    background: var(--border);
    margin: 0.2rem 0.05rem;
  }
  .dropdown .submenu {
    display: flex;
    flex-direction: column;
    gap: 0.05rem;
  }
  .dropdown .sub-head {
    padding: 0.25rem 0.55rem 0.05rem;
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
  }
  /* Shake animation on undo/redo when stack is empty. */
  @keyframes ivac-undo-shake {
    0% {
      transform: translateX(0);
    }
    25% {
      transform: translateX(-3px);
    }
    50% {
      transform: translateX(3px);
    }
    75% {
      transform: translateX(-2px);
    }
    100% {
      transform: translateX(0);
    }
  }
  .dropdown .item.shake {
    animation: ivac-undo-shake 100ms ease-in-out;
  }
</style>
