<script lang="ts">
  /// Drill op-properties fieldset. Shown only when op.kind === 'drill'.
  /// Owns the drill-cycle picker (G81 / G83 / G73), peck-step + dwell
  /// inputs, and the Stufenfase chamfer-after-width field.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import {
    project,
    type DrillCycle,
    type DrillOp,
    type OpField,
    type OpFieldValue,
  } from '../../state/project.svelte';
  import { t } from '../../i18n';

  interface Props {
    op: DrillOp;
    /// Kind-aware patch (OpField + OpFieldValue) so calls like
    /// `patch('drillCycle', …)` type-check across every section
    /// without each section redeclaring a per-variant signature.
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<fieldset>
  <legend>{t('ops.drill.legend')}</legend>
  <label class="row">
    <span>{t('ops.drill.cycle.label')}</span>
    <select
      value={op.drillCycle?.kind ?? 'simple'}
      onchange={(e) => {
        const v = (e.currentTarget as HTMLSelectElement).value as 'simple' | 'peck' | 'chip_break';
        const cur = op.drillCycle ?? ({ kind: 'simple', dwellSec: 0 } as DrillCycle);
        const dwell = cur.dwellSec ?? 0;
        const step = cur.kind === 'peck' || cur.kind === 'chip_break' ? cur.peckStepMm : 1.0;
        if (v === 'simple') {
          patch('drillCycle', { kind: 'simple', dwellSec: dwell } as DrillCycle);
        } else if (v === 'peck') {
          patch('drillCycle', {
            kind: 'peck',
            peckStepMm: step,
            dwellSec: dwell,
          } as DrillCycle);
        } else {
          patch('drillCycle', {
            kind: 'chip_break',
            peckStepMm: step,
            dwellSec: dwell,
          } as DrillCycle);
        }
      }}
    >
      <option value="simple" title={t('ops.drill.cycle.simple.help')}>
        {t('ops.drill.cycle.simple')}
      </option>
      <option value="peck" title={t('ops.drill.cycle.peck.help')}>
        {t('ops.drill.cycle.peck')}
      </option>
      <option value="chip_break" title={t('ops.drill.cycle.chip_break.help')}>
        {t('ops.drill.cycle.chip_break')}
      </option>
    </select>
  </label>
  {#if op.drillCycle && (op.drillCycle.kind === 'peck' || op.drillCycle.kind === 'chip_break')}
    <details class="subsection" open>
      <summary>{t('ops.drill.cycle_options')}</summary>
      <label class="row">
        <span>{t('ops.drill.peck_step.label')}</span>
        <div class="num-cell">
          <input
            type="number"
            step="0.1"
            min="0.1"
            value={op.drillCycle.peckStepMm}
            onchange={(e) => {
              const v = parseFloat((e.currentTarget as HTMLInputElement).value);
              if (!isNaN(v) && v > 0 && op.drillCycle) {
                const cur = op.drillCycle;
                if (cur.kind === 'peck' || cur.kind === 'chip_break') {
                  patch('drillCycle', {
                    ...cur,
                    peckStepMm: v,
                  } as DrillCycle);
                }
              }
            }}
          />
          <span class="unit">mm</span>
        </div>
      </label>
    </details>
  {/if}
  <label class="row">
    <span>{t('ops.drill.dwell.label')}</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.1"
        min="0"
        value={op.drillCycle?.dwellSec ?? 0}
        onchange={(e) => {
          const v = parseFloat((e.currentTarget as HTMLInputElement).value);
          if (!isNaN(v) && v >= 0) {
            const cur = op.drillCycle ?? ({ kind: 'simple' } as DrillCycle);
            patch('drillCycle', { ...cur, dwellSec: v } as DrillCycle);
          }
        }}
      />
      <span class="unit">s</span>
    </div>
  </label>
  <label class="row" title={t('ops.drill.chamfer_width.help')}>
    <span>{t('ops.drill.chamfer_width.label')}</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.1"
        min="0"
        placeholder="0"
        value={op.chamferAfterWidthMm ?? ''}
        onchange={(e) => {
          const v = parseFloat((e.currentTarget as HTMLInputElement).value);
          patch('chamferAfterWidthMm', isNaN(v) || v <= 0 ? undefined : v);
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label class="row" title={t('ops.drill.spot_first.help')}>
    <span>{t('ops.drill.spot_first.label')}</span>
    <input
      type="checkbox"
      checked={op.spotFirst !== undefined}
      onchange={(e) => {
        const on = (e.currentTarget as HTMLInputElement).checked;
        if (!on) {
          patch('spotFirst', undefined);
          return;
        }
        const firstTool = project.data.tools[0]?.id ?? op.toolId;
        patch('spotFirst', {
          spotDepthMm: op.spotFirst?.spotDepthMm ?? -0.5,
          spotToolId: op.spotFirst?.spotToolId ?? firstTool,
        });
      }}
    />
  </label>
  {#if op.spotFirst}
    <details class="subsection" open>
      <summary>{t('ops.drill.spot_options')}</summary>
      <label class="row">
        <span>{t('ops.drill.spot_depth.label')}</span>
        <div class="num-cell">
          <input
            type="number"
            step="0.1"
            max="0"
            value={op.spotFirst.spotDepthMm}
            title={t('ops.drill.spot_depth.help')}
            onchange={(e) => {
              const v = parseFloat((e.currentTarget as HTMLInputElement).value);
              if (!isNaN(v) && v < 0 && op.spotFirst) {
                patch('spotFirst', { ...op.spotFirst, spotDepthMm: v });
              }
            }}
          />
          <span class="unit">mm</span>
        </div>
      </label>
      <label class="row">
        <span>{t('ops.drill.spot_tool.label')}</span>
        <select
          value={op.spotFirst.spotToolId}
          onchange={(e) => {
            const id = parseInt((e.currentTarget as HTMLSelectElement).value, 10);
            if (op.spotFirst) patch('spotFirst', { ...op.spotFirst, spotToolId: id });
          }}
        >
          {#each project.data.tools as t (t.id)}
            <option value={t.id}>{t.id}: {t.name}</option>
          {/each}
        </select>
      </label>
    </details>
  {/if}
</fieldset>
