<script lang="ts">
  import { project } from '../state/project.svelte';
  import { _ } from 'svelte-i18n';

  let visible = $derived(project.stock.visible);
  function patch(p: Partial<typeof project.stock>) {
    project.stock = { ...project.stock, ...p };
  }
</script>

<div class="stock">
  <label class="row toggle">
    <input
      type="checkbox"
      checked={visible}
      onchange={(e) => patch({ visible: (e.currentTarget as HTMLInputElement).checked })}
    />
    <span>{$_('stock.title')}</span>
  </label>
  {#if visible}
    <div class="row">
      <span class="lbl">{$_('stock.mode')}</span>
      <select
        value={project.stock.mode}
        onchange={(e) =>
          patch({ mode: ((e.target as HTMLSelectElement).value === 'manual'
            ? 'manual'
            : 'auto') })}
      >
        <option value="auto">{$_('stock.auto')}</option>
        <option value="manual">{$_('stock.manual')}</option>
      </select>
    </div>
    {#if project.stock.mode === 'auto'}
      <div class="row">
        <span class="lbl">{$_('stock.margin')}</span>
        <input
          type="number"
          step="0.1"
          value={project.stock.margin}
          onchange={(e) => patch({ margin: parseFloat((e.target as HTMLInputElement).value) })}
        />
      </div>
    {:else}
      <div class="row">
        <span class="lbl">X (mm)</span>
        <input
          type="number"
          step="0.1"
          value={project.stock.customX}
          onchange={(e) => patch({ customX: parseFloat((e.target as HTMLInputElement).value) })}
        />
      </div>
      <div class="row">
        <span class="lbl">Y (mm)</span>
        <input
          type="number"
          step="0.1"
          value={project.stock.customY}
          onchange={(e) => patch({ customY: parseFloat((e.target as HTMLInputElement).value) })}
        />
      </div>
      <div class="row">
        <span class="lbl">{$_('stock.thickness')}</span>
        <input
          type="number"
          step="0.1"
          value={project.stock.thickness}
          onchange={(e) => patch({ thickness: parseFloat((e.target as HTMLInputElement).value) })}
        />
      </div>
    {/if}
  {/if}
</div>

<style>
  .stock {
    display: grid;
    gap: 0.25rem;
    padding: 0.4rem 0;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0.5rem;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 4.5rem) minmax(0, 1fr);
    gap: 0.4rem;
    align-items: center;
  }
  .row.toggle {
    grid-template-columns: auto auto;
    justify-content: start;
    gap: 0.4rem;
  }
  .lbl {
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  select,
  input[type='number'] {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.35rem;
    font-size: 0.78rem;
    min-width: 0;
  }
  input[type='checkbox'] {
    accent-color: var(--accent);
  }
</style>
