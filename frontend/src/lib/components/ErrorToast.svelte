<script lang="ts">
  /// Error toast that adapts to either a plain string (legacy paths) or
  /// the structured `WiacError` produced by the backend. Structured errors
  /// render the recovery hint and an "Apply fix" button when an auto-fix
  /// is attached; `kind === 'internal'` adds a "Report this bug" action.
  ///
  /// Surfaced as a fixed-position toast bottom-right at `var(--z-toast)`
  /// — above modals, because backend errors that fire while a dialog is
  /// open (e.g. a tool-library generate-on-save) still need to reach the
  /// user. Successive errors queue instead of silently replacing the
  /// previous one. Non-critical errors auto-dismiss after 8 s; `internal`
  /// errors stay until the user dismisses so the bug-report link is
  /// reachable.

  import { project } from '../state/project.svelte';
  import { autoFixToCommand } from '../state/commands';
  import { confirmStore } from '../state/confirm.svelte';
  import type { WiacError } from '../api/types';

  type ToastError = string | WiacError;
  interface QueueItem {
    id: number;
    error: ToastError;
    /// `null` when no auto-dismiss is scheduled (used for `internal` errors).
    timer: ReturnType<typeof setTimeout> | null;
  }

  const AUTO_DISMISS_MS = 8_000;

  let nextId = 0;
  let queue = $state<QueueItem[]>([]);
  /// Marker that lets the $effect deduplicate when project.error is
  /// re-assigned to the same reference (Svelte's reactivity will still
  /// fire the effect on plain assignment).
  let lastEnqueued: ToastError | null = null;

  /// Drain project.error into the local queue so successive errors
  /// stack instead of clobbering the previous one. We `clearError()`
  /// once we've copied — keeping the slot busy would block subsequent
  /// `setError` calls from triggering this effect.
  $effect(() => {
    const cur = project.error;
    if (cur == null) {
      lastEnqueued = null;
      return;
    }
    if (cur === lastEnqueued) return;
    lastEnqueued = cur;
    enqueue(cur);
    queueMicrotask(() => {
      if (project.error === cur) project.clearError();
    });
  });

  function enqueue(err: ToastError) {
    const id = ++nextId;
    const isInternal = typeof err === 'object' && err?.kind === 'internal';
    const timer = isInternal ? null : setTimeout(() => dismissById(id), AUTO_DISMISS_MS);
    queue.push({ id, error: err, timer });
  }

  function dismissHead() {
    if (queue.length === 0) return;
    const head = queue[0];
    if (head.timer) clearTimeout(head.timer);
    queue.shift();
  }

  function dismissById(id: number) {
    const idx = queue.findIndex((q) => q.id === id);
    if (idx < 0) return;
    const item = queue[idx];
    if (item.timer) clearTimeout(item.timer);
    queue.splice(idx, 1);
  }

  /// Window-level ESC dismisses the head only when no modal is intercepting.
  /// Modal.svelte calls `stopPropagation()` on ESC, so this listener never
  /// fires while a dialog is open — exactly the priority we want.
  function onWindowKey(e: KeyboardEvent) {
    if (e.key !== 'Escape') return;
    if (queue.length === 0) return;
    if (confirmStore.pending) return; // confirm prompt eats ESC first
    dismissHead();
  }

  function structuredOf(err: ToastError): WiacError | null {
    return typeof err === 'object' && err !== null ? err : null;
  }
  function plainOf(err: ToastError): string | null {
    return typeof err === 'string' ? err : null;
  }

  function applyFix(structured: WiacError) {
    if (!structured.auto_fix) return;
    const cmd = autoFixToCommand(structured.auto_fix);
    project.history.exec(cmd, project);
    // The LowerSimResolution auto-fix mutates project.settings — its
    // backing store is localStorage (separate from the project file),
    // so persist immediately or the change is lost on reload.
    if (structured.auto_fix.kind === 'lower_sim_resolution') {
      project.saveSettings();
    }
    dismissHead();
  }

  function fixLabel(fix: WiacError['auto_fix']): string {
    if (!fix) return 'Apply fix';
    switch (fix.kind) {
      case 'assign_tool':
        return `Assign tool ${fix.suggested_tool_id} to op ${fix.op_id}`;
      case 'disable_op':
        return `Disable op ${fix.op_id}`;
      case 'change_profile_offset':
        return `Set op ${fix.op_id} offset to ${fix.suggested}`;
      case 'lower_sim_resolution':
        return `Lower sim resolution to ${fix.suggested_cell_mm} mm`;
    }
  }

  function reportBug(structured: WiacError) {
    const body = encodeURIComponent(
      `Backend error:\n\n\`\`\`json\n${JSON.stringify(structured, null, 2)}\n\`\`\``,
    );
    const title = encodeURIComponent(`Internal error: ${structured.message.slice(0, 80)}`);
    const url = `https://github.com/ivacam/ivacam/issues/new?title=${title}&body=${body}`;
    if (typeof window !== 'undefined') {
      try {
        window.open(url, '_blank', 'noopener');
        return;
      } catch {
        // fall through to clipboard
      }
      if (navigator?.clipboard?.writeText) {
        void navigator.clipboard.writeText(JSON.stringify(structured, null, 2));
      }
    }
  }
</script>

<svelte:window onkeydown={onWindowKey} />

{#if queue.length > 0}
  {@const head = queue[0]}
  {@const structured = structuredOf(head.error)}
  {@const plain = plainOf(head.error)}
  <div class="toast-host" aria-live="assertive" aria-atomic="true">
    {#if queue.length > 1}
      <div class="queue-tag" title="More errors queued — dismiss to see the next">
        +{queue.length - 1} more
      </div>
    {/if}
    {#if structured}
      <div class={`toast kind-${structured.kind}`} role="alert" data-testid="error-toast">
        <div class="head">
          <strong class="message">{structured.message}</strong>
          <button type="button" class="dlg-close" onclick={dismissHead} aria-label="Dismiss"
            >×</button
          >
        </div>
        {#if structured.recovery_hint}
          <em class="hint" data-testid="error-hint">{structured.recovery_hint}</em>
        {/if}
        {#if structured.span}
          <small class="span">at {structured.span.file}:{structured.span.line}</small>
        {/if}
        {#if structured.auto_fix || structured.kind === 'internal'}
          <div class="actions">
            {#if structured.auto_fix}
              <button
                type="button"
                class="fix"
                onclick={() => applyFix(structured)}
                data-testid="apply-fix"
              >
                {fixLabel(structured.auto_fix)}
              </button>
            {/if}
            {#if structured.kind === 'internal'}
              <button
                type="button"
                class="report"
                onclick={() => reportBug(structured)}
                data-testid="report-bug"
              >
                Report this bug
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {:else if plain}
      <div class="toast kind-misconfigured" role="alert" data-testid="error-legacy">
        <div class="head">
          <strong class="message legacy">{plain}</strong>
          <button type="button" class="dlg-close" onclick={dismissHead} aria-label="Dismiss"
            >×</button
          >
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .toast-host {
    position: fixed;
    right: 0.8rem;
    bottom: 0.8rem;
    z-index: var(--z-toast);
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 0.3rem;
    max-width: min(32rem, calc(100vw - 1.6rem));
    pointer-events: none;
  }
  .toast {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.55rem 0.8rem;
    border-radius: 4px;
    background: var(--bg-elevated);
    border: 1px solid var(--error);
    color: var(--text-strong);
    font-size: 0.82rem;
    box-shadow: 0 6px 20px var(--shadow-modal);
    pointer-events: auto;
    animation: ivac-toast-in 140ms ease-out;
  }
  .toast.kind-internal {
    border-color: var(--danger);
  }
  .queue-tag {
    background: color-mix(in srgb, var(--warn) 24%, var(--bg-elevated));
    color: var(--text-strong);
    border: 1px solid var(--warn);
    border-radius: 3px;
    padding: 0.1rem 0.4rem;
    font-size: 0.7rem;
    pointer-events: auto;
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
  }
  .message {
    flex: 1;
    font-weight: 600;
  }
  .message.legacy {
    color: var(--error);
    font-weight: 500;
  }
  .hint {
    font-style: italic;
    color: var(--text-muted);
  }
  .span {
    color: var(--text-muted);
    font-family: ui-monospace, monospace;
    font-size: 0.72rem;
  }
  .actions {
    display: flex;
    gap: 0.4rem;
    margin-top: 0.2rem;
  }
  .actions button {
    background: var(--accent);
    color: white;
    border: 0;
    border-radius: 3px;
    padding: 0.25rem 0.6rem;
    font-size: 0.74rem;
    cursor: pointer;
  }
  .actions button:hover {
    background: var(--accent-strong);
  }
  .actions .report {
    background: transparent;
    color: var(--accent);
    border: 1px solid currentColor;
  }
  .actions .report:hover {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-strong);
  }
  @keyframes ivac-toast-in {
    from {
      transform: translateY(8px);
      opacity: 0;
    }
    to {
      transform: translateY(0);
      opacity: 1;
    }
  }
</style>
