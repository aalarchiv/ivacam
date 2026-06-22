<script lang="ts">
  /// GcodeInclude op-properties: file-picker + path display + content
  /// textarea, plus the variable cheat-sheet hint and the verbose
  /// unsim-warning toggle.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import type { GcodeIncludeOp, OpField, OpFieldValue } from '../../state/project.svelte';
  import { t } from '../../i18n';

  interface Props {
    op: GcodeIncludeOp;
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<label class="row">
  <span>{t('ops.gcode_include.name.label')}</span>
  <input
    type="text"
    value={op.name}
    oninput={(e) => patch('name', (e.currentTarget as HTMLInputElement).value)}
  />
</label>
<label class="row" title={t('ops.gcode_include.file.help')}>
  <span>{t('ops.gcode_include.file.label')}</span>
  <input
    type="file"
    accept=".nc,.ngc,.gcode,.tap,.cnc,text/plain"
    onchange={async (e) => {
      const f = (e.currentTarget as HTMLInputElement).files?.[0];
      if (!f) return;
      const text = await f.text();
      patch('path', f.name);
      patch('content', text);
      // Reset the input so re-picking the same filename re-fires onchange.
      (e.currentTarget as HTMLInputElement).value = '';
    }}
  />
</label>
{#if op.path}
  <p class="hint">
    {t('ops.gcode_include.loaded_from.hint')} <code>{op.path}</code> · {t(
      'ops.gcode_include.loaded_stats.hint',
      {
        characters: op.content?.length ?? 0,
        lines: op.content?.split('\n').length ?? 0,
      },
    )}
  </p>
{/if}
<label class="row" title={t('ops.gcode_include.content.help')}>
  <span>{t('ops.gcode_include.content.label')}</span>
  <textarea
    rows="10"
    spellcheck="false"
    value={op.content ?? ''}
    placeholder={t('ops.gcode_include.content.placeholder')}
    oninput={(e) => patch('content', (e.currentTarget as HTMLTextAreaElement).value)}
    style="font-family: ui-monospace, monospace; white-space: pre;"
  ></textarea>
</label>
<p class="hint hint-pause">
  {t('ops.gcode_include.tokens_intro.hint')}
  <code>{'{x}'}</code> <code>{'{y}'}</code> <code>{'{z}'}</code>
  {t('ops.gcode_include.token_xyz.hint')}
  <code>{'{f}'}</code>
  {t('ops.gcode_include.token_feed.hint')}
  <code>{'{s}'}</code>
  {t('ops.gcode_include.token_spindle.hint')}
  <code>{'{safe_z}'}</code>
  {t('ops.gcode_include.token_safez.hint')} <code>{'{tokens}'}</code>
  {t('ops.gcode_include.tokens_outro.hint')}
</p>
<!-- Verbose per-line warning toggle. Off by default; users
     debugging an exotic block flip it on to see exactly which
     lines were skipped and why. -->
<label class="row" title={t('ops.gcode_include.verbose_unsim_warnings.help')}>
  <span>{t('ops.gcode_include.verbose_unsim_warnings.label')}</span>
  <input
    type="checkbox"
    checked={op.verboseUnsimWarnings ?? false}
    onchange={(e) => patch('verboseUnsimWarnings', (e.currentTarget as HTMLInputElement).checked)}
  />
</label>
