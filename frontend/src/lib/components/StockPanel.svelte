<script lang="ts">
  /// Stock settings — the always-present workpiece every layer / op
  /// attaches to. The stock-first rework redesigned this
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
  import { parseFiniteNumber } from '../cam/units';
  import { inferDefaultWorkOffset, type Wcs, type WorkOffset } from '../state/project-types';

  function patch(p: Partial<typeof project.data.stock>) {
    project.setStock(p);
  }
  function patchWorkOffset(p: Partial<WorkOffset>) {
    project.setWorkOffset(p);
  }
  /// Snap the WCS origin to the geometry bbox's bottom-left corner —
  /// the canonical CNC-zero default the import-time helper applies.
  /// Bound to the "Set to bbox bottom-left" button below + the warnings-
  /// panel Apply-Fix path. No-op when no geometry is loaded.
  function snapWorkOffsetToBboxMin() {
    const imp = project.transformedImport;
    if (!imp) return;
    // Pass a "fresh default" so the helper unconditionally computes
    // the snap target even though the user's current offset isn't
    // default (the inference defaults are about FRESH IMPORTS; this
    // button is the user explicitly asking for the snap regardless).
    const candidate = inferDefaultWorkOffset(imp.bbox, {
      x_mm: 0,
      y_mm: 0,
      z_mm: 0,
      wcs: project.data.workOffset.wcs,
    });
    patchWorkOffset({ x_mm: candidate.x_mm, y_mm: candidate.y_mm });
  }

  const WCS_OPTIONS: Wcs[] = ['G54', 'G55', 'G56', 'G57', 'G58', 'G59'];

  /// Wrap a stock-field onchange handler with shared invalid-feedback
  /// behavior: on a parse failure / out-of-range value the input keeps
  /// the rejected value but doesn't commit it, and the `.invalid` class
  /// renders a red border so the user sees their input was refused.
  /// Without this the old `parseFloat(v) || 0` patterns silently
  /// snapped stock dims to 0 when the user typed "abc" or a negative.
  function commitStockNumber(
    key: keyof typeof project.data.stock,
    raw: string,
    opts: { min?: number; allowNegative?: boolean } = {},
  ) {
    const minimum = opts.allowNegative ? undefined : (opts.min ?? 0);
    const parsed = parseFiniteNumber(raw, { min: minimum });
    if (parsed.value == null) return false;
    patch({ [key]: parsed.value });
    return true;
  }

  /// Reactive marker — set when the most recent commit attempt for the
  /// keyed field returned `invalid`. Drives the `.invalid` class.
  /// Resets per-field on a successful commit. The single-shared
  /// invalid-key avoids piling per-field $state slots for what is
  /// essentially a transient validation flash.
  let invalidKey = $state<string | null>(null);

  function onStockNumberChange(
    key: keyof typeof project.data.stock,
    e: Event,
    opts: { min?: number; allowNegative?: boolean } = {},
  ) {
    const ok = commitStockNumber(key, (e.target as HTMLInputElement).value, opts);
    invalidKey = ok ? null : key;
  }
  /// Same invalid-feedback shape as `onStockNumberChange`, but writes
  /// through `setWorkOffset`. The `wo:` prefix keeps the invalid-key
  /// namespace separate so a stock-thickness validation flash doesn't
  /// also red-border the WCS-X input.
  function onWorkOffsetNumberChange(key: 'x_mm' | 'y_mm', e: Event) {
    const parsed = parseFiniteNumber((e.target as HTMLInputElement).value);
    const ns = `wo:${key}`;
    if (parsed.value == null) {
      invalidKey = ns;
      return;
    }
    invalidKey = null;
    patchWorkOffset({ [key]: parsed.value });
  }

  const footprint = $derived(
    computeFootprint(project.stockSizingImport, project.data.stock, project.data.machine.workArea),
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
        checked={project.data.stock.mode === 'auto'}
        onchange={() => patch({ mode: 'auto' })}
      />
      <span>Auto (fit to drawing)</span>
    </label>
    <label class="radio">
      <input
        type="radio"
        name="stock-mode"
        value="manual"
        checked={project.data.stock.mode === 'manual'}
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
        {#if project.data.stock.mode === 'auto'}
          <input type="number" value={computedLength.toFixed(1)} readonly tabindex="-1" />
        {:else}
          <input
            type="number"
            step="0.5"
            min="1"
            value={project.data.stock.customX}
            class:invalid={invalidKey === 'customX'}
            onchange={(e) => onStockNumberChange('customX', e, { min: 1 })}
          />
        {/if}
        <span class="unit">mm</span>
      </span>
    </label>
    <label>
      <span>Width</span>
      <span class="field">
        {#if project.data.stock.mode === 'auto'}
          <input type="number" value={computedWidth.toFixed(1)} readonly tabindex="-1" />
        {:else}
          <input
            type="number"
            step="0.5"
            min="1"
            value={project.data.stock.customY}
            class:invalid={invalidKey === 'customY'}
            onchange={(e) => onStockNumberChange('customY', e, { min: 1 })}
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
          value={project.data.stock.thickness}
          class:invalid={invalidKey === 'thickness'}
          onchange={(e) => onStockNumberChange('thickness', e, { min: 0.1 })}
        />
        <span class="unit">mm</span>
      </span>
    </label>
    {#if project.data.stock.mode === 'auto'}
      <label>
        <span>Margin</span>
        <span class="field">
          <input
            type="number"
            step="0.5"
            min="0"
            value={project.data.stock.margin}
            class:invalid={invalidKey === 'margin'}
            onchange={(e) => onStockNumberChange('margin', e, { min: 0 })}
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
          value={project.data.stock.offsetX ?? 0}
          class:invalid={invalidKey === 'offsetX'}
          onchange={(e) => onStockNumberChange('offsetX', e, { allowNegative: true })}
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
          value={project.data.stock.offsetY ?? 0}
          class:invalid={invalidKey === 'offsetY'}
          onchange={(e) => onStockNumberChange('offsetY', e, { allowNegative: true })}
        />
        <span class="unit">mm</span>
      </span>
    </label>
    <!-- Stock-top Z. 0 = top at the WCS origin plane (you zeroed on
         the stock top). A positive value raises the stock above z=0 (e.g.
         you zeroed on the bed → set this to the stock thickness). Drives
         the 3D stock box, the sim heightmap top, and the out-of-stock
         scan. Distinct from the WCS Z offset below (which moves the
         origin, not the material). -->
    <label>
      <span>Z</span>
      <span class="field">
        <input
          type="number"
          step="0.5"
          value={project.data.stock.offsetZ ?? 0}
          class:invalid={invalidKey === 'offsetZ'}
          onchange={(e) => onStockNumberChange('offsetZ', e, { allowNegative: true })}
          title="Z of the stock top plane (mm). 0 = top at the WCS origin (zeroed on the stock top). Positive raises the stock above z=0 — e.g. set it to the stock thickness if you zeroed on the bed. Shifts the 3D stock box, sim heightmap, and out-of-stock check."
        />
        <span class="unit">mm</span>
      </span>
    </label>
  </fieldset>

  <!-- Work-coordinate-system origin (per-project). Where on the
       drawing the operator zeroed the machine. Fresh imports auto-default
       to bbox bottom-left; this section lets the user pick a
       different WCS slot (G54..G59) or nudge the origin manually. -->
  <fieldset class="wcs">
    <legend
      title="The WCS origin — where on the imported drawing the operator zeroed the machine. Cuts are emitted relative to this point; if it falls outside the geometry bbox the simulator warns. Auto-defaults to the bbox bottom-left on fresh import."
      >Work origin (WCS)</legend
    >
    <label>
      <span>WCS</span>
      <span class="field">
        <select
          value={project.data.workOffset.wcs}
          onchange={(e) =>
            patchWorkOffset({
              wcs: (e.currentTarget as HTMLSelectElement).value as Wcs,
            })}
        >
          {#each WCS_OPTIONS as w (w)}
            <option value={w}>{w}</option>
          {/each}
        </select>
      </span>
    </label>
    <label>
      <span>X</span>
      <span class="field">
        <input
          type="number"
          step="0.5"
          value={project.data.workOffset.x_mm}
          class:invalid={invalidKey === 'wo:x_mm'}
          onchange={(e) => onWorkOffsetNumberChange('x_mm', e)}
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
          value={project.data.workOffset.y_mm}
          class:invalid={invalidKey === 'wo:y_mm'}
          onchange={(e) => onWorkOffsetNumberChange('y_mm', e)}
        />
        <span class="unit">mm</span>
      </span>
    </label>
    <!-- Z spinner intentionally omitted. work_offset.z_mm stays
         in the wire format + cache key for forward-compat, but the sim
         and pipeline currently treat stock-top as z=0, so exposing a
         spinner that does nothing was actively misleading. Restore the
         label + input when the pipeline grows real Z-offset support. -->
    <button
      type="button"
      class="snap-btn"
      onclick={snapWorkOffsetToBboxMin}
      disabled={!project.transformedImport}
      title="Snap the WCS origin to the bottom-left of the imported drawing's bounding box — the canonical CNC zeroing convention."
    >
      Snap to bbox bottom-left
    </button>
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
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
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
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
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
  fieldset.origin label,
  fieldset.wcs label {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    color: var(--text-muted);
    font-size: 0.72rem;
  }
  /* WCS section uses the same 2-col grid as Origin offset but adds a
     full-width snap-button row beneath. The select gets the same field
     wrapper styling as the number inputs. */
  fieldset.wcs .field select {
    flex: 1;
    min-width: 0;
    width: 100%;
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 0.15rem 0.3rem;
    font-size: 0.78rem;
  }
  .snap-btn {
    margin-top: 0.3rem;
    grid-column: 1 / -1;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 0.2rem 0.5rem;
    font-size: 0.74rem;
    cursor: pointer;
  }
  .snap-btn:hover:not(:disabled) {
    background: var(--hover-bg-elevated);
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .snap-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
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
  .field input.invalid {
    border-color: var(--danger);
  }
  .field .unit {
    font-size: 0.7rem;
    color: var(--text-muted);
  }
</style>
