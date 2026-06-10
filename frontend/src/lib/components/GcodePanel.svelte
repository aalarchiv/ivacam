<script lang="ts">
  /// G-code text panel — the "inspect" half of the bidirectional link
  /// between gcode and the 3D toolpath:
  ///   * Clicking a line moves the playhead to the matching segment so
  ///     the tool jumps to that move in the 3D pane.
  ///   * As the playhead moves (scrubber, autoplay), the panel scrolls
  ///     the active line into view + highlights it.
  ///
  /// The panel is divided into per-op CHAPTERS detected via the
  /// `; OP <id>` markers the backend emits — each chapter renders a
  /// header row labeled with the op name. Prev/next-op jump buttons
  /// live in the PlaybackBar so they stay reachable when this panel is
  /// folded. When the user disables an op in the OperationsList
  /// (without re-Generating), that op's chapter renders commented-out —
  /// the actual gcode bytes don't change until the user clicks Generate
  /// again.
  ///
  /// Powered by project.gen.generated.gcode_index (lines_to_segment +
  /// segments_to_line) emitted by ivac_core::gcode::preview.

  import { project, playheadToSegment } from '../state/project.svelte';
  import { parseGcodeChapters, NO_SEGMENT } from '../state/gcode_chapters';

  // Split the gcode lazily — only when the project's generated output
  // changes — so scrolling a 5000-line program doesn't redo work.
  const lines = $derived(project.gen.generated?.gcode.split('\n') ?? []);
  const idx = $derived(project.gen.generated?.gcode_index ?? null);

  const chapters = $derived(parseGcodeChapters(lines, project.data.operations));

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
    const gen = project.gen.generated;
    if (!gen || gen.toolpath.length === 0 || !idx) return null;
    const total = gen.toolpath.length;
    const mapped = playheadToSegment(
      project.playhead,
      project.gen.toolpathCumLen,
      project.gen.toolpathTotalLen,
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
    const gen = project.gen.generated;
    if (!gen || !idx) return;
    // 1-based → array index. Walk back to the nearest preceding line
    // that does have a segment (comment-only lines have NO_SEGMENT).
    let probe = line - 1;
    while (probe >= 0 && idx.lines_to_segment[probe] === NO_SEGMENT) probe--;
    if (probe < 0) return;
    const segIdx = idx.lines_to_segment[probe];
    // Map segIdx → playhead via cumulative arc length so the
    // arc-length playback consumer (Scene3D, this panel) lands on the
    // same segment.
    const cum = project.gen.toolpathCumLen;
    const total = project.gen.toolpathTotalLen;
    const segs = gen.toolpath.length;
    if (cum && total > 0 && segIdx < cum.length) {
      project.playhead = Math.min(1, cum[segIdx] / total);
    } else if (segs > 0) {
      project.playhead = (segIdx + 1) / segs;
    }
  }

  /// Keyboard-focused line for the container-level listbox roving
  /// tabindex pattern. Updated by ArrowUp/Down/Home/End; Enter calls
  /// `jumpToLine`. Default tracks `activeLine` so Tab into a paused
  /// playback lands on the highlighted row.
  let focusedLine = $state<number | null>(null);
  function onPanelKey(e: KeyboardEvent) {
    if (lines.length === 0) return;
    const cur = focusedLine ?? activeLine ?? 1;
    let next: number;
    if (e.key === 'ArrowDown') next = Math.min(lines.length, cur + 1);
    else if (e.key === 'ArrowUp') next = Math.max(1, cur - 1);
    else if (e.key === 'PageDown') next = Math.min(lines.length, cur + 25);
    else if (e.key === 'PageUp') next = Math.max(1, cur - 25);
    else if (e.key === 'Home') next = 1;
    else if (e.key === 'End') next = lines.length;
    else if (e.key === 'Enter' || e.key === ' ') {
      jumpToLine(cur);
      e.preventDefault();
      return;
    } else return;
    e.preventDefault();
    focusedLine = next;
    const row = host?.querySelector(`[data-line="${next}"]`) as HTMLElement | null;
    row?.scrollIntoView({ block: 'nearest', behavior: 'auto' });
  }

  /// When the playhead moves onto a new chapter (driven by PlaybackBar's
  /// prev/next-op buttons), scroll the chapter header into view. Without
  /// this nudge the row-level $effect lands on `block: 'nearest'` of the
  /// first segment line, which can leave the chapter header off-screen
  /// when the chapter starts with comment-only setup lines.
  let prevChapterIdx = $state<number | null>(null);
  $effect(() => {
    const ci = activeLine == null ? null : (lineChapter[activeLine - 1] ?? null);
    if (ci != null && ci !== prevChapterIdx && host) {
      prevChapterIdx = ci;
      queueMicrotask(() => {
        const el = host?.querySelector(`[data-chapter-idx="${ci}"]`);
        (el as HTMLElement | null)?.scrollIntoView({ block: 'start', behavior: 'smooth' });
      });
    }
  });
</script>

{#if project.gen.generated && project.gen.generated.gcode}
  <div
    class="gcode"
    class:stale={project.data.dirty}
    bind:this={host}
    role="listbox"
    aria-label="G-code"
    tabindex="0"
    onkeydown={onPanelKey}
  >
    {#if project.data.dirty}
      <div
        class="stale-badge"
        title="The project has been edited since this G-code was generated. Click Generate G-code to refresh."
      >
        ⚠ Stale — re-Generate to refresh
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
              <span
                class="chapter-tag"
                title="This op is disabled — commented-out below and hidden in 3D. Toggle the checkbox in the operations list to re-enable. Click Generate to bake the change into the G-code."
                >silenced</span
              >
            {/if}
          </div>
        {/if}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- Keyboard support lives at the container (.gcode listbox);
             each option is a -1 tabindex to keep the roving pattern. -->
        <div
          role="option"
          tabindex="-1"
          aria-selected={activeLine === line}
          class="row"
          class:active={activeLine === line}
          class:focused={focusedLine === line}
          class:silenced={ch?.disabled ?? false}
          data-line={line}
          onclick={() => jumpToLine(line)}
        >
          <span class="num">{line}</span>
          <span class="text">{ch?.disabled && text.length > 0 ? '; ' + text : text}</span>
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .gcode {
    position: relative; /* anchor stale-badge */
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
  /* Dim panel content when stale so the badge can do the talking. */
  .gcode.stale .gcode-inner {
    opacity: 0.55;
  }
  /* Stale-state callout — same affordance as the Sim chip in
     GenerateBar, applied to the actual code panel. Sticky so it stays
     visible even when the user scrolls deep into a long program. */
  .stale-badge {
    position: sticky;
    top: 0;
    z-index: var(--z-anchor);
    background: color-mix(in srgb, var(--warn) 22%, var(--bg-panel));
    color: var(--text-strong);
    border-bottom: 1px solid color-mix(in srgb, var(--warn) 50%, var(--border));
    padding: 0.3rem 0.7rem;
    font-family: system-ui, sans-serif;
    font-size: 0.74rem;
    text-align: center;
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
  /* Roving-tabindex listbox focused row (keyboard ArrowUp/Down moves
     `focusedLine`). Subtle outline so it's distinct from `.active`,
     which marks the playhead line. */
  .row.focused {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
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
    /* Allow drag-select-to-copy of the gcode text. The container is now
       a div with onclick (not a `<button>`), so dragging starts a real
       text selection instead of getting eaten by the button's click
       gesture. The `.num` column keeps user-select:none so line numbers
       aren't pulled into the copy. */
    user-select: text;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: pre;
  }
</style>
