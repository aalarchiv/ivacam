<script lang="ts">
  // Edge-swipe activity navigation for narrow layouts. Two thin zones
  // pinned to the left/right screen edges capture a horizontal swipe that
  // STARTS at the edge — so the canvas interior keeps its own pan/pinch.
  // Left-edge swipe-right → onPrev; right-edge swipe-left → onNext.
  //
  // The zones start below the top app bar (`topOffset`) so they don't sit
  // over — and swallow taps on — the app bar's ◂ ▸ chevrons.

  interface Props {
    onPrev: () => void;
    onNext: () => void;
    /// CSS length the catch-zones start at, clearing the top app bar.
    topOffset?: string;
  }
  let { onPrev, onNext, topOffset = '3rem' }: Props = $props();

  /// Width of each edge catch-zone.
  const ZONE_PX = 22;
  /// Horizontal travel (px) required to count as a swipe.
  const TRIGGER_PX = 56;

  let startX = 0;
  let startY = 0;
  let pid: number | null = null;
  let side: 'left' | 'right' = 'left';

  function down(e: PointerEvent, which: 'left' | 'right') {
    side = which;
    pid = e.pointerId;
    startX = e.clientX;
    startY = e.clientY;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function up(e: PointerEvent) {
    if (e.pointerId !== pid) return;
    pid = null;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;
    // Need a clear, mostly-horizontal travel; a tap or vertical scroll
    // at the edge does nothing.
    if (Math.abs(dx) < TRIGGER_PX || Math.abs(dx) <= Math.abs(dy)) return;
    if (side === 'left' && dx > 0) onPrev();
    else if (side === 'right' && dx < 0) onNext();
  }
</script>

<div
  class="edge left"
  style:width="{ZONE_PX}px"
  style:top={topOffset}
  onpointerdown={(e) => down(e, 'left')}
  onpointerup={up}
  onpointercancel={up}
  aria-hidden="true"
></div>
<div
  class="edge right"
  style:width="{ZONE_PX}px"
  style:top={topOffset}
  onpointerdown={(e) => down(e, 'right')}
  onpointerup={up}
  onpointercancel={up}
  aria-hidden="true"
></div>

<style>
  .edge {
    position: fixed;
    bottom: 0;
    /* Above the canvas so the swipe is caught, but below the sidebar
       overlay / modals (the parent stops rendering this while the
       overlay is open anyway). */
    z-index: var(--z-floating);
    /* Let vertical scrolls pass through; we only act on horizontal. */
    touch-action: pan-y;
  }
  .edge.left {
    left: 0;
  }
  .edge.right {
    right: 0;
  }
</style>
