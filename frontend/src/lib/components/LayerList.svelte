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
  }
  let { onOpenFileClick }: Props = $props();

  let collapsed = $state(false);

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

  let usableLayers = $derived(
    project.imported?.layers.filter((l) => l.segment_count > 0) ?? [],
  );

  let allVisible = $derived(
    usableLayers.length > 0
      && usableLayers.every((l) => project.visibleLayers.has(l.name)),
  );

  function setAllVisible(on: boolean) {
    for (const l of usableLayers) {
      const has = project.visibleLayers.has(l.name);
      if (has !== on) project.toggleLayer(l.name);
    }
  }
</script>

<aside class="layers">
  <div class="group-head">
    <button
      class="caret-btn"
      onclick={() => (collapsed = !collapsed)}
      title={collapsed ? 'Expand layers' : 'Collapse layers'}
      aria-label="Toggle layers panel"
    >{collapsed ? '▸' : '▾'}</button>
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
    <span class="group-name">Layers</span>
    <span class="group-count">{usableLayers.length}</span>
  </div>
  {#if !collapsed}
    <div class="group-body">
      {#if usableLayers.length > 0}
        <ul>
          {#each usableLayers as layer (layer.name)}
            <li>
              <label>
                <input
                  type="checkbox"
                  checked={project.visibleLayers.has(layer.name)}
                  onchange={() => project.toggleLayer(layer.name)}
                />
                <span class="swatch" style="background: {swatch(layer.color)}"></span>
                <span class="name">{layer.name}</span>
                <span class="count">{layer.segment_count}</span>
              </label>
            </li>
          {/each}
        </ul>
      {:else}
        <div class="empty-hint">
          <p>No file loaded.</p>
          {#if onOpenFileClick}
            <button class="link" type="button" onclick={onOpenFileClick}>Open a DXF or SVG…</button>
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
  /* Header mirrors OperationsList's group-head for visual parity. */
  .group-head {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr) auto;
    gap: 0.3rem;
    align-items: center;
    padding: 0.2rem 0.35rem;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: color-mix(in srgb, var(--accent) 6%, var(--bg-panel));
    font-size: 0.78rem;
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
  label {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.82rem;
    cursor: pointer;
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
  .empty-hint {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.4rem;
    padding: 0.3rem 0;
  }
  .empty-hint p {
    margin: 0;
    color: var(--text-faint);
    font-size: 0.82rem;
  }
  .link {
    background: transparent;
    border: 0;
    padding: 0;
    color: var(--accent-strong);
    text-decoration: underline;
    cursor: pointer;
    font-size: 0.8rem;
  }
  .link:hover {
    color: var(--accent);
  }
</style>
