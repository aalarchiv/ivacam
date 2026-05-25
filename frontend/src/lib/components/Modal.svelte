<script lang="ts">
  import { onMount, tick } from 'svelte';
  import {
    __scrollCache,
    __geomCache,
    centeredModalPosition,
    clampModalPosition,
    handleModalKey,
    FOCUSABLE_SELECTOR,
    type ModalGeom,
  } from './modal_behavior';

  interface Props {
    onClose: () => void;
    persistKey?: string;
    modalClass?: string;
    /// Inline width / max-height for the dialog body. Lets callers size
    /// the modal without resorting to `:global(.X-modal)` overrides
    /// (Svelte's escape hatch leaks selector scope into the global
    /// namespace, which the linter audit flagged).
    width?: string;
    maxHeight?: string;
    /// zi6p: opt-in window behaviors (default off so the ~6 plain
    /// dialogs stay centered + static). `draggable` lets the user
    /// reposition by the inner <header>; `resizable` adds a corner grip.
    draggable?: boolean;
    resizable?: boolean;
    /// id of the heading element inside the dialog; wired to
    /// `aria-labelledby` so screen readers announce the dialog by
    /// title rather than as an unlabelled "dialog".
    ariaLabelledBy?: string;
    children: import('svelte').Snippet;
  }
  let {
    onClose,
    persistKey,
    modalClass,
    width,
    maxHeight,
    draggable = false,
    resizable = false,
    ariaLabelledBy,
    children,
  }: Props = $props();

  let trigger: Element | null = null;
  let overlay: HTMLDivElement;
  let body: HTMLDivElement;

  /// zi6p: when draggable/resizable, the modal is positioned with
  /// `position: fixed` at this top-left and (optionally) a custom size.
  /// null until measured on mount, so the first paint can fall back to
  /// the centered flex layout to avoid a flash.
  let geom = $state<ModalGeom | null>(null);
  const floating = $derived(draggable || resizable);

  /// Drag state. The header pointerdown records the grab offset; window
  /// pointermove updates geom.left/top clamped so the header stays
  /// grabbable; pointerup releases.
  let dragging = false;
  let grabDx = 0;
  let grabDy = 0;
  let headerEl: HTMLElement | null = null;

  function onHeaderPointerDown(e: PointerEvent) {
    if (!draggable || !body || e.button !== 0) return;
    // Don't start a drag from an interactive control in the header
    // (e.g. the × close button) — let it handle its own click.
    const t = e.target as HTMLElement;
    if (t.closest('button, a, input, select, textarea')) return;
    const rect = body.getBoundingClientRect();
    if (!geom) {
      geom = { left: rect.left, top: rect.top, width: null, height: null };
    }
    grabDx = e.clientX - rect.left;
    grabDy = e.clientY - rect.top;
    dragging = true;
    window.addEventListener('pointermove', onPointerMove);
    window.addEventListener('pointerup', onPointerUp);
    e.preventDefault();
  }

  function onPointerMove(e: PointerEvent) {
    if (!dragging || !body || !geom) return;
    const headerH = headerEl?.getBoundingClientRect().height ?? 32;
    const next = clampModalPosition(
      e.clientX - grabDx,
      e.clientY - grabDy,
      body.getBoundingClientRect().width,
      window.innerWidth,
      window.innerHeight,
      headerH,
    );
    geom = { ...geom, left: next.left, top: next.top };
  }

  function onPointerUp() {
    dragging = false;
    window.removeEventListener('pointermove', onPointerMove);
    window.removeEventListener('pointerup', onPointerUp);
    persistGeom();
  }

  /// Record the modal's current box (after a resize too) into geom +
  /// the per-persistKey cache so reopen restores it.
  function persistGeom() {
    if (!body) return;
    const rect = body.getBoundingClientRect();
    geom = {
      left: rect.left,
      top: rect.top,
      width: resizable ? Math.round(rect.width) : (geom?.width ?? null),
      height: resizable ? Math.round(rect.height) : (geom?.height ?? null),
    };
    if (persistKey) __geomCache.set(persistKey, geom);
  }

  let resizeObserver: ResizeObserver | null = null;

  onMount(() => {
    trigger = document.activeElement;
    if (persistKey && body) {
      const saved = __scrollCache.get(persistKey);
      if (saved !== undefined) body.scrollTop = saved;
    }

    if (floating && body) {
      // Restore a remembered geometry, else center at the intrinsic
      // size after layout settles.
      const remembered = persistKey ? __geomCache.get(persistKey) : undefined;
      void tick().then(() => {
        if (!body) return;
        const rect = body.getBoundingClientRect();
        if (remembered) {
          geom = { ...remembered };
        } else {
          const c = centeredModalPosition(
            rect.width,
            rect.height,
            window.innerWidth,
            window.innerHeight,
          );
          geom = { left: c.left, top: c.top, width: null, height: null };
        }
        headerEl = body.querySelector('header');
        if (draggable && headerEl) {
          headerEl.addEventListener('pointerdown', onHeaderPointerDown);
          headerEl.classList.add('modal-drag-handle');
        }
        // A user-driven resize updates the cached geometry on release;
        // ResizeObserver fires while resizing, so we just persist the
        // latest box (cheap — only when resizable).
        if (resizable && typeof ResizeObserver !== 'undefined') {
          resizeObserver = new ResizeObserver(() => persistGeom());
          resizeObserver.observe(body);
        }
      });
    }

    // Autofocus the first focusable element on open so keyboard users
    // immediately enter the trap and screen readers read the labelled
    // dialog. Falls back to focusing the dialog body itself (it's
    // tabindex="-1") if no inner control is focusable yet.
    queueMicrotask(() => {
      if (!body) return;
      const first = body.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
      if (first) first.focus();
      else body.focus();
    });
    return () => {
      if (persistKey && body) __scrollCache.set(persistKey, body.scrollTop);
      if (headerEl) headerEl.removeEventListener('pointerdown', onHeaderPointerDown);
      window.removeEventListener('pointermove', onPointerMove);
      window.removeEventListener('pointerup', onPointerUp);
      resizeObserver?.disconnect();
      if (trigger instanceof HTMLElement && document.contains(trigger)) trigger.focus();
    };
  });

  function onKey(e: KeyboardEvent) {
    handleModalKey(e, body, onClose);
  }

  function onOverlayClick(e: MouseEvent) {
    if (e.target === overlay) onClose();
  }
</script>

<div
  bind:this={overlay}
  class="overlay"
  class:floating
  role="presentation"
  onkeydown={onKey}
  onclick={onOverlayClick}
>
  <div
    bind:this={body}
    class="modal {modalClass ?? ''}"
    class:floating
    class:resizable
    role="dialog"
    aria-modal="true"
    aria-labelledby={ariaLabelledBy ?? null}
    tabindex="-1"
    style:width={geom?.width != null ? `${geom.width}px` : (width ?? null)}
    style:max-height={floating ? null : (maxHeight ?? null)}
    style:height={geom?.height != null ? `${geom.height}px` : null}
    style:left={floating && geom ? `${geom.left}px` : null}
    style:top={floating && geom ? `${geom.top}px` : null}
  >
    {@render children()}
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--bg-app, #000) 60%, transparent);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 5vh;
    z-index: var(--z-modal);
  }
  .modal {
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: 0 10px 40px var(--shadow-modal);
    max-height: 86vh;
    overflow: auto;
    min-width: min(480px, 100vw);
    /* tabindex="-1" on the body makes it focusable so we can pull focus
       into the dialog programmatically on open, without it appearing in
       the natural Tab order. */
    outline: none;
  }
  /* zi6p: a floating (draggable/resizable) modal is positioned by JS
     with position:fixed, so it ignores the overlay's flex centering.
     The overlay still covers the screen for the backdrop + click-to-
     close. */
  .modal.floating {
    position: fixed;
    margin: 0;
    max-height: 92vh;
  }
  .modal.resizable {
    resize: both;
    min-width: 320px;
    min-height: 200px;
    max-width: 96vw;
    max-height: 92vh;
  }
  /* The header doubles as the drag handle when draggable — signal it. */
  .modal.floating :global(.modal-drag-handle) {
    cursor: move;
    user-select: none;
    touch-action: none;
  }
  /* Shared "×" close button. Dialogs render it via `<button class="dlg-close">`
     inside their header — declared :global here (was duplicated across 9
     dialogs at 1.0/1.1/1.2/1.4 rem sizes). Single source of truth. */
  :global(.dlg-close) {
    background: transparent;
    color: var(--text-muted);
    border: 0;
    font-size: 1.2rem;
    cursor: pointer;
    padding: 0 0.3rem;
    line-height: 1;
  }
  :global(.dlg-close):hover {
    color: var(--text-strong);
  }
</style>
