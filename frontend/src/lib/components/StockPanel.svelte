<script lang="ts">
  /// Stock settings — the always-present workpiece every layer / op
  /// attaches to. Phase qmbr of the stock-first rework redesigned this
  /// panel around the project's only piece of stock (a box):
  ///
  /// * Auto-bbox vs Manual as radio buttons.
  /// * Length / Width / Thickness labels (instead of bare X / Y / Z).
  /// * Auto mode: Length + Width are computed (greyed-out readouts);
  ///   Margin adds to both. Thickness is always user-editable.
  /// * Origin offsets X / Y / Z (Z reserved for future). All default 0.
  /// * Fixtures section dropped — sim still tracks them under the hood,
  ///   but the UI doesn't expose adding them any more.

  import { project } from '../state/project.svelte';
  import { computeFootprint } from '../sim/driver';

  function patch(p: Partial<typeof project.stock>) {
    project.setStock(p);
  }

  const footprint = $derived(
    computeFootprint(project.transformedImport, project.stock, project.machine.workArea),
  );
  const computedLength = $derived(Math.max(0, footprint.maxX - footprint.minX));
  const computedWidth = $derived(Math.max(0, footprint.maxY - footprint.minY));
</script>

<div class="stock">
  <fieldset class="mode">
    <legend>Mode</legend>
    <label class="radio">
      <input
        type="radio"
        name="stock-mode"
        value="auto"
        checked={project.stock.mode === 'auto'}
        onchange={() => patch({ mode: 'auto' })}
      />
      <span>Auto (fit to drawing)</span>
    </label>
    <label class="radio">
      <input
        type="radio"
        name="stock-mode"
        value="manual"
        checked={project.stock.mode === 'manual'}
        onchange={() => patch({ mode: 'manual' })}
      />
      <span>Manual</span>
    </label>
  </fieldset>

  <fieldset class="dims">
    <legend>Dimensions</legend>
    <label>
      <span>Length</span>
      <span class="field">
        {#if project.stock.mode === 'auto'}
          <input type="number" value={computedLength.toFixed(1)} readonly tabindex="-1" />
        {:else}
          <input
            type="number"
            step="0.5"
            min="1"
            value={project.stock.customX}
            onchange={(e) =>
              patch({ customX: parseFloat((e.target as HTMLInputElement).value) || 0 })}
          />
        {/if}
        <span class="unit">mm</span>
      </span>
    </label>
    <label>
      <span>Width</span>
      <span class="field">
        {#if project.stock.mode === 'auto'}
          <input type="number" value={computedWidth.toFixed(1)} readonly tabindex="-1" />
        {:else}
          <input
            type="number"
            step="0.5"
            min="1"
            value={project.stock.customY}
            onchange={(e) =>
              patch({ customY: parseFloat((e.target as HTMLInputElement).value) || 0 })}
          />
        {/if}
        <span class="unit">mm</span>
      </span>
    </label>
    <label>
      <span>Thickness</span>
      <span class="field">
        <input
          type="number"
          step="0.5"
          min="0.1"
          value={project.stock.thickness}
          onchange={(e) =>
            patch({ thickness: parseFloat((e.target as HTMLInputElement).value) || 0 })}
        />
        <span class="unit">mm</span>
      </span>
    </label>
    {#if project.stock.mode === 'auto'}
      <label>
        <span>Margin</span>
        <span class="field">
          <input
            type="number"
            step="0.5"
            min="0"
            value={project.stock.margin}
            onchange={(e) =>
              patch({ margin: parseFloat((e.target as HTMLInputElement).value) || 0 })}
            title="Adds to Length + Width (auto-fit case); Thickness is unaffected."
          />
          <span class="unit">mm</span>
        </span>
      </label>
    {/if}
  </fieldset>

  <fieldset class="origin">
    <legend>Origin offset</legend>
    <label>
      <span>X</span>
      <span class="field">
        <input
          type="number"
          step="0.5"
          value={project.stock.offsetX ?? 0}
          onchange={(e) =>
            patch({ offsetX: parseFloat((e.target as HTMLInputElement).value) || 0 })}
        />
        <span class="unit">mm</span>
      </span>
    </label>
    <label>
      <span>Y</span>
      <span class="field">
        <input
          type="number"
          step="0.5"
          value={project.stock.offsetY ?? 0}
          onchange={(e) =>
            patch({ offsetY: parseFloat((e.target as HTMLInputElement).value) || 0 })}
        />
        <span class="unit">mm</span>
      </span>
    </label>
    <label>
      <span>Z</span>
      <span class="field">
        <input
          type="number"
          step="0.5"
          value={project.stock.offsetZ ?? 0}
          onchange={(e) =>
            patch({ offsetZ: parseFloat((e.target as HTMLInputElement).value) || 0 })}
          title="Reserved — currently the pipeline assumes stock top at z = 0."
        />
        <span class="unit">mm</span>
      </span>
    </label>
  </fieldset>
</div>

<style>
  .stock {
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    padding: 0.2rem 0;
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.35rem 0.55rem 0.45rem;
    margin: 0;
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.3rem 0.5rem;
  }
  legend {
    grid-column: 1 / -1;
    padding: 0 0.3rem;
    color: var(--text-muted);
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  fieldset.mode {
    grid-template-columns: 1fr 1fr;
  }
  fieldset.mode .radio {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.78rem;
    color: var(--text);
    cursor: pointer;
  }
  fieldset.mode .radio input[type='radio'] {
    accent-color: var(--accent);
  }
  fieldset.dims label,
  fieldset.origin label {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    color: var(--text-muted);
    font-size: 0.72rem;
  }
  .field {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
  }
  .field input[type='number'] {
    flex: 1;
    min-width: 0;
    width: 100%;
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.35rem;
    font-size: 0.78rem;
  }
  .field input[readonly] {
    color: var(--text-muted);
    background: color-mix(in srgb, var(--bg-input) 70%, transparent);
    cursor: default;
  }
  .field .unit {
    font-size: 0.7rem;
    color: var(--text-muted);
  }
</style>
