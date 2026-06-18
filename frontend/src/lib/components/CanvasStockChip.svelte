<script lang="ts">
  /// Phone-only on-canvas "Stock settings" chip. On narrow screens the
  /// desktop Stock/Layers sidebar is gone; the on-canvas stock gizmo
  /// handles X/Y size + position by dragging, but the remaining numeric
  /// stock fields (mode, thickness, margin, Z offset) and the work
  /// offset (WCS zero) have no touch surface. This chip folds those onto
  /// the canvas as a corner popover, mirroring CanvasLayersChip's pattern.
  ///
  /// All mutations route through the same `project` store as the desktop
  /// StockPanel (`setStock` / `setWorkOffset`, both undoable via the
  /// command bus), so the two surfaces stay in lockstep — this is purely
  /// an alternate, touch-first presentation of StockPanel's fields.
  ///
  /// INTEGRATOR NOTE: positioned `left: 0.5rem; bottom: 0.5rem`, the same
  /// anchor CanvasLayersChip uses. If both render together the integrator
  /// must shift one (e.g. bump this chip's `left` past the Layers chip, or
  /// stack it above) so they don't overlap.

  import { project } from '../state/project.svelte';
  import { computeFootprint } from '../sim/driver';
  import { parseFiniteNumber } from '../cam/units';
  import { inferDefaultWorkOffset, type Wcs, type WorkOffset } from '../state/project-types';

  let open = $state(false);

  function patch(p: Partial<typeof project.data.stock>) {
    project.setStock(p);
  }
  function patchWorkOffset(p: Partial<WorkOffset>) {
    project.setWorkOffset(p);
  }

  const WCS_OPTIONS: Wcs[] = ['G54', 'G55', 'G56', 'G57', 'G58', 'G59'];

  /// Single shared invalid-key marker — set when the most recent commit
  /// for the keyed field was rejected; drives the `.invalid` red border.
  /// Same transient-flash shape StockPanel uses; the `wo:` prefix keeps
  /// the work-offset namespace separate from stock fields.
  let invalidKey = $state<string | null>(null);

  function commitStockNumber(
    key: keyof typeof project.data.stock,
    raw: string,
    opts: { min?: number; allowNegative?: boolean } = {},
  ): boolean {
    const minimum = opts.allowNegative ? undefined : (opts.min ?? 0);
    const parsed = parseFiniteNumber(raw, { min: minimum });
    if (parsed.value == null) return false;
    patch({ [key]: parsed.value });
    return true;
  }

  function onStockNumberChange(
    key: keyof typeof project.data.stock,
    e: Event,
    opts: { min?: number; allowNegative?: boolean } = {},
  ) {
    const ok = commitStockNumber(key, (e.target as HTMLInputElement).value, opts);
    invalidKey = ok ? null : key;
  }

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

  /// Snap the WCS origin to the geometry bbox bottom-left — same handler
  /// logic as StockPanel.snapWorkOffsetToBboxMin. No-op without geometry.
  function snapWorkOffsetToBboxMin() {
    const imp = project.transformedImport;
    if (!imp) return;
    const candidate = inferDefaultWorkOffset(imp.bbox, {
      x_mm: 0,
      y_mm: 0,
      z_mm: 0,
      wcs: project.data.workOffset.wcs,
    });
    patchWorkOffset({ x_mm: candidate.x_mm, y_mm: candidate.y_mm });
  }

  const footprint = $derived(
    computeFootprint(project.transformedImport, project.data.stock, project.data.machine.workArea),
  );
  const computedLength = $derived(Math.max(0, footprint.maxX - footprint.minX));
  const computedWidth = $derived(Math.max(0, footprint.maxY - footprint.minY));
  /// Read-only footprint readout for popover context — the manual dims
  /// the user set, or the auto-computed extents.
  const footprintLabel = $derived(
    project.data.stock.mode === 'manual'
      ? `${project.data.stock.customX.toFixed(1)} × ${project.data.stock.customY.toFixed(1)} mm`
      : `${computedLength.toFixed(1)} × ${computedWidth.toFixed(1)} mm`,
  );

  function onWindowPointer(e: MouseEvent) {
    if (!open) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest('.canvas-stock-chip')) return;
    open = false;
  }
  function onWindowKey(e: KeyboardEvent) {
    if (e.key === 'Escape' && open) {
      e.preventDefault();
      open = false;
    }
  }
</script>

<svelte:window onclick={onWindowPointer} onkeydown={onWindowKey} />

<div class="canvas-stock-chip">
  <button
    type="button"
    class="chip-trigger"
    aria-haspopup="menu"
    aria-expanded={open}
    aria-label="Stock settings"
    title="Stock settings"
    onclick={() => (open = !open)}
  >
    <span class="chip-glyph" aria-hidden="true">▭</span>
    <span class="chip-count">{footprintLabel}</span>
  </button>

  {#if open}
    <div class="chip-menu" role="menu" aria-label="Stock settings">
      <div class="grp" role="group" aria-label="Mode">
        <span class="grp-label">Mode</span>
        <div class="radios">
          <label class="radio">
            <input
              type="radio"
              name="canvas-stock-mode"
              value="auto"
              checked={project.data.stock.mode === 'auto'}
              onchange={() => patch({ mode: 'auto' })}
            />
            <span>Auto</span>
          </label>
          <label class="radio">
            <input
              type="radio"
              name="canvas-stock-mode"
              value="manual"
              checked={project.data.stock.mode === 'manual'}
              onchange={() => patch({ mode: 'manual' })}
            />
            <span>Manual</span>
          </label>
        </div>
      </div>

      <div class="readout">
        <span class="grp-label">Size</span>
        <span class="readout-val">{footprintLabel}</span>
      </div>

      <label class="row">
        <span class="rlabel">Thickness</span>
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
        <label class="row">
          <span class="rlabel">Margin</span>
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

      <label class="row">
        <span class="rlabel">Z offset</span>
        <span class="field">
          <input
            type="number"
            step="0.5"
            value={project.data.stock.offsetZ ?? 0}
            class:invalid={invalidKey === 'offsetZ'}
            onchange={(e) => onStockNumberChange('offsetZ', e, { allowNegative: true })}
            title="Z of the stock top plane (mm). 0 = top at the WCS origin. Positive raises the stock above z=0."
          />
          <span class="unit">mm</span>
        </span>
      </label>

      <div class="menu-sep" role="separator"></div>

      <div class="grp-label">Work origin (WCS)</div>
      <label class="row">
        <span class="rlabel">WCS</span>
        <span class="field">
          <select
            value={project.data.workOffset.wcs}
            onchange={(e) =>
              patchWorkOffset({ wcs: (e.currentTarget as HTMLSelectElement).value as Wcs })}
          >
            {#each WCS_OPTIONS as w (w)}
              <option value={w}>{w}</option>
            {/each}
          </select>
        </span>
      </label>
      <label class="row">
        <span class="rlabel">X</span>
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
      <label class="row">
        <span class="rlabel">Y</span>
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
      <button
        type="button"
        class="snap-btn"
        onclick={snapWorkOffsetToBboxMin}
        disabled={!project.transformedImport}
        title="Snap the WCS origin to the bottom-left of the imported drawing's bounding box."
      >
        Snap to bbox bottom-left
      </button>
    </div>
  {/if}
</div>

<style>
  /* Positioned by the parent .canvas-chip-dock (shared with the Layers
     chip); the chip is a relative box so its popover anchors to it. */
  .canvas-stock-chip {
    position: relative;
  }
  .chip-trigger {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    min-height: 2rem;
    padding: 0 0.55rem;
    border-radius: 1rem;
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text);
    opacity: 0.85;
    cursor: pointer;
    transition:
      opacity 0.12s,
      color 0.12s;
  }
  .chip-trigger:hover,
  .chip-trigger:focus-visible {
    opacity: 1;
    color: var(--text-strong);
  }
  .chip-glyph {
    font-size: 0.95rem;
    line-height: 1;
  }
  .chip-count {
    font-size: 0.78rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .chip-menu {
    position: absolute;
    left: 0;
    bottom: calc(100% + 0.35rem);
    min-width: 14rem;
    max-width: min(20rem, 86vw);
    max-height: 70vh;
    overflow-y: auto;
    padding: 0.45rem;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 6px 22px rgb(0 0 0 / 35%);
  }

  .grp-label {
    color: var(--text-muted);
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .grp {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    min-height: 44px;
  }
  .radios {
    display: inline-flex;
    gap: 0.75rem;
  }
  .radio {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.82rem;
    color: var(--text);
    cursor: pointer;
  }
  .radio input[type='radio'] {
    accent-color: var(--accent);
    width: 1.1rem;
    height: 1.1rem;
  }

  .readout {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .readout-val {
    font-size: 0.8rem;
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    min-height: 44px;
    color: var(--text-muted);
    font-size: 0.78rem;
  }
  .rlabel {
    flex: 0 0 auto;
  }
  .field {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    flex: 1 1 auto;
    min-width: 0;
    max-width: 9rem;
  }
  .field input[type='number'],
  .field select {
    flex: 1;
    min-width: 0;
    width: 100%;
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.3rem 0.35rem;
    font-size: 0.82rem;
  }
  .field input.invalid {
    border-color: var(--danger);
  }
  .field .unit {
    font-size: 0.7rem;
    color: var(--text-muted);
  }

  .menu-sep {
    height: 1px;
    margin: 0.2rem 0.1rem;
    background: var(--border);
  }

  .snap-btn {
    min-height: 44px;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 0.2rem 0.5rem;
    font-size: 0.8rem;
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
</style>
