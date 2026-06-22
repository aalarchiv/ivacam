<script lang="ts">
  /// Pattern (repeat-this-op) fieldset. Universal — applies to every
  /// op kind. Styles inherited from OpPropertiesPanel's :global(.props ...)
  /// rules.
  import {
    project,
    type OpEntry,
    type OpField,
    type OpFieldValue,
    type PatternConfig,
  } from '../../state/project.svelte';
  import { t } from '../../i18n';

  interface Props {
    op: OpEntry;
    /// Kind-aware patch — see ChamferSection for rationale.
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();

  /// Compute the bbox center of the currently-selected imported
  /// objects and write it into the polar-pattern center fields.
  /// No-op when nothing is selected (button is disabled in that
  /// state). Uses `object_meta[id-1].bbox` so the math matches the
  /// pipeline / box-select hit-test (data-space, not pixels).
  function setCenterFromSelection(pol: Extract<PatternConfig, { kind: 'polar' }>) {
    const meta = project.transformedImport?.object_meta ?? [];
    const sel = project.sel.selectedObjects;
    if (sel.size === 0 || meta.length === 0) return;
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const id of sel) {
      const m = meta.find((mm) => mm.id === id);
      if (!m) continue;
      if (m.bbox.min_x < minX) minX = m.bbox.min_x;
      if (m.bbox.min_y < minY) minY = m.bbox.min_y;
      if (m.bbox.max_x > maxX) maxX = m.bbox.max_x;
      if (m.bbox.max_y > maxY) maxY = m.bbox.max_y;
    }
    if (!Number.isFinite(minX)) return;
    patch('pattern', {
      ...pol,
      centerX: (minX + maxX) * 0.5,
      centerY: (minY + maxY) * 0.5,
    });
  }
</script>

<fieldset>
  <legend>{t('ops.pattern.legend')}</legend>
  <p class="hint">
    Run this operation once per pattern instance with the source geometry translated or rotated. The
    original geometry stays at the (0, 0) / 0° instance — single-count patterns are equivalent to no
    pattern.
  </p>
  <label
    class="row"
    title="Pattern shape — Linear array, rectangular Grid, or Polar (rotational) array."
  >
    <span>Pattern</span>
    <select
      value={op.pattern?.kind ?? 'none'}
      onchange={(e) => {
        const v = (e.currentTarget as HTMLSelectElement).value;
        if (v === 'none') {
          patch('pattern', undefined);
        } else if (v === 'linear') {
          patch('pattern', { kind: 'linear', count: 2, dx: 10, dy: 0 });
        } else if (v === 'grid') {
          patch('pattern', { kind: 'grid', countX: 2, countY: 2, dx: 10, dy: 10 });
        } else if (v === 'polar') {
          patch('pattern', {
            kind: 'polar',
            count: 4,
            centerX: 0,
            centerY: 0,
            angleStepDeg: 90,
          });
        }
      }}
    >
      <option value="none">{t('ops.pattern.kind.none')}</option>
      <option value="linear">{t('ops.pattern.kind.linear')}</option>
      <option value="grid">{t('ops.pattern.kind.grid')}</option>
      <option value="polar">{t('ops.pattern.kind.polar')}</option>
    </select>
  </label>
  {#if op.pattern?.kind === 'linear'}
    {@const lin = op.pattern}
    <label
      class="row"
      title="Total number of instances along the array, including the original at offset (0, 0)."
    >
      <span>Count</span>
      <input
        type="number"
        min="1"
        step="1"
        value={lin.count}
        onchange={(e) => {
          const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
          if (Number.isFinite(v) && v >= 1) patch('pattern', { ...lin, count: v });
        }}
      />
    </label>
    <label class="row" title="X offset between consecutive instances (mm).">
      <span>Δx</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={lin.dx}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...lin, dx: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label class="row" title="Y offset between consecutive instances (mm).">
      <span>Δy</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={lin.dy}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...lin, dy: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
  {:else if op.pattern?.kind === 'grid'}
    {@const grid = op.pattern}
    <label class="row" title="Instances along the X axis.">
      <span>Count X</span>
      <input
        type="number"
        min="1"
        step="1"
        value={grid.countX}
        onchange={(e) => {
          const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
          if (Number.isFinite(v) && v >= 1) patch('pattern', { ...grid, countX: v });
        }}
      />
    </label>
    <label class="row" title="Instances along the Y axis.">
      <span>Count Y</span>
      <input
        type="number"
        min="1"
        step="1"
        value={grid.countY}
        onchange={(e) => {
          const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
          if (Number.isFinite(v) && v >= 1) patch('pattern', { ...grid, countY: v });
        }}
      />
    </label>
    <label class="row" title="X spacing between grid columns (mm).">
      <span>Δx</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={grid.dx}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...grid, dx: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label class="row" title="Y spacing between grid rows (mm).">
      <span>Δy</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={grid.dy}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...grid, dy: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
  {:else if op.pattern?.kind === 'polar'}
    {@const pol = op.pattern}
    <label class="row" title="Total instances around the center, including the original at 0°.">
      <span>Count</span>
      <input
        type="number"
        min="1"
        step="1"
        value={pol.count}
        onchange={(e) => {
          const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
          if (Number.isFinite(v) && v >= 1) patch('pattern', { ...pol, count: v });
        }}
      />
    </label>
    <label class="row" title="X coordinate of the rotation center (mm).">
      <span>Center X</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={pol.centerX}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...pol, centerX: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label class="row" title="Y coordinate of the rotation center (mm).">
      <span>Center Y</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={pol.centerY}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...pol, centerY: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label
      class="row"
      title="Angle between consecutive instances (degrees). 360 / count for a full revolution."
    >
      <span>Step</span>
      <div class="num-cell">
        <input
          type="number"
          step="1"
          value={pol.angleStepDeg}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...pol, angleStepDeg: v });
          }}
        />
        <span class="unit">°</span>
      </div>
    </label>
    <label
      class="row"
      title="Angle of the first instance — shifts the whole ring so it doesn't have to start at 0°."
    >
      <span>Start</span>
      <div class="num-cell">
        <input
          type="number"
          step="1"
          value={pol.startAngleDeg ?? 0}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...pol, startAngleDeg: v });
          }}
        />
        <span class="unit">°</span>
      </div>
    </label>
    <div class="row">
      <span></span>
      <button
        type="button"
        class="center-btn"
        onclick={() => setCenterFromSelection(pol)}
        disabled={project.sel.selectedObjects.size === 0}
        title={project.sel.selectedObjects.size === 0
          ? 'Select one or more objects on the canvas first.'
          : 'Compute center X / Y as the bbox center of the currently selected objects.'}
      >
        Set center from selection
      </button>
    </div>
  {/if}
</fieldset>

<style>
  .center-btn {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    color: var(--text);
    border-radius: 3px;
    padding: 0.2rem 0.55rem;
    font-size: 0.74rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .center-btn:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .center-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
