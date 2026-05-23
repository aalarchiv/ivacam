<script lang="ts">
  // Simplified bar: post-processor + Generate + Download. The full setup
  // tree lives in SetupPanel and feeds project.setup.

  import { defaultClient } from '../api/http';
  import { CancelledError, tryParseStructuredError } from '../api/client';
  import {
    project,
    simWarningSeverity,
    simWarningSegmentIdx,
    simWarningSummary,
  } from '../state/project.svelte';
  import { buildProject, type GenerateRequestWithProject } from '../api/build-project';
  import { exportGeneratedGcode } from '../state/file_ops';
  import { computeFootprint } from '../sim/driver';
  import type { SimWarning, TimeEstimate } from '../api/types';
  import {
    countCriticalPipelineWarnings,
    pipelineWarningSeverity,
    type PipelineWarning,
  } from '../api/pipeline-warnings';
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
    const current = post;
    // Defer the workspace write off the synchronous effect flush.
    // Writing $state (workspace.version) inside an effect body aborts
    // Svelte 5's reactivity scheduler silently — see project.svelte.ts
    // persistPerProjectState for the full diagnosis.
    queueMicrotask(() => {
      try {
        workspace.setLastPostProcessor(current);
      } catch (e) {
        console.warn('persist post processor:', e);
      }
    });
  });
  let progressMsg = $state<string>('');
  let progressFrac = $state<number>(0);
  let warningPanelOpen = $state(false);
  let abortController: AbortController | null = null;

  function cancelRun() {
    if (project.pipelineState !== 'running') return;
    project.cancelGenerate();
    abortController?.abort();
  }

  // 75op: auto-regenerate on edit. Watch project.dirty + the setting;
  // when both are true and we're not already running, debounce ~1.5s
  // and fire run(). Cancel prior pending debounce on each new edit.
  let autoTimer: ReturnType<typeof setTimeout> | null = null;
  const AUTO_REGEN_DEBOUNCE_MS = 1500;
  $effect(() => {
    void project.dirty;
    void project.settings.autoRegenerate;
    if (autoTimer) {
      clearTimeout(autoTimer);
      autoTimer = null;
    }
    if (
      !project.settings.autoRegenerate ||
      !project.dirty ||
      !project.transformedImport ||
      project.pipelineState === 'running' ||
      project.pipelineState === 'cancelling'
    ) {
      return;
    }
    autoTimer = setTimeout(() => {
      autoTimer = null;
      // Re-check guards at fire time — the user may have already hit
      // Generate manually, or pipelineState may have flipped.
      if (
        project.settings.autoRegenerate &&
        project.dirty &&
        project.transformedImport &&
        project.pipelineState !== 'running' &&
        project.pipelineState !== 'cancelling'
      ) {
        void run();
      }
    }, AUTO_REGEN_DEBOUNCE_MS);
  });

  let warnings = $derived(project.simDiagnostics?.warnings ?? []);
  // dvs4: surface pipeline-level warnings in the same panel that
  // showed sim warnings before. Previously the panel was hard-coded to
  // `project.simDiagnostics?.warnings` and the panel gate required a
  // non-null simDiagnostics, so a Generate that raised, say,
  // `op_source_empty` or `tool_too_large` flagged the chip but
  // clicking it showed "No warnings — sim is clean." now we render
  // BOTH lists in one panel with a source tag per row.
  let pipelineWarnings = $derived<PipelineWarning[]>(
    (project.generated as { warnings?: PipelineWarning[] } | null)?.warnings ?? [],
  );
  // 94sf: critical-count now spans BOTH sim warnings AND pipeline-level
  // warnings (tool_too_large, op_order_suspect, frame_padding_below_tool_radius,
  // spindle_speed_clamped_above_max, stock_origin_outside_geometry_bbox, …).
  // Before, the safety gate ignored everything the pipeline emitted at
  // planning time — only sim post-mortem warnings could block the
  // Generate button. The audit caught that pattern (a Pocket whose tool
  // didn't fit emitted zero toolpath, raised `tool_too_large`, and the
  // user's "block on critical" setting did NOT prevent the broken gcode
  // from shipping).
  let pipelineCriticalCount = $derived(countCriticalPipelineWarnings(pipelineWarnings));
  let criticalCount = $derived(
    warnings.filter((w) => simWarningSeverity(w) === 'critical').length + pipelineCriticalCount,
  );
  let totalWarningCount = $derived(warnings.length + pipelineWarnings.length);
  let isClean = $derived(totalWarningCount === 0 && pipelineCriticalCount === 0);

  /// Post-Generate bounds scan — counts cut/plunge/arc segments whose
  /// endpoints fall outside the stock OR outside the machine work area.
  /// Different from the existing sim warnings (which catch rapid-through-
  /// stock / fixture collision); this is purely an "is your gcode valid
  /// for this stock + machine envelope" check.
  const boundsScan = $derived.by(() => {
    const gen = project.generated;
    if (!gen) return null;
    const wa = project.machine.workArea;
    const stockFp = computeFootprint(project.transformedImport, project.stock, wa);
    const stockTop = 0;
    const stockBottom = -Math.max(0.01, project.stock.thickness);
    const isCut = (k: string) => k === 'cut' || k === 'plunge' || k === 'arc';
    let outWA = 0;
    let outStock = 0;
    let firstWaLine = 0;
    let firstStockLine = 0;
    for (const seg of gen.toolpath) {
      if (!isCut(seg.kind)) continue;
      const p = seg.to;
      if (wa && wa.x > 0 && wa.y > 0 && wa.z > 0) {
        if (p.x < -1e-6 || p.x > wa.x + 1e-6 || p.y < -1e-6 || p.y > wa.y + 1e-6 || p.z < -wa.z - 1e-6 || p.z > 1e-6) {
          outWA++;
          if (firstWaLine === 0) firstWaLine = seg.gcode_line;
        }
      }
      if (
        p.x < stockFp.minX - 1e-6 ||
        p.x > stockFp.maxX + 1e-6 ||
        p.y < stockFp.minY - 1e-6 ||
        p.y > stockFp.maxY + 1e-6 ||
        p.z < stockBottom - 1e-6 ||
        p.z > stockTop + 1e-6
      ) {
        outStock++;
        if (firstStockLine === 0) firstStockLine = seg.gcode_line;
      }
    }
    if (outWA === 0 && outStock === 0) return null;
    return { outWA, outStock, firstWaLine, firstStockLine };
  });

  async function run() {
    if (!project.transformedImport) return;
    if (project.settings.blockOnCriticalSimWarnings && criticalCount > 0) {
      project.setError(
        `Sim has ${criticalCount} critical warning${criticalCount === 1 ? '' : 's'} — fix or disable the safety check in Settings`,
      );
      return;
    }
    project.beginGenerate();
    progressMsg = '';
    progressFrac = 0;
    abortController = new AbortController();
    try {
      const opProject = buildProject(project);
      if (!opProject) {
        // Early bail: `beginGenerate()` flipped pipelineState to 'running'
        // above, so just `setError + return` would leave the UI stuck on
        // the progress bar + cancel button. Route through `failGenerate`
        // which snaps state back to 'idle'.
        project.failGenerate('Add at least one operation to generate G-code.');
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
            project.notePipelineEvent(ev);
            if (ev.kind === 'op_started') {
              progressMsg = ev.name;
              progressFrac = ev.idx / Math.max(1, ev.total);
            } else if (ev.kind === 'op_progress') {
              progressMsg = ev.message;
            } else if (ev.kind === 'op_completed') {
              if (project.pipelineProgress) {
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
      project.finishGenerate();
    } catch (e) {
      if (e instanceof CancelledError) {
        // Cancelled by the user — just snap back to idle.
        project.pipelineState = 'idle';
      } else {
        const raw = e instanceof Error ? e.message : String(e);
        const structured = tryParseStructuredError(raw);
        project.failGenerate(structured ?? raw);
      }
    } finally {
      project.endGenerate();
      abortController = null;
      progressMsg = '';
      progressFrac = 0;
    }
  }

  async function downloadGcode() {
    // 94sf: if the most recent generate raised critical pipeline
    // warnings (tool_too_large, op_order_suspect, …) and the user
    // hasn't disabled the safety gate, refuse to write the file.
    // The toolpath we'd ship is the one the pipeline flagged as
    // substantively wrong — saving it to disk just gives the user
    // a broken .ngc that ends up on a machine.
    if (project.settings.blockOnCriticalSimWarnings && pipelineCriticalCount > 0) {
      project.setError(
        `Pipeline raised ${pipelineCriticalCount} critical warning${pipelineCriticalCount === 1 ? '' : 's'} on the last Generate — fix or disable the safety check in Settings`,
      );
      return;
    }
    await exportGeneratedGcode(post);
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

  /// Sim status goes STALE the moment the user edits anything that
  /// would change gcode (project.dirty flips true on every op / tool /
  /// stock / text mutation, and clears on the next successful Generate).
  /// The chip reflects this so the user knows the previous sim verdict
  /// no longer matches what's on the canvas.
  let simStale = $derived(project.simDiagnostics != null && project.dirty);

  function chipClass(): string {
    // dvs4: chip color reflects WORST of sim + pipeline warnings.
    // "idle" only when there's no generate-side state AND no sim run
    // yet (chip is hidden anyway in that case).
    if (project.simDiagnostics == null && pipelineWarnings.length === 0) return 'sim-chip idle';
    if (simStale) return 'sim-chip stale';
    if (criticalCount > 0) return 'sim-chip critical';
    if (totalWarningCount > 0) return 'sim-chip warning';
    return 'sim-chip clean';
  }

  function chipLabel(): string {
    if (project.simDiagnostics == null && pipelineWarnings.length === 0) {
      return 'Sim: not run yet — Generate first';
    }
    if (simStale) return 'Sim: stale — re-Generate';
    if (isClean) return 'Sim clean';
    if (criticalCount > 0) {
      return `${totalWarningCount} warning${totalWarningCount === 1 ? '' : 's'} (${criticalCount} critical)`;
    }
    return `${totalWarningCount} warning${totalWarningCount === 1 ? '' : 's'}`;
  }

  function chipGlyph(): string {
    if (project.simDiagnostics == null && pipelineWarnings.length === 0) return '🛡';
    if (simStale) return '↻';
    if (isClean) return '✓';
    if (criticalCount > 0) return '⛔';
    return '⚠';
  }

  const SIM_IDLE_HINT =
    'Sim verification runs after Generate. Catches rapid moves through stock, fixture collisions, and cutter holder collisions before you cut.';
</script>

<div class="bar">
  <span class="title">Generate:</span>
  <label
    title="Output dialect. LinuxCNC: standard RS-274 G-code. GRBL: hobby-CNC subset with manual tool-change prompts. HPGL: vinyl-cutter / plotter language (drag-knife mode)."
    >post
    <select bind:value={post}>
      <option value="linuxcnc">LinuxCNC</option>
      <option value="grbl">GRBL</option>
      <option value="hpgl">HPGL</option>
    </select>
  </label>
  {#if project.pipelineState === 'running' || project.pipelineState === 'cancelling'}
    <GenerateProgress onCancel={cancelRun} />
  {:else}
    <button
      onclick={run}
      disabled={!project.transformedImport || project.generating}
      title="Run the CAM pipeline and produce a toolpath. Reads the current ops, tools, stock, and machine — output is cached so unchanged ops re-emit instantly."
    >
      {project.generating ? 'Generating G-code…' : 'Generate G-code'}
    </button>
  {/if}
  {#if project.generated}
    <button
      onclick={downloadGcode}
      class="download"
      title="Save the generated toolpath to disk in the selected dialect's file extension."
    >
      {post === 'hpgl' ? 'Download .plt' : 'Download .ngc'}
    </button>
    <span class="stats">
      {project.generated.stats.object_count} obj · {project.generated.stats.offset_count} offsets · {project.generated.toolpath.length} moves
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
  {#if project.sourceFileStaleNotice}
    <!-- opqb: source-file-changed chip lives in the toolbar where the
         user looks for actionable state, instead of the standalone
         bottom-right toast that competed with other floating UI. -->
    <span class="stale-chip" role="alert" aria-live="polite" title="The source file on disk has changed since the last import. Reload to pick up the changes; Ignore to keep the current view.">
      <span class="stale-msg">
        ⟳ <strong
          >{project.sourceFileStaleNotice.path.split(/[\\/]/).pop()}</strong
        > changed
      </span>
      <button
        type="button"
        class="stale-reload"
        onclick={async () => {
          const path = project.sourceFileStaleNotice?.path;
          if (!path) return;
          project.sourceFileStaleNotice = null;
          await project.reimportFromPath(path);
        }}
      >
        Reload
      </button>
      <button
        type="button"
        class="stale-ignore"
        onclick={() => (project.sourceFileStaleNotice = null)}
      >
        ×
      </button>
    </span>
  {/if}
  {#if project.simDiagnostics == null && project.generated == null}
    <span class="sim-chip idle" title={SIM_IDLE_HINT}>
      🛡 {chipLabel()}
    </span>
  {:else if project.simDiagnostics != null || totalWarningCount > 0}
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
  {#if boundsScan}
    <span
      class="sim-chip bounds"
      title={[
        boundsScan.outWA > 0
          ? `${boundsScan.outWA} cut move${boundsScan.outWA === 1 ? '' : 's'} outside the machine work area (first @ gcode line ${boundsScan.firstWaLine || '?'})`
          : '',
        boundsScan.outStock > 0
          ? `${boundsScan.outStock} cut move${boundsScan.outStock === 1 ? '' : 's'} outside the stock (first @ gcode line ${boundsScan.firstStockLine || '?'})`
          : '',
      ]
        .filter(Boolean)
        .join('\n')}
    >
      <span class="glyph" aria-hidden="true">⚠</span>
      {#if boundsScan.outWA > 0 && boundsScan.outStock > 0}
        {boundsScan.outStock} out-of-stock · {boundsScan.outWA} out-of-machine
      {:else if boundsScan.outWA > 0}
        {boundsScan.outWA} cut move{boundsScan.outWA === 1 ? '' : 's'} outside work area
      {:else}
        {boundsScan.outStock} cut move{boundsScan.outStock === 1 ? '' : 's'} outside stock
      {/if}
    </span>
  {/if}
</div>

{#if warningPanelOpen}
  <!-- dvs4: panel now lists BOTH sim warnings AND pipeline warnings.
       Each row tags its source (Sim / Pipeline) so the user can tell
       which subsystem flagged it. Pipeline warnings use the
       pipeline-warnings.ts severity classifier (fj88) so the row dot
       colour matches what the safety gate sees. -->
  <div class="panel" role="dialog" aria-label="Warnings">
    <header>
      <h3>Warnings ({totalWarningCount})</h3>
      <button class="close" onclick={() => (warningPanelOpen = false)} aria-label="Close">×</button>
    </header>
    <div class="list">
      {#if totalWarningCount === 0}
        <p class="empty">No warnings — sim and pipeline are clean.</p>
      {:else}
        {#each warnings as w, i (`sim-${i}`)}
          <button
            class="row severity-{simWarningSeverity(w)}"
            onclick={() => flyToWarning(w)}
            type="button"
          >
            <span class="dot" aria-hidden="true"></span>
            <span class="source" title="Surfaced by the simulator after gcode generation.">sim</span>
            <span class="kind">{w.kind}</span>
            <span class="msg">{simWarningSummary(w)}</span>
          </button>
        {/each}
        {#each pipelineWarnings as pw, i (`pipe-${i}`)}
          <div
            class="row severity-{pipelineWarningSeverity(pw)} pipeline"
            title={pw.message}
          >
            <span class="dot" aria-hidden="true"></span>
            <span class="source pipeline" title="Surfaced by the CAM pipeline during gcode generation.">pipeline</span>
            <span class="kind">{pw.kind}</span>
            <span class="msg">{pw.message}</span>
          </div>
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
  /* opqb: source-file-changed chip. Warning palette, inline Reload /
     dismiss buttons so the user can act without leaving the toolbar. */
  .stale-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    border-radius: 999px;
    padding: 0.18rem 0.5rem 0.18rem 0.65rem;
    font-size: 0.74rem;
    background: var(--sim-warn-bg, color-mix(in srgb, var(--warn) 18%, var(--bg-elevated)));
    color: var(--sim-warn-fg, var(--text-strong));
    border: 1px solid color-mix(in srgb, var(--warn) 45%, transparent);
  }
  .stale-chip .stale-msg {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 24ch;
  }
  .stale-chip .stale-msg strong {
    font-weight: 600;
  }
  .stale-chip button {
    border: 1px solid transparent;
    background: transparent;
    color: inherit;
    cursor: pointer;
    font-size: 0.72rem;
    padding: 0.05rem 0.4rem;
    border-radius: 3px;
    line-height: 1.2;
  }
  .stale-chip .stale-reload {
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  .stale-chip .stale-reload:hover {
    background: var(--accent-strong);
  }
  .stale-chip .stale-ignore {
    opacity: 0.8;
    padding: 0.05rem 0.3rem;
  }
  .stale-chip .stale-ignore:hover {
    opacity: 1;
    background: color-mix(in srgb, var(--warn) 25%, transparent);
  }
  .sim-chip.idle {
    background: var(--bg-elevated);
    color: var(--text-muted);
    border-color: var(--border);
    font-style: italic;
    cursor: help;
  }
  .sim-chip.bounds {
    /* Out-of-stock / out-of-work-area count chip. Same warning palette
       as sim warnings — these are gcode validity issues the user should
       address before cutting. */
    background: var(--sim-warn-bg);
    color: var(--sim-warn-fg);
    border-color: color-mix(in srgb, var(--warn) 40%, transparent);
    white-space: nowrap;
    cursor: help;
  }
  .sim-chip.stale {
    /* Neutral / dim variant — signals "the previous sim verdict no
       longer reflects the current project". Same shape as the other
       chips so it doesn't grab the eye like a warning would. */
    background: var(--bg-elevated);
    color: var(--text-muted);
    border-color: color-mix(in srgb, var(--text-muted) 50%, transparent);
    font-style: italic;
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
    z-index: var(--z-floating);
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
    grid-template-columns: 0.8rem 3.6rem 8rem 1fr;
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
  /* Sim rows are interactive (button) — flyToWarning seeks the
     playhead. Pipeline rows are static (div) — they have no segment
     index to fly to. Use the same hover for click-affordance parity
     on the interactive ones only. */
  button.row {
    cursor: pointer;
  }
  button.row:hover {
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
  .panel .row .source {
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0 0.3rem;
    text-align: center;
    line-height: 1.3;
    background: var(--bg-app);
  }
  .panel .row .source.pipeline {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  }
  .panel .row .kind {
    font-family: ui-monospace, monospace;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .panel .row .msg {
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
