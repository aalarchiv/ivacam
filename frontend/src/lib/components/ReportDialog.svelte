<script lang="ts">
  /// j7n: printable project-summary report. Single-column layout that
  /// prints cleanly via window.print() — operators can hand-off the
  /// printout with the cut. Pulls every value from data the frontend
  /// already has (project state + project.generated). No new backend
  /// computation; deeper analyses (material removed, per-op time
  /// breakdown) deferred to a follow-up — the bd issue notes 4-5 days
  /// of work, this MVP ships the user-visible 80%.
  import Modal from './Modal.svelte';
  import { project, type OpEntry } from '../state/project.svelte';
  import type { TimeEstimate } from '../api/types';

  // Local HH:MM:SS formatter — mirrors the one in GenerateBar but kept
  // self-contained so the report stays printable in isolation.
  function formatHms(s: number | undefined): string {
    if (!s || !isFinite(s) || s < 0) return '00:00:00';
    const total = Math.round(s);
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const sec = total % 60;
    return [h, m, sec].map((n) => String(n).padStart(2, '0')).join(':');
  }

  interface Props {
    open: boolean;
    onClose: () => void;
  }
  let { open, onClose }: Props = $props();

  const now = new Date();
  const date = $derived(now.toLocaleString());

  const gen = $derived(project.generated);
  const timeEst = $derived<TimeEstimate | null>(
    (gen as { time_estimate?: TimeEstimate } | null)?.time_estimate ?? null,
  );

  const enabledOps = $derived(project.operations.filter((o) => o.enabled));
  /// Distinct tool IDs referenced by enabled ops, excluding Pause
  /// (which carries toolId 0 but uses no tool).
  const usedToolIds = $derived(
    Array.from(
      new Set(
        enabledOps
          .filter((o) => o.kind !== 'pause')
          .map((o) => o.toolId),
      ),
    ),
  );
  const usedTools = $derived(
    usedToolIds
      .map((id) => project.tools.find((t) => t.id === id))
      .filter((t): t is NonNullable<typeof t> => t != null),
  );

  function opSourceSummary(op: OpEntry): string {
    if (op.kind === 'pause') return '—';
    if (op.sourceObjects && op.sourceObjects.length > 0) {
      return `${op.sourceObjects.length} object${op.sourceObjects.length === 1 ? '' : 's'}`;
    }
    if (op.sourceLayers === null) return 'all geometry';
    if (op.sourceLayers.length === 0) return '— no layers —';
    return op.sourceLayers.map((l) => `"${l}"`).join(', ');
  }

  function opDepth(op: OpEntry): string {
    if (op.kind === 'pause') return '—';
    if ('depth' in op && op.depth != null) {
      return `${op.depth.toFixed(2)} mm`;
    }
    return '—';
  }

  function opStatusLabel(op: OpEntry): string {
    if (op.kind === 'pause') return 'pause';
    if (!gen) return 'not generated';
    const w = (gen.warnings ?? []).filter((x) => x.op_id === op.id);
    if (w.some((x) => x.kind === 'tool_kind_mismatch' || x.kind === 'tool_geometry_impossible')) {
      return 'error';
    }
    if (w.length > 0) return `${w.length} warning${w.length === 1 ? '' : 's'}`;
    return 'ok';
  }

  const warningsBySeverity = $derived.by(() => {
    const all = gen?.warnings ?? [];
    const bad = all.filter(
      (w) => w.kind === 'tool_kind_mismatch' || w.kind === 'tool_geometry_impossible',
    );
    return { bad: bad.length, warn: all.length - bad.length, top: all.slice(0, 5) };
  });

  function printNow() {
    window.print();
  }

  function projectName(): string {
    const src = project.transformedImport;
    if (src?.filename) return src.filename;
    if (project.activeProjectPath) {
      return project.activeProjectPath.split(/[\\/]/).pop() ?? 'Untitled project';
    }
    return 'Untitled project';
  }
</script>

{#if open}
  <Modal {onClose} modalClass="report-modal" width="min(640px, 96vw)">
    <header class="report-head">
      <h2>Project report</h2>
      <div class="report-actions">
        <button type="button" class="primary" onclick={printNow}>Print</button>
        <button type="button" class="close" onclick={onClose} aria-label="Close">×</button>
      </div>
    </header>
    <article class="report-body" id="report-print-root">
      <header class="rb-meta">
        <h1>{projectName()}</h1>
        <p class="rb-sub">{date} · wiaconstructor report</p>
      </header>

      <section>
        <h3>Toolpath stats</h3>
        {#if gen}
          <table>
            <tbody>
              <tr><th>Objects</th><td>{gen.stats.object_count}</td></tr>
              <tr><th>Offsets</th><td>{gen.stats.offset_count}</td></tr>
              <tr><th>Toolpath segments</th><td>{gen.toolpath.length}</td></tr>
              <tr><th>Enabled ops</th><td>{enabledOps.length}</td></tr>
            </tbody>
          </table>
        {:else}
          <p class="rb-empty">No G-code generated yet — run Generate first.</p>
        {/if}
      </section>

      {#if timeEst}
        <section>
          <h3>Estimated time</h3>
          <table>
            <tbody>
              <tr><th>Total</th><td>{formatHms(timeEst.total_s)}</td></tr>
              <tr><th>Cut</th><td>{formatHms(timeEst.cut_s)}</td></tr>
              <tr><th>Rapid</th><td>{formatHms(timeEst.rapid_s)}</td></tr>
              <tr><th>Plunge</th><td>{formatHms(timeEst.plunge_s)}</td></tr>
              <tr><th>Retract</th><td>{formatHms(timeEst.retract_s)}</td></tr>
              <tr><th>Arc</th><td>{formatHms(timeEst.arc_s)}</td></tr>
              <tr><th>Tool change</th><td>{formatHms(timeEst.toolchange_s)}</td></tr>
              <tr><th>Spindle warm-up</th><td>{formatHms(timeEst.spindle_warmup_s)}</td></tr>
            </tbody>
          </table>
        </section>
      {/if}

      <section>
        <h3>Tools used</h3>
        {#if usedTools.length === 0}
          <p class="rb-empty">No tools referenced by enabled operations.</p>
        {:else}
          <table>
            <thead>
              <tr><th>#</th><th>Name</th><th>⌀ mm</th><th>Kind</th><th>Feed</th><th>Speed</th></tr>
            </thead>
            <tbody>
              {#each usedTools as t (t.id)}
                <tr>
                  <td>{t.id}</td>
                  <td>{t.name}</td>
                  <td>{t.diameter.toFixed(2)}</td>
                  <td>{t.kind}</td>
                  <td>{t.feedRate ?? '—'}</td>
                  <td>{t.speed ?? '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>

      <section>
        <h3>Operations</h3>
        {#if enabledOps.length === 0}
          <p class="rb-empty">No enabled operations.</p>
        {:else}
          <table>
            <thead>
              <tr>
                <th>#</th><th>Name</th><th>Kind</th><th>Tool</th>
                <th>Source</th><th>Depth</th><th>Status</th>
              </tr>
            </thead>
            <tbody>
              {#each enabledOps as op (op.id)}
                <tr>
                  <td>{op.id}</td>
                  <td>{op.name}</td>
                  <td>{op.kind}</td>
                  <td>
                    {op.kind === 'pause' ? '—' : project.tools.find((t) => t.id === op.toolId)?.name ?? `#${op.toolId}`}
                  </td>
                  <td>{opSourceSummary(op)}</td>
                  <td>{opDepth(op)}</td>
                  <td>{opStatusLabel(op)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>

      {#if gen && warningsBySeverity.bad + warningsBySeverity.warn > 0}
        <section>
          <h3>Warnings</h3>
          <p>{warningsBySeverity.bad} critical · {warningsBySeverity.warn} non-critical</p>
          {#if warningsBySeverity.top.length > 0}
            <ul class="rb-warn-list">
              {#each warningsBySeverity.top as w}
                <li><strong>{w.kind}</strong> — {w.message}</li>
              {/each}
            </ul>
          {/if}
        </section>
      {/if}

      <footer class="rb-foot">
        <p>Generated by wiaconstructor · {date}</p>
      </footer>
    </article>
  </Modal>
{/if}

<style>
  .report-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.6rem 0.4rem;
    border-bottom: 1px solid var(--border);
  }
  .report-head h2 {
    margin: 0;
    font-size: 0.95rem;
  }
  .report-actions {
    display: inline-flex;
    gap: 0.3rem;
    align-items: center;
  }
  .report-actions .primary {
    background: var(--accent);
    color: #fff;
    border: 0;
    padding: 0.25rem 0.7rem;
    border-radius: 3px;
    font-size: 0.78rem;
    cursor: pointer;
  }
  .report-actions .primary:hover {
    background: var(--accent-strong);
  }
  .report-actions .close {
    background: transparent;
    color: var(--text-muted);
    border: 0;
    padding: 0.1rem 0.4rem;
    font-size: 1.1rem;
    cursor: pointer;
    line-height: 1;
  }
  .report-actions .close:hover {
    color: var(--text);
  }
  .report-body {
    padding: 1rem 1.1rem;
    max-height: 75vh;
    overflow-y: auto;
    font-size: 0.85rem;
  }
  .rb-meta h1 {
    margin: 0 0 0.15rem;
    font-size: 1.15rem;
  }
  .rb-sub {
    margin: 0 0 0.8rem;
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .report-body section {
    margin-bottom: 1.1rem;
  }
  .report-body h3 {
    margin: 0 0 0.35rem;
    font-size: 0.85rem;
    color: var(--text-strong);
    border-bottom: 1px solid var(--border);
    padding-bottom: 0.15rem;
  }
  .report-body table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.78rem;
  }
  .report-body table th,
  .report-body table td {
    text-align: left;
    padding: 0.18rem 0.3rem;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
  }
  .report-body table th {
    color: var(--text-muted);
    font-weight: 500;
  }
  .rb-empty {
    color: var(--text-muted);
    font-style: italic;
    font-size: 0.78rem;
  }
  .rb-warn-list {
    margin: 0.3rem 0 0;
    padding-left: 1.1rem;
    font-size: 0.74rem;
  }
  .rb-warn-list li {
    margin-bottom: 0.2rem;
  }
  .rb-warn-list strong {
    color: var(--text-strong);
    font-weight: 600;
    text-transform: uppercase;
    font-size: 0.7rem;
    letter-spacing: 0.04em;
  }
  .rb-foot {
    margin-top: 1.2rem;
    padding-top: 0.4rem;
    border-top: 1px solid var(--border);
    color: var(--text-muted);
    font-size: 0.7rem;
    text-align: right;
  }
  /* Print: hide every overlay chrome and let the report body fill the
     page. Uses :global so it punches through the Modal / app wrappers
     when window.print() fires. */
  @media print {
    :global(body) {
      background: white;
      color: black;
    }
    :global(.report-modal *) {
      visibility: hidden;
    }
    :global(.report-modal #report-print-root),
    :global(.report-modal #report-print-root *) {
      visibility: visible;
    }
    :global(.report-modal #report-print-root) {
      position: absolute;
      left: 0;
      top: 0;
      width: 100%;
      padding: 1rem 1.5rem;
      color: black;
    }
    .report-body table th,
    .report-body table td {
      border-color: #ccc;
    }
    .report-body h3 {
      border-color: #999;
    }
  }
</style>
