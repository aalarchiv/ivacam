<script lang="ts">
  /// G-code "subtitles" — an EXPERIMENTAL caption strip (bd 7jug.17)
  /// pinned over the 3D simulation view, showing the gcode line(s) at
  /// the current playhead position like movie subtitles. Updates live as
  /// the toolpath plays or scrubs.
  ///
  /// Phone-first, pointer-events:none so it never eats canvas gestures.
  /// The integrator mounts this inside the 3D pane container; it renders
  /// nothing when disabled, when there's no generated program, or when
  /// the program has no toolpath.
  ///
  /// The playhead → active-gcode-line mapping is the SAME one GcodePanel
  /// uses (GcodePanel.svelte:103-118): map the playhead fraction to a
  /// toolpath segment via arc length (`playheadToSegment`), then look the
  /// 1-based source line up in `gcode_index.segments_to_line`. This keeps
  /// the caption in lock-step with the highlighted row in GcodePanel.

  import { project, playheadToSegment } from '../state/project.svelte';

  let {
    /// Gate from Settings / the integrator. Render nothing when false.
    enabled = true,
    /// How many lines of context to show above + below the active line
    /// (dimmed, like real subtitles). 0 = active line only.
    context = 1,
  }: { enabled?: boolean; context?: number } = $props();

  // Split the gcode lazily — only when the generated output changes — so
  // a scrub of a 5000-line program doesn't redo the split every frame.
  const lines = $derived(project.gen.generated?.gcode.split('\n') ?? []);
  const idx = $derived(project.gen.generated?.gcode_index ?? null);

  // Active gcode line = the 1-based source line of the segment the
  // playhead currently points at. Copied verbatim from GcodePanel's
  // `activeLine` derivation so the two stay in sync.
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

  /// The window of lines to caption: the active line plus `context` rows
  /// either side, each tagged with whether it's the active one. 1-based
  /// line numbers, clamped to the program bounds.
  const captionLines = $derived.by<{ line: number; text: string; active: boolean }[]>(() => {
    if (activeLine == null || lines.length === 0) return [];
    const ctx = Math.max(0, Math.floor(context));
    const first = Math.max(1, activeLine - ctx);
    const last = Math.min(lines.length, activeLine + ctx);
    const out: { line: number; text: string; active: boolean }[] = [];
    for (let ln = first; ln <= last; ln++) {
      const text = lines[ln - 1] ?? '';
      // Skip blank lines for context rows so the strip stays compact;
      // always keep the active row even if it's blank-ish.
      if (text.trim() === '' && ln !== activeLine) continue;
      out.push({ line: ln, text, active: ln === activeLine });
    }
    return out;
  });

  const show = $derived(enabled && captionLines.length > 0);
</script>

{#if show}
  <div class="subtitles" aria-hidden="true">
    {#each captionLines as row (row.line)}
      <div class="cap" class:active={row.active}>
        <span class="ln">{row.line}</span>
        <span class="tx">{row.text}</span>
      </div>
    {/each}
  </div>
{/if}

<style>
  .subtitles {
    /* Pinned bottom-centre of the host (the 3D pane container, which
       must be position:relative). Never interactive — gestures pass
       straight through to the canvas below. */
    position: absolute;
    left: 50%;
    bottom: 0.9rem;
    transform: translateX(-50%);
    z-index: var(--z-anchor, 5);
    pointer-events: none;
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 1px;
    max-width: min(92%, 36rem);
    padding: 0.3rem 0.5rem;
    border-radius: 6px;
    background: color-mix(in srgb, var(--bg-elevated, #111) 78%, transparent);
    backdrop-filter: blur(2px);
    box-shadow: 0 1px 6px rgba(0, 0, 0, 0.35);
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.74rem;
    line-height: 1.35;
    font-variant-numeric: tabular-nums;
  }
  .cap {
    display: grid;
    grid-template-columns: minmax(0, 2.6rem) minmax(0, 1fr);
    gap: 0.55rem;
    align-items: baseline;
    /* Context rows are dimmed; the active row pops — subtitle feel. */
    color: var(--text-muted, #999);
    opacity: 0.7;
  }
  .cap.active {
    color: var(--text-strong, #fff);
    opacity: 1;
    font-weight: 600;
  }
  .ln {
    text-align: right;
    color: var(--text-faint, #666);
    user-select: none;
  }
  .tx {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: pre;
  }
</style>
