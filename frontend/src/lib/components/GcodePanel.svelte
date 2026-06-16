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
  ///
  /// Rendering is windowed: only the rows in the scroll viewport (plus a
  /// small overscan) become DOM nodes, with spacer divs above/below
  /// preserving the scrollbar extent. A 100k-line program therefore
  /// mounts ~60 nodes instead of 100k+ — see `state/gcode_window.ts`.

  import { project, playheadToSegment } from '../state/project.svelte';
  import { parseGcodeChapters, NO_SEGMENT } from '../state/gcode_chapters';
  import { buildRowOffsets, computeWindow } from '../state/gcode_window';

  /// Extra rows rendered above/below the viewport so a fast scroll or
  /// flung wheel doesn't reveal blank gaps before the next frame.
  const OVERSCAN = 12;
  /// Fallback row / header heights (px) used until the live probe below
  /// measures the real values. Sized for the panel's default font so the
  /// very first frame is close even before measurement lands.
  const DEFAULT_ROW_H = 16;
  const DEFAULT_CHAPTER_H = 30;

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

  /// Per-line flag: does this line begin a (real, non-program) chapter,
  /// i.e. should a header block render stacked above the row? Drives the
  /// virtualization row heights so the scrollbar accounts for headers.
  const chapterStart = $derived.by<Uint8Array>(() => {
    const arr = new Uint8Array(lines.length);
    for (let i = 0; i < lines.length; i++) {
      const ch = chapters[lineChapter[i]];
      if (ch != null && i + 1 === ch.startLine && ch.opId !== 0) arr[i] = 1;
    }
    return arr;
  });

  // Measured row / header heights. A hidden probe (rendered below)
  // reports the real pixel heights so the windowing math survives font /
  // zoom / theme changes instead of hard-coding a row height.
  let rowH = $state(DEFAULT_ROW_H);
  let chapterH = $state(DEFAULT_CHAPTER_H);

  /// Cumulative pixel offset of every line (length lines.length + 1).
  /// Rebuilt only when the line set, chapter starts, or measured heights
  /// change — never per scroll frame.
  const offsets = $derived(buildRowOffsets(chapterStart, rowH, chapterH));

  // Live scroll state. `scrollTop` / `viewportH` drive the window; both
  // come from the host element (onscroll + a ResizeObserver).
  let scrollTop = $state(0);
  let viewportH = $state(0);

  const win = $derived(computeWindow(offsets, lines.length, scrollTop, viewportH, OVERSCAN));

  /// 1-based line numbers currently in the render window.
  const visibleLines = $derived.by<number[]>(() => {
    if (win.last < win.first) return [];
    const out: number[] = [];
    for (let i = win.first; i <= win.last; i++) out.push(i + 1);
    return out;
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
  let probeEl = $state<HTMLDivElement | undefined>();

  function onScroll() {
    if (host) scrollTop = host.scrollTop;
  }

  /// Scroll line `line` (1-based) just into view — equivalent to the old
  /// `scrollIntoView({ block: 'nearest' })`, but computed from `offsets`
  /// so it works even when the target row is virtualized out of the DOM.
  /// `align: 'start'` pins the line's top to the viewport top (used for
  /// chapter jumps so the header isn't left off-screen).
  function ensureVisible(line: number, align: 'nearest' | 'start' = 'nearest') {
    if (!host || line < 1 || line > offsets.length - 1) return;
    const top = offsets[line - 1];
    const bottom = offsets[line];
    const viewTop = host.scrollTop;
    const viewBottom = viewTop + host.clientHeight;
    if (align === 'start') {
      host.scrollTop = top;
    } else if (top < viewTop) {
      host.scrollTop = top;
    } else if (bottom > viewBottom) {
      host.scrollTop = bottom - host.clientHeight;
    }
  }

  // Scroll the active row into view as the playhead moves.
  $effect(() => {
    void activeLine;
    void offsets;
    if (activeLine == null) return;
    ensureVisible(activeLine);
  });

  // Measure the real row / header heights from a hidden probe so the
  // windowing offsets match the rendered layout. A ResizeObserver keeps
  // them current across font-load / zoom / theme changes.
  $effect(() => {
    if (!probeEl) return;
    const measure = () => {
      const r = probeEl?.querySelector('.row')?.getBoundingClientRect().height;
      const c = probeEl?.querySelector('.chapter-head')?.getBoundingClientRect().height;
      if (r && r > 0) rowH = r;
      if (c && c > 0) chapterH = c;
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(probeEl);
    return () => ro.disconnect();
  });

  // Track the viewport height (and seed the initial value) so the window
  // covers the whole visible area, not just one row.
  $effect(() => {
    if (!host) return;
    viewportH = host.clientHeight;
    scrollTop = host.scrollTop;
    const ro = new ResizeObserver(() => {
      if (host) viewportH = host.clientHeight;
    });
    ro.observe(host);
    return () => ro.disconnect();
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
    ensureVisible(next);
  }

  /// When the playhead moves onto a new chapter (driven by PlaybackBar's
  /// prev/next-op buttons), scroll the chapter header into view. Without
  /// this nudge the row-level scroll lands on `block: 'nearest'` of the
  /// first segment line, which can leave the chapter header off-screen
  /// when the chapter starts with comment-only setup lines.
  let prevChapterIdx = $state<number | null>(null);
  $effect(() => {
    const ci = activeLine == null ? null : (lineChapter[activeLine - 1] ?? null);
    if (ci != null && ci !== prevChapterIdx && host) {
      prevChapterIdx = ci;
      const start = chapters[ci]?.startLine;
      if (start != null) {
        queueMicrotask(() => ensureVisible(start, 'start'));
      }
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
    onscroll={onScroll}
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
      <!-- Top spacer stands in for the rows scrolled above the window. -->
      <div class="spacer" style:height="{win.padTop}px"></div>
      {#each visibleLines as line (line)}
        {@const i = line - 1}
        {@const chIdx = lineChapter[i] ?? 0}
        {@const ch = chapters[chIdx]}
        {@const text = lines[i] ?? ''}
        {#if chapterStart[i]}
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
      <!-- Bottom spacer stands in for the rows below the window. -->
      <div class="spacer" style:height="{win.padBottom}px"></div>
    </div>
    <!-- Off-screen probe: one header + one row whose measured heights
         feed the windowing offsets. Never interactive, never read. -->
    <div class="measure" aria-hidden="true" bind:this={probeEl}>
      <div class="chapter-head">
        <span class="chapter-caret">▾</span>
        <span class="chapter-name">probe</span>
      </div>
      <div class="row"><span class="num">0</span><span class="text">probe</span></div>
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
  .spacer {
    /* Pure scroll-extent filler for the virtualized rows outside the
       window — no content, no interaction. */
    width: 100%;
  }
  /* Hidden measurement probe — laid out (so it has real heights) but
     visually gone and out of the a11y / hit-test tree. */
  .measure {
    position: absolute;
    visibility: hidden;
    pointer-events: none;
    left: -9999px;
    top: 0;
    width: 20rem;
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
