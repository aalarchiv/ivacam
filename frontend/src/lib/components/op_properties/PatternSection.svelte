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
  <p class="hint">{t('ops.pattern.intro.hint')}</p>
  <label class="row" title={t('ops.pattern.kind.help')}>
    <span>{t('ops.pattern.kind.label')}</span>
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
    <label class="row" title={t('ops.pattern.linear_count.help')}>
      <span>{t('ops.pattern.linear_count.label')}</span>
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
    <label class="row" title={t('ops.pattern.linear_dx.help')}>
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
    <label class="row" title={t('ops.pattern.linear_dy.help')}>
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
    <label class="row" title={t('ops.pattern.count_x.help')}>
      <span>{t('ops.pattern.count_x.label')}</span>
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
    <label class="row" title={t('ops.pattern.count_y.help')}>
      <span>{t('ops.pattern.count_y.label')}</span>
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
    <label class="row" title={t('ops.pattern.grid_dx.help')}>
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
    <label class="row" title={t('ops.pattern.grid_dy.help')}>
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
    <label class="row" title={t('ops.pattern.polar_count.help')}>
      <span>{t('ops.pattern.polar_count.label')}</span>
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
    <label class="row" title={t('ops.pattern.center_x.help')}>
      <span>{t('ops.pattern.center_x.label')}</span>
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
    <label class="row" title={t('ops.pattern.center_y.help')}>
      <span>{t('ops.pattern.center_y.label')}</span>
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
    <label class="row" title={t('ops.pattern.step.help')}>
      <span>{t('ops.pattern.step.label')}</span>
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
    <label class="row" title={t('ops.pattern.start.help')}>
      <span>{t('ops.pattern.start.label')}</span>
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
          ? t('ops.pattern.set_center.disabled_help')
          : t('ops.pattern.set_center.help')}
      >
        {t('ops.pattern.set_center')}
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
