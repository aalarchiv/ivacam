<script lang="ts">
  /// Phone status chip + warnings/generate panel. The desktop GenerateBar
  /// (Generate button + warnings chip + warnings panel) is hidden on
  /// narrow, so its panel — which lives inside that hidden subtree — can
  /// never show. This is the phone-native equivalent: a single app-bar
  /// chip that reflects generate/warning state AND opens a panel with a
  /// reliable Generate/Re-Generate button plus the sim + pipeline warning
  /// list. Generate is routed through `generateBus` (the same signal the
  /// pull-to-refresh gesture uses), which the still-mounted GenerateBar
  /// honours.
  import { project } from '../state/project.svelte';
  import { generateBus } from '../state/generate-bus.svelte';
  import FloatingPanel from './FloatingPanel.svelte';
  import { simWarningSeverity, simWarningSummary } from '../sim/warnings';
  import {
    countCriticalPipelineWarnings,
    pipelineWarningSeverity,
    type PipelineWarning,
  } from '../api/pipeline-warnings';

  let open = $state(false);
  let copied = $state(false);

  const generating = $derived(
    project.gen.pipelineState === 'running' || project.gen.pipelineState === 'cancelling',
  );
  const simWarnings = $derived(project.gen.simDiagnostics?.warnings ?? []);
  const pipeWarnings = $derived<PipelineWarning[]>(
    (project.gen.generated as { warnings?: PipelineWarning[] } | null)?.warnings ?? [],
  );
  /// Has a generate/sim run produced anything yet?
  const hasRun = $derived(project.gen.generated != null || project.gen.simDiagnostics != null);
  const stale = $derived(project.gen.simDiagnostics != null && project.data.dirty);
  const critical = $derived(
    simWarnings.filter((w) => simWarningSeverity(w) === 'critical').length +
      countCriticalPipelineWarnings(pipeWarnings),
  );
  const total = $derived(simWarnings.length + pipeWarnings.length);

  /// Plain-text dump of every current warning, for the clipboard.
  function warningsText(): string {
    const lines: string[] = [];
    for (const w of simWarnings) lines.push(`[sim] ${w.kind}: ${simWarningSummary(w)}`);
    for (const pw of pipeWarnings) lines.push(`[pipeline] ${pw.kind}: ${pw.message}`);
    return lines.join('\n');
  }
  async function copyWarnings() {
    try {
      await navigator.clipboard.writeText(warningsText());
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      /* clipboard unavailable (insecure context / denied) — no-op */
    }
  }

  /// The `stock_origin_outside_geometry_bbox` warning means the WCS zero
  /// isn't on the stock/geometry — common when text is engraved inset from
  /// the stock corner. Offer a one-tap fix: move the WCS origin to the stock
  /// corner (where the operator zeros) and re-generate.
  const wcsWarning = $derived(
    pipeWarnings.find((w) => w.kind === 'stock_origin_outside_geometry_bbox') ?? null,
  );
  function fixWcsOrigin() {
    project.snapWorkOffsetToFootprint();
    generateBus.request();
  }
  /// Generate is meaningful once geometry OR text exists to run (text-only
  /// projects render to geometry in the backend pre-pass).
  const canGenerate = $derived(project.geometryView != null || project.data.textLayers.length > 0);

  const glyph = $derived.by(() => {
    if (generating) return '⏳';
    if (!hasRun) return '▶';
    if (stale) return '↻';
    if (critical > 0) return '⛔';
    if (total > 0) return '⚠';
    return '✓';
  });
  const cls = $derived.by(() => {
    if (generating) return 'busy';
    if (!hasRun) return 'idle';
    if (stale) return 'stale';
    if (critical > 0) return 'critical';
    if (total > 0) return 'warning';
    return 'clean';
  });
  const text = $derived.by(() => {
    if (generating) return '…';
    if (!hasRun) return 'Generate';
    if (stale) return 'Stale';
    if (total === 0) return 'OK';
    return critical > 0 ? `${total} (${critical}!)` : `${total}`;
  });
  const title = $derived.by(() => {
    if (generating) return 'Generating…';
    if (!hasRun) return 'No program yet — tap to Generate';
    if (stale) return 'Toolpath is stale — tap for details / re-Generate';
    if (total === 0) return 'No warnings — tap for details';
    return `${total} warning${total === 1 ? '' : 's'} — tap for details`;
  });

  /// Chip tap: when there's nothing to view (no run yet) or the toolpath
  /// is stale, the tap (re-)generates; otherwise it toggles the warnings
  /// window. So a "Stale" chip is a one-tap re-Generate, and a chip
  /// showing warnings opens the list — no separate Generate button needed.
  function onChipClick() {
    if (generating) return;
    if (!hasRun || stale) {
      if (canGenerate) generateBus.request();
    } else {
      open = !open;
    }
  }
</script>

<div class="phone-warn">
  <button
    type="button"
    class="warn-chip {cls}"
    onclick={onChipClick}
    {title}
    aria-label={title}
    aria-haspopup={!hasRun || stale ? undefined : 'dialog'}
  >
    <span class="warn-glyph" aria-hidden="true">{glyph}</span>
    <span class="warn-text">{text}</span>
  </button>
</div>

<FloatingPanel
  {open}
  onClose={() => (open = false)}
  title={`Warnings (${total})`}
  ariaLabel="Warnings"
  initialWidth={420}
  initialHeight={460}
>
  <div class="wpanel">
    {#if hasRun}
      {#if total > 0}
        <div class="wactions">
          {#if wcsWarning}
            <button type="button" class="wfix" onclick={fixWcsOrigin}>
              Set zero to stock corner
            </button>
          {/if}
          <button type="button" class="wcopy" onclick={copyWarnings}>
            {copied ? 'Copied' : 'Copy'}
          </button>
        </div>
      {/if}
      <div class="wlist">
        {#if total === 0}
          <p class="empty">No warnings — sim and pipeline are clean.</p>
        {:else}
          {#each simWarnings as w, i (`sim-${i}`)}
            <div class="row sev-{simWarningSeverity(w)}">
              <div class="row-head">
                <span class="src">sim</span>
                <span class="kind">{w.kind}</span>
              </div>
              <span class="msg">{simWarningSummary(w)}</span>
            </div>
          {/each}
          {#each pipeWarnings as pw, i (`pipe-${i}`)}
            <div class="row sev-{pipelineWarningSeverity(pw)}">
              <div class="row-head">
                <span class="src pipe">pipeline</span>
                <span class="kind">{pw.kind}</span>
              </div>
              <span class="msg">{pw.message}</span>
            </div>
          {/each}
        {/if}
      </div>
    {/if}
  </div>
</FloatingPanel>

<style>
  .phone-warn {
    display: inline-flex;
  }
  .warn-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    min-height: 40px;
    padding: 0 0.55rem;
    border-radius: 1rem;
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text);
    font-size: 0.82rem;
    font-variant-numeric: tabular-nums;
    cursor: pointer;
    max-width: 100%;
    white-space: nowrap;
  }
  .warn-glyph {
    font-size: 0.95rem;
    line-height: 1;
  }
  .warn-chip.idle {
    border-color: var(--accent);
    color: var(--accent);
  }
  .warn-chip.busy {
    color: var(--text-muted);
  }
  .warn-chip.stale {
    border-color: var(--accent);
    color: var(--accent);
  }
  .warn-chip.critical {
    border-color: var(--danger, #d44);
    color: var(--danger, #d44);
  }
  .warn-chip.warning {
    border-color: var(--warning, #d49a00);
    color: var(--warning, #d49a00);
  }
  .warn-chip.clean {
    color: var(--text-muted);
  }

  .wpanel {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    padding: 0.7rem;
    overflow: auto;
  }
  .wactions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    justify-content: flex-end;
  }
  .wcopy,
  .wfix {
    min-height: 36px;
    padding: 0 0.7rem;
    border-radius: 5px;
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text);
    font-size: 0.8rem;
    cursor: pointer;
  }
  .wfix {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
    color: var(--text-strong);
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    margin-right: auto;
  }
  .empty {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.82rem;
  }
  .wlist {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  /* Stacked: a header line (source · name) then the message below — a
     two-column name|text split is too cramped on a phone. */
  .row {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    padding: 0.4rem 0.5rem;
    border-left: 3px solid var(--border);
    border-radius: 4px;
    background: var(--bg-panel);
    font-size: 0.8rem;
  }
  .row-head {
    display: flex;
    align-items: baseline;
    gap: 0.4rem;
  }
  .row.sev-critical {
    border-left-color: var(--danger, #d44);
  }
  .row.sev-warning {
    border-left-color: var(--warning, #d49a00);
  }
  .row .src {
    color: var(--text-muted);
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .row .kind {
    color: var(--text-strong);
    font-family: ui-monospace, monospace;
    font-size: 0.72rem;
  }
  .row .msg {
    color: var(--text);
    min-width: 0;
  }
</style>
