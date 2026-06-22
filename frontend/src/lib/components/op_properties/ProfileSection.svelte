<script lang="ts">
  /// Profile op-properties: Tool offset picker + lead-in / lead-out
  /// inputs. Shown only when op.kind === 'profile'.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import {
    type OpField,
    type OpFieldValue,
    type ProfileOffset,
    type ProfileOp,
  } from '../../state/project.svelte';
  import { t } from '../../i18n';

  interface Props {
    op: ProfileOp;
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<fieldset>
  <legend>{t('ops.profile.legend')}</legend>
  <label class="row">
    <span>{t('ops.profile.offset.label')}</span>
    <select
      value={op.offset}
      onchange={(e) =>
        patch('offset', (e.currentTarget as HTMLSelectElement).value as ProfileOffset)}
    >
      <option value="outside">{t('ops.profile.side.outside')}</option>
      <option value="inside">{t('ops.profile.side.inside')}</option>
      <option value="on">{t('ops.profile.side.on')}</option>
    </select>
  </label>
</fieldset>

<fieldset>
  <legend>{t('ops.profile.leads.legend')}</legend>
  <label class="row" title={t('ops.profile.lead_in.help')}>
    <span>{t('ops.profile.lead_in.label')}</span>
    <select
      value={op.leadInKind ?? 'off'}
      onchange={(e) =>
        patch(
          'leadInKind',
          (e.currentTarget as HTMLSelectElement).value as 'off' | 'straight' | 'arc',
        )}
    >
      <option value="off">{t('ops.profile.lead.off')}</option>
      <option value="straight">{t('ops.profile.lead.straight')}</option>
      <option value="arc">{t('ops.profile.lead_in.arc')}</option>
    </select>
  </label>
  {#if op.leadInKind && op.leadInKind !== 'off'}
    <label
      class="row"
      title={op.leadInKind === 'arc'
        ? t('ops.profile.lead_in_radius.help')
        : t('ops.profile.lead_in_length.help')}
    >
      <span
        >{op.leadInKind === 'arc'
          ? t('ops.profile.radius.label')
          : t('ops.profile.length.label')}</span
      >
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          min="0"
          value={op.leadIn ?? 5}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            // Don't write negative values back: the prior
            // silent-clamp-to-0 looked broken to users who typed -1
            // expecting either an error or a bounce. Refuse the change
            // and the visible input snaps back to the stored value.
            if (Number.isFinite(v) && v >= 0) patch('leadIn', v);
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
  {/if}
  <label class="row" title={t('ops.profile.lead_out.help')}>
    <span>{t('ops.profile.lead_out.label')}</span>
    <select
      value={op.leadOutKind ?? 'off'}
      onchange={(e) =>
        patch(
          'leadOutKind',
          (e.currentTarget as HTMLSelectElement).value as 'off' | 'straight' | 'arc',
        )}
    >
      <option value="off">{t('ops.profile.lead.off')}</option>
      <option value="straight">{t('ops.profile.lead.straight')}</option>
      <option value="arc">{t('ops.profile.lead_out.arc')}</option>
    </select>
  </label>
  {#if op.leadOutKind && op.leadOutKind !== 'off'}
    <label
      class="row"
      title={op.leadOutKind === 'arc'
        ? t('ops.profile.lead_out_radius.help')
        : t('ops.profile.lead_out_length.help')}
    >
      <span
        >{op.leadOutKind === 'arc'
          ? t('ops.profile.radius.label')
          : t('ops.profile.length.label')}</span
      >
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          min="0"
          value={op.leadOut ?? 5}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v) && v >= 0) patch('leadOut', v);
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
  {/if}
</fieldset>
