<script lang="ts">
  /// "Recent ▾" toolbar dropdown. Reads the workspace recent-projects list;
  /// selection is forwarded to the App's open-recent flow (which owns
  /// the dirty-check + path routing).
  import { workspace } from '../state/workspace.svelte';

  interface Props {
    onOpen: (path: string) => void;
  }
  let { onOpen }: Props = $props();

  let open = $state(false);
  const recents = $derived.by(() => {
    void workspace.version;
    return workspace.get().recent_projects;
  });

  function onWindowClick(e: MouseEvent) {
    if (!open) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest('.recent-menu')) return;
    open = false;
  }
  function pick(path: string) {
    open = false;
    onOpen(path);
  }
</script>

<svelte:window onclick={onWindowClick} />

<div class="recent-menu">
  <button
    type="button"
    class="tb-btn"
    disabled={recents.length === 0}
    aria-haspopup="menu"
    aria-expanded={open}
    title={recents.length === 0 ? 'No recent projects yet' : 'Open a recently used project'}
    onclick={() => (open = !open)}>Recent ▾</button
  >
  {#if open}
    <div class="dropdown" role="menu">
      {#each recents as r (r.path)}
        <button
          type="button"
          role="menuitem"
          class="item"
          title={r.path}
          onclick={() => pick(r.path)}>{r.filename}</button
        >
      {/each}
      <div class="sep"></div>
      <button
        type="button"
        role="menuitem"
        class="item muted"
        onclick={() => {
          open = false;
          workspace.clearRecentProjects();
        }}>Clear list</button
      >
    </div>
  {/if}
</div>

<style>
  .recent-menu {
    position: relative;
    display: inline-block;
  }
  /* .tb-btn styling comes from App.svelte's toolbar scope — mirror the
     essentials so the button matches its neighbors. */
  .tb-btn {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.3rem 0.6rem;
    font-size: 0.78rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .tb-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .dropdown {
    position: absolute;
    top: calc(100% + 2px);
    left: 0;
    z-index: var(--z-dropdown);
    min-width: 220px;
    max-width: 360px;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.35);
    padding: 0.25rem;
    display: flex;
    flex-direction: column;
  }
  .item {
    background: none;
    border: none;
    color: var(--text);
    text-align: left;
    padding: 0.3rem 0.5rem;
    font-size: 0.78rem;
    cursor: pointer;
    border-radius: 3px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .item:hover {
    background: var(--hover-bg-elevated, var(--bg-panel));
  }
  .item.muted {
    color: var(--text-muted);
  }
  .sep {
    height: 1px;
    background: var(--border);
    margin: 0.2rem 0.3rem;
  }
</style>
