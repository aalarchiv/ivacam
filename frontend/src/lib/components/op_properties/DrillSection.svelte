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
  <legend>Drill cycle</legend>
  <label class="row">
    <span>Cycle</span>
    <select
      value={op.drillCycle?.kind ?? 'simple'}
      onchange={(e) => {
        const v = (e.currentTarget as HTMLSelectElement).value as 'simple' | 'peck' | 'chip_break';
        const cur = op.drillCycle ?? ({ kind: 'simple', dwell_sec: 0 } as DrillCycle);
        const dwell = cur.dwell_sec ?? 0;
        const step = cur.kind === 'peck' || cur.kind === 'chip_break' ? cur.peck_step_mm : 1.0;
        if (v === 'simple') {
          patch('drillCycle', { kind: 'simple', dwell_sec: dwell } as DrillCycle);
        } else if (v === 'peck') {
          patch('drillCycle', {
            kind: 'peck',
            peck_step_mm: step,
            dwell_sec: dwell,
          } as DrillCycle);
        } else {
          patch('drillCycle', {
            kind: 'chip_break',
            peck_step_mm: step,
            dwell_sec: dwell,
          } as DrillCycle);
        }
      }}
    >
      <option value="simple" title="G81 — single plunge to depth, retract."> simple (G81) </option>
      <option value="peck" title="G83 — peck with full retract to clearance plane between pecks.">
        peck (G83)
      </option>
      <option
        value="chip_break"
        title="G73 — peck with chip-break (small partial retract between pecks)."
      >
        chip-break (G73)
      </option>
    </select>
  </label>
  {#if op.drillCycle && (op.drillCycle.kind === 'peck' || op.drillCycle.kind === 'chip_break')}
    <details class="subsection" open>
      <summary>Cycle options</summary>
      <label class="row">
        <span>Peck step</span>
        <div class="num-cell">
          <input
            type="number"
            step="0.1"
            min="0.1"
            value={op.drillCycle.peck_step_mm}
            onchange={(e) => {
              const v = parseFloat((e.currentTarget as HTMLInputElement).value);
              if (!isNaN(v) && v > 0 && op.drillCycle) {
                const cur = op.drillCycle;
                if (cur.kind === 'peck' || cur.kind === 'chip_break') {
                  patch('drillCycle', {
                    ...cur,
                    peck_step_mm: v,
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
    <span>Dwell</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.1"
        min="0"
        value={op.drillCycle?.dwell_sec ?? 0}
        onchange={(e) => {
          const v = parseFloat((e.currentTarget as HTMLInputElement).value);
          if (!isNaN(v) && v >= 0) {
            const cur = op.drillCycle ?? ({ kind: 'simple' } as DrillCycle);
            patch('drillCycle', { ...cur, dwell_sec: v } as DrillCycle);
          }
        }}
      />
      <span class="unit">s</span>
    </div>
  </label>
  <label
    class="row"
    title="Countersink: after drilling each hole, the cutter walks a constant-Z revolution at the rim to break the edge. Depth is computed from the cutter's V-bit tip angle. Set Finish tool below to swap to a dedicated chamfer cutter (drill, then T<n> M6, then chamfer). Empty / 0 = no countersink."
  >
    <span>Chamfer width</span>
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
  <label
    class="row"
    title="Spot pre-pass: before the main drill, the machine spots each hole with a shallow centre mark using a stiffer tool, so a twist drill doesn't walk on hard or polished stock. Drill, then T<n> M6 to the spot tool, spot every hole, then back to the drill."
  >
    <span>Spot pre-pass</span>
    <input
      type="checkbox"
      checked={op.spotFirst !== undefined}
      onchange={(e) => {
        const on = (e.currentTarget as HTMLInputElement).checked;
        if (!on) {
          patch('spotFirst', undefined);
          return;
        }
        const firstTool = project.tools[0]?.id ?? op.toolId;
        patch('spotFirst', {
          spotDepthMm: op.spotFirst?.spotDepthMm ?? -0.5,
          spotToolId: op.spotFirst?.spotToolId ?? firstTool,
        });
      }}
    />
  </label>
  {#if op.spotFirst}
    <details class="subsection" open>
      <summary>Spot options</summary>
      <label class="row">
        <span>Spot depth</span>
        <div class="num-cell">
          <input
            type="number"
            step="0.1"
            max="0"
            value={op.spotFirst.spotDepthMm}
            title="Depth of the centre spot below the stock top. Negative number, mm — just deep enough to start the drill (e.g. -0.5)."
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
        <span>Spot tool</span>
        <select
          value={op.spotFirst.spotToolId}
          onchange={(e) => {
            const id = parseInt((e.currentTarget as HTMLSelectElement).value, 10);
            if (op.spotFirst) patch('spotFirst', { ...op.spotFirst, spotToolId: id });
          }}
        >
          {#each project.tools as t (t.id)}
            <option value={t.id}>{t.id}: {t.name}</option>
          {/each}
        </select>
      </label>
    </details>
  {/if}
</fieldset>
