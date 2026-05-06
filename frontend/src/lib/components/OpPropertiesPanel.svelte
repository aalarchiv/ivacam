<script lang="ts">
  /// Operation properties panel — bound to project.selectedOpId. Shows
  /// the kind-specific parameters of the selected op plus a tool picker
  /// fed from project.tools. Edits are pushed straight back through
  /// project.updateOperation, so the operation list updates instantly.

  import {
    project,
    type OpEntry,
    type ProfileOffset,
    type PocketStrategy,
  } from '../state/project.svelte';

  const op = $derived<OpEntry | null>(
    project.selectedOpId == null
      ? null
      : project.operations.find((o) => o.id === project.selectedOpId) ?? null,
  );

  function patch<K extends keyof OpEntry>(key: K, value: OpEntry[K]) {
    if (op) project.updateOperation(op.id, { [key]: value } as Partial<OpEntry>);
  }
</script>

<aside class="props">
  <h3>Properties</h3>

  {#if !op}
    <p class="empty">Select an operation in the list to edit it.</p>
  {:else}
    <label class="row">
      <span>Name</span>
      <input
        type="text"
        value={op.name}
        oninput={(e) => patch('name', (e.currentTarget as HTMLInputElement).value)}
      />
    </label>

    <label class="row">
      <span>Tool</span>
      <select
        value={op.toolId}
        onchange={(e) => patch('toolId', parseInt((e.currentTarget as HTMLSelectElement).value, 10))}
      >
        {#each project.tools as t (t.id)}
          <option value={t.id}>#{t.id} {t.name} ({t.diameter}mm)</option>
        {/each}
      </select>
    </label>

    <fieldset>
      <legend>Source</legend>
      <label class="row">
        <span>Mode</span>
        <select
          value={op.sourceObjects && op.sourceObjects.length > 0
            ? '_objects_'
            : op.sourceLayers === null
            ? '_all_'
            : '_layer_'}
          onchange={(e) => {
            const v = (e.currentTarget as HTMLSelectElement).value;
            if (v === '_all_') {
              patch('sourceLayers', null);
              patch('sourceObjects', undefined);
            } else if (v === '_layer_') {
              patch('sourceObjects', undefined);
              if (op && op.sourceLayers === null) patch('sourceLayers', []);
            } else {
              patch('sourceLayers', null);
              if (op && (op.sourceObjects?.length ?? 0) === 0)
                patch('sourceObjects', []);
            }
          }}
        >
          <option value="_all_">all imported geometry</option>
          <option value="_layer_">specific layer(s)</option>
          <option value="_objects_">selected objects</option>
        </select>
      </label>
      {#if op.sourceLayers !== null && (op.sourceObjects?.length ?? 0) === 0}
        <label class="row">
          <span>Layer</span>
          <select
            value={op.sourceLayers[0] ?? ''}
            onchange={(e) => patch('sourceLayers', [(e.currentTarget as HTMLSelectElement).value])}
          >
            <option value="">— pick a layer —</option>
            {#if project.imported}
              {#each project.imported.layers.filter((l) => l.segment_count > 0) as layer (layer.name)}
                <option value={layer.name}>"{layer.name}"</option>
              {/each}
            {/if}
          </select>
        </label>
      {:else if op.sourceObjects && op.sourceObjects.length > 0}
        <p class="hint">{op.sourceObjects.length} object(s) selected</p>
      {:else if op.sourceLayers === null}
        <p class="hint">runs on every chain in the import</p>
      {/if}
      <button
        class="from-selection"
        type="button"
        disabled={project.selectedObjects.size === 0}
        onclick={() => {
          patch('sourceLayers', null);
          patch('sourceObjects', [...project.selectedObjects]);
        }}
        title="Use the chains currently highlighted in the 2D pane"
      >Set from current selection ({project.selectedObjects.size})</button>
    </fieldset>

    <fieldset>
      <legend>Cut</legend>
      <label class="row">
        <span>Final depth</span>
        <input
          type="number" step="0.1" value={op.depth}
          onchange={(e) => patch('depth', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
        />
      </label>
      <label class="row">
        <span>Start depth</span>
        <input
          type="number" step="0.1" value={op.startDepth}
          onchange={(e) => patch('startDepth', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
        />
      </label>
      <label class="row">
        <span>Step / pass</span>
        <input
          type="number" step="0.1" value={op.step}
          onchange={(e) => patch('step', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
        />
      </label>
    </fieldset>

    {#if op.kind === 'profile'}
      <fieldset>
        <legend>Profile</legend>
        <label class="row">
          <span>Tool offset</span>
          <select
            value={op.offset}
            onchange={(e) => patch('offset', (e.currentTarget as HTMLSelectElement).value as ProfileOffset)}
          >
            <option value="outside">outside</option>
            <option value="inside">inside</option>
            <option value="on">on path</option>
          </select>
        </label>
      </fieldset>
    {:else if op.kind === 'pocket'}
      <fieldset>
        <legend>Pocket</legend>
        <label class="row">
          <span>Strategy</span>
          <select
            value={op.pocketStrategy ?? 'cascade'}
            onchange={(e) => patch('pocketStrategy', (e.currentTarget as HTMLSelectElement).value as PocketStrategy)}
          >
            <option value="cascade">cascade (concentric)</option>
            <option value="zigzag">zigzag (raster fill)</option>
            <option value="spiral">spiral</option>
          </select>
        </label>
      </fieldset>
    {/if}

    {#if op.kind === 'drill' || op.kind === 'thread' || op.kind === 'chamfer' || op.kind === 'helix'}
      <p class="empty">
        This operation kind is parsed but the gcode emitter for it ships
        with the next backend slice; the run will return
        <code>UnimplementedKind</code> for now.
      </p>
    {/if}
  {/if}
</aside>

<style>
  .props {
    width: 100%;
    height: 100%;
    background: var(--bg-panel);
    color: var(--text);
    border-left: 1px solid var(--border);
    overflow-y: auto;
    padding: 0.6rem 0.7rem 1rem;
    box-sizing: border-box;
    min-width: 0;
  }
  h3 {
    margin: 0 0 0.4rem 0;
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
  }
  .empty {
    color: var(--text-faint);
    font-size: 0.78rem;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 6.5rem) minmax(0, 1fr);
    gap: 0.5rem;
    align-items: center;
    margin: 0.2rem 0;
    font-size: 0.78rem;
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: 3px;
    margin: 0.4rem 0;
    padding: 0.3rem 0.5rem 0.4rem;
    background: var(--bg-elevated);
  }
  legend {
    font-size: 0.7rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0 0.3rem;
  }
  input,
  select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.32rem;
    font-size: 0.78rem;
    min-width: 0;
    width: 100%;
    box-sizing: border-box;
  }
  code {
    background: var(--bg-input);
    padding: 0 0.2rem;
    border-radius: 2px;
  }
  .hint {
    margin: 0.2rem 0 0;
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .from-selection {
    margin-top: 0.3rem;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.25rem 0.6rem;
    font-size: 0.74rem;
    cursor: pointer;
    width: 100%;
  }
  .from-selection:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
</style>
