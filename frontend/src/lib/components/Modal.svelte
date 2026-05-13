<script lang="ts">
  import { onMount } from 'svelte';
  import { __scrollCache, handleModalKey } from './modal_behavior';

  interface Props {
    onClose: () => void;
    persistKey?: string;
    modalClass?: string;
    children: import('svelte').Snippet;
  }
  let { onClose, persistKey, modalClass, children }: Props = $props();

  let trigger: Element | null = null;
  let overlay: HTMLDivElement;
  let body: HTMLDivElement;

  onMount(() => {
    trigger = document.activeElement;
    if (persistKey && body) {
      const saved = __scrollCache.get(persistKey);
      if (saved !== undefined) body.scrollTop = saved;
    }
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
  <div bind:this={body} class="modal {modalClass ?? ''}" role="dialog" aria-modal="true">
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
    border-radius: 6px;
    box-shadow: 0 10px 40px var(--shadow-modal, rgba(0, 0, 0, 0.4));
    max-height: 86vh;
    overflow: auto;
    min-width: 480px;
  }
</style>
