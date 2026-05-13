<script lang="ts">
  /// G-code text panel — the "inspect" half of the bidirectional link
  /// between gcode and the 3D toolpath:
  ///   * Clicking a line moves the playhead to the matching segment so
  ///     the tool jumps to that move in the 3D pane.
  ///   * As the playhead moves (scrubber, autoplay), the panel scrolls
  ///     the active line into view + highlights it.
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
</script>

{#if project.generated && project.generated.gcode}
  <div class="gcode" bind:this={host} role="region" aria-label={$_('gcode.title') ?? 'G-code'}>
    <div class="gcode-inner">
      {#each lines as text, i (i)}
        {@const line = i + 1}
        <button
          type="button"
          class="row"
          class:active={activeLine === line}
          data-line={line}
          onclick={() => jumpToLine(line)}
          tabindex="-1"
        >
          <span class="num">{line}</span>
          <span class="text">{text}</span>
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
  .gcode-inner {
    /* Fixed-row baseline so scrollIntoView lands cleanly. */
    padding: 0.25rem 0;
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
