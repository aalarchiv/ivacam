<script lang="ts">
  // 3D toolpath scrubber. Drives `project.playhead` (a fraction in
  // [0,1] of total ARC LENGTH — not segment count) which Scene3D and
  // GcodePanel read via `playheadToSegment` to position the tool tip
  // and active line. Arc-length interpretation keeps cutter speed
  // visually consistent across short connectors and long edges.
  //
  // Renders sim-warning markers along the timeline at the segment
  // positions where they fired. Critical = red, warning = yellow.
  // Click a marker to scrub the playhead onto it.

  import {
    project,
    playheadToSegment,
    simWarningSeverity,
    simWarningSegmentIdx,
    simWarningSummary,
  } from '../state/project.svelte';
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
</script>

{#if project.generated && project.generated.toolpath.length > 0}
  <div class="bar">
    <button
      onclick={togglePlay}
      disabled={!project.generated}
      aria-label={playing ? 'Pause' : 'Play'}
      title={playing ? 'Pause' : 'Play'}
    >
      {playing ? '❚❚' : '▶'}
    </button>
    <div class="track">
      <input
        type="range"
        min="0"
        max="1"
        step="0.001"
        value={project.playhead}
        oninput={onScrub}
      />
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
        const shown = mapped.segIdx >= 0
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
  button {
    background: var(--accent);
    color: white;
    border: 0;
    border-radius: 3px;
    padding: 0.15rem 0.55rem;
    font-size: 0.85rem;
    cursor: pointer;
    min-width: 2.2rem;
  }
  .track {
    position: relative;
    flex: 1;
    display: flex;
    align-items: center;
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
