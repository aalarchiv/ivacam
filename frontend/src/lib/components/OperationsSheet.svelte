<script lang="ts">
  // Operations bottom sheet — the phone hot-path surface (7jug.9). On
  // narrow layouts the Operations pane is a draggable sheet docked to the
  // bottom edge over the canvas, rather than the full-screen sidebar
  // overlay, so selecting a 2D element and editing its op happen on one
  // screen. The 2D canvas stays visible above the sheet's open height.
  //
  // Fold mechanics (snap fractions, tap-toggle, drag-to-nearest snap) live
  // in the rune-free `bottom-panel-fold.ts` so they're unit-tested without
  // the rune runtime; this component is just the gesture + layout shell
  // that drives them, persists the open snap, and rehosts OperationsList
  // unchanged.

  import OperationsList from './OperationsList.svelte';
  import { project } from '../state/project.svelte';
  import { workspace } from '../state/workspace.svelte';
  import {
    FOLD_SNAPS,
    FOLDED,
    nearestSnap,
    snapHeightPx,
    toggleFold,
  } from '../state/bottom-panel-fold';

  /// Handle-strip height (always visible, even folded). Sized for a
  /// coarse-pointer target.
  const HANDLE_PX = 44;
  /// Pointer travel under this (px) on the handle counts as a tap, not a
  /// drag — a tap toggles fold, a drag snaps to the nearest position.
  const TAP_TOL_PX = 6;
  /// Tallest snap — the drag can't pull the sheet past the last fold.
  const MAX_SNAP = FOLD_SNAPS[FOLD_SNAPS.length - 1];

  // Live viewport height drives the px snap math; tracked here (rather than
  // pure CSS) so a drag can convert pointer travel into a fold fraction.
  let viewportPx = $state(typeof window !== 'undefined' ? window.innerHeight : 800);

  // Committed fold position (fraction of viewport height). Starts folded so
  // the canvas is fully visible when the user lands on the Design surface;
  // tapping the handle opens to the persisted snap.
  let foldSnap = $state(FOLDED);

  // Free fraction while a drag is in flight (null otherwise). The body
  // height follows this for a smooth pull, then snaps on release.
  let dragFrac = $state<number | null>(null);

  const heightFrac = $derived(dragFrac ?? foldSnap);
  const open = $derived(heightFrac > 0);
  const bodyPx = $derived(snapHeightPx(heightFrac, viewportPx));

  /// Last open snap, persisted to the workspace so the sheet reopens to the
  /// user's preferred height.
  function savedOpenSnap(): number {
    void workspace.version;
    return workspace.get().panels.ops_fold_snap;
  }
  function persistOpen(snap: number) {
    if (snap > 0) workspace.setPanels({ ops_fold_snap: snap });
  }

  /// On unfold, surface the MRU op — most-recently-used (the current
  /// selection, left as-is) else most-recently-added (last in the list) —
  /// so the user lands on something editable. OperationsList expands the
  /// selected op inline and scrolls it into view.
  function surfaceMruOp() {
    if (project.sel.selectedOpId != null) return;
    const last = project.data.operations.at(-1);
    if (last) project.sel.selectedOpId = last.id;
  }

  /// Commit a new fold position, persisting + surfacing the MRU op when it
  /// opens the sheet.
  function commit(snap: number) {
    const wasFolded = foldSnap <= 0;
    foldSnap = snap;
    if (snap > 0) {
      persistOpen(snap);
      if (wasFolded) surfaceMruOp();
    }
  }

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
      commit(toggleFold(foldSnap, savedOpenSnap()));
    } else if (free != null) {
      commit(nearestSnap(free));
    }
  }

  function handleKey(e: KeyboardEvent) {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      commit(toggleFold(foldSnap, savedOpenSnap()));
    }
  }

  const opCount = $derived(project.data.operations.length);
</script>

<svelte:window onresize={() => (viewportPx = window.innerHeight)} />

<section
  class="ops-sheet"
  class:open
  style:--body-px="{bodyPx}px"
  style:--handle-px="{HANDLE_PX}px"
  class:dragging={dragFrac != null}
  aria-label="Operations"
>
  <!-- Drag handle / grabber. Tap toggles fold; drag snaps to the nearest
       fold position on release. Labeled with the op count only when folded
       — when open, OperationsList's own header carries the controls. -->
  <div
    class="handle"
    role="button"
    tabindex="0"
    aria-expanded={open}
    aria-label={open ? 'Collapse operations' : 'Expand operations'}
    title={open ? 'Drag or tap to collapse Operations' : 'Drag or tap to open Operations'}
    onpointerdown={handlePointerDown}
    onpointermove={handlePointerMove}
    onpointerup={handlePointerUp}
    onpointercancel={handlePointerUp}
    onkeydown={handleKey}
  >
    <span class="grip" aria-hidden="true"></span>
    {#if !open}
      <span class="handle-label">Operations<span class="count">{opCount}</span></span>
    {/if}
  </div>

  <!-- Body: OperationsList, rehosted unchanged and always expanded. Kept
       mounted while folded (so the 5000-object lookups don't rebuild on
       every open) but height-collapsed and made inert. -->
  <div class="body" inert={!open} aria-hidden={!open}>
    <OperationsList active={true} onActivate={() => {}} />
  </div>
</section>

<style>
  .ops-sheet {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: var(--z-floating);
    display: flex;
    flex-direction: column;
    background: var(--panel-bg, #1e1e1e);
    border-top: 1px solid var(--border, #3a3a3a);
    border-top-left-radius: 12px;
    border-top-right-radius: 12px;
    box-shadow: 0 -4px 18px rgb(0 0 0 / 35%);
    /* Total height = handle strip + the snapped body. */
    height: calc(var(--handle-px) + var(--body-px));
    max-height: calc(var(--handle-px) + 75vh);
    /* Don't animate while the finger is dragging — only on snap release. */
    transition: height 0.18s ease;
  }
  .ops-sheet.dragging {
    transition: none;
  }

  .handle {
    flex: 0 0 var(--handle-px);
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
    position: relative;
    cursor: grab;
    /* The handle owns vertical drags; let the browser not hijack them. */
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
    /* Sit the label to the right of the centered grip (folded strip shares
       its left with the future g-code handle — see .11). */
    position: absolute;
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
  /* OperationsList (`.ops`) already scrolls internally at height:100%; in
     the sheet it spans the full width, so drop its sidebar border. */
  .body :global(.ops) {
    border-left: none;
  }
</style>
