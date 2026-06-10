<script lang="ts">
  // Inline progress UI shown in place of the Generate button while the
  // pipeline streams per-op events. Reads pipelineState / pipelineProgress
  // from the project store; the parent (GenerateBar) flips state and feeds
  // events through `client.generateStreaming`.

  import { project } from '../state/project.svelte';

  let { onCancel }: { onCancel: () => void } = $props();

  let percent = $derived(
    project.gen.pipelineProgress
      ? Math.round(
          ((project.gen.pipelineProgress.opIdx + project.gen.pipelineProgress.opFraction) /
            Math.max(1, project.gen.pipelineProgress.opTotal)) *
            100,
        )
      : 0,
  );
  let label = $derived(
    project.gen.pipelineProgress
      ? `${project.gen.pipelineProgress.opIdx + 1} / ${project.gen.pipelineProgress.opTotal} — ${project.gen.pipelineProgress.opName}`
      : '',
  );
  let cancelling = $derived(project.gen.pipelineState === 'cancelling');
  let cachedHint = $derived(
    project.gen.lastGenerateCachedCount > 0
      ? ` (${project.gen.lastGenerateCachedCount} cached)`
      : '',
  );
</script>

<div class="progress-row" role="status" aria-live="polite">
  <div
    class="progress"
    role="progressbar"
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow={percent}
    title={label}
  >
    <div class="bar-fill" style:width="{percent}%"></div>
    <span class="progress-text">
      {#if cancelling}
        Cancelling…
      {:else}
        {label || 'starting…'}<span class="cached-hint">{cachedHint}</span>
      {/if}
    </span>
  </div>
  <button class="cancel" type="button" onclick={onCancel} disabled={cancelling}>
    {cancelling ? 'Cancelling…' : 'Cancel'}
  </button>
</div>

<style>
  .progress-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex: 1;
    min-width: 12rem;
  }
  .progress {
    position: relative;
    flex: 1;
    height: 1.4rem;
    min-width: 8rem;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: 3px;
    overflow: hidden;
  }
  .bar-fill {
    height: 100%;
    background: var(--accent);
    transition: width 120ms ease-out;
  }
  .progress-text {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.72rem;
    color: var(--text-strong);
    pointer-events: none;
    text-shadow: 0 0 4px var(--bg-app);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 0 0.4rem;
  }
  button.cancel {
    background: var(--danger);
    color: white;
    border: none;
    padding: 0.3rem 0.7rem;
    border-radius: 4px;
    font-size: 0.78rem;
    cursor: pointer;
  }
  button.cancel:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .cached-hint {
    opacity: 0.65;
    font-style: italic;
    margin-left: 0.25rem;
  }
</style>
