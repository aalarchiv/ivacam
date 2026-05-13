<script lang="ts">
  // 3D toolpath scrubber. Drives `project.playhead` (a fraction in
  // [0,1] of total ARC LENGTH — not segment count) which Scene3D and
  // GcodePanel read via `playheadToSegment` to position the tool tip
  // and active line. Arc-length interpretation keeps cutter speed
  // visually consistent across short connectors and long edges.
  //
  // Renders two overlays on the timeline:
  //   * Op-chapter TICKS — vertical bars at the first segment of each
  //     `; OP N` chapter. Clicking a tick (or the ⏮/⏭ buttons flanking
  //     play) jumps the playhead onto that op so the gcode + scene
  //     follow. Ticks live here (not just in GcodePanel) so they remain
  //     usable when the gcode panel is collapsed.
  //   * Sim-warning markers at the segment positions where they fired.
  //     Critical = red, warning = yellow. Click to scrub onto it.

  import {
    project,
    playheadToSegment,
    simWarningSeverity,
    simWarningSegmentIdx,
    simWarningSummary,
  } from '../state/project.svelte';
  import {
    parseGcodeChapters,
    firstSegmentInRange,
    type GcodeChapter,
  } from '../state/gcode_chapters';
  import type { SimWarning } from '../api/types';

  let speed = $state(1.0);
  let playing = $state(false);
  let raf = 0;
  let lastTs = 0;

  $effect(() => {
    if (playing && project.generated) {
      lastTs = performance.now();
      raf = requestAnimationFrame(tick);
    }
    return () => cancelAnimationFrame(raf);
  });

  function tick(now: number) {
    if (!playing) return;
    const dt = (now - lastTs) / 1000;
    lastTs = now;
    let next = project.playhead + dt * 0.1 * speed;
    if (next >= 1) {
      next = 1;
      playing = false;
    }
    project.playhead = next;
    if (playing) raf = requestAnimationFrame(tick);
  }

  function togglePlay() {
    if (!project.generated) return;
    if (project.playhead >= 0.999) project.playhead = 0;
    playing = !playing;
  }

  function onScrub(e: Event) {
    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
    project.playhead = isNaN(v) ? 0 : v;
    playing = false;
  }

  /// Map a segment index to its [0..1] arc-length position so warning
  /// markers line up with the slider. Returns null when there's no
  /// length table yet.
  function segIdxToFraction(segIdx: number): number | null {
    const cum = project.toolpathCumLen;
    const total = project.toolpathTotalLen;
    if (!cum || cum.length === 0 || total <= 0) return null;
    const i = Math.max(0, Math.min(cum.length - 1, segIdx));
    return Math.max(0, Math.min(1, cum[i] / total));
  }

  function onMarkerClick(w: SimWarning) {
    const segIdx = simWarningSegmentIdx(w);
    const f = segIdxToFraction(segIdx);
    if (f != null) {
      project.playhead = f;
      playing = false;
    }
  }

  let warnings = $derived(project.simDiagnostics?.warnings ?? []);

  // Op chapters — driven by the same `; OP N` markers GcodePanel reads.
  // Each chapter that produces at least one motion segment becomes a
  // tick on the timeline and a stop on the prev/next nav.
  interface ChapterTick {
    chapter: GcodeChapter;
    segIdx: number;
    fraction: number;
  }
  const gcodeLines = $derived(project.generated?.gcode.split('\n') ?? []);
  const gcodeIdx = $derived(project.generated?.gcode_index ?? null);
  const chapters = $derived(parseGcodeChapters(gcodeLines, project.operations));

  const chapterTicks = $derived.by<ChapterTick[]>(() => {
    if (!gcodeIdx) return [];
    const out: ChapterTick[] = [];
    for (const ch of chapters) {
      if (ch.opId === 0) continue; // skip program header
      const segIdx = firstSegmentInRange(gcodeIdx.lines_to_segment, ch.startLine, ch.endLine);
      if (segIdx == null) continue;
      const f = segIdxToFraction(segIdx);
      if (f == null) continue;
      out.push({ chapter: ch, segIdx, fraction: f });
    }
    return out;
  });

  /// Index (into chapterTicks) of the chapter the playhead currently sits in.
  /// Returns -1 when the playhead is BEFORE the first op chapter.
  const activeTickIdx = $derived.by<number>(() => {
    if (chapterTicks.length === 0) return -1;
    const h = project.playhead;
    let result = -1;
    for (let i = 0; i < chapterTicks.length; i++) {
      if (chapterTicks[i].fraction <= h + 1e-6) result = i;
      else break;
    }
    return result;
  });

  function jumpToTick(t: ChapterTick) {
    project.playhead = t.fraction;
    playing = false;
  }

  function jumpPrevOp() {
    if (chapterTicks.length === 0) return;
    // If we're past the first tick by more than a hair, snap to the
    // start of the CURRENT chapter (common DAW prev-track behavior).
    const cur = activeTickIdx;
    if (cur >= 0) {
      const here = chapterTicks[cur].fraction;
      if (project.playhead - here > 0.005) {
        jumpToTick(chapterTicks[cur]);
        return;
      }
      if (cur > 0) {
        jumpToTick(chapterTicks[cur - 1]);
        return;
      }
    }
    jumpToTick(chapterTicks[0]);
  }
  function jumpNextOp() {
    if (chapterTicks.length === 0) return;
    const cur = activeTickIdx;
    const next = cur + 1;
    if (next < chapterTicks.length) jumpToTick(chapterTicks[next]);
  }

  const hasChapters = $derived(chapterTicks.length > 0);
  const prevOpLabel = $derived.by<string>(() => {
    if (!hasChapters) return 'Previous op';
    const cur = activeTickIdx;
    const target =
      cur > 0 && project.playhead - chapterTicks[cur].fraction <= 0.005
        ? chapterTicks[cur - 1]
        : (chapterTicks[Math.max(0, cur)] ?? chapterTicks[0]);
    return `Previous op (${target.chapter.name})`;
  });
  const nextOpLabel = $derived.by<string>(() => {
    if (!hasChapters) return 'Next op';
    const cur = activeTickIdx;
    const target = chapterTicks[cur + 1];
    return target ? `Next op (${target.chapter.name})` : 'Next op';
  });
</script>

{#if project.generated && project.generated.toolpath.length > 0}
  <div class="bar">
    <button
      type="button"
      class="op-nav"
      onclick={jumpPrevOp}
      disabled={!hasChapters}
      aria-label={prevOpLabel}
      title={prevOpLabel}
    >
      ⏮
    </button>
    <button
      class="play"
      onclick={togglePlay}
      disabled={!project.generated}
      aria-label={playing ? 'Pause' : 'Play'}
      title={playing ? 'Pause' : 'Play'}
    >
      {playing ? '❚❚' : '▶'}
    </button>
    <button
      type="button"
      class="op-nav"
      onclick={jumpNextOp}
      disabled={!hasChapters || activeTickIdx + 1 >= chapterTicks.length}
      aria-label={nextOpLabel}
      title={nextOpLabel}
    >
      ⏭
    </button>
    <div class="track">
      <input type="range" min="0" max="1" step="0.001" value={project.playhead} oninput={onScrub} />
      {#if chapterTicks.length > 0}
        <div class="chapter-ticks" aria-hidden="true">
          {#each chapterTicks as t, i (i)}
            <button
              type="button"
              class="chapter-tick"
              class:disabled={t.chapter.disabled}
              class:active={activeTickIdx === i}
              style:left={`${(t.fraction * 100).toFixed(2)}%`}
              title={`${t.chapter.name}${t.chapter.disabled ? ' (silenced)' : ''}`}
              aria-label={`Jump to ${t.chapter.name}`}
              onclick={() => jumpToTick(t)}
            ></button>
          {/each}
        </div>
      {/if}
      {#if warnings.length > 0}
        <div class="markers" aria-hidden="true">
          {#each warnings as w (w)}
            {@const f = segIdxToFraction(simWarningSegmentIdx(w))}
            {#if f != null}
              <button
                class="marker {simWarningSeverity(w)}"
                style:left={`${(f * 100).toFixed(2)}%`}
                title={simWarningSummary(w)}
                onclick={() => onMarkerClick(w)}
                type="button"
                aria-label={simWarningSummary(w)}
              ></button>
            {/if}
          {/each}
        </div>
      {/if}
    </div>
    <label
      >×<input
        type="number"
        bind:value={speed}
        step="0.5"
        min="0.1"
        max="10"
        title="Playback speed"
        aria-label="Playback speed multiplier"
      /></label
    >
    <span class="counter">
      {(() => {
        const total = project.generated.toolpath.length;
        const mapped = playheadToSegment(
          project.playhead,
          project.toolpathCumLen,
          project.toolpathTotalLen,
        );
        const shown =
          mapped.segIdx >= 0
            ? Math.min(total, mapped.segIdx + 1)
            : Math.round(project.playhead * total);
        return `${shown}/${total}`;
      })()}
    </span>
  </div>
{/if}

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.3rem 0.6rem;
    background: var(--bg-panel);
    border-top: 1px solid var(--border);
    color: var(--text-muted);
    font-size: 0.74rem;
  }
  .play {
    background: var(--accent);
    color: white;
    border: 0;
    border-radius: 3px;
    padding: 0.15rem 0.55rem;
    font-size: 0.85rem;
    cursor: pointer;
    min-width: 2.2rem;
  }
  .op-nav {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.1rem 0.45rem;
    font-size: 0.85rem;
    cursor: pointer;
    line-height: 1;
    min-width: 2rem;
  }
  .op-nav:hover:not(:disabled) {
    background: var(--bg-input);
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .op-nav:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .track {
    position: relative;
    flex: 1;
    display: flex;
    align-items: center;
  }
  .chapter-ticks {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .chapter-tick {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    margin: 0;
    padding: 0;
    border: 0;
    background: color-mix(in srgb, var(--accent) 70%, transparent);
    cursor: pointer;
    pointer-events: auto;
    transform: translateX(-50%);
    z-index: 2;
    min-width: 0;
  }
  .chapter-tick:hover {
    width: 3px;
    background: var(--accent);
  }
  .chapter-tick.active {
    background: var(--accent-strong);
    box-shadow: 0 0 4px color-mix(in srgb, var(--accent) 60%, transparent);
  }
  .chapter-tick.disabled {
    background: color-mix(in srgb, var(--text-muted) 60%, transparent);
  }
  input[type='range'] {
    flex: 1;
    accent-color: var(--accent);
    position: relative;
    z-index: 1;
  }
  .markers {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .marker {
    position: absolute;
    top: 50%;
    width: 0.55rem;
    height: 0.9rem;
    transform: translate(-50%, -50%);
    padding: 0;
    border: 1px solid color-mix(in srgb, var(--bg-app) 60%, transparent);
    border-radius: 1px;
    cursor: pointer;
    pointer-events: auto;
    min-width: 0;
    z-index: 2;
  }
  .marker.critical {
    background: var(--marker-critical);
  }
  .marker.warning {
    background: var(--marker-warn);
  }
  .marker.info {
    background: var(--marker-info);
  }
  .marker::before {
    content: '';
    position: absolute;
    inset: 0;
    pointer-events: none;
    background-repeat: no-repeat;
    background-position: center;
    background-size: contain;
  }
  .marker.critical::before {
    content: '✕';
    color: #fff;
    font-size: 0.55rem;
    line-height: 0.9rem;
    text-align: center;
    font-weight: 700;
  }
  .marker.warning::before {
    content: '!';
    color: #000;
    font-size: 0.6rem;
    line-height: 0.9rem;
    text-align: center;
    font-weight: 700;
  }
  input[type='number'] {
    width: 4rem;
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.1rem 0.3rem;
    font-size: 0.74rem;
  }
  label {
    display: inline-flex;
    align-items: center;
    gap: 0.15rem;
  }
  .counter {
    font-variant-numeric: tabular-nums;
  }
</style>
