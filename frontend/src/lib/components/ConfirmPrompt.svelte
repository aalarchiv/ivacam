<script lang="ts">
  /// Renders `confirmStore.pending` as a Modal-shaped dialog with two or
  /// three buttons. Mounted once at the App.svelte root; transparent when
  /// nothing is pending. Replaces the inline close-prompt overlay (which
  /// lacked ESC / backdrop / focus-trap / focus-restore) and the
  /// `window.confirm` Tauri anti-pattern in file_ops + clickRecent.
  import Modal from './Modal.svelte';
  import { confirmStore } from '../state/confirm.svelte';

  function onCancel() {
    confirmStore.answer('cancel');
  }
  function onExtra() {
    confirmStore.answer('extra');
  }
  function onConfirm() {
    confirmStore.answer('primary');
  }
</script>

{#if confirmStore.pending}
  {@const p = confirmStore.pending}
  <Modal onClose={onCancel} width="min(28rem, 95vw)" ariaLabelledBy="confirm-prompt-title">
    <header class="header">
      <h2 id="confirm-prompt-title">{p.title}</h2>
    </header>
    <p class="body">{p.body}</p>
    <div class="actions">
      <!-- Cancel is declared first so Modal's "autofocus first focusable"
           lands on the safe choice — accidentally hitting Enter on a
           freshly-opened discard prompt won't destroy work. The optional
           middle (extra) button sits between Cancel and the primary. -->
      <button type="button" class="btn-secondary" onclick={onCancel}>{p.cancelLabel}</button>
      {#if p.extraLabel}
        <button
          type="button"
          class={p.extraDanger ? 'btn-danger' : 'btn-secondary'}
          onclick={onExtra}
        >
          {p.extraLabel}
        </button>
      {/if}
      <button type="button" class={p.danger ? 'btn-danger' : 'btn-primary'} onclick={onConfirm}>
        {p.primaryLabel}
      </button>
    </div>
  </Modal>
{/if}

<style>
  .header {
    padding: 0.7rem 0.9rem 0.2rem;
  }
  h2 {
    margin: 0;
    font-size: 1.05rem;
    color: var(--text-strong);
  }
  .body {
    margin: 0;
    padding: 0.3rem 0.9rem 0.9rem;
    color: var(--text-muted);
    line-height: 1.4;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    padding: 0.5rem 0.9rem 0.7rem;
    border-top: 1px solid var(--border);
  }
</style>
