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
  import { inferDefaultWorkOffset } from '../state/project-types';

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
  /// Tracks which post-processor produced the currently cached `project.generated`
  /// gcode buffer. When the user flips the dropdown to a different dialect, the
  /// cached text is now wrong-dialect — exporting it via the Download button
  /// would write LinuxCNC gcode into a .plt (HPGL) file. Clear the cache so the
  /// user is forced to regen, matching how a machine swap invalidates the run.
  let generatedPost: PostId | null = null;
  $effect(() => {
    // Drop the post tag whenever the cached gcode disappears (project
    // reload, manual clear, machine swap) so the next run re-captures
    // the current dropdown choice.
    if (project.generated == null) generatedPost = null;
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
    if (
      generatedPost != null &&
      generatedPost !== current &&
      project.generated != null
    ) {
      // Dialect changed — drop the cached gcode so Download can't emit
      // it into a file with the new dialect's extension.
      project.generated = null;
      generatedPost = null;
    }
  });
  let progressMsg = $state<string>('');
  let progressFrac = $state<number>(0);
  let warningPanelOpen = $state(false);
  let abortController: AbortController | null = null;

  // ──────────────────────────────────────────────────────────────────
  // Warnings floating panel: drag-movable header + resize handle (aw8j).
  // Position is in viewport pixels relative to (0,0); size is the
  // panel's content box. Defaults sit the panel in the top-right
  // (same place it lived when it was inline-absolute), but the user
  // can drag the header to reposition and pull the bottom-right corner
  // to resize. State is component-local — re-opening resets to the
  // last in-session position unless the window has shrunk past it,
  // in which case `clampPanelRect` snaps it back into view.
  const WP_DEFAULT_W = 480;
  const WP_DEFAULT_H = Math.round(typeof window === 'undefined' ? 480 : window.innerHeight * 0.6);
  let wpX = $state<number | null>(null); // null = uncomputed → default to top-right on first open
  let wpY = $state<number | null>(null);
  let wpW = $state<number>(WP_DEFAULT_W);
  let wpH = $state<number>(WP_DEFAULT_H);
  let wpDrag: { mode: 'move' | 'resize'; offX: number; offY: number; pointerId: number } | null = null;

  function clampPanelRect() {
    if (typeof window === 'undefined') return;
    const minW = 320,
      minH = 220;
    wpW = Math.max(minW, Math.min(window.innerWidth - 16, wpW));
    wpH = Math.max(minH, Math.min(window.innerHeight - 16, wpH));
    if (wpX != null) wpX = Math.max(8, Math.min(window.innerWidth - wpW - 8, wpX));
    if (wpY != null) wpY = Math.max(8, Math.min(window.innerHeight - wpH - 8, wpY));
  }

  function onWarningPanelOpen() {
    if (typeof window === 'undefined') return;
    if (wpX == null || wpY == null) {
      // First-open default: top-right with 1rem (~16 px) margins, matches
      // the inline-absolute position the panel had before.
      wpX = Math.max(16, window.innerWidth - wpW - 16);
      wpY = 56; // toolbar height + a bit
    }
    clampPanelRect();
  }
  $effect(() => {
    if (warningPanelOpen) onWarningPanelOpen();
  });

  function wpHeaderPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest('button')) return; // don't grab a drag from the close button
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    wpDrag = {
      mode: 'move',
      offX: e.clientX - (wpX ?? 0),
      offY: e.clientY - (wpY ?? 0),
      pointerId: e.pointerId,
    };
    e.preventDefault();
  }
  function wpResizePointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    wpDrag = {
      mode: 'resize',
      offX: e.clientX - wpW,
      offY: e.clientY - wpH,
      pointerId: e.pointerId,
    };
    e.preventDefault();
  }
  function wpPointerMove(e: PointerEvent) {
    if (!wpDrag || e.pointerId !== wpDrag.pointerId) return;
    if (wpDrag.mode === 'move') {
      wpX = e.clientX - wpDrag.offX;
      wpY = e.clientY - wpDrag.offY;
    } else {
      wpW = e.clientX - wpDrag.offX;
      wpH = e.clientY - wpDrag.offY;
    }
    clampPanelRect();
  }
  function wpPointerUp(e: PointerEvent) {
    if (!wpDrag || e.pointerId !== wpDrag.pointerId) return;
    wpDrag = null;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {}
  }
  // Re-clamp when the viewport changes so a previously-sized panel can't
  // sit off-screen after the user shrinks the window.
  function onWindowResize() {
    if (warningPanelOpen) clampPanelRect();
  }

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
  // `allPipelineWarnings` is defined after `boundsScan` / `boundsWarnings`
  // so the synthesized bounds rows (out_of_stock / out_of_work_area)
  // get folded into the same severity classifier + warnings-panel render
  // as the pipeline's own warnings. The combined-count + critical-count
  // derivations move down with it.

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
  /// Bounds findings projected as PipelineWarning-shaped rows so they
  /// render in the warnings panel alongside the pipeline's own findings
  /// (and participate in the same severity classifier + critical gate).
  /// Returns 0 / 1 / 2 entries — one per offending axis.
  const boundsWarnings = $derived.by<PipelineWarning[]>(() => {
    const b = boundsScan;
    if (!b) return [];
    const out: PipelineWarning[] = [];
    if (b.outStock > 0) {
      out.push({
        kind: 'out_of_stock',
        message: `${b.outStock} cut move${b.outStock === 1 ? '' : 's'} outside the stock${b.firstStockLine ? ` (first at gcode line ${b.firstStockLine})` : ''}. The controller will try to cut into air or below the stock — either re-zero the machine, expand the stock, or translate the geometry into the stock bbox.`,
      });
    }
    if (b.outWA > 0) {
      out.push({
        kind: 'out_of_work_area',
        message: `${b.outWA} cut move${b.outWA === 1 ? '' : 's'} outside the machine work area${b.firstWaLine ? ` (first at gcode line ${b.firstWaLine})` : ''}. The controller may refuse the move (soft-limit fault) or, worse, crash into the gantry. Set Project.work_offset so the cuts land inside the work envelope.`,
      });
    }
    return out;
  });
  /// Pipeline warnings + the synthesized bounds rows, fed through the
  /// same panel render + severity gate. Declared HERE (not next to
  /// `pipelineWarnings`) so `boundsWarnings` above is initialized first.
  let allPipelineWarnings = $derived<PipelineWarning[]>([
    ...pipelineWarnings,
    ...boundsWarnings,
  ]);
  let pipelineCriticalCount = $derived(countCriticalPipelineWarnings(allPipelineWarnings));
  let criticalCount = $derived(
    warnings.filter((w) => simWarningSeverity(w) === 'critical').length + pipelineCriticalCount,
  );
  let totalWarningCount = $derived(warnings.length + allPipelineWarnings.length);
  let isClean = $derived(totalWarningCount === 0 && pipelineCriticalCount === 0);

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
      generatedPost = post;
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

  /// Apply-Fix handler for the `stock_origin_outside_geometry_bbox`
  /// pipeline warning (audit abdk). Snaps the WCS origin to the geometry
  /// bbox's bottom-left corner — the same inference the import-time
  /// auto-default uses (audit gldc), but applied to the CURRENT state
  /// rather than fresh-import-only. Routes through `setWorkOffset` so
  /// the change is undoable.
  function applyWcsBboxSnapFix() {
    const imp = project.transformedImport;
    if (!imp) return;
    // Force the inference even when the current offset isn't default —
    // user clicked Apply Fix, they're explicitly asking.
    const next = inferDefaultWorkOffset(imp.bbox, {
      x_mm: 0,
      y_mm: 0,
      z_mm: 0,
      wcs: project.workOffset.wcs,
    });
    project.setWorkOffset({ x_mm: next.x_mm, y_mm: next.y_mm });
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
    // qvsa: chip wording is neutral — sim AND pipeline both feed the
    // count, so labelling the chip "Sim" misled users into hunting in
    // the sim diagnostic UI for a warning that was actually emitted
    // by the CAM pipeline. The panel that opens still tags each row
    // with its source (sim / pipeline) so attribution stays visible.
    if (project.simDiagnostics == null && pipelineWarnings.length === 0) {
      return 'Warnings: not run yet — Generate first';
    }
    if (simStale) return 'Warnings: stale — re-Generate';
    if (isClean) return 'No warnings';
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
      class:stale={project.dirty && project.generated != null}
      title={project.dirty && project.generated != null
        ? 'The visible toolpath is stale — the project has changed since the last Generate. Click to refresh.'
        : 'Run the CAM pipeline and produce a toolpath. Reads the current ops, tools, stock, and machine — output is cached so unchanged ops re-emit instantly.'}
    >
      {#if project.generating}
        Generating G-code…
      {:else if project.dirty && project.generated != null}
        Regenerate G-code
      {:else}
        Generate G-code
      {/if}
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
    <span class="stale-chip" role="alert" aria-live="polite" title={`Source file changed on disk: ${project.sourceFileStaleNotice.path}. Reload to pick up the changes; Ignore to keep the current view.`}>
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
    <button
      type="button"
      class="sim-chip bounds"
      onclick={() => (warningPanelOpen = !warningPanelOpen)}
      aria-expanded={warningPanelOpen}
      title="Click for details — the bounds findings are listed in the warnings panel alongside the pipeline / sim warnings."
    >
      <span class="glyph" aria-hidden="true">⚠</span>
      {#if boundsScan.outWA > 0 && boundsScan.outStock > 0}
        {boundsScan.outStock} out-of-stock · {boundsScan.outWA} out-of-machine
      {:else if boundsScan.outWA > 0}
        {boundsScan.outWA} cut move{boundsScan.outWA === 1 ? '' : 's'} outside work area
      {:else}
        {boundsScan.outStock} cut move{boundsScan.outStock === 1 ? '' : 's'} outside stock
      {/if}
    </button>
  {/if}
</div>

<svelte:window onresize={onWindowResize} />

{#if warningPanelOpen}
  <!-- aw8j: floating, drag-movable + resizable panel. Each row is a
       browser-dev-tools-style <details> — summary collapses to a one-line
       header (dot · source · kind · ellipsed msg + Go-to for sim rows),
       expanded body shows the full message with user-select: text so the
       user can drag-select and copy. dvs4: lists both sim + pipeline
       warnings, tagged by source. -->
  <div
    class="panel"
    role="dialog"
    aria-label="Warnings"
    style:left="{wpX ?? 0}px"
    style:top="{wpY ?? 0}px"
    style:width="{wpW}px"
    style:height="{wpH}px"
  >
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <header
      role="toolbar"
      tabindex="-1"
      aria-label="Warnings panel header — drag to move"
      onpointerdown={wpHeaderPointerDown}
      onpointermove={wpPointerMove}
      onpointerup={wpPointerUp}
      onpointercancel={wpPointerUp}
      title="Drag to move"
    >
      <h3>Warnings ({totalWarningCount})</h3>
      <button class="dlg-close" onclick={() => (warningPanelOpen = false)} aria-label="Close">×</button>
    </header>
    <div class="list">
      {#if totalWarningCount === 0}
        <p class="empty">No warnings — sim and pipeline are clean.</p>
      {:else}
        {#each warnings as w, i (`sim-${i}`)}
          {@const sev = simWarningSeverity(w)}
          {@const summary = simWarningSummary(w)}
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <details class="row severity-{sev}">
            <summary>
              <span class="dot" aria-hidden="true"></span>
              <span class="source" title="Surfaced by the simulator after gcode generation.">sim</span>
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
          <details class="row severity-{sev} pipeline">
            <summary>
              <span class="dot" aria-hidden="true"></span>
              <span class="source pipeline" title="Surfaced by the CAM pipeline during gcode generation.">pipeline</span>
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
                  disabled={!project.transformedImport}
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
    <!-- Bottom-right resize handle. svg corner-glyph repeats the
         convention used by every other floating-resizable widget on
         the platform. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="resize-handle"
      onpointerdown={wpResizePointerDown}
      onpointermove={wpPointerMove}
      onpointerup={wpPointerUp}
      onpointercancel={wpPointerUp}
      title="Drag to resize"
      aria-hidden="true"
    ></div>
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
  /* aw8j: floating panel — fixed positioning so the drag-movable
     top/left coordinates work in screen space rather than inheriting
     a relative offset from the toolbar. Resize handle in the SE
     corner; user-select: text on the body so users can copy. */
  .panel {
    position: fixed;
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    box-shadow: 0 6px 18px var(--shadow-modal);
    z-index: var(--z-floating);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    min-width: 320px;
    min-height: 220px;
  }
  .panel header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.7rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
    cursor: grab;
    user-select: none;
    touch-action: none;
  }
  .panel header:active {
    cursor: grabbing;
  }
  .panel h3 {
    font-size: 0.85rem;
    margin: 0;
    color: var(--text-strong);
  }
  /* The panel's close uses the shared `.dlg-close` (audit hbi7). */
  .panel .list {
    flex: 1;
    overflow: auto;
    padding: 0.4rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    user-select: text;
  }
  .panel .empty {
    color: var(--text-muted);
    font-size: 0.78rem;
    margin: 0.5rem;
    text-align: center;
  }
  /* Each row is a <details>. Summary lays out the row contents in a
     grid with the chevron suppressed (we don't need the default
     browser caret next to the dot — the row already signals
     interactivity via background/hover). */
  .panel details.row {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    font-size: 0.74rem;
    overflow: hidden;
  }
  .panel details.row > summary {
    display: grid;
    grid-template-columns: 0.8rem 3.6rem 8rem minmax(0, 1fr) auto;
    align-items: center;
    gap: 0.5rem;
    padding: 0.35rem 0.55rem;
    cursor: pointer;
    list-style: none;
  }
  .panel details.row > summary::-webkit-details-marker {
    display: none;
  }
  .panel details.row > summary:hover {
    background: color-mix(in srgb, var(--accent) 8%, var(--bg-elevated));
  }
  .panel details.row[open] > summary {
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--accent) 6%, var(--bg-elevated));
  }
  .panel .dot {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
  }
  .panel .severity-critical .dot {
    background: var(--marker-critical);
  }
  .panel .severity-warning .dot {
    background: var(--marker-warn);
  }
  .panel .severity-info .dot {
    background: var(--marker-info);
  }
  .panel .source {
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
  .panel .source.pipeline {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  }
  .panel .kind {
    font-family: ui-monospace, monospace;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .panel .msg {
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .panel .row-action {
    background: transparent;
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, transparent);
    border-radius: 3px;
    padding: 0.05rem 0.4rem;
    font-size: 0.7rem;
    cursor: pointer;
    line-height: 1.3;
  }
  .panel .row-action:hover {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    color: var(--accent-strong);
    border-color: var(--accent-strong);
  }
  /* Expanded body — full message + JSON dump. user-select: text so
     drag-select to copy works (previously the row was a <button>
     which broke selection in some browsers). */
  .panel .row-body {
    padding: 0.4rem 0.6rem 0.55rem;
    background: var(--bg-app);
    user-select: text;
  }
  .panel .row-body .full-msg {
    margin: 0 0 0.35rem;
    color: var(--text-strong);
    font-size: 0.78rem;
    line-height: 1.4;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .panel .row-body .json {
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
  .panel .resize-handle {
    position: absolute;
    right: 0;
    bottom: 0;
    width: 14px;
    height: 14px;
    cursor: nwse-resize;
    touch-action: none;
    /* Two diagonal lines drawn as a corner glyph — matches the
       OS-native resize affordance. */
    background:
      linear-gradient(135deg, transparent 45%, var(--text-muted) 45%, var(--text-muted) 55%, transparent 55%) center / 100% 100% no-repeat,
      linear-gradient(135deg, transparent 70%, var(--text-muted) 70%, var(--text-muted) 80%, transparent 80%) center / 100% 100% no-repeat;
  }
  .panel .resize-handle:hover {
    background:
      linear-gradient(135deg, transparent 45%, var(--text-strong) 45%, var(--text-strong) 55%, transparent 55%) center / 100% 100% no-repeat,
      linear-gradient(135deg, transparent 70%, var(--text-strong) 70%, var(--text-strong) 80%, transparent 80%) center / 100% 100% no-repeat;
  }
</style>
