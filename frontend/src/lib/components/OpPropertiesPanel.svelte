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
    type SourceCombine,
    type CutDirection,
  } from '../state/project.svelte';

  /// One-line description per cut direction for the option titles.
  const CUT_DIR_HELP: Record<CutDirection, string> = {
    conventional: 'Cutter rotation OPPOSES feed at contact. Safer on machines with backlash; chip starts thin.',
    climb: 'Cutter rotation matches feed at contact. Better surface finish; needs a stiff machine.',
  };
  const PLUNGE_HELP: Record<'direct' | 'ramp', string> = {
    direct:
      'Straight Z plunge into material. Safe for center-cutting end mills on shallow steps; risky on harder materials.',
    ramp:
      'Ramped descent: cutter walks forward along the path while Z descends, taking a chip in both directions. Required for non-center-cutting bits.',
  };

  /// Tooltip blurb per combine mode — kept short so it fits in a native
  /// option's `title` attribute (most browsers cut after ~2 lines).
  const COMBINE_HELP: Record<SourceCombine, string> = {
    auto: 'Containment-aware: nested closed objects become holes (outer + inner = annulus). Default.',
    union: 'Boolean union of all selected closed contours.',
    difference: 'First selected minus the union of the rest.',
    intersection: 'Boolean intersection of all selected closed contours.',
    xor: 'Symmetric difference (xor) of all selected closed contours.',
    none: 'No combination — one boundary per selected object, no holes.',
  };

  interface Props {
    /// True when rendered inline under an OperationsList row (drops the
    /// outer aside chrome + the standalone "Properties" header).
    embedded?: boolean;
  }
  let { embedded = false }: Props = $props();

  const op = $derived<OpEntry | null>(
    project.selectedOpId == null
      ? null
      : project.operations.find((o) => o.id === project.selectedOpId) ?? null,
  );

  function patch<K extends keyof OpEntry>(key: K, value: OpEntry[K]) {
    if (op) project.updateOperation(op.id, { [key]: value } as Partial<OpEntry>);
  }
</script>

<aside class="props" class:embedded>
  {#if !embedded}
    <h3>Properties</h3>
  {/if}

  {#if !op}
    {#if !embedded}
      <p class="empty">Select an operation in the list to edit it.</p>
    {/if}
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
      {#if (op.sourceObjects?.length ?? 0) > 1 || (op.sourceLayers !== null && op.sourceLayers.length > 0)}
        <label class="row" title={COMBINE_HELP[op.sourceCombine ?? 'auto']}>
          <span>Combine</span>
          <select
            value={op.sourceCombine ?? 'auto'}
            onchange={(e) =>
              patch('sourceCombine', (e.currentTarget as HTMLSelectElement).value as SourceCombine)}
          >
            <option value="auto" title={COMBINE_HELP.auto}>auto (containment)</option>
            <option value="union" title={COMBINE_HELP.union}>union</option>
            <option value="difference" title={COMBINE_HELP.difference}>difference</option>
            <option value="intersection" title={COMBINE_HELP.intersection}>intersection</option>
            <option value="xor" title={COMBINE_HELP.xor}>xor</option>
            <option value="none" title={COMBINE_HELP.none}>none (per object)</option>
          </select>
        </label>
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
      {#if op.kind === 'profile' || op.kind === 'pocket'}
        <label class="row" title={CUT_DIR_HELP[op.cutDirection ?? 'conventional']}>
          <span>Direction</span>
          <select
            value={op.cutDirection ?? 'conventional'}
            onchange={(e) =>
              patch('cutDirection', (e.currentTarget as HTMLSelectElement).value as CutDirection)}
          >
            <option value="conventional" title={CUT_DIR_HELP.conventional}>conventional</option>
            <option value="climb" title={CUT_DIR_HELP.climb}>climb</option>
          </select>
        </label>
        <label class="row" title={CUT_DIR_HELP[op.finishCutDirection ?? 'conventional']}>
          <span>Finish dir</span>
          <select
            value={op.finishCutDirection ?? 'conventional'}
            onchange={(e) =>
              patch('finishCutDirection', (e.currentTarget as HTMLSelectElement).value as CutDirection)}
          >
            <option value="conventional" title={CUT_DIR_HELP.conventional}>conventional</option>
            <option value="climb" title={CUT_DIR_HELP.climb}>climb</option>
          </select>
        </label>
        <label class="row" title={PLUNGE_HELP[op.plunge?.kind ?? 'direct']}>
          <span>Plunge</span>
          <select
            value={op.plunge?.kind ?? 'direct'}
            onchange={(e) => {
              const v = (e.currentTarget as HTMLSelectElement).value;
              if (v === 'ramp') {
                patch('plunge', {
                  kind: 'ramp',
                  angle_deg: op.plunge && op.plunge.kind === 'ramp' ? op.plunge.angle_deg : 3,
                });
              } else {
                patch('plunge', { kind: 'direct' });
              }
            }}
          >
            <option value="direct" title={PLUNGE_HELP.direct}>direct</option>
            <option value="ramp" title={PLUNGE_HELP.ramp}>ramp</option>
          </select>
        </label>
        {#if op.plunge && op.plunge.kind === 'ramp'}
          <label class="row" title="Ramp angle in degrees. 1°–5° is gentle, 10°+ is aggressive. The ramp's horizontal length is step / tan(angle).">
            <span>Ramp angle</span>
            <input
              type="number"
              step="0.5"
              min="0.5"
              max="45"
              value={op.plunge.angle_deg}
              onchange={(e) => {
                const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                if (!isNaN(v))
                  patch('plunge', { kind: 'ramp', angle_deg: Math.max(0.5, Math.min(45, v)) });
              }}
            />
          </label>
        {/if}
      {/if}
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
  .props.embedded {
    height: auto;
    border-left: 0;
    background: transparent;
    padding: 0.4rem 0.5rem 0.6rem 1.6rem;
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
