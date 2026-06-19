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
  import { generateBus } from '../state/generate-bus.svelte';
  import { exportGeneratedGcode, exportSimulatedStockStl } from '../services/file_ops';
  import type { SimWarning, TimeEstimate } from '../api/types';
  import { pipelineWarningSeverity, type PipelineWarning } from '../api/pipeline-warnings';
  import { summarizeWarnings } from '../state/warnings-summary';
  import GenerateProgress from './GenerateProgress.svelte';
  import FloatingPanel from './FloatingPanel.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { warningFocus } from '../state/warning-focus.svelte';
  import { tick } from 'svelte';

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
    const r = project.gen.generated as { time_estimate?: TimeEstimate } | null;
    return r?.time_estimate ?? null;
  }

  const client = defaultClient();
  type PostId = 'linuxcnc' | 'grbl' | 'hpgl';
  function coercePost(v: string): PostId {
    return v === 'grbl' || v === 'hpgl' ? v : 'linuxcnc';
  }
  // The gcode dialect is now a MACHINE setting (chosen in the Machine
  // dialog) rather than a toolbar dropdown — a controller speaks one
  // dialect. Derive from the machine, falling back to the last-used /
  // persisted choice when the machine doesn't specify one.
  const post = $derived<PostId>(
    coercePost(project.data.machine.gcodeDialect ?? workspace.get().last_post_processor),
  );
  /// Tracks which post-processor produced the currently cached `project.gen.generated`
  /// gcode buffer. When the user flips the dropdown to a different dialect, the
  /// cached text is now wrong-dialect — exporting it via the Download button
  /// would write LinuxCNC gcode into a .plt (HPGL) file. Clear the cache so the
  /// user is forced to regen, matching how a machine swap invalidates the run.
  let generatedPost: PostId | null = null;
  $effect(() => {
    // Drop the post tag whenever the cached gcode disappears (project
    // reload, manual clear, machine swap) so the next run re-captures
    // the current dropdown choice.
    if (project.gen.generated == null) generatedPost = null;
  });
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
    if (generatedPost != null && generatedPost !== current && project.gen.generated != null) {
      // Dialect changed — drop the cached gcode so Download can't emit
      // it into a file with the new dialect's extension.
      project.gen.generated = null;
      generatedPost = null;
    }
  });
  let warningPanelOpen = $state(false);
  let abortController: AbortController | null = null;

  /// React to a per-op warning-focus request (set by the op status
  /// badge in OperationsList). Open the warnings panel, then — after
  /// the rows render — expand every <details> for that op and scroll
  /// the first into view. Reads `seq` so repeat clicks on the same op
  /// re-fire. Clearing `opId` re-runs this once more, no-op'd by the
  /// early return.
  $effect(() => {
    const opId = warningFocus.opId;
    void warningFocus.seq;
    if (opId == null) return;
    warningPanelOpen = true;
    void tick().then(() => {
      if (typeof document === 'undefined') return;
      const panel = document.querySelector('[aria-label="Warnings"]');
      const rows = panel?.querySelectorAll<HTMLDetailsElement>(`details[data-op-id="${opId}"]`);
      rows?.forEach((r, i) => {
        r.open = true;
        if (i === 0) r.scrollIntoView({ block: 'nearest' });
      });
      warningFocus.clear();
    });
  });

  function cancelRun() {
    if (project.gen.pipelineState !== 'running') return;
    project.cancelGenerate();
    abortController?.abort();
  }

  // Auto-regenerate on edit. Watch project.data.dirty + the setting;
  // when both are true and we're not already running, debounce ~1.5s
  // and fire run(). Cancel prior pending debounce on each new edit.
  let autoTimer: ReturnType<typeof setTimeout> | null = null;
  const AUTO_REGEN_DEBOUNCE_MS = 1500;
  $effect(() => {
    void project.data.dirty;
    void project.data.settings.autoRegenerate;
    if (autoTimer) {
      clearTimeout(autoTimer);
      autoTimer = null;
    }
    if (
      !project.data.settings.autoRegenerate ||
      !project.data.dirty ||
      !project.geometryView ||
      project.gen.pipelineState === 'running' ||
      project.gen.pipelineState === 'cancelling'
    ) {
      return;
    }
    autoTimer = setTimeout(() => {
      autoTimer = null;
      // Re-check guards at fire time — the user may have already hit
      // Generate manually, or pipelineState may have flipped.
      if (
        project.data.settings.autoRegenerate &&
        project.data.dirty &&
        project.geometryView &&
        project.gen.pipelineState !== 'running' &&
        project.gen.pipelineState !== 'cancelling'
      ) {
        void run();
      }
    }, AUTO_REGEN_DEBOUNCE_MS);
    // Cleanup on unmount (not just on re-run): without this a pending
    // debounce survives teardown and fires run() after the bar is gone,
    // re-entering the generate pipeline against stale state.
    return () => {
      if (autoTimer) {
        clearTimeout(autoTimer);
        autoTimer = null;
      }
    };
  });

  // Warning aggregation + severity lane is shared with the phone
  // PhoneWarnings bar via state/warnings-summary.ts — the two copies had
  // drifted (this desktop one treated a clean Generate as "not run yet").
  // Surface pipeline-level warnings in the same panel as sim warnings: a
  // Generate that raises, say, `op_source_empty` or `tool_too_large` flags
  // the chip, so we render BOTH lists in one panel with a source tag per
  // row rather than gating the panel on a non-null simDiagnostics.
  //
  // Critical-count spans BOTH sim warnings AND pipeline-level warnings
  // (tool_too_large, op_order_suspect, frame_padding_below_tool_radius,
  // spindle_speed_clamped_above_max, stock_origin_outside_geometry_bbox, …).
  // The safety gate must count planning-time pipeline warnings, not just
  // sim post-mortem ones — otherwise a Pocket whose tool didn't fit emits
  // zero toolpath, raises `tool_too_large`, and the user's "block on
  // critical" setting does NOT prevent the broken gcode from shipping.
  // Both envelope checks are pipeline-side: the work-area half
  // (`out_of_work_area`) and the STOCK half (`out_of_stock`,
  // warnings.rs::push_stock_warning), since the core `Project` carries a
  // resolved stock box. Both ride in on `project.gen.generated.warnings`,
  // so the frontend does not synthesize either — doing so would double-count.
  const warningSummary = $derived(
    summarizeWarnings({
      simWarnings: project.gen.simDiagnostics?.warnings ?? [],
      pipelineWarnings:
        (project.gen.generated as { warnings?: PipelineWarning[] } | null)?.warnings ?? [],
      hasGenerated: project.gen.generated != null,
      hasSimDiagnostics: project.gen.simDiagnostics != null,
      dirty: project.data.dirty,
    }),
  );
  let warnings = $derived(warningSummary.sim);
  let allPipelineWarnings = $derived(warningSummary.pipeline);
  // Tier-4 safety: count out-of-work-area moves from the last Generate.
  // `out_of_work_area` is intentionally NOT a critical kind (the default
  // envelope is often a placeholder), so it never blocks via the critical
  // gate; the opt-in `blockOnWorkAreaViolation` setting promotes it to a
  // hard EXPORT block for operators who've set their real envelope.
  let workAreaViolationCount = $derived(
    allPipelineWarnings.filter((w) => w.kind === 'out_of_work_area').length,
  );
  let criticalCount = $derived(warningSummary.critical);
  let totalWarningCount = $derived(warningSummary.total);

  async function run() {
    if (!project.geometryView) return;
    // NOTE: Generate is deliberately NOT gated on `criticalCount`. The
    // critical-warning safety check blocks DOWNLOAD (see downloadGcode +
    // the SettingsDialog hint: "required before downloading G-code"), not
    // generation. Gating Generate here deadlocked the fix loop: a critical
    // warning (e.g. pocket_fill_incomplete) can only be cleared by
    // re-generating after a parameter change, but the gate refused that
    // very re-generate because the count still reflected the stale prior
    // run — so the warning never went away.
    project.beginGenerate();
    abortController = new AbortController();
    try {
      const opProject = buildProject({
        transformedImport: project.transformedImport,
        geometryView: project.geometryView,
        stockSizingImport: project.stockSizingImport,
        machine: project.data.machine,
        tools: project.data.tools,
        operations: project.data.operations,
        fixtures: project.data.fixtures,
        textLayers: project.data.textLayers,
        workOffset: project.data.workOffset,
        stock: project.data.stock,
        reliefSources: project.data.reliefSources,
        groupOpsByTool: project.data.groupOpsByTool,
      });
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
            // Live progress is read from project.gen.pipelineProgress, which
            // notePipelineEvent maintains; the bar binds to that.
            project.notePipelineEvent(ev);
          },
          abortController.signal,
        );
      } else if (client.generateStream) {
        // Coarse-grained streaming fallback (no per-op events). The bar
        // shows an indeterminate running state via project.gen.pipelineState;
        // there's no fraction to surface here, so the callback is a no-op.
        r = await client.generateStream(req, () => {});
      } else {
        r = await client.generate(req);
      }
      project.setGenerated(r);
      generatedPost = post;
      project.finishGenerate();
    } catch (e) {
      if (e instanceof CancelledError) {
        // Cancelled by the user — just snap back to idle.
        project.gen.pipelineState = 'idle';
      } else {
        const raw = e instanceof Error ? e.message : String(e);
        const structured = tryParseStructuredError(raw);
        project.failGenerate(structured ?? raw);
      }
    } finally {
      project.endGenerate();
      abortController = null;
    }
  }

  // Honour external (re-)generate requests — e.g. the phone pull-to-refresh
  // gesture (7jug.12). Only act on a fresh request and only when the
  // pipeline is idle; `run()` itself early-bails (no ops → failGenerate).
  let lastGenSeq = 0;
  $effect(() => {
    const seq = generateBus.seq;
    if (seq === lastGenSeq) return;
    lastGenSeq = seq;
    if (seq > 0 && project.gen.pipelineState === 'idle') void run();
  });

  async function downloadGcode() {
    // If the program we'd ship has critical warnings and the user hasn't
    // disabled the safety gate, refuse to write the file — a broken /
    // unsafe .ngc on a machine is the failure we're guarding against.
    // This is the ONLY place the critical-warning safety check blocks
    // (Generate stays open so the operator can iterate to a fix); it
    // covers BOTH the pipeline's findings (tool_too_large,
    // pocket_fill_incomplete, out_of_stock, …) AND the heightfield sim's
    // collisions / rapid-through-material. `criticalCount` aggregates
    // both, and simDiagnostics is cleared on every Generate (see
    // setGenerated) so it always reflects the toolpath being exported.
    if (project.data.settings.blockOnCriticalSimWarnings && criticalCount > 0) {
      project.setError(
        `${criticalCount} critical warning${criticalCount === 1 ? '' : 's'} (collisions / unsafe cuts) — fix or disable the safety check in Settings before downloading`,
      );
      return;
    }
    // Tier-4: opt-in hard gate on out-of-work-area moves. Blocks EXPORT
    // only (Generate/preview stay open so the operator can see + fix the
    // violation). The toolpath leaves the machine envelope — sending it
    // risks a soft-limit fault or a gantry crash.
    if (project.data.settings.blockOnWorkAreaViolation && workAreaViolationCount > 0) {
      project.setError(
        `${workAreaViolationCount} move${workAreaViolationCount === 1 ? '' : 's'} leave the machine work area — fix the path / work-offset, widen the work area, or disable the work-area gate in Settings`,
      );
      return;
    }
    await exportGeneratedGcode(post);
  }

  function flyToWarning(w: SimWarning) {
    const segIdx = simWarningSegmentIdx(w);
    const cum = project.gen.toolpathCumLen;
    const total = project.gen.toolpathTotalLen;
    if (cum && total > 0 && segIdx >= 0 && segIdx < cum.length) {
      project.playhead = Math.min(1, cum[segIdx] / total);
    } else {
      const segs = project.gen.generated?.toolpath.length ?? 0;
      if (segs > 0) project.playhead = Math.min(1, (segIdx + 1) / segs);
    }
  }

  /// Apply-Fix handler for the `stock_origin_outside_geometry_bbox`
  /// pipeline warning. Snaps the WCS origin to the footprint's bottom-left
  /// corner and re-generates. Delegates to the shared
  /// `project.snapWorkOffsetToFootprint()` so desktop and phone behave
  /// identically (the divergent inline copies had drifted — the old web
  /// path bailed for text-only projects, where `transformedImport` is
  /// null, even though that's the common case for this warning).
  function applyWcsBboxSnapFix() {
    project.snapWorkOffsetToFootprint();
    generateBus.request();
  }

  // The chip presentation is driven by the shared severity lane
  // (state/warnings-summary.ts). Color class maps 1:1 to the severity
  // value; only the wording + glyph are desktop-specific.
  function chipClass(): string {
    return `sim-chip ${warningSummary.severity}`;
  }

  function chipLabel(): string {
    // Chip wording is neutral — sim AND pipeline both feed the count,
    // so labelling the chip "Sim" misled users into hunting in the sim
    // diagnostic UI for a warning that was actually emitted by the CAM
    // pipeline. The panel that opens still tags each row with its source
    // (sim / pipeline) so attribution stays visible.
    switch (warningSummary.severity) {
      case 'idle':
        return 'Warnings: not run yet — Generate first';
      case 'stale':
        return 'Warnings: stale — re-Generate';
      case 'clean':
        return 'No warnings';
      case 'critical':
        return `${totalWarningCount} warning${totalWarningCount === 1 ? '' : 's'} (${criticalCount} critical)`;
      default:
        return `${totalWarningCount} warning${totalWarningCount === 1 ? '' : 's'}`;
    }
  }

  function chipGlyph(): string {
    switch (warningSummary.severity) {
      case 'idle':
        return '🛡';
      case 'stale':
        return '↻';
      case 'clean':
        return '✓';
      case 'critical':
        return '⛔';
      default:
        return '⚠';
    }
  }

  const SIM_IDLE_HINT =
    'Sim verification runs after Generate. Catches rapid moves through stock, fixture collisions, and cutter holder collisions before you cut.';
</script>

<div class="bar">
  {#if project.gen.pipelineState === 'running' || project.gen.pipelineState === 'cancelling'}
    <GenerateProgress onCancel={cancelRun} />
  {:else}
    <button
      onclick={run}
      disabled={(!project.geometryView && project.data.textLayers.length === 0) ||
        project.gen.generating}
      class:stale={project.data.dirty && project.gen.generated != null}
      title={project.data.dirty && project.gen.generated != null
        ? 'The visible toolpath is stale — the project has changed since the last Generate. Click to refresh.'
        : 'Run the CAM pipeline and produce a toolpath. Reads the current ops, tools, stock, and machine — output is cached so unchanged ops re-emit instantly.'}
    >
      {#if project.gen.generating}
        Generating G-code…
      {:else if project.data.dirty && project.gen.generated != null}
        Regenerate G-code
      {:else}
        Generate G-code
      {/if}
    </button>
  {/if}
  {#if project.gen.generated}
    <button
      onclick={downloadGcode}
      class="download"
      title="Save the generated toolpath to disk in the selected dialect's file extension."
    >
      {post === 'hpgl' ? 'Download .plt' : 'Download .ngc'}
    </button>
    <button
      onclick={() => void exportSimulatedStockStl()}
      class="download"
      title="Export the simulated (carved) stock as a binary STL mesh."
    >
      STL
    </button>
    <span class="stats">
      {project.gen.generated.stats.object_count} obj · {project.gen.generated.stats.offset_count} offsets
      · {project.gen.generated.toolpath.length} moves
      {#if project.gen.lastGenerateCachedCount > 0}
        <span class="cached-tag"
          >· {project.gen.lastGenerateCachedCount} of {project.gen.lastGenerateOpCount} cached</span
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
    <!-- Source-file-changed chip lives in the toolbar where the user
         looks for actionable state, instead of the standalone
         bottom-right toast that competed with other floating UI. -->
    <span
      class="stale-chip"
      role="alert"
      aria-live="polite"
      title={`Source file changed on disk: ${project.sourceFileStaleNotice.path}. Reload to pick up the changes; Ignore to keep the current view.`}
    >
      <span class="stale-msg">
        ⟳ <strong>{project.sourceFileStaleNotice.path.split(/[\\/]/).pop()}</strong> changed
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
  {#if warningSummary.severity === 'idle'}
    <span class="sim-chip idle" title={SIM_IDLE_HINT}>
      🛡 {chipLabel()}
    </span>
  {:else}
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
  <!-- The separate 'bounds' chip was removed — its out-of-stock /
       out-of-work-area findings are already folded into
       `allPipelineWarnings` (synthesized as PipelineWarning rows), so
       they're counted in `totalWarningCount`, listed in the panel, and
       drive the single warnings chip's critical color. One button now. -->
</div>

<!-- Floating, drag-movable + resizable panel (mechanics live in
     FloatingPanel; it stays mounted so the in-session position/size
     survive close + reopen). Each row is a browser-dev-tools-style
     <details> — summary collapses to a one-line header (dot · source ·
     kind · ellipsed msg + Go-to for sim rows), expanded body shows the
     full message with user-select: text so the user can drag-select and
     copy. Lists both sim + pipeline warnings, tagged by source. -->
<FloatingPanel
  open={warningPanelOpen}
  onClose={() => (warningPanelOpen = false)}
  title="Warnings ({totalWarningCount})"
  ariaLabel="Warnings"
>
  <div class="list">
    {#if totalWarningCount === 0}
      <p class="empty">No warnings — sim and pipeline are clean.</p>
    {:else}
      {#each warnings as w, i (`sim-${i}`)}
        {@const sev = simWarningSeverity(w)}
        {@const summary = simWarningSummary(w)}
        <details class="row severity-{sev}">
          <summary>
            <span class="dot" aria-hidden="true"></span>
            <span class="source" title="Surfaced by the simulator after G-code generation."
              >sim</span
            >
            <span class="kind">{w.kind}</span>
            <span class="msg">{summary}</span>
            <button
              type="button"
              class="row-action"
              onclick={(e) => {
                e.stopPropagation();
                flyToWarning(w);
              }}
              title="Move the 3D playhead to this warning"
              aria-label="Go to warning in 3D scene"
            >
              go to
            </button>
          </summary>
          <div class="row-body">
            <p class="full-msg">{summary}</p>
            <pre class="json">{JSON.stringify(w, null, 2)}</pre>
          </div>
        </details>
      {/each}
      {#each allPipelineWarnings as pw, i (`pipe-${i}`)}
        {@const sev = pipelineWarningSeverity(pw)}
        {@const hasFix = pw.kind === 'stock_origin_outside_geometry_bbox'}
        <details class="row severity-{sev} pipeline" data-op-id={pw.op_id ?? undefined}>
          <summary>
            <span class="dot" aria-hidden="true"></span>
            <span
              class="source pipeline"
              title="Surfaced by the CAM pipeline during G-code generation.">pipeline</span
            >
            <span class="kind">{pw.kind}</span>
            <span class="msg">{pw.message}</span>
            {#if hasFix}
              <button
                type="button"
                class="row-action"
                onclick={(e) => {
                  e.stopPropagation();
                  applyWcsBboxSnapFix();
                }}
                title="Snap the WCS origin to the geometry bbox's bottom-left corner — the canonical CNC zeroing convention."
                aria-label="Apply suggested WCS origin"
              >
                apply fix
              </button>
            {/if}
          </summary>
          <div class="row-body">
            <p class="full-msg">{pw.message}</p>
            <pre class="json">{JSON.stringify(pw, null, 2)}</pre>
          </div>
        </details>
      {/each}
    {/if}
  </div>
</FloatingPanel>

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
  button {
    background: var(--accent);
    color: white;
    border: none;
    padding: 0.3rem 0.7rem;
    border-radius: 4px;
    font-size: 0.78rem;
    cursor: pointer;
    transition:
      background 80ms,
      box-shadow 80ms;
  }
  button:hover:not(:disabled) {
    background: var(--accent-strong);
  }
  button.stale {
    /* Generate button when the cached toolpath is older than the current
       project state. Warning-tinted background so the user notices the
       toolpath they're looking at doesn't reflect their latest edits. */
    background: color-mix(in srgb, var(--warn) 70%, var(--accent));
  }
  button.stale:hover:not(:disabled) {
    background: var(--warn);
    color: var(--text-strong);
  }
  button.download {
    background: var(--success-bg);
  }
  button.download:hover:not(:disabled) {
    background: color-mix(in srgb, var(--success-bg) 80%, white);
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
    z-index: var(--z-floating);
    background: var(--bg-panel);
    outline: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px var(--shadow-modal);
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
  /* Source-file-changed chip. Warning palette, inline Reload / dismiss
     buttons so the user can act without leaving the toolbar. */
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
    /* Cap relative to viewport so long file names get more room on
       wide displays; the title tooltip carries the full path either way. */
    max-width: min(40ch, 40vw);
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
  /* Warnings content inside the FloatingPanel body — `flex: 1` fills
     the panel's column layout; user-select: text on the body so users
     can copy. */
  .list {
    flex: 1;
    overflow: auto;
    padding: 0.4rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    user-select: text;
  }
  .list .empty {
    color: var(--text-muted);
    font-size: 0.78rem;
    margin: 0.5rem;
    text-align: center;
  }
  /* Each row is a <details>. Summary lays out the row contents in a
     grid with the chevron suppressed (we don't need the default
     browser caret next to the dot — the row already signals
     interactivity via background/hover). */
  .list details.row {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    font-size: 0.74rem;
    overflow: hidden;
  }
  .list details.row > summary {
    display: grid;
    grid-template-columns: 0.8rem 3.6rem 8rem minmax(0, 1fr) auto;
    align-items: center;
    gap: 0.5rem;
    padding: 0.35rem 0.55rem;
    cursor: pointer;
    list-style: none;
  }
  .list details.row > summary::-webkit-details-marker {
    display: none;
  }
  .list details.row > summary:hover {
    background: color-mix(in srgb, var(--accent) 8%, var(--bg-elevated));
  }
  .list details.row[open] > summary {
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--accent) 6%, var(--bg-elevated));
  }
  .list .dot {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
  }
  .list .severity-critical .dot {
    background: var(--marker-critical);
  }
  .list .severity-warning .dot {
    background: var(--marker-warn);
  }
  .list .severity-info .dot {
    background: var(--marker-info);
  }
  .list .source {
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
  .list .source.pipeline {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  }
  .list .kind {
    font-family: ui-monospace, monospace;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .list .msg {
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .list .row-action {
    background: transparent;
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, transparent);
    border-radius: 3px;
    padding: 0.05rem 0.4rem;
    font-size: 0.7rem;
    cursor: pointer;
    line-height: 1.3;
  }
  .list .row-action:hover {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    color: var(--accent-strong);
    border-color: var(--accent-strong);
  }
  /* Expanded body — full message + JSON dump. user-select: text so
     drag-select to copy works (a <button> wrapper would break
     selection in some browsers). */
  .list .row-body {
    padding: 0.4rem 0.6rem 0.55rem;
    background: var(--bg-app);
    user-select: text;
  }
  .list .row-body .full-msg {
    margin: 0 0 0.35rem;
    color: var(--text-strong);
    font-size: 0.78rem;
    line-height: 1.4;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .list .row-body .json {
    margin: 0;
    padding: 0.4rem 0.55rem;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: 3px;
    color: var(--text-muted);
    font-family: ui-monospace, monospace;
    font-size: 0.7rem;
    line-height: 1.35;
    max-height: 16rem;
    overflow: auto;
    white-space: pre;
    user-select: text;
  }
</style>
