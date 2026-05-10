<script lang="ts">
  // Simplified bar: post-processor + Generate + Download. The full setup
  // tree lives in SetupPanel and feeds project.setup.

  import { defaultClient } from '../api/http';
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
  let post: 'linuxcnc' | 'grbl' | 'hpgl' = $state('linuxcnc');
  let progressMsg = $state<string>('');
  let progressFrac = $state<number>(0);
  let warningPanelOpen = $state(false);

  let warnings = $derived(project.simDiagnostics?.warnings ?? []);
  let criticalCount = $derived(
    warnings.filter((w) => simWarningSeverity(w) === 'critical').length,
  );
  let isClean = $derived(warnings.length === 0);

  async function run() {
    if (!project.imported) return;
    if (
      project.settings.blockOnCriticalSimWarnings &&
      criticalCount > 0
    ) {
      project.setError(
        `Sim has ${criticalCount} critical warning${criticalCount === 1 ? '' : 's'} — fix or disable the safety check in Settings`,
      );
      return;
    }
    project.generating = true;
    project.error = null;
    progressMsg = '';
    progressFrac = 0;
    try {
      const opProject = buildProject(project);
      if (!opProject) {
        project.setError('Add at least one operation to generate gcode.');
        return;
      }
      const req: GenerateRequestWithProject = {
        post_processor: post,
        project: opProject,
      };
      const r = client.generateStream
        ? await client.generateStream(req, (ev) => {
            progressMsg = ev.message;
            progressFrac = ev.fraction;
          })
        : await client.generate(req);
      project.setGenerated(r);
    } catch (e) {
      project.setError(e instanceof Error ? e.message : String(e));
    } finally {
      project.generating = false;
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
    if (criticalCount > 0) return 'sim-chip critical';
    if (warnings.length > 0) return 'sim-chip warning';
    return 'sim-chip clean';
  }

  function chipLabel(): string {
    if (project.simDiagnostics == null) return 'Sim: not run';
    if (isClean) return 'Sim clean';
    if (criticalCount > 0) {
      return `Sim: ${warnings.length} warning${warnings.length === 1 ? '' : 's'} (${criticalCount} critical)`;
    }
    return `Sim: ${warnings.length} warning${warnings.length === 1 ? '' : 's'}`;
  }
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
  <button onclick={run} disabled={!project.imported || project.generating}>
    {project.generating ? $_('generate.running') : $_('generate.run')}
  </button>
  {#if project.generating}
    <div
      class="progress"
      role="progressbar"
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={Math.round(progressFrac * 100)}
      title={progressMsg}
    >
      <div class="bar-fill" style="width: {Math.round(progressFrac * 100)}%"></div>
      <span class="progress-text">{progressMsg || $_('generate.starting')}</span>
    </div>
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
    </span>
    {#if timeEstimate()}
      {@const t = timeEstimate()!}
      <span
        class="time"
        title={`Cut: ${formatShort(t.cut_s)}\nArc: ${formatShort(t.arc_s)}\nPlunge: ${formatShort(t.plunge_s)}\nRetract: ${formatShort(t.retract_s)}\nRapid: ${formatShort(t.rapid_s)}\nTool change: ${formatShort(t.toolchange_s)}\nSpindle warm-up: ${formatShort(t.spindle_warmup_s)}`}
      >
        ⏱ {formatHms(t.total_s)}
      </span>
    {/if}
  {/if}
  {#if project.simDiagnostics != null || warnings.length > 0}
    <button
      class={chipClass()}
      onclick={() => (warningPanelOpen = !warningPanelOpen)}
      type="button"
      title="Click for details"
      aria-expanded={warningPanelOpen}
    >
      {#if isClean}<span class="ok">✓</span>{/if}
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
  .time {
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    white-space: pre;
    cursor: help;
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
  .sim-chip.clean {
    background: rgba(95, 208, 110, 0.18);
    color: #5fd06e;
    border-color: rgba(95, 208, 110, 0.4);
  }
  .sim-chip.warning {
    background: rgba(240, 192, 32, 0.18);
    color: #f0c020;
    border-color: rgba(240, 192, 32, 0.4);
  }
  .sim-chip.critical {
    background: rgba(229, 72, 72, 0.18);
    color: #e54848;
    border-color: rgba(229, 72, 72, 0.4);
  }
  .sim-chip .ok {
    margin-right: 0.2rem;
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
    background: #e54848;
  }
  .panel .row.severity-warning .dot {
    background: #f0c020;
  }
  .panel .row.severity-info .dot {
    background: #4a8df0;
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
