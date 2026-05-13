<script lang="ts">
  import { loadingMessage, shouldShow } from './loading_overlay';

  interface Props {
    visible: boolean;
    message?: string | null;
  }
  let { visible, message }: Props = $props();

  const label = $derived(loadingMessage(message));
  const show = $derived(shouldShow(visible));
</script>

{#if show}
  <div class="overlay" role="status" aria-live="polite" aria-busy="true">
    <div class="card">
      <div class="spinner" aria-hidden="true"></div>
      <span class="msg">{label}</span>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--bg-app) 60%, transparent);
    backdrop-filter: blur(2px);
    z-index: var(--z-overlay);
    pointer-events: all;
  }
  .card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.6rem;
    padding: 1rem 1.4rem;
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    box-shadow: 0 6px 20px var(--shadow-modal);
    color: var(--text);
    font-size: 0.85rem;
  }
  .spinner {
    width: 1.8rem;
    height: 1.8rem;
    border: 3px solid color-mix(in srgb, var(--text) 18%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: wiac-loading-spin 0.9s linear infinite;
  }
  .msg {
    color: var(--text-muted);
  }
  @keyframes wiac-loading-spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
