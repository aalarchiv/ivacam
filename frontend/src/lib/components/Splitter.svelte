<script lang="ts">
  /// Drag handle for resizing two adjacent panes.
  ///
  /// Sits *between* the panes in a CSS grid / flex column. The handle is
  /// a 4 px-wide hover-targeted gutter; clicking and dragging fires
  /// `onResize(delta)` with the cursor delta in the active axis. The
  /// parent owns the pane size (typically a `--var-px` CSS custom prop)
  /// and is responsible for clamping. We only report cursor motion in
  /// the same client-space the size lives in, so the parent's math is
  /// trivial: `size += delta` (or `size -= delta` when the splitter sits
  /// to the LEFT/ABOVE of the resizable pane).
  ///
  /// Pointer capture is taken on pointerdown so the drag continues past
  /// the gutter (cursor can leave by 50+ px on fast moves without losing
  /// the grab). Double-click invokes `onReset` so the user can recover
  /// the default layout without hunting through settings.

  interface Props {
    direction: 'horizontal' | 'vertical';
    onResize: (delta: number) => void;
    onReset?: () => void;
    title?: string;
  }
  let { direction, onResize, onReset, title }: Props = $props();

  let dragging = $state(false);
  let last = 0;

  function onPointerDown(e: PointerEvent) {
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    last = direction === 'horizontal' ? e.clientX : e.clientY;
    dragging = true;
    e.preventDefault();
  }
  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    const cur = direction === 'horizontal' ? e.clientX : e.clientY;
    const delta = cur - last;
    if (delta !== 0) {
      last = cur;
      onResize(delta);
    }
  }
  function onPointerUp(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {}
  }
  function onDblClick() {
    onReset?.();
  }
</script>

<div
  class="splitter"
  class:horizontal={direction === 'horizontal'}
  class:vertical={direction === 'vertical'}
  class:dragging
  role="separator"
  aria-orientation={direction === 'horizontal' ? 'vertical' : 'horizontal'}
  title={title ?? (onReset ? 'Drag to resize · double-click to reset' : 'Drag to resize')}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerUp}
  ondblclick={onDblClick}
></div>

<style>
  /* Visible 2-px seam between two panes with a 10-px transparent overlay
     widening the hit-target to ~24 px effective grab area. Brightens to
     accent on hover/drag so the affordance is obvious. The old 4-px slab
     was a hostile target on trackpads and touch (WCAG recommends ≥24×24). */
  .splitter {
    background: var(--border);
    transition: background 80ms;
    flex-shrink: 0;
    user-select: none;
    touch-action: none;
    position: relative;
  }
  .splitter::after {
    /* Invisible overlay extending the pointer-hit area without bloating
       the visual seam. inset chosen so the hit zone is ~10 px wide
       (5 px on each side of the 2-px band), within the column-resize
       cursor's tolerance. */
    content: '';
    position: absolute;
    inset: 0;
  }
  .splitter.horizontal {
    width: 2px;
    cursor: col-resize;
  }
  .splitter.horizontal::after {
    left: -5px;
    right: -5px;
  }
  .splitter.vertical {
    height: 2px;
    cursor: row-resize;
  }
  .splitter.vertical::after {
    top: -5px;
    bottom: -5px;
  }
  .splitter:hover,
  .splitter.dragging {
    background: var(--accent);
  }
</style>
