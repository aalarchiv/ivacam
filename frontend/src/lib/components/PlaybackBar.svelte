<script lang="ts">
  // 3D toolpath scrubber. Drives `project.playhead` (a fraction in [0,1])
  // which Scene3D reads to position a tool tip cone along the path.

  import { project } from '../state/project.svelte';

  let speed = $state(1.0);
  let playing = $state(false);
  let raf = 0;
  let lastTs = 0;

  $effect(() => {
    if (playing && project.generated) {
      lastTs = performance.now();
      raf = requestAnimationFrame(tick);
    }
    return () => cancelAnimationFrame(raf);
  });

  function tick(now: number) {
    if (!playing) return;
    const dt = (now - lastTs) / 1000;
    lastTs = now;
    let next = project.playhead + dt * 0.1 * speed;
    if (next >= 1) {
      next = 1;
      playing = false;
    }
    project.playhead = next;
    if (playing) raf = requestAnimationFrame(tick);
  }

  function togglePlay() {
    if (!project.generated) return;
    if (project.playhead >= 0.999) project.playhead = 0;
    playing = !playing;
  }

  function onScrub(e: Event) {
    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
    project.playhead = isNaN(v) ? 0 : v;
    playing = false;
  }
</script>

{#if project.generated && project.generated.toolpath.length > 0}
  <div class="bar">
    <button onclick={togglePlay} disabled={!project.generated}>
      {playing ? '❚❚' : '▶'}
    </button>
    <input
      type="range"
      min="0"
      max="1"
      step="0.001"
      value={project.playhead}
      oninput={onScrub}
    />
    <label
      >×<input
        type="number"
        bind:value={speed}
        step="0.5"
        min="0.1"
        max="10"
        title="Playback speed"
      /></label
    >
    <span class="counter">
      {Math.round(project.playhead * project.generated.toolpath.length)}/{project.generated
        .toolpath.length}
    </span>
  </div>
{/if}

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.3rem 0.6rem;
    background: var(--bg-panel);
    border-top: 1px solid var(--border);
    color: var(--text-muted);
    font-size: 0.74rem;
  }
  button {
    background: var(--accent);
    color: white;
    border: 0;
    border-radius: 3px;
    padding: 0.15rem 0.55rem;
    font-size: 0.85rem;
    cursor: pointer;
    min-width: 2.2rem;
  }
  input[type='range'] {
    flex: 1;
    accent-color: var(--accent);
  }
  input[type='number'] {
    width: 4rem;
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.1rem 0.3rem;
    font-size: 0.74rem;
  }
  label {
    display: inline-flex;
    align-items: center;
    gap: 0.15rem;
  }
  .counter {
    font-variant-numeric: tabular-nums;
  }
</style>
