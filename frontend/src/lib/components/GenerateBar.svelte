<script lang="ts">
  // Simplified bar: post-processor + Generate + Download. The full setup
  // tree lives in SetupPanel and feeds project.setup.

  import { defaultClient } from '../api/http';
  import { CancelledError, tryParseStructuredError } from '../api/client';
  import { isTauri } from '../api/env';
  import {
    project,
    simWarningSeverity,
    simWarningSegmentIdx,
    simWarningSummary,
  } from '../state/project.svelte';
  import { buildProject, type GenerateRequestWithProject } from '../api/build-project';
  import type { SimWarning, TimeEstimate } from '../api/types';
  import { _ } from 'svelte-i18n';
  import GenerateProgress from './GenerateProgress.svelte';
  import { workspace } from '../state/workspace.svelte';

  // Format a duration in seconds as HH:MM:SS (always two digits per
  // unit). Negative / NaN inputs render as 00:00:00.
  function formatHms(s: number | undefined): string {
    if (!s || !isFinite(s) || s < 0) return '00:00:00';
    const total = Math.round(s);
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const sec = total % 60;
    return [h, m, sec].map((n) => String(n).padStart(2, '0')).join(':');
  }

  // Short form for breakdown items: M:SS for sub-hour, just seconds for
  // tiny values. Returns "—" for zero / undefined.
  function formatShort(s: number | undefined): string {
    if (s === undefined || !isFinite(s)) return '—';
    if (s <= 0.05) return '0s';
    if (s < 60) return `${s.toFixed(1)}s`;
    if (s < 3600) {
      const m = Math.floor(s / 60);
      const sec = Math.round(s % 60);
      return `${m}:${String(sec).padStart(2, '0')}`;
    }
    return formatHms(s);
  }

  function timeEstimate(): TimeEstimate | null {
    const r = project.generated as { time_estimate?: TimeEstimate } | null;
    return r?.time_estimate ?? null;
  }

  const client = defaultClient();
  type PostId = 'linuxcnc' | 'grbl' | 'hpgl';
  function coercePost(v: string): PostId {
    return v === 'grbl' || v === 'hpgl' ? v : 'linuxcnc';
  }
  let post: PostId = $state(coercePost(workspace.get().last_post_processor));
  $effect(() => {
    workspace.setLastPostProcessor(post);
  });
  let progressMsg = $state<string>('');
  let progressFrac = $state<number>(0);
  let warningPanelOpen = $state(false);
  let abortController: AbortController | null = null;

  function cancelRun() {
    if (project.pipelineState !== 'running') return;
    project.pipelineState = 'cancelling';
    abortController?.abort();
  }

  let warnings = $derived(project.simDiagnostics?.warnings ?? []);
  let criticalCount = $derived(warnings.filter((w) => simWarningSeverity(w) === 'critical').length);
  let isClean = $derived(warnings.length === 0);

  async function run() {
    if (!project.imported) return;
    if (project.settings.blockOnCriticalSimWarnings && criticalCount > 0) {
      project.setError(
        `Sim has ${criticalCount} critical warning${criticalCount === 1 ? '' : 's'} — fix or disable the safety check in Settings`,
      );
      return;
    }
    project.generating = true;
    project.pipelineState = 'running';
    project.pipelineProgress = null;
    project.error = null;
    project.lastGenerateCachedCount = 0;
    project.lastGenerateOpCount = 0;
    progressMsg = '';
    progressFrac = 0;
    abortController = new AbortController();
    try {
      const opProject = buildProject(project);
      if (!opProject) {
        project.setError('Add at least one operation to generate G-code.');
        return;
      }
      // The hand-rolled WireProject in build-project.ts trims the
      // openapi-generated request shape: every serde-default field on
      // the Rust side appears as required in the generated TS, while
      // we omit them when they match defaults. Cast through `unknown`
      // — the runtime payload is correct; the structural mismatch is
      // purely about which fields are optional.
      const req: GenerateRequestWithProject = {
        post_processor: post,
        project: opProject as unknown as GenerateRequestWithProject['project'],
      };
      let r;
      if (client.generateStreaming) {
        r = await client.generateStreaming(
          req,
          (ev) => {
            if (ev.kind === 'op_started') {
              project.pipelineProgress = {
                opIdx: ev.idx,
                opTotal: ev.total,
                opFraction: 0,
                opName: ev.name,
              };
              progressMsg = ev.name;
              progressFrac = ev.idx / Math.max(1, ev.total);
            } else if (ev.kind === 'op_progress') {
              if (project.pipelineProgress) {
                project.pipelineProgress = { ...project.pipelineProgress, opFraction: ev.fraction };
              }
              progressMsg = ev.message;
            } else if (ev.kind === 'op_completed') {
              project.lastGenerateOpCount += 1;
              if (ev.cached) project.lastGenerateCachedCount += 1;
              if (project.pipelineProgress) {
                project.pipelineProgress = {
                  ...project.pipelineProgress,
                  opFraction: 1,
                  opIdx: project.pipelineProgress.opIdx + 1,
                };
                progressFrac =
                  project.pipelineProgress.opIdx / Math.max(1, project.pipelineProgress.opTotal);
              }
            } else if (ev.kind === 'done') {
              progressFrac = 1;
            }
          },
          abortController.signal,
        );
      } else if (client.generateStream) {
        r = await client.generateStream(req, (ev) => {
          progressMsg = ev.message;
          progressFrac = ev.fraction;
        });
      } else {
        r = await client.generate(req);
      }
      project.setGenerated(r);
      project.pipelineState = 'completed';
      setTimeout(() => {
        if (project.pipelineState === 'completed') project.pipelineState = 'idle';
      }, 1000);
    } catch (e) {
      if (e instanceof CancelledError) {
        project.pipelineState = 'idle';
      } else {
        const raw = e instanceof Error ? e.message : String(e);
        const structured = tryParseStructuredError(raw);
        project.setError(structured ?? raw);
        project.pipelineState = 'idle';
      }
    } finally {
      project.generating = false;
      project.pipelineProgress = null;
      abortController = null;
      progressMsg = '';
      progressFrac = 0;
    }
  }

  async function downloadGcode() {
    if (!project.generated) return;
    const base = project.imported?.filename?.replace(/\.[^.]+$/, '') ?? 'output';
    const ext = post === 'hpgl' ? 'plt' : 'ngc';
    const filename = `${base}.${ext}`;
    if (isTauri()) {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const { writeTextFile } = await import('@tauri-apps/plugin-fs');
      const path = await save({
        defaultPath: filename,
        filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
      });
      if (typeof path === 'string') {
        try {
          await writeTextFile(path, project.generated.gcode);
        } catch (e) {
          project.setError(`save: ${e instanceof Error ? e.message : String(e)}`);
        }
      }
      return;
    }
    const blob = new Blob([project.generated.gcode], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
  }

  function flyToWarning(w: SimWarning) {
    const segIdx = simWarningSegmentIdx(w);
    const cum = project.toolpathCumLen;
    const total = project.toolpathTotalLen;
    if (cum && total > 0 && segIdx >= 0 && segIdx < cum.length) {
      project.playhead = Math.min(1, cum[segIdx] / total);
    } else {
      const segs = project.generated?.toolpath.length ?? 0;
      if (segs > 0) project.playhead = Math.min(1, (segIdx + 1) / segs);
    }
  }

  function chipClass(): string {
    if (project.simDiagnostics == null) return 'sim-chip idle';
    if (criticalCount > 0) return 'sim-chip critical';
    if (warnings.length > 0) return 'sim-chip warning';
    return 'sim-chip clean';
  }

  function chipLabel(): string {
    if (project.simDiagnostics == null) return 'Sim: not run yet — Generate first';
    if (isClean) return 'Sim clean';
    if (criticalCount > 0) {
      return `Sim: ${warnings.length} warning${warnings.length === 1 ? '' : 's'} (${criticalCount} critical)`;
    }
    return `Sim: ${warnings.length} warning${warnings.length === 1 ? '' : 's'}`;
  }

  function chipGlyph(): string {
    if (project.simDiagnostics == null) return '🛡';
    if (isClean) return '✓';
    if (criticalCount > 0) return '⛔';
    return '⚠';
  }

  const SIM_IDLE_HINT =
    'Sim verification runs after Generate. Catches rapid moves through stock, fixture collisions, and cutter holder collisions before you cut.';
</script>

<div class="bar">
  <span class="title">{$_('generate.title')}</span>
  <label
    >{$_('generate.post')}
    <select bind:value={post}>
      <option value="linuxcnc">LinuxCNC</option>
      <option value="grbl">GRBL</option>
      <option value="hpgl">HPGL</option>
    </select>
  </label>
  {#if project.pipelineState === 'running' || project.pipelineState === 'cancelling'}
    <GenerateProgress onCancel={cancelRun} />
  {:else}
    <button onclick={run} disabled={!project.imported || project.generating}>
      {project.generating ? $_('generate.running') : $_('generate.run')}
    </button>
  {/if}
  {#if project.generated}
    <button onclick={downloadGcode} class="download">
      {post === 'hpgl' ? $_('generate.download_plt') : $_('generate.download_ngc')}
    </button>
    <span class="stats">
      {$_('generate.stats', {
        values: {
          objects: project.generated.stats.object_count,
          offsets: project.generated.stats.offset_count,
          moves: project.generated.toolpath.length,
        },
      })}
      {#if project.lastGenerateCachedCount > 0}
        <span class="cached-tag"
          >· {project.lastGenerateCachedCount} of {project.lastGenerateOpCount} cached</span
        >
      {/if}
    </span>
    {#if timeEstimate()}
      {@const t = timeEstimate()!}
      <span class="time-chip" tabindex="0" role="button" aria-label="Time breakdown">
        <span class="time">⏱ {formatHms(t.total_s)}</span>
        <div class="time-breakdown" role="tooltip">
          <table>
            <tbody>
              <tr><th>Total</th><td>{formatHms(t.total_s)}</td></tr>
              <tr><th>Cut</th><td>{formatShort(t.cut_s)}</td></tr>
              <tr><th>Rapid</th><td>{formatShort(t.rapid_s)}</td></tr>
              <tr><th>Plunge</th><td>{formatShort(t.plunge_s)}</td></tr>
              <tr><th>Retract</th><td>{formatShort(t.retract_s)}</td></tr>
              <tr><th>Arc</th><td>{formatShort(t.arc_s)}</td></tr>
              <tr><th>Tool change</th><td>{formatShort(t.toolchange_s)}</td></tr>
              <tr><th>Spindle warm-up</th><td>{formatShort(t.spindle_warmup_s)}</td></tr>
            </tbody>
          </table>
        </div>
      </span>
    {/if}
  {/if}
  {#if project.simDiagnostics == null && project.generated == null}
    <span class="sim-chip idle" title={SIM_IDLE_HINT}>
      🛡 {chipLabel()}
    </span>
  {:else if project.simDiagnostics != null || warnings.length > 0}
    <button
      class={chipClass()}
      onclick={() => (warningPanelOpen = !warningPanelOpen)}
      type="button"
      title="Click for details"
      aria-expanded={warningPanelOpen}
    >
      <span class="glyph" aria-hidden="true">{chipGlyph()}</span>
      {chipLabel()}
    </button>
  {/if}
</div>

{#if warningPanelOpen}
  <div class="panel" role="dialog" aria-label="Sim warnings">
    <header>
      <h3>Sim warnings ({warnings.length})</h3>
      <button class="close" onclick={() => (warningPanelOpen = false)} aria-label="Close">×</button>
    </header>
    <div class="list">
      {#if warnings.length === 0}
        <p class="empty">No warnings — sim is clean.</p>
      {:else}
        {#each warnings as w, i (i)}
          <button
            class="row severity-{simWarningSeverity(w)}"
            onclick={() => flyToWarning(w)}
            type="button"
          >
            <span class="dot" aria-hidden="true"></span>
            <span class="kind">{w.kind}</span>
            <span class="msg">{simWarningSummary(w)}</span>
          </button>
        {/each}
      {/if}
    </div>
  </div>
{/if}

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 0.7rem;
    padding: 0.4rem 0.9rem;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--border);
    color: var(--text);
    flex-wrap: wrap;
    font-size: 0.78rem;
  }
  .title {
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-size: 0.7rem;
  }
  label {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
  }
  select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.3rem;
    font-size: 0.78rem;
  }
  button {
    background: var(--accent);
    color: white;
    border: none;
    padding: 0.3rem 0.7rem;
    border-radius: 4px;
    font-size: 0.78rem;
    cursor: pointer;
  }
  button.download {
    background: var(--success-bg);
  }
  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .stats {
    color: var(--success);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
  .time-chip {
    position: relative;
    display: inline-block;
    cursor: help;
    outline: none;
  }
  .time-chip:focus-visible {
    box-shadow: 0 0 0 2px var(--accent);
    border-radius: 3px;
  }
  .time {
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    white-space: pre;
  }
  .time-breakdown {
    display: none;
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 50;
    background: var(--bg-panel);
    outline: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.3);
    padding: 0.4rem 0.55rem;
    font-size: 0.74rem;
    white-space: nowrap;
  }
  .time-chip:hover .time-breakdown,
  .time-chip:focus-within .time-breakdown {
    display: block;
  }
  .time-breakdown table {
    border-collapse: collapse;
  }
  .time-breakdown th {
    color: var(--text-muted);
    text-align: right;
    font-weight: normal;
    padding: 0.08rem 0.6rem 0.08rem 0;
  }
  .time-breakdown td {
    color: var(--text-strong);
    font-family: ui-monospace, monospace;
    font-variant-numeric: tabular-nums;
    text-align: left;
    padding: 0.08rem 0;
  }
  .cached-tag {
    color: var(--text-muted);
    margin-left: 0.4rem;
    font-style: italic;
  }
  .progress {
    position: relative;
    flex: 1;
    height: 1.2rem;
    min-width: 8rem;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: 3px;
    overflow: hidden;
  }
  .bar-fill {
    height: 100%;
    background: var(--accent);
    transition: width 120ms ease-out;
  }
  .progress-text {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.7rem;
    color: var(--text-strong);
    pointer-events: none;
    text-shadow: 0 0 4px var(--bg-app);
  }
  .sim-chip {
    border-radius: 999px;
    padding: 0.18rem 0.65rem;
    font-size: 0.74rem;
    border: 1px solid transparent;
    color: var(--text-strong);
  }
  .sim-chip.idle {
    background: var(--bg-elevated);
    color: var(--text-muted);
    border-color: var(--border);
    font-style: italic;
    cursor: help;
  }
  .sim-chip.clean {
    background: var(--sim-clean-bg);
    color: var(--sim-clean-fg);
    border-color: color-mix(in srgb, var(--success) 40%, transparent);
  }
  .sim-chip.warning {
    background: var(--sim-warn-bg);
    color: var(--sim-warn-fg);
    border-color: color-mix(in srgb, var(--warn) 40%, transparent);
  }
  .sim-chip.critical {
    background: var(--sim-critical-bg);
    color: var(--sim-critical-fg);
    border-color: color-mix(in srgb, var(--error) 40%, transparent);
  }
  .sim-chip .glyph {
    margin-right: 0.25rem;
    font-weight: bold;
  }
  .panel {
    position: absolute;
    right: 1rem;
    top: 3rem;
    width: min(420px, 90vw);
    max-height: 60vh;
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.3);
    z-index: 40;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .panel header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.7rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
  }
  .panel h3 {
    font-size: 0.85rem;
    margin: 0;
    color: var(--text-strong);
  }
  .panel .close {
    background: transparent;
    color: var(--text-muted);
    border: 0;
    font-size: 1.2rem;
    cursor: pointer;
    padding: 0 0.3rem;
  }
  .panel .list {
    overflow: auto;
    padding: 0.4rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .panel .empty {
    color: var(--text-muted);
    font-size: 0.78rem;
    margin: 0.5rem;
    text-align: center;
  }
  .panel .row {
    display: grid;
    grid-template-columns: 0.8rem 8rem 1fr;
    align-items: center;
    gap: 0.5rem;
    text-align: left;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.35rem 0.55rem;
    font-size: 0.74rem;
  }
  .panel .row:hover {
    background: var(--bg-hover, var(--bg-input));
  }
  .panel .row .dot {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
  }
  .panel .row.severity-critical .dot {
    background: var(--marker-critical);
  }
  .panel .row.severity-warning .dot {
    background: var(--marker-warn);
  }
  .panel .row.severity-info .dot {
    background: var(--marker-info);
  }
  .panel .row .kind {
    font-family: ui-monospace, monospace;
    color: var(--text-muted);
  }
  .panel .row .msg {
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
