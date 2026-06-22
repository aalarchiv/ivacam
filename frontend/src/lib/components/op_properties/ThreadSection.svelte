<script lang="ts">
  /// Thread op-properties fieldset. Shown only when op.kind === 'thread';
  /// the parent narrows so the section receives a strongly-typed
  /// `ThreadOp` rather than the full OpEntry union.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import type { OpField, OpFieldValue, ThreadOp } from '../../state/project.svelte';
  import { t } from '../../i18n';

  interface Props {
    op: ThreadOp;
    /// Kind-aware patch — see ChamferSection for rationale.
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<fieldset>
  <legend>{t('ops.thread.legend')}</legend>
  <p class="hint" title={t('ops.thread.source.help')}>
    {t('ops.thread.source.hint')}
  </p>
  <label class="row" title={t('ops.thread.pitch.help')}>
    <span>{t('ops.thread.pitch.label')}</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.05"
        min="0"
        placeholder="1"
        value={op.threadPitchMm ?? ''}
        onchange={(e) => {
          const v = parseFloat((e.currentTarget as HTMLInputElement).value);
          patch('threadPitchMm', isNaN(v) || v <= 0 ? undefined : v);
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label class="row" title={t('ops.thread.direction.help')}>
    <span>{t('ops.thread.direction.label')}</span>
    <select
      value={(op.threadInternal ?? true) ? 'internal' : 'external'}
      onchange={(e) => {
        const v = (e.currentTarget as HTMLSelectElement).value;
        patch('threadInternal', v === 'internal');
      }}
    >
      <option value="internal">{t('ops.thread.side.internal')}</option>
      <option value="external">{t('ops.thread.side.external')}</option>
    </select>
  </label>
  <label class="row" title={t('ops.thread.climb.help')}>
    <span>{t('ops.thread.climb.label')}</span>
    <input
      type="checkbox"
      checked={op.threadClimb ?? false}
      onchange={(e) => patch('threadClimb', (e.currentTarget as HTMLInputElement).checked)}
    />
  </label>
</fieldset>
