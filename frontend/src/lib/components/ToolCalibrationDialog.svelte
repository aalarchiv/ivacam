<script lang="ts">
  /// Tool wear calibration — the slot-test flow. The user cuts a
  /// shallow single-pass slot with the tool, measures the slot WIDTH
  /// with a caliper (the width IS the effective cutting diameter), and
  /// we store `nominal − measured` as the wear offset plus today's
  /// date. Pure math lives in `state/tool_wear.ts`; this dialog is the
  /// step-by-step shell so first-time users measure the right thing.
  import Modal from './Modal.svelte';
  import { t } from '../i18n';
  import { todayIso, wearOffsetFromSlotWidth } from '../state/tool_wear';

  interface Props {
    open: boolean;
    toolName: string;
    nominalDiameterMm: number;
    currentWearOffsetMm: number;
    onApply: (wearOffsetMm: number, dateIso: string) => void;
    onClose: () => void;
  }
  let { open, toolName, nominalDiameterMm, currentWearOffsetMm, onApply, onClose }: Props =
    $props();

  let measuredRaw = $state('');
  $effect(() => {
    if (open) measuredRaw = '';
  });

  const measured = $derived(parseFloat(measuredRaw));
  const wear = $derived(wearOffsetFromSlotWidth(nominalDiameterMm, measured));
  /// Sanity bound: a slot more than 25% off nominal is almost
  /// certainly a measuring mistake (wrong slot, wrong unit, wrong
  /// tool), not wear. Block Apply and say so.
  const implausible = $derived(
    wear != null && Math.abs(wear) > Math.max(nominalDiameterMm * 0.25, 0.5),
  );

  function apply() {
    if (wear == null || implausible) return;
    onApply(wear, todayIso());
    onClose();
  }
</script>

{#if open}
  <Modal onClose={() => onClose()} width="min(460px, 94vw)" ariaLabelledBy="cal-title">
    <header>
      <h2 id="cal-title">{t('calib.title', { tool: toolName })}</h2>
      <button class="dlg-close" onclick={() => onClose()} aria-label={t('common.close')}>×</button>
    </header>
    <div class="body">
      <ol>
        <li>
          <!-- eslint-disable-next-line svelte/no-at-html-tags -- static, translator-authored markup -->
          {@html t('calib.step.cut')}
        </li>
        <li>
          <!-- eslint-disable-next-line svelte/no-at-html-tags -- static, translator-authored markup -->
          {@html t('calib.step.measure')}
        </li>
        <li>{t('calib.step.enter')}</li>
      </ol>
      <label class="measure">
        <span>{t('calib.measured_width')}</span>
        <input
          type="number"
          step="0.01"
          min="0"
          placeholder={String(nominalDiameterMm)}
          bind:value={measuredRaw}
        />
      </label>
      {#if wear != null && !implausible}
        <p class="result">
          <!-- eslint-disable-next-line svelte/no-at-html-tags -- static, translator-authored markup -->
          {@html t('calib.result', {
            wear,
            effective: Math.max(nominalDiameterMm - wear, 0.01),
            nominal: nominalDiameterMm,
          })}
        </p>
      {:else if implausible}
        <p class="result warn">
          {t('calib.implausible', { nominal: nominalDiameterMm })}
        </p>
      {:else if measuredRaw !== ''}
        <p class="result warn">{t('calib.invalid')}</p>
      {/if}
      {#if currentWearOffsetMm !== 0}
        <p class="current">{t('calib.current', { current: currentWearOffsetMm })}</p>
      {/if}
    </div>
    <footer>
      <button class="btn-secondary" onclick={() => onClose()}>{t('common.cancel')}</button>
      <button class="btn-primary" disabled={wear == null || implausible} onclick={apply}
        >{t('common.apply')}</button
      >
    </footer>
  </Modal>
{/if}

<style>
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.7rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
  }
  h2 {
    font-size: 0.95rem;
    margin: 0;
    color: var(--text-strong);
  }
  .body {
    padding: 0.7rem;
    font-size: 0.82rem;
    line-height: 1.45;
  }
  ol {
    margin: 0 0 0.6rem;
    padding-left: 1.2rem;
  }
  li + li {
    margin-top: 0.3rem;
  }
  .measure {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .measure input {
    width: 7rem;
  }
  .result {
    margin: 0.6rem 0 0;
  }
  .result.warn {
    color: var(--danger);
  }
  .current {
    margin: 0.4rem 0 0;
    color: var(--text-muted);
    font-size: 0.76rem;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
    padding: 0.5rem 0.7rem;
    border-top: 1px solid var(--border);
    background: var(--bg-elevated);
  }
</style>
