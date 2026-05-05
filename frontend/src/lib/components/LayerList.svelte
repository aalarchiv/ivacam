<script lang="ts">
  import { project } from '../state/project.svelte';

  const ACI: Record<number, string> = {
    1: '#ff0000',
    2: '#ffff00',
    3: '#00ff00',
    4: '#00ffff',
    5: '#0000ff',
    6: '#ff00ff',
    7: '#e6e6e6',
  };
  const swatch = (c: number) => ACI[c] ?? '#bbbbbb';
</script>

<aside class="layers">
  <h3>Layers</h3>
  {#if project.imported && project.imported.layers.length > 0}
    <ul>
      {#each project.imported.layers as layer (layer.name)}
        <li>
          <label>
            <input
              type="checkbox"
              checked={project.visibleLayers.has(layer.name)}
              onchange={() => project.toggleLayer(layer.name)}
            />
            <span class="swatch" style:background={swatch(layer.color)}></span>
            <span class="name">{layer.name}</span>
            <span class="count">{layer.segment_count}</span>
          </label>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="empty">No file loaded.</p>
  {/if}
</aside>

<style>
  .layers {
    width: 100%;
    height: 100%;
    background: #161616;
    color: #d6d6d6;
    border-left: 1px solid #2b2b2b;
    overflow-y: auto;
    padding: 0.75rem 0.75rem 1rem;
    box-sizing: border-box;
  }
  h3 {
    margin: 0 0 0.5rem 0;
    font-size: 0.85rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #888;
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
    accent-color: #2d6cdf;
  }
  .swatch {
    width: 10px;
    height: 10px;
    border-radius: 2px;
    display: inline-block;
    border: 1px solid #2b2b2b;
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-variant-numeric: tabular-nums;
    color: #777;
    font-size: 0.75rem;
  }
  .empty {
    color: #666;
    font-size: 0.85rem;
  }
</style>
