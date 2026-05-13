<script lang="ts">
  /// G-code text panel — the "inspect" half of the bidirectional link
  /// between gcode and the 3D toolpath:
  ///   * Clicking a line moves the playhead to the matching segment so
  ///     the tool jumps to that move in the 3D pane.
  ///   * As the playhead moves (scrubber, autoplay), the panel scrolls
  ///     the active line into view + highlights it.
  ///
  /// The panel is divided into per-op CHAPTERS detected via the
  /// `; OP <id>` markers the backend emits. The header offers
  /// prev/next-op jump buttons. When the user disables an op in the
  /// OperationsList (without re-Generating), that op's chapter renders
  /// commented-out — the actual gcode bytes don't change until the
  /// user clicks Generate again.
  ///
  /// Powered by project.generated.gcode_index (lines_to_segment +
  /// segments_to_line) emitted by wiac_core::gcode::preview.

  import { project, playheadToSegment } from '../state/project.svelte';
  import { _ } from 'svelte-i18n';

  type GcodeIndex = {
    lines_to_segment: number[];
    segments_to_line: number[];
  };

  // Split the gcode lazily — only when the project's generated output
  // changes — so scrolling a 5000-line program doesn't redo work.
  const lines = $derived(project.generated?.gcode.split('\n') ?? []);
  const idx = $derived(
    (project.generated as { gcode_index?: GcodeIndex } | null)?.gcode_index ?? null,
  );

  /// Chapter = one op's run of lines, demarcated by `; OP <id>`
  /// markers the backend emits between ops. The header line that
  /// declares the marker counts as the chapter start. The implicit
  /// "header" chapter at the program start (lines before the first
  /// marker, e.g. G21 / G90) gets opId=0 and a synthetic name.
  interface Chapter {
    opId: number;
    name: string;
    startLine: number; // 1-based
    endLine: number; // 1-based, inclusive
    disabled: boolean;
  }

  function parseOpMarker(raw: string): number | null {
    const s = raw.trim();
    const body = s.startsWith(';')
      ? s.slice(1).trim()
      : s.startsWith('(') && s.endsWith(')')
        ? s.slice(1, -1).trim()
        : null;
    if (body === null) return null;
    const rest = body.startsWith('OP ')
      ? body.slice(3).trim()
      : body.startsWith('op ')
        ? body.slice(3).trim()
        : null;
    if (rest === null) return null;
    const n = parseInt(rest, 10);
    return Number.isFinite(n) && n > 0 ? n : null;
  }

  const chapters = $derived.by<Chapter[]>(() => {
    const out: Chapter[] = [];
    if (lines.length === 0) return out;
    const opById = new Map(project.operations.map((o) => [o.id, o]));
    const nameFor = (id: number) => {
      if (id === 0) return 'Program header';
      const op = opById.get(id);
      return op ? `#${op.id} ${op.name}` : `Op #${id}`;
    };
    const disabledFor = (id: number) => {
      if (id === 0) return false;
      const op = opById.get(id);
      return op ? !op.enabled : false;
    };
    let curOp = 0;
    let curStart = 1;
    for (let i = 0; i < lines.length; i++) {
      const opId = parseOpMarker(lines[i]);
      if (opId != null) {
        // Close the previous chapter at the line before the marker.
        if (i > 0) {
          out.push({
            opId: curOp,
            name: nameFor(curOp),
            startLine: curStart,
            endLine: i, // 1-based, inclusive of the line just before this marker
            disabled: disabledFor(curOp),
          });
        }
        curOp = opId;
        curStart = i + 1; // marker line is the chapter start
      }
    }
    out.push({
      opId: curOp,
      name: nameFor(curOp),
      startLine: curStart,
      endLine: lines.length,
      disabled: disabledFor(curOp),
    });
    return out;
  });

  /// Lookup: line → chapter index. Fast for the prev/next nav + the
  /// per-line "is this line silenced" check inside the row render.
  const lineChapter = $derived.by<Int32Array>(() => {
    const arr = new Int32Array(lines.length);
    let chapterIdx = 0;
    for (let i = 0; i < lines.length; i++) {
      const ln = i + 1;
      while (chapterIdx + 1 < chapters.length && ln > chapters[chapterIdx].endLine) {
        chapterIdx++;
      }
      arr[i] = chapterIdx;
    }
    return arr;
  });

  // Active gcode line = the line of the segment the playhead currently
  // points at. 1-based per preview::interpret. The playhead → segment
  // mapping goes via arc length so dense connectors don't blow past
  // the gcode-panel highlight faster than long boundary edges.
  const activeLine = $derived.by<number | null>(() => {
    const gen = project.generated;
    if (!gen || gen.toolpath.length === 0 || !idx) return null;
    const total = gen.toolpath.length;
    const mapped = playheadToSegment(
      project.playhead,
      project.toolpathCumLen,
      project.toolpathTotalLen,
    );
    const headIdx =
      mapped.segIdx >= 0
        ? Math.max(0, Math.min(total - 1, mapped.segIdx))
        : Math.max(0, Math.min(total - 1, Math.round(project.playhead * total) - 1));
    const line = idx.segments_to_line[headIdx];
    return typeof line === 'number' && line > 0 ? line : null;
  });

  let host = $state<HTMLDivElement | undefined>();

  // Scroll the active row into view as the playhead moves. We use the
  // row's data-line attribute to find it; cheaper than keeping a Map of
  // refs for thousands of lines.
  $effect(() => {
    void activeLine;
    if (!host || activeLine == null) return;
    const row = host.querySelector(`[data-line="${activeLine}"]`);
    if (row) {
      (row as HTMLElement).scrollIntoView({ block: 'nearest', behavior: 'auto' });
    }
  });

  function jumpToLine(line: number) {
    const gen = project.generated;
    if (!gen || !idx) return;
    // 1-based → array index. Walk back to the nearest preceding line
    // that does have a segment (comment-only lines have NO_SEGMENT).
    const NO_SEGMENT = 4_294_967_295; // u32::MAX
    let probe = line - 1;
    while (probe >= 0 && idx.lines_to_segment[probe] === NO_SEGMENT) probe--;
    if (probe < 0) return;
    const segIdx = idx.lines_to_segment[probe];
    // Map segIdx → playhead via cumulative arc length so the
    // arc-length playback consumer (Scene3D, this panel) lands on the
    // same segment.
    const cum = project.toolpathCumLen;
    const total = project.toolpathTotalLen;
    const segs = gen.toolpath.length;
    if (cum && total > 0 && segIdx < cum.length) {
      project.playhead = Math.min(1, cum[segIdx] / total);
    } else if (segs > 0) {
      project.playhead = (segIdx + 1) / segs;
    }
  }

  function jumpToChapter(ci: number) {
    if (ci < 0 || ci >= chapters.length) return;
    const ch = chapters[ci];
    jumpToLine(ch.startLine);
    // jumpToLine walks backward to the nearest line that has a
    // segment; if the chapter starts with comment-only lines, the
    // playhead lands BEFORE this chapter. Force the panel to scroll
    // the chapter header into view regardless.
    queueMicrotask(() => {
      const el = host?.querySelector(`[data-chapter-idx="${ci}"]`);
      (el as HTMLElement | null)?.scrollIntoView({ block: 'start', behavior: 'smooth' });
    });
  }

  /// Index of the chapter currently under the playhead.
  const activeChapter = $derived.by<number | null>(() => {
    if (activeLine == null) return null;
    return lineChapter[activeLine - 1] ?? null;
  });

  function jumpPrevOp() {
    const cur = activeChapter ?? 0;
    // Find the nearest preceding op chapter (skip the program header
    // chapter (opId=0) if we're already at chapter 1+).
    for (let i = cur - 1; i >= 0; i--) {
      if (chapters[i].opId !== 0) {
        jumpToChapter(i);
        return;
      }
    }
    if (chapters.length > 0) jumpToChapter(0);
  }
  function jumpNextOp() {
    const cur = activeChapter ?? -1;
    for (let i = cur + 1; i < chapters.length; i++) {
      if (chapters[i].opId !== 0) {
        jumpToChapter(i);
        return;
      }
    }
  }

  let opChapterCount = $derived(chapters.filter((c) => c.opId !== 0).length);
</script>

{#if project.generated && project.generated.gcode}
  <div class="gcode" bind:this={host} role="region" aria-label={$_('gcode.title') ?? 'G-code'}>
    {#if opChapterCount > 0}
      <div class="chapter-nav">
        <button
          type="button"
          class="nav-btn"
          title="Jump to previous op chapter"
          aria-label="Previous op"
          onclick={jumpPrevOp}>⏮</button
        >
        <button
          type="button"
          class="nav-btn"
          title="Jump to next op chapter"
          aria-label="Next op"
          onclick={jumpNextOp}>⏭</button
        >
        <span class="nav-summary">
          {opChapterCount} op{opChapterCount === 1 ? '' : 's'}
          {#if activeChapter != null && chapters[activeChapter].opId !== 0}
            · at <strong>{chapters[activeChapter].name}</strong>
          {/if}
        </span>
      </div>
    {/if}
    <div class="gcode-inner">
      {#each lines as text, i (i)}
        {@const line = i + 1}
        {@const chIdx = lineChapter[i] ?? 0}
        {@const ch = chapters[chIdx]}
        {@const isChapterStart = ch != null && line === ch.startLine && ch.opId !== 0}
        {#if isChapterStart}
          <div class="chapter-head" data-chapter-idx={chIdx} class:disabled={ch.disabled}>
            <span class="chapter-caret">▾</span>
            <span class="chapter-name">{ch.name}</span>
            {#if ch.disabled}
              <span class="chapter-tag" title="This op is disabled — commented-out below and hidden in 3D. Toggle the checkbox in the operations list to re-enable. Click Generate to bake the change into the gcode.">silenced</span>
            {/if}
          </div>
        {/if}
        <button
          type="button"
          class="row"
          class:active={activeLine === line}
          class:silenced={ch?.disabled ?? false}
          data-line={line}
          onclick={() => jumpToLine(line)}
          tabindex="-1"
        >
          <span class="num">{line}</span>
          <span class="text">{ch?.disabled && text.length > 0 ? '; ' + text : text}</span>
        </button>
      {/each}
    </div>
  </div>
{/if}

<style>
  .gcode {
    width: 100%;
    height: 100%;
    overflow: auto;
    background: var(--bg-input);
    border-top: 1px solid var(--border);
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.72rem;
    line-height: 1.35;
    color: var(--text);
    contain: strict; /* keeps layout costs sane on big programs */
  }
  .chapter-nav {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.3rem 0.55rem;
    background: var(--bg-elevated);
    border-bottom: 1px solid var(--border);
    position: sticky;
    top: 0;
    z-index: 1;
  }
  .nav-btn {
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.1rem 0.45rem;
    font-size: 0.85rem;
    cursor: pointer;
    line-height: 1;
  }
  .nav-btn:hover {
    background: var(--bg-input);
    border-color: var(--accent);
  }
  .nav-summary {
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .nav-summary strong {
    color: var(--text-strong);
    font-weight: 600;
  }
  .gcode-inner {
    /* Fixed-row baseline so scrollIntoView lands cleanly. */
    padding: 0.25rem 0;
  }
  .chapter-head {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    padding: 0.25rem 0.55rem;
    background: color-mix(in srgb, var(--accent) 8%, var(--bg-elevated));
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    color: var(--text-strong);
    font-weight: 600;
    font-size: 0.74rem;
    user-select: none;
    margin-top: 0.2rem;
  }
  .chapter-head.disabled {
    background: color-mix(in srgb, var(--text-muted) 10%, var(--bg-elevated));
    color: var(--text-muted);
    text-decoration: line-through;
  }
  .chapter-caret {
    color: var(--text-muted);
    font-size: 0.8rem;
  }
  .chapter-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chapter-tag {
    font-size: 0.65rem;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--warn, #b86f00);
    background: color-mix(in srgb, var(--warn, #b86f00) 12%, transparent);
    border: 1px solid var(--warn, #b86f00);
    border-radius: 2px;
    padding: 0 0.3rem;
    text-decoration: none;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 3rem) minmax(0, 1fr);
    gap: 0.6rem;
    align-items: center;
    width: 100%;
    border: 0;
    background: transparent;
    color: inherit;
    text-align: left;
    font: inherit;
    padding: 0 0.5rem;
    cursor: pointer;
  }
  .row:hover {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }
  .row.active {
    background: color-mix(in srgb, var(--accent) 30%, transparent);
    color: var(--text-strong);
  }
  .row.silenced {
    color: var(--text-muted);
    opacity: 0.55;
    font-style: italic;
  }
  .num {
    color: var(--text-faint);
    text-align: right;
    user-select: none;
    font-variant-numeric: tabular-nums;
  }
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: pre;
  }
</style>
