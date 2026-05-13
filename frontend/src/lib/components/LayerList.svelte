<script lang="ts">
  /// Imported-drawing layer list. Restyled (audit follow-up) to match
  /// the OperationsList group-header pattern: caret-to-collapse, a
  /// "show / hide all" master checkbox, a count chip, and a body that
  /// the user can fold away when they're not adjusting visibility.
  /// Same visual language as op groups so the sidebar reads as one
  /// coherent panel.
  import { project } from '../state/project.svelte';

  interface Props {
    onOpenFileClick?: () => void;
    /// Add Text entry point — phase B of the stock-first rework. When
    /// provided, the panel header sprouts an Add+ dropdown that offers
    /// both creation paths in one place.
    onAddTextClick?: () => void;
    /// Startup reopen affordance — replaces the old top-of-window
    /// reopen banner. When set, the empty card offers a "Reopen <name>"
    /// button beside the regular Open file CTA so the no-drawing state
    /// is the single coherent surface for getting a project loaded.
    reopenPrompt?: { path: string; filename: string } | null;
    onReopenAccept?: () => void;
    onReopenDismiss?: () => void;
  }
  let {
    onOpenFileClick,
    onAddTextClick,
    reopenPrompt = null,
    onReopenAccept,
    onReopenDismiss,
  }: Props = $props();

  let collapsed = $state(false);
  /// Add+ dropdown visibility. Closes on any item click and on the
  /// next window click outside the panel (svelte:window onclick handler
  /// below).
  let addMenuOpen = $state(false);
  function toggleAddMenu() {
    addMenuOpen = !addMenuOpen;
  }
  function closeAddMenu() {
    addMenuOpen = false;
  }
  function clickAddFile() {
    closeAddMenu();
    onOpenFileClick?.();
  }
  function clickAddText() {
    closeAddMenu();
    onAddTextClick?.();
  }
  function onWindowClick(e: MouseEvent) {
    if (!addMenuOpen) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest('.add-menu')) return;
    closeAddMenu();
  }

  const ACI: Record<number, string> = {
    1: '#ff0000',
    2: '#ffff00',
    3: '#00ff00',
    4: '#00ffff',
    5: '#0000ff',
    6: '#ff00ff',
  };
  // ACI 7 / 256 = BYLAYER white (paper-color). Theme-tracked.
  function swatch(c: number): string {
    if (c === 7 || c === 256) return 'var(--text-strong)';
    if (c === 8) return 'var(--text-muted)';
    return ACI[c] ?? 'var(--text-faint)';
  }

  let usableLayers = $derived(project.imported?.layers.filter((l) => l.segment_count > 0) ?? []);

  let allVisible = $derived(
    usableLayers.length > 0 && usableLayers.every((l) => project.visibleLayers.has(l.name)),
  );

  function setAllVisible(on: boolean) {
    for (const l of usableLayers) {
      const has = project.visibleLayers.has(l.name);
      if (has !== on) project.toggleLayer(l.name);
    }
  }
</script>

<svelte:window onclick={onWindowClick} />

<aside class="layers">
  <div class="group-head">
    <button
      class="caret-btn"
      onclick={() => (collapsed = !collapsed)}
      title={collapsed ? 'Expand layers' : 'Collapse layers'}
      aria-label="Toggle layers panel">{collapsed ? '▸' : '▾'}</button
    >
    {#if usableLayers.length > 0}
      <input
        type="checkbox"
        checked={allVisible}
        title="Show / hide every layer"
        aria-label="Toggle all layers"
        onclick={(e) => e.stopPropagation()}
        onchange={(e) => setAllVisible((e.currentTarget as HTMLInputElement).checked)}
      />
    {/if}
    {#if usableLayers.length > 0 && project.imported?.filename}
      <span class="filename" title={project.imported.filename}>
        {project.imported.filename}
      </span>
      <span class="file-stats" title="Segments · layers · units">
        {project.imported.segments.length} seg · {usableLayers.length} layer{usableLayers.length ===
        1
          ? ''
          : 's'}
      </span>
    {:else}
      <span class="group-name">Layers</span>
      <span class="group-count">{usableLayers.length}</span>
    {/if}
    {#if onOpenFileClick || onAddTextClick}
      <div class="add-menu" class:open={addMenuOpen}>
        <button
          type="button"
          class="add-btn"
          onclick={toggleAddMenu}
          aria-haspopup="menu"
          aria-expanded={addMenuOpen}
          title="Add a layer — open a drawing or add text geometry"
        >
          + Add
        </button>
        {#if addMenuOpen}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="add-dropdown" role="menu" tabindex="-1" onmouseleave={closeAddMenu}>
            {#if onOpenFileClick}
              <button
                type="button"
                role="menuitem"
                class="add-item"
                onclick={clickAddFile}
                title="Open a DXF or SVG file"
              >
                <span class="label">Open drawing file…</span>
              </button>
            {/if}
            {#if onAddTextClick}
              <button
                type="button"
                role="menuitem"
                class="add-item"
                onclick={clickAddText}
                title="Add editable text geometry"
              >
                <span class="label">Add text…</span>
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </div>
  {#if !collapsed}
    <div class="group-body">
      {#if usableLayers.length > 0}
        <ul>
          {#each usableLayers as layer (layer.name)}
            <li class="layer-row">
              <label class="layer-label">
                <input
                  type="checkbox"
                  checked={project.visibleLayers.has(layer.name)}
                  onchange={() => project.toggleLayer(layer.name)}
                  title="Active = included in pipeline + visible on canvas. Uncheck to deactivate."
                />
                <span class="swatch" style="background: {swatch(layer.color)}"></span>
                <span class="name">{layer.name}</span>
                <span class="count">{layer.segment_count}</span>
              </label>
              <button
                type="button"
                class="del-btn"
                onclick={() => project.removeImportedLayer(layer.name)}
                title="Delete this layer (drops every segment on it)"
                aria-label={`Delete layer ${layer.name}`}
              >
                ×
              </button>
            </li>
          {/each}
        </ul>
      {:else}
        <div class="empty-card">
          <p class="empty-title">No drawing yet</p>
          <p class="empty-sub">Import a DXF or SVG to see its layers here.</p>
          {#if onOpenFileClick}
            <button class="primary-cta" type="button" onclick={onOpenFileClick}>
              + Open file
            </button>
          {/if}
          {#if reopenPrompt}
            <div class="reopen">
              <span class="reopen-text">
                Reopen <strong>{reopenPrompt.filename}</strong>?
              </span>
              <div class="reopen-actions">
                <button class="reopen-accept" type="button" onclick={() => onReopenAccept?.()}>
                  Reopen
                </button>
                <button class="reopen-dismiss" type="button" onclick={() => onReopenDismiss?.()}>
                  Dismiss
                </button>
              </div>
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</aside>

<style>
  .layers {
    width: 100%;
    background: var(--bg-panel);
    color: var(--text);
    border-left: 1px solid var(--border);
    padding: 0.4rem 0.6rem 0.5rem;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }
  /* Header mirrors OperationsList's group-head for visual parity.
     Five-column layout: caret · all-visible toggle · filename / label ·
     count / file-stats · Add+ menu. */
  .group-head {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr) auto auto;
    gap: 0.3rem;
    align-items: center;
    padding: 0.2rem 0.35rem;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: color-mix(in srgb, var(--accent) 6%, var(--bg-panel));
    font-size: 0.78rem;
  }
  .add-menu {
    position: relative;
  }
  .add-menu.open {
    /* When open, lift the entire `.add-menu` into its own stacking
       context so the absolutely-positioned dropdown can paint above
       later sidebar rows (Text panel, Operations, etc.) instead of
       being painted under by their DOM-order. */
    z-index: 100;
  }
  .add-btn {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    color: var(--text);
    border-radius: 3px;
    padding: 0.15rem 0.4rem;
    font-size: 0.7rem;
    cursor: pointer;
    line-height: 1.2;
  }
  .add-btn:hover {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .add-menu.open .add-btn {
    background: var(--bg-elevated);
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .add-dropdown {
    position: absolute;
    top: 100%;
    right: 0;
    min-width: 200px;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.3);
    padding: 0.2rem;
    z-index: 60;
    display: flex;
    flex-direction: column;
    gap: 0.05rem;
    margin-top: 4px;
  }
  .add-item {
    display: flex;
    align-items: center;
    gap: 0.5rem;
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
  .add-item:hover {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .add-item .label {
    white-space: nowrap;
  }
  .caret-btn {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    cursor: pointer;
    padding: 0 0.2rem;
    font-size: 0.85rem;
    line-height: 1;
  }
  .group-name {
    color: var(--text-strong);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .group-count {
    color: var(--text-muted);
    font-size: 0.72rem;
    padding: 0 0.3rem;
    background: var(--bg);
    border-radius: 10px;
    line-height: 1.4;
  }
  .filename {
    color: var(--text-strong);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .file-stats {
    color: var(--text-muted);
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .group-body {
    margin: 0.2rem 0 0 0.5rem;
    padding-left: 0.3rem;
    border-left: 2px solid color-mix(in srgb, var(--accent) 30%, transparent);
    /* Cap so a huge layer set doesn't dominate; scrolls internally. */
    max-height: 28vh;
    overflow-y: auto;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    margin: 0.18rem 0;
  }
  li.layer-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: 0.2rem;
  }
  label,
  .layer-label {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.82rem;
    cursor: pointer;
    min-width: 0;
  }
  .del-btn {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 1rem;
    line-height: 1;
    padding: 0 0.3rem;
  }
  .del-btn:hover {
    color: var(--error);
  }
  input[type='checkbox'] {
    accent-color: var(--accent);
  }
  .swatch {
    width: 10px;
    height: 10px;
    border-radius: 2px;
    display: inline-block;
    border: 1px solid var(--border);
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-variant-numeric: tabular-nums;
    color: var(--text-faint);
    font-size: 0.72rem;
  }
  /* Empty-state card matches OperationsList's pattern for visual
     consistency across the sidebar (audit 0ki0). */
  .empty-card {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0.35rem;
    padding: 0.6rem;
    margin: 0.4rem 0;
    border: 1px dashed var(--border);
    border-radius: 5px;
    background: color-mix(in srgb, var(--accent) 4%, var(--bg-panel));
    text-align: center;
  }
  .empty-title {
    margin: 0;
    color: var(--text-strong);
    font-size: 0.82rem;
    font-weight: 600;
  }
  .empty-sub {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.72rem;
    line-height: 1.3;
  }
  .primary-cta {
    margin-top: 0.3rem;
    background: var(--accent);
    color: #fff;
    border: 0;
    padding: 0.35rem 0.7rem;
    border-radius: 4px;
    font-size: 0.82rem;
    font-weight: 600;
    cursor: pointer;
  }
  .primary-cta:hover {
    background: var(--accent-strong);
  }
  /* Startup reopen-last-project affordance. Lives below the Open file
     CTA so the no-drawing state surfaces both ways to get a project
     loaded in one place. */
  .reopen {
    margin-top: 0.45rem;
    padding-top: 0.45rem;
    border-top: 1px dashed var(--border);
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.72rem;
    color: var(--text-muted);
    text-align: left;
  }
  .reopen-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .reopen-text strong {
    color: var(--text-strong);
    font-weight: 600;
  }
  .reopen-actions {
    display: flex;
    gap: 0.3rem;
  }
  .reopen-accept,
  .reopen-dismiss {
    flex: 1;
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text);
    padding: 0.2rem 0.5rem;
    border-radius: 3px;
    cursor: pointer;
    font-size: 0.74rem;
  }
  .reopen-accept {
    background: var(--accent);
    color: #fff;
    border-color: var(--accent);
  }
  .reopen-accept:hover {
    background: var(--accent-strong);
  }
  .reopen-dismiss:hover {
    background: var(--bg-input);
  }
</style>
