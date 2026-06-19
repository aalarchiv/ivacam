<script lang="ts">
  // Generic phone bottom sheet — the shared mechanics behind the
  // Operations (.9) and G-code (.11) panels. A draggable sheet docked to
  // the bottom edge over the canvas; the body (passed as a snippet) stays
  // visible above the open height. Fold positions and snap math live in
  // the rune-free `bottom-panel-fold.ts`; mutual exclusion (only one panel
  // open at a time, so their folded handles tile the bottom strip) lives
  // in the `bottomPanels` store. This component is the gesture + layout
  // shell tying them together.
  import type { Snippet } from 'svelte';
  import { bottomPanels, type BottomPanelKey } from '../state/bottom-panels.svelte';
  import {
    FOLD_SNAPS,
    nearestSnap,
    restoreOpenSnap,
    snapHeightPx,
    toggleFold,
  } from '../state/bottom-panel-fold';

  interface Props {
    /// Panel identity — drives the shared open/fold coordination.
    key: BottomPanelKey;
    /// Folded-handle label (e.g. "Operations", "G-code"). Full text, used
    /// for the accessible name.
    label: string;
    /// Short code shown in the folded handle strip (e.g. "OPS", "NGC",
    /// "S+L") — the strip is narrow and the two panels share a row, so a
    /// compact code reads better than the full label. Falls back to `label`.
    code?: string;
    /// Which half of the folded bottom strip this handle occupies. The two
    /// panels take opposite sides so they tile when both are folded.
    side: 'left' | 'right';
    /// Optional badge shown next to the folded label (op count, line count).
    count?: number | null;
    /// Persisted preferred open snap (workspace), restored on open.
    savedSnap: number;
    /// Persist a new open snap (workspace write).
    onPersistSnap: (snap: number) => void;
    /// Called when the sheet opens from folded — e.g. Operations surfaces
    /// the MRU op. Not called when it merely re-snaps while already open.
    onOpen?: () => void;
    /// Monotonic counter the parent bumps to request the sheet open (e.g. a
    /// canvas element-tap that jumps to its operation — 7jug.16).
    openSignal?: number;
    /// Sheet body.
    children: Snippet;
  }
  let {
    key,
    label,
    code,
    side,
    count = null,
    savedSnap,
    onPersistSnap,
    onOpen,
    openSignal = 0,
    children,
  }: Props = $props();

  /// Handle-strip height (always visible). Sized for a coarse-pointer target.
  const HANDLE_PX = 44;
  /// Pointer travel under this (px) counts as a tap, not a drag.
  const TAP_TOL_PX = 6;
  /// Tallest snap — the drag can't pull the sheet past the last fold.
  const MAX_SNAP = FOLD_SNAPS[FOLD_SNAPS.length - 1];

  let viewportPx = $state(typeof window !== 'undefined' ? window.innerHeight : 800);

  // Committed fold position comes from the shared store (so opening this
  // panel folds the other). Free fraction while dragging follows the finger.
  const foldSnap = $derived(bottomPanels.snapOf(key));
  let dragFrac = $state<number | null>(null);

  const heightFrac = $derived(dragFrac ?? foldSnap);
  const open = $derived(heightFrac > 0);
  const bodyPx = $derived(snapHeightPx(heightFrac, viewportPx));

  /// Commit a new fold position; persist + fire onOpen on an open transition.
  function commit(snap: number) {
    const wasFolded = foldSnap <= 0;
    bottomPanels.setSnap(key, snap);
    if (snap > 0) {
      onPersistSnap(snap);
      if (wasFolded) onOpen?.();
    }
  }

  // Open on a parent request (counter dedupes so unrelated re-runs don't
  // re-open; only a fresh increment opens, to the persisted snap).
  // Seed from the MOUNT-TIME signal value, not 0: the sheet unmounts when
  // the activity leaves Project 2D/3D and remounts on return, but
  // `openSignal` (an app-level counter) keeps its value across that. Seeding
  // at 0 made every remount see "0 → N" as a fresh request and re-open the
  // folded sheet (punch-list 2). Only a real post-mount increment opens it.
  // svelte-ignore state_referenced_locally
  let lastOpenSignal = openSignal;
  $effect(() => {
    if (openSignal === lastOpenSignal) return;
    lastOpenSignal = openSignal;
    if (openSignal > 0) commit(restoreOpenSnap(savedSnap));
  });

  // ---- handle gestures -------------------------------------------------
  let drag: { pointerId: number; startY: number; baseFrac: number; moved: number } | null = null;

  function handlePointerDown(e: PointerEvent) {
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag = { pointerId: e.pointerId, startY: e.clientY, baseFrac: foldSnap, moved: 0 };
  }
  function handlePointerMove(e: PointerEvent) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    const dy = drag.startY - e.clientY; // drag up → positive → taller sheet
    drag.moved = Math.max(drag.moved, Math.abs(e.clientY - drag.startY));
    const frac = drag.baseFrac + dy / viewportPx;
    dragFrac = Math.max(0, Math.min(frac, MAX_SNAP));
  }
  function handlePointerUp(e: PointerEvent) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    const wasTap = drag.moved < TAP_TOL_PX;
    const free = dragFrac;
    drag = null;
    dragFrac = null;
    if (wasTap) {
      commit(toggleFold(foldSnap, savedSnap));
    } else if (free != null) {
      commit(nearestSnap(free));
    }
  }
  function handleKey(e: KeyboardEvent) {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      commit(toggleFold(foldSnap, savedSnap));
    }
  }
</script>

<svelte:window onresize={() => (viewportPx = window.innerHeight)} />

<section
  class="bottom-sheet {side}"
  class:open
  class:dragging={dragFrac != null}
  style:--body-px="{bodyPx}px"
  style:--handle-px="{HANDLE_PX}px"
  style:z-index={open ? 'calc(var(--z-floating) + 1)' : 'var(--z-floating)'}
  aria-label={label}
>
  <div
    class="handle"
    role="button"
    tabindex="0"
    aria-expanded={open}
    aria-label={open ? `Collapse ${label}` : `Expand ${label}`}
    title={open ? `Drag or tap to collapse ${label}` : `Drag or tap to open ${label}`}
    onpointerdown={handlePointerDown}
    onpointermove={handlePointerMove}
    onpointerup={handlePointerUp}
    onpointercancel={handlePointerUp}
    onkeydown={handleKey}
  >
    <span class="grip" aria-hidden="true"></span>
    {#if !open}
      <span class="handle-label">
        {code ?? label}{#if count != null}<span class="count">{count}</span>{/if}
      </span>
    {/if}
  </div>

  <div class="body" inert={!open} aria-hidden={!open}>
    {@render children()}
  </div>
</section>

<style>
  .bottom-sheet {
    position: fixed;
    bottom: 0;
    display: flex;
    flex-direction: column;
    /* Opaque, theme-correct panel colour — was var(--panel-bg) which is
       undefined (the token is --bg-panel), so it fell back to a hardcoded
       dark that looked wrong in the light theme and let the status bar
       show through the empty half of the folded strip. */
    background: var(--bg-panel);
    border-top: 1px solid var(--border);
    box-shadow: 0 -4px 18px rgb(0 0 0 / 35%);
    height: calc(var(--handle-px) + var(--body-px));
    max-height: calc(var(--handle-px) + 75vh);
    transition: height 0.18s ease;
  }
  .bottom-sheet.dragging {
    transition: none;
  }
  /* Folded: each panel owns half the bottom strip (handles tile). Open:
     the panel takes the full width over the canvas. */
  .bottom-sheet.left {
    left: 0;
    width: 50%;
    border-top-right-radius: 12px;
  }
  .bottom-sheet.right {
    right: 0;
    width: 50%;
    border-top-left-radius: 12px;
  }
  .bottom-sheet.open {
    left: 0;
    right: 0;
    width: 100%;
    border-top-left-radius: 12px;
    border-top-right-radius: 12px;
  }

  .handle {
    flex: 0 0 var(--handle-px);
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
    position: relative;
    cursor: grab;
    touch-action: none;
    user-select: none;
  }
  .handle:active {
    cursor: grabbing;
  }
  .grip {
    width: 36px;
    height: 4px;
    border-radius: 2px;
    background: var(--border, #555);
  }
  .handle-label {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.85rem;
    font-weight: 600;
    color: var(--text, #ddd);
    position: absolute;
    /* Keep the label clear of the centered grip on the strip's outer edge. */
  }
  .bottom-sheet.left .handle-label {
    left: 0.9rem;
  }
  .bottom-sheet.right .handle-label {
    right: 0.9rem;
  }
  .count {
    min-width: 1.2em;
    padding: 0 0.3em;
    border-radius: 999px;
    background: var(--border, #444);
    color: var(--text, #ddd);
    font-size: 0.72rem;
    text-align: center;
  }

  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow: hidden;
  }
  /* Hosted lists/panels (`.ops`, gcode) already scroll internally at
     height:100%; drop any sidebar border in the sheet context. */
  .body :global(.ops) {
    border-left: none;
  }
</style>
