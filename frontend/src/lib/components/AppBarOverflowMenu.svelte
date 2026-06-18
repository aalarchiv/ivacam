<script lang="ts">
  /// Phone top-app-bar overflow menu (7jug.2). The desktop toolbar's
  /// secondary controls have no room in the single-row mobile app bar, so
  /// they live behind a "⋮" overflow: Undo / Redo, Recent projects, and
  /// the Regions visibility toggle. The primary actions (Open, Save,
  /// Report) stay as direct app-bar buttons; Generate is pull-to-refresh
  /// (.12); 2D/3D and the whole-screen tabs are activities (.2).
  import { project } from '../state/project.svelte';
  import { workspace } from '../state/workspace.svelte';

  interface Props {
    /// Open a recently-used project by path (App owns the dirty-check +
    /// path routing — same flow as the desktop RecentMenu).
    onOpenRecent: (path: string) => void;
  }
  let { onOpenRecent }: Props = $props();

  let open = $state(false);

  const recents = $derived.by(() => {
    void workspace.version;
    return workspace.get().recent_projects;
  });
  /// Regions toggle is only meaningful once a generated program carries
  /// pocket regions to shade.
  const hasRegions = $derived(
    !!project.gen.generated?.regions && project.gen.generated.regions.length > 0,
  );

  function onWindowPointer(e: MouseEvent) {
    if (!open) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest('.overflow-menu')) return;
    open = false;
  }

  function run(action: () => void) {
    open = false;
    action();
  }
  function pickRecent(path: string) {
    open = false;
    onOpenRecent(path);
  }
</script>

<svelte:window onclick={onWindowPointer} />

<div class="overflow-menu">
  <button
    type="button"
    class="ab-btn"
    aria-haspopup="menu"
    aria-expanded={open}
    aria-label="More actions"
    onclick={() => (open = !open)}
  >
    ⋮
  </button>

  {#if open}
    <div class="menu" role="menu" aria-label="More actions">
      <button
        type="button"
        class="menu-item"
        role="menuitem"
        disabled={!project.canUndo()}
        onclick={() => run(() => project.undo())}
      >
        <span>Undo</span>
        <span class="hint">{project.undoLabel() ?? ''}</span>
      </button>
      <button
        type="button"
        class="menu-item"
        role="menuitem"
        disabled={!project.canRedo()}
        onclick={() => run(() => project.redo())}
      >
        <span>Redo</span>
        <span class="hint">{project.redoLabel() ?? ''}</span>
      </button>

      {#if hasRegions}
        <div class="menu-sep" role="separator"></div>
        <button
          type="button"
          class="menu-item"
          role="menuitemcheckbox"
          aria-checked={project.data.regionsVisible}
          onclick={() => (project.data.regionsVisible = !project.data.regionsVisible)}
        >
          <span>Show regions</span>
          <span class="check" aria-hidden="true">{project.data.regionsVisible ? '✓' : ''}</span>
        </button>
      {/if}

      <div class="menu-sep" role="separator"></div>
      <div class="menu-label">Recent projects</div>
      {#if recents.length === 0}
        <div class="menu-empty">No recent projects yet</div>
      {:else}
        {#each recents as r (r.path)}
          <button
            type="button"
            class="menu-item recent"
            role="menuitem"
            title={r.path}
            onclick={() => pickRecent(r.path)}
          >
            <span class="recent-name">{r.filename}</span>
          </button>
        {/each}
      {/if}
    </div>
  {/if}
</div>

<style>
  .overflow-menu {
    position: relative;
    display: inline-flex;
  }
  /* Svelte scopes the parent's `.mobile-appbar .ab-btn` rule to App.svelte,
     so it does NOT reach this child component's trigger. Restate the
     app-bar button look here so the ⋮ matches the Open / Save / Report
     buttons' height + style (punch-list 9). */
  .overflow-menu .ab-btn {
    min-height: 40px;
    min-width: 40px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0 0.6rem;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 5px;
    font-size: 1.05rem;
    line-height: 1;
    cursor: pointer;
  }
  .overflow-menu .ab-btn:hover {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    border-color: var(--accent);
    color: var(--text-strong);
  }

  .menu {
    position: absolute;
    top: calc(100% + 0.3rem);
    right: 0;
    z-index: var(--z-dropdown);
    min-width: 14rem;
    max-width: min(20rem, 90vw);
    max-height: 70vh;
    overflow-y: auto;
    padding: 0.3rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 6px 22px rgb(0 0 0 / 35%);
  }

  .menu-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    width: 100%;
    min-height: 44px;
    padding: 0 0.6rem;
    background: none;
    border: none;
    border-radius: 5px;
    color: var(--text);
    font-size: 0.9rem;
    text-align: left;
    cursor: pointer;
  }
  .menu-item:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    color: var(--text-strong);
  }
  .menu-item:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .menu-item .check {
    color: var(--accent);
    font-size: 0.95rem;
    min-width: 1em;
    text-align: center;
  }
  .menu-item .hint {
    color: var(--text-muted);
    font-size: 0.78rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .menu-item.recent .recent-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .menu-sep {
    height: 1px;
    margin: 0.3rem 0.2rem;
    background: var(--border);
  }
  .menu-label {
    padding: 0.3rem 0.6rem 0.15rem;
    color: var(--text-muted);
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .menu-empty {
    padding: 0.4rem 0.6rem 0.5rem;
    color: var(--text-muted);
    font-size: 0.82rem;
  }
</style>
