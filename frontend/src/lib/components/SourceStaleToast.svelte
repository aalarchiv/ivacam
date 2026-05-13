<script lang="ts">
  /// "Source file changed externally — Reload?" toast. Shown only when
  /// the user has `autoReloadSources` disabled in settings; otherwise
  /// the App.svelte effect reloads silently.

  import { project } from '../state/project.svelte';

  interface Props {
    onReload: (path: string) => void | Promise<void>;
  }
  let { onReload }: Props = $props();

  function basename(p: string): string {
    return p.split(/[\\/]/).pop() ?? p;
  }

  async function reload() {
    const notice = project.sourceFileStaleNotice;
    if (!notice) return;
    project.sourceFileStaleNotice = null;
    await onReload(notice.path);
  }

  function ignore() {
    project.sourceFileStaleNotice = null;
  }
</script>

{#if project.sourceFileStaleNotice}
  <div class="toast" role="alert" aria-live="polite">
    <span class="msg">
      <strong>{basename(project.sourceFileStaleNotice.path)}</strong>
      changed externally.
    </span>
    <button type="button" class="primary" onclick={reload}>Reload</button>
    <button type="button" class="secondary" onclick={ignore}>Ignore</button>
  </div>
{/if}

<style>
  .toast {
    position: fixed;
    bottom: 1rem;
    right: 1rem;
    z-index: var(--z-toast);
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.5rem 0.7rem;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.35);
    font-size: 0.8rem;
    max-width: min(420px, 90vw);
  }
  .msg {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .toast strong {
    color: var(--text-strong);
    font-weight: 600;
  }
  button {
    border: 0;
    border-radius: 3px;
    padding: 0.25rem 0.6rem;
    font-size: 0.75rem;
    cursor: pointer;
  }
  button.primary {
    background: var(--accent);
    color: white;
  }
  button.secondary {
    background: transparent;
    color: var(--text-muted);
    border: 1px solid var(--border);
  }
  button.secondary:hover {
    color: var(--text);
  }
</style>
