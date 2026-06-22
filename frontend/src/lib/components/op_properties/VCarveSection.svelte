<script lang="ts">
  /// V-Carve op-properties fieldset. Shown only when op.kind === 'vcarve'.
  /// Styling is mostly inherited from OpPropertiesPanel's :global(.props ...)
  /// rules; the local style block below only covers the inlay button.
  /// (svelte-check treats a literal "style" tag in a JSDoc comment as the
  /// start of a real style block, so the word is spelled out rather than
  /// tagged here.)
  import {
    project,
    type OpField,
    type OpFieldValue,
    type VCarveOp,
  } from '../../state/project.svelte';
  import { t } from '../../i18n';
  // `project` import kept for the inlay-plug Duplicate-as-plug button
  // below (line 99). The toolMismatch chip moved to OpPropertiesPanel.

  interface Props {
    op: VCarveOp;
    /// Kind-aware patch — see ChamferSection for rationale.
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<fieldset>
  <legend>{t('ops.vcarve.legend')}</legend>
  <!-- Tool-kind warning is now emitted by OpPropertiesPanel against
       the central op_tool_constraint helper — see k94n. -->

  <!--
    Carve mode is the most consequential V-Carve setting (it changes the
    toolpath structure entirely), so it sits at the top of the section
    rather than buried inside Advanced. Stored as the
    `fullMedialAxis: boolean | undefined` field — undefined means the
    default perimeter-only mode.
  -->
  <div class="row" title={t('ops.vcarve.mode.help')}>
    <span>{t('ops.vcarve.mode.label')}</span>
    <div class="segmented">
      <button
        type="button"
        class:active={!(op.fullMedialAxis ?? false)}
        onclick={() => patch('fullMedialAxis', undefined)}
      >
        {t('ops.vcarve.mode.perimeter')}
      </button>
      <button
        type="button"
        class:active={op.fullMedialAxis ?? false}
        onclick={() => patch('fullMedialAxis', true)}
      >
        {t('ops.vcarve.mode.medial_axis')}
      </button>
    </div>
  </div>
  <details class="subsection" open>
    <summary>{t('ops.vcarve.advanced.summary')}</summary>
    <label class="row" title={t('ops.vcarve.max_width.help')}>
      <span>{t('ops.vcarve.max_width.label')}</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.1"
          min="0"
          placeholder={t('ops.vcarve.max_width.placeholder')}
          value={op.carveMaxWidthMm ?? ''}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            patch('carveMaxWidthMm', isNaN(v) || v <= 0 ? undefined : v);
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label class="row" title={t('ops.vcarve.source_inset.help')}>
      <span>{t('ops.vcarve.source_inset.label')}</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.05"
          min="0"
          placeholder="0"
          value={op.sourceInsetMm ?? ''}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            patch('sourceInsetMm', isNaN(v) || v <= 0 ? undefined : v);
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label class="row" title={t('ops.vcarve.inlay_plug.help')}>
      <span>{t('ops.vcarve.inlay_plug.label')}</span>
      <button
        type="button"
        class="inlay-btn"
        onclick={() => {
          const dup = project.duplicateOperation(op.id);
          if (!dup) return;
          project.updateOperation(dup.id, {
            name: `${op.name} (plug)`,
            sourceInsetMm: 0.1,
          });
        }}
      >
        {t('ops.vcarve.duplicate_as_inlay_plug')}
      </button>
    </label>
    <label class="row" title={t('ops.vcarve.refine_pass.help')}>
      <span>{t('ops.vcarve.refine_pass.label')}</span>
      <input type="checkbox" checked={false} disabled />
      <span class="hint hint-trailing">{t('ops.vcarve.refine_pass.not_implemented')}</span>
    </label>
  </details>
</fieldset>

<style>
  /* Trailing hint sibling — sits inline after a checkbox / input. Was an
     inline `style="margin-left:.5rem"`; promoted to a class. */
  :global(.hint.hint-trailing) {
    margin-left: 0.5rem;
  }
  /* 'Duplicate as inlay plug' button. Sized to match the existing
     per-section action buttons (re-pick, repick) so the V-Carve
     fieldset doesn't look like it bolted on something special. */
  .inlay-btn {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    color: var(--text-strong);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--border));
    border-radius: 3px;
    padding: 0.15rem 0.55rem;
    font-size: 0.74rem;
    cursor: pointer;
  }
  .inlay-btn:hover {
    background: color-mix(in srgb, var(--accent) 30%, transparent);
  }
</style>
