<script lang="ts">
  // Generic floating window: drag-movable header + bottom-right resize
  // handle, clamped to the viewport. Extracted from GenerateBar's
  // warnings panel so other hosts can reuse the mechanics; the host
  // fills the body via the `children` snippet.
  //
  // Position is in viewport pixels relative to (0,0); size is the
  // panel's content box. Defaults sit the panel in the top-right, but
  // the user can drag the header to reposition and pull the
  // bottom-right corner to resize. State is component-local and the
  // component stays mounted while closed (`open` gates rendering, the
  // host must NOT wrap it in {#if}) — re-opening therefore resets to
  // the last in-session position unless the window has shrunk past it,
  // in which case `clampPanelRect` snaps it back into view.
  import { clampPanelRect, initialPanelPosition } from './floating-panel';

  interface Props {
    /// Render gate. Keep the component mounted and toggle this so the
    /// in-session position/size survives close + reopen.
    open: boolean;
    onClose: () => void;
    /// Header text; may carry live data (e.g. a warning count).
    title: string;
    /// Accessible dialog name. Defaults to `title` — pass a stable
    /// string when the title embeds changing counts.
    ariaLabel?: string;
    initialWidth?: number;
    initialHeight?: number;
    minWidth?: number;
    minHeight?: number;
    children: import('svelte').Snippet;
  }
  let {
    open,
    onClose,
    title,
    ariaLabel,
    initialWidth = 480,
    initialHeight = Math.round(typeof window === 'undefined' ? 480 : window.innerHeight * 0.6),
    minWidth = 320,
    minHeight = 220,
    children,
  }: Props = $props();

  let x = $state<number | null>(null); // null = uncomputed → default to top-right on first open
  let y = $state<number | null>(null);
  // initial* props are deliberately initial-value-only: once the user
  // resizes, the panel keeps its own size and a late prop change must
  // not stomp it.
  // svelte-ignore state_referenced_locally
  let w = $state<number>(initialWidth);
  // svelte-ignore state_referenced_locally
  let h = $state<number>(initialHeight);
  let drag: { mode: 'move' | 'resize'; offX: number; offY: number; pointerId: number } | null =
    null;

  function clamp() {
    if (typeof window === 'undefined') return;
    const next = clampPanelRect(
      { x, y, w, h },
      window.innerWidth,
      window.innerHeight,
      minWidth,
      minHeight,
    );
    x = next.x;
    y = next.y;
    w = next.w;
    h = next.h;
  }

  function onOpen() {
    if (typeof window === 'undefined') return;
    if (x == null || y == null) {
      const p = initialPanelPosition(window.innerWidth, w);
      x = p.x;
      y = p.y;
    }
    clamp();
  }
  $effect(() => {
    if (open) onOpen();
  });

  function headerPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest('button')) return; // don't grab a drag from the close button
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag = {
      mode: 'move',
      offX: e.clientX - (x ?? 0),
      offY: e.clientY - (y ?? 0),
      pointerId: e.pointerId,
    };
    e.preventDefault();
  }
  function resizePointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag = {
      mode: 'resize',
      offX: e.clientX - w,
      offY: e.clientY - h,
      pointerId: e.pointerId,
    };
    e.preventDefault();
  }
  function pointerMove(e: PointerEvent) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    if (drag.mode === 'move') {
      x = e.clientX - drag.offX;
      y = e.clientY - drag.offY;
    } else {
      w = e.clientX - drag.offX;
      h = e.clientY - drag.offY;
    }
    clamp();
  }
  function pointerUp(e: PointerEvent) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    drag = null;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {}
  }
  // Re-clamp when the viewport changes so a previously-sized panel can't
  // sit off-screen after the user shrinks the window.
  function onWindowResize() {
    if (open) clamp();
  }
</script>

<svelte:window onresize={onWindowResize} />

{#if open}
  <div
    class="panel"
    role="dialog"
    aria-label={ariaLabel ?? title}
    style:left="{x ?? 0}px"
    style:top="{y ?? 0}px"
    style:width="{w}px"
    style:height="{h}px"
    style:min-width="{minWidth}px"
    style:min-height="{minHeight}px"
  >
    <header
      role="toolbar"
      tabindex="-1"
      aria-label="{ariaLabel ?? title} panel header — drag to move"
      onpointerdown={headerPointerDown}
      onpointermove={pointerMove}
      onpointerup={pointerUp}
      onpointercancel={pointerUp}
      title="Drag to move"
    >
      <h3>{title}</h3>
      <button class="dlg-close" onclick={onClose} aria-label="Close">×</button>
    </header>
    {@render children()}
    <!-- Bottom-right resize handle. svg corner-glyph repeats the
         convention used by every other floating-resizable widget on
         the platform. -->
    <div
      class="resize-handle"
      onpointerdown={resizePointerDown}
      onpointermove={pointerMove}
      onpointerup={pointerUp}
      onpointercancel={pointerUp}
      title="Drag to resize"
      aria-hidden="true"
    ></div>
  </div>
{/if}

<style>
  /* Floating panel — fixed positioning so the drag-movable top/left
     coordinates work in screen space rather than inheriting a relative
     offset from the host. Resize handle in the SE corner. Laid out as
     a column so a `flex: 1` body fills the space between the header
     and the bottom edge. */
  .panel {
    position: fixed;
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    box-shadow: 0 6px 18px var(--shadow-modal);
    z-index: var(--z-floating);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .panel header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.7rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
    cursor: grab;
    user-select: none;
    touch-action: none;
  }
  .panel header:active {
    cursor: grabbing;
  }
  .panel h3 {
    font-size: 0.85rem;
    margin: 0;
    color: var(--text-strong);
  }
  /* The panel's close uses the shared `.dlg-close` (declared :global
     in Modal.svelte). */
  .panel .resize-handle {
    position: absolute;
    right: 0;
    bottom: 0;
    width: 14px;
    height: 14px;
    cursor: nwse-resize;
    touch-action: none;
    /* Two diagonal lines drawn as a corner glyph — matches the
       OS-native resize affordance. */
    background:
      linear-gradient(
          135deg,
          transparent 45%,
          var(--text-muted) 45%,
          var(--text-muted) 55%,
          transparent 55%
        )
        center / 100% 100% no-repeat,
      linear-gradient(
          135deg,
          transparent 70%,
          var(--text-muted) 70%,
          var(--text-muted) 80%,
          transparent 80%
        )
        center / 100% 100% no-repeat;
  }
  .panel .resize-handle:hover {
    background:
      linear-gradient(
          135deg,
          transparent 45%,
          var(--text-strong) 45%,
          var(--text-strong) 55%,
          transparent 55%
        )
        center / 100% 100% no-repeat,
      linear-gradient(
          135deg,
          transparent 70%,
          var(--text-strong) 70%,
          var(--text-strong) 80%,
          transparent 80%
        )
        center / 100% 100% no-repeat;
  }
</style>
