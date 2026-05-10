<script lang="ts">
  /// Error toast that adapts to either a plain string (legacy paths) or
  /// the structured `WiacError` produced by the backend. Structured errors
  /// render the recovery hint and an "Apply fix" button when an auto-fix
  /// is attached; `kind === 'internal'` adds a "Report this bug" action.

  import { project } from '../state/project.svelte';
  import { autoFixToCommand } from '../state/commands';
  import type { WiacError } from '../api/types';

  type Props = { error: string | WiacError | null };
  let { error }: Props = $props();

  let structured = $derived(typeof error === 'object' && error !== null ? error : null);
  let plain = $derived(typeof error === 'string' ? error : null);

  function applyFix() {
    if (!structured?.auto_fix) return;
    const cmd = autoFixToCommand(structured.auto_fix);
    project.history.exec(cmd, project);
    project.clearError();
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

  function reportBug() {
    if (!structured) return;
    const body = encodeURIComponent(
      `Backend error:\n\n\`\`\`json\n${JSON.stringify(structured, null, 2)}\n\`\`\``,
    );
    const title = encodeURIComponent(`Internal error: ${structured.message.slice(0, 80)}`);
    const url = `https://github.com/wiaconstructor/wiaconstructor/issues/new?title=${title}&body=${body}`;
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

  function dismiss() {
    project.clearError();
  }
</script>

{#if structured}
  <div class={`toast kind-${structured.kind}`} role="alert" data-testid="error-toast">
    <div class="head">
      <strong class="message">{structured.message}</strong>
      <button type="button" class="close" onclick={dismiss} aria-label="Dismiss">×</button>
    </div>
    {#if structured.recovery_hint}
      <em class="hint" data-testid="error-hint">{structured.recovery_hint}</em>
    {/if}
    {#if structured.span}
      <small class="span">at {structured.span.file}:{structured.span.line}</small>
    {/if}
    <div class="actions">
      {#if structured.auto_fix}
        <button type="button" class="fix" onclick={applyFix} data-testid="apply-fix">
          {fixLabel(structured.auto_fix)}
        </button>
      {/if}
      {#if structured.kind === 'internal'}
        <button type="button" class="report" onclick={reportBug} data-testid="report-bug">
          Report this bug
        </button>
      {/if}
    </div>
  </div>
{:else if plain}
  <span class="legacy" role="alert" data-testid="error-legacy">{plain}</span>
{/if}

<style>
  .toast {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.5rem 0.75rem;
    border-radius: 4px;
    background: var(--bg-elevated, #2a1f1f);
    border: 1px solid var(--border, #5a3030);
    color: var(--text-strong, #f0e6e6);
    font-size: 0.78rem;
    max-width: 32rem;
  }
  .toast.kind-internal {
    border-color: #c25050;
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
  .close {
    background: transparent;
    border: 0;
    color: var(--text-muted, #aaa);
    cursor: pointer;
    font-size: 1rem;
    padding: 0 0.2rem;
  }
  .hint {
    font-style: italic;
    color: var(--text-muted, #c0c0c0);
  }
  .span {
    color: var(--text-muted, #999);
    font-family: ui-monospace, monospace;
  }
  .actions {
    display: flex;
    gap: 0.4rem;
    margin-top: 0.2rem;
  }
  .actions button {
    background: var(--accent, #4a8df0);
    color: white;
    border: 0;
    border-radius: 3px;
    padding: 0.25rem 0.6rem;
    font-size: 0.74rem;
    cursor: pointer;
  }
  .actions .report {
    background: transparent;
    color: var(--accent, #4a8df0);
    border: 1px solid currentColor;
  }
  .legacy {
    color: #e54848;
    font-size: 0.85rem;
  }
</style>
