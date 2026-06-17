<script lang="ts">
  // Pull-to-refresh = Re-Generate (7jug.12). On phone the Generate button
  // is gone; instead a downward pull from the top of the canvas triggers a
  // (re-)generate. To avoid fighting the canvas's own pan/pinch, the pull
  // is caught by a thin strip pinned to the top edge (same idea as
  // EdgeSwipeNav's edge zones) — the canvas interior keeps its gestures.
  import { project } from '../state/project.svelte';

  interface Props {
    /// Fired when a pull past the threshold is released.
    onRefresh: () => void;
    /// Pull distance (px) required to arm the refresh.
    threshold?: number;
  }
  let { onRefresh, threshold = 64 }: Props = $props();

  /// Height of the top catch strip, and the most the indicator travels.
  const ZONE_PX = 28;
  const MAX_PULL = 96;

  let pid: number | null = null;
  let startY = 0;
  let pull = $state(0);

  const busy = $derived(
    project.gen.pipelineState === 'running' || project.gen.pipelineState === 'cancelling',
  );
  const armed = $derived(pull >= threshold);

  function down(e: PointerEvent) {
    if (busy) return;
    pid = e.pointerId;
    startY = e.clientY;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function move(e: PointerEvent) {
    if (e.pointerId !== pid) return;
    pull = Math.max(0, Math.min(e.clientY - startY, MAX_PULL));
  }
  function up(e: PointerEvent) {
    if (e.pointerId !== pid) return;
    pid = null;
    const trigger = pull >= threshold && !busy;
    pull = 0;
    if (trigger) onRefresh();
  }
</script>

<div
  class="ptr-zone"
  style:height="{ZONE_PX}px"
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
  aria-hidden="true"
></div>

{#if pull > 0 || busy}
  <div
    class="ptr-indicator"
    class:armed
    class:busy
    style:transform="translateX(-50%) translateY({busy ? 16 : Math.max(8, pull - 12)}px)"
  >
    <span class="ptr-glyph" style:transform={busy ? '' : `rotate(${pull * 3}deg)`}>⟳</span>
  </div>
{/if}

<style>
  .ptr-zone {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    z-index: var(--z-floating);
    /* Vertical pull only; let the browser keep horizontal handling. */
    touch-action: pan-x;
  }
  .ptr-indicator {
    position: absolute;
    top: 0;
    left: 50%;
    z-index: var(--z-floating);
    display: flex;
    align-items: center;
    justify-content: center;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    box-shadow: 0 2px 10px rgb(0 0 0 / 30%);
    color: var(--text-muted);
    pointer-events: none;
    transition: transform 0.05s linear;
  }
  .ptr-indicator.armed {
    color: var(--accent);
    border-color: var(--accent);
  }
  .ptr-glyph {
    font-size: 1.1rem;
    line-height: 1;
  }
  .ptr-indicator.busy {
    color: var(--accent);
    border-color: var(--accent);
  }
  .ptr-indicator.busy .ptr-glyph {
    animation: ptr-spin 0.8s linear infinite;
  }
  @keyframes ptr-spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
