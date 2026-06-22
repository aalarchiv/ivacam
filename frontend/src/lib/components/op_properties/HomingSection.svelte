<script lang="ts">
  /// Homing op-properties: name + safe-Z toggle. The op emits G28
  /// (machine-specific home) and, optionally, a rapid Z lift afterwards.
  /// No tool, no source, no Z schedule.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import type { HomingOp, OpField, OpFieldValue } from '../../state/project.svelte';
  import { t } from '../../i18n';

  interface Props {
    op: HomingOp;
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<label class="row">
  <span>{t('ops.homing.name.label')}</span>
  <input
    type="text"
    value={op.name}
    oninput={(e) => patch('name', (e.currentTarget as HTMLInputElement).value)}
  />
</label>
<label class="row" title={t('ops.homing.retract_safe_z.help')}>
  <span>{t('ops.homing.retract_safe_z.label')}</span>
  <input
    type="checkbox"
    checked={op.retractToSafeZ ?? true}
    onchange={(e) => patch('retractToSafeZ', (e.currentTarget as HTMLInputElement).checked)}
  />
</label>
<p class="hint hint-pause">
  {t('ops.homing.pipeline.hint_prefix')}<code>G28</code>{t('ops.homing.pipeline.hint_suffix', {
    suffix: (op.retractToSafeZ ?? true) ? t('ops.homing.pipeline.safe_z_suffix') : '',
  })}
</p>
