<script lang="ts">
  import { onMount } from 'svelte';
  import { __scrollCache, handleModalKey, FOCUSABLE_SELECTOR } from './modal_behavior';

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
    /// id of the heading element inside the dialog; wired to
    /// `aria-labelledby` so screen readers announce the dialog by
    /// title rather than as an unlabelled "dialog".
    ariaLabelledBy?: string;
    children: import('svelte').Snippet;
  }
  let { onClose, persistKey, modalClass, width, maxHeight, ariaLabelledBy, children }: Props =
    $props();

  let trigger: Element | null = null;
  let overlay: HTMLDivElement;
  let body: HTMLDivElement;

  onMount(() => {
    trigger = document.activeElement;
    if (persistKey && body) {
      const saved = __scrollCache.get(persistKey);
      if (saved !== undefined) body.scrollTop = saved;
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
  role="presentation"
  onkeydown={onKey}
  onclick={onOverlayClick}
>
  <div
    bind:this={body}
    class="modal {modalClass ?? ''}"
    role="dialog"
    aria-modal="true"
    aria-labelledby={ariaLabelledBy ?? null}
    tabindex="-1"
    style:width={width ?? null}
    style:max-height={maxHeight ?? null}
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
