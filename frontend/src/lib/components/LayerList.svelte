<script lang="ts">
  import { project } from '../state/project.svelte';

  interface Props {
    onOpenFileClick?: () => void;
  }
  let { onOpenFileClick }: Props = $props();

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
</script>

<aside class="layers">
  <h3>Layers</h3>
  {#if project.imported && project.imported.layers.some((l) => l.segment_count > 0)}
    <ul>
      {#each project.imported.layers.filter((l) => l.segment_count > 0) as layer (layer.name)}
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
</aside>

<style>
  .layers {
    width: 100%;
    height: 100%;
    background: var(--bg-panel);
    color: var(--text);
    border-left: 1px solid var(--border);
    overflow-y: auto;
    padding: 0.75rem 0.75rem 1rem;
    box-sizing: border-box;
  }
  h3 {
    margin: 0 0 0.5rem 0;
    font-size: 0.85rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    margin-bottom: 0.25rem;
  }
  label {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.85rem;
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
    font-size: 0.75rem;
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
    font-size: 0.85rem;
  }
  .link {
    background: transparent;
    border: 0;
    padding: 0;
    color: var(--accent-strong);
    text-decoration: underline;
    cursor: pointer;
    font-size: 0.82rem;
  }
  .link:hover {
    color: var(--accent);
  }
</style>
