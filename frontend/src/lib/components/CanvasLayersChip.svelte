<script lang="ts">
  /// On-canvas Layers affordance for phone (7jug.15). On narrow screens
  /// there is no Stock/Layers/Text sidebar, so layer visibility + the
  /// "add drawing / add text" entry points fold onto the 2D canvas as a
  /// corner chip. Tapping the chip opens a compact popover that mirrors
  /// LayerList's core controls — per-layer visibility, show/hide all, and
  /// the Add menu — without the desktop panel's file-transform foldout
  /// (that stays a desktop-sidebar concern).
  ///
  /// State + mutations route through the same `project` store as
  /// LayerList (toggleLayer is undoable via the command bus), so the two
  /// surfaces stay in lockstep; this is purely an alternate, touch-first
  /// presentation.
  import { project } from '../state/project.svelte';

  interface Props {
    /// Open the native/hidden file picker (DXF / SVG / project).
    onOpenFileClick?: () => void;
    /// Open the Add Text dialog.
    onAddTextClick?: () => void;
  }
  let { onOpenFileClick, onAddTextClick }: Props = $props();

  let open = $state(false);

  // ACI → swatch colour, identical mapping to LayerList so a layer reads
  // the same colour in both surfaces.
  const ACI: Record<number, string> = {
    1: '#ff0000',
    2: '#ffff00',
    3: '#00ff00',
    4: '#00ffff',
    5: '#0000ff',
    6: '#ff00ff',
  };
  function swatch(c: number): string {
    if (c === 7 || c === 256) return 'var(--text-strong)';
    if (c === 8) return 'var(--text-muted)';
    return ACI[c] ?? 'var(--text-faint)';
  }

  /// Usable layers summed across all imports — same derivation as
  /// LayerList.usableLayers (drop empty layers; merge duplicate names).
  const usableLayers = $derived.by(() => {
    const byName = new Map<string, { name: string; color: number; segment_count: number }>();
    for (const entry of project.data.imports) {
      for (const l of entry.source.layers) {
        if (l.segment_count <= 0) continue;
        const existing = byName.get(l.name);
        if (existing) existing.segment_count += l.segment_count;
        else byName.set(l.name, { ...l });
      }
    }
    return Array.from(byName.values());
  });

  const visibleCount = $derived(
    usableLayers.filter((l) => project.data.visibleLayers.has(l.name)).length,
  );
  const allVisible = $derived(
    usableLayers.length > 0 && visibleCount === usableLayers.length,
  );

  function setAllVisible(on: boolean) {
    for (const l of usableLayers) {
      const has = project.data.visibleLayers.has(l.name);
      if (has !== on) project.toggleLayer(l.name);
    }
  }

  function onWindowPointer(e: MouseEvent) {
    if (!open) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest('.canvas-layers-chip')) return;
    open = false;
  }
  function onWindowKey(e: KeyboardEvent) {
    if (e.key === 'Escape' && open) {
      e.preventDefault();
      open = false;
    }
  }
  function pickAddFile() {
    open = false;
    onOpenFileClick?.();
  }
  function pickAddText() {
    open = false;
    onAddTextClick?.();
  }
</script>

<svelte:window onclick={onWindowPointer} onkeydown={onWindowKey} />

<div class="canvas-layers-chip">
  <button
    type="button"
    class="chip-trigger"
    aria-haspopup="menu"
    aria-expanded={open}
    aria-label="Layers and add"
    title="Layers · add drawing / text"
    onclick={() => (open = !open)}
  >
    <span class="chip-glyph" aria-hidden="true">▤</span>
    {#if usableLayers.length > 0}
      <span class="chip-count">{visibleCount}/{usableLayers.length}</span>
    {:else}
      <span class="chip-count">+</span>
    {/if}
  </button>

  {#if open}
    <div class="chip-menu" role="menu" aria-label="Layers and add">
      {#if usableLayers.length > 0}
        <button
          type="button"
          class="menu-item master"
          role="menuitemcheckbox"
          aria-checked={allVisible}
          onclick={() => setAllVisible(!allVisible)}
        >
          <span class="eye" aria-hidden="true">{allVisible ? '◉' : '○'}</span>
          <span class="lname">{allVisible ? 'Hide all layers' : 'Show all layers'}</span>
        </button>
        <div class="menu-sep" role="separator"></div>
        <div class="layer-scroll">
          {#each usableLayers as l (l.name)}
            {@const vis = project.data.visibleLayers.has(l.name)}
            <button
              type="button"
              class="menu-item layer"
              class:off={!vis}
              role="menuitemcheckbox"
              aria-checked={vis}
              onclick={() => project.toggleLayer(l.name)}
            >
              <span class="dot" style:background={swatch(l.color)} aria-hidden="true"></span>
              <span class="lname">{l.name}</span>
              <span class="lcount">{l.segment_count}</span>
              <span class="eye" aria-hidden="true">{vis ? '◉' : '○'}</span>
            </button>
          {/each}
        </div>
        <div class="menu-sep" role="separator"></div>
      {:else}
        <div class="menu-empty">No drawing loaded</div>
      {/if}

      <button type="button" class="menu-item" role="menuitem" onclick={pickAddFile}>
        <span class="eye" aria-hidden="true">+</span>
        <span class="lname">Open drawing…</span>
      </button>
      <button type="button" class="menu-item" role="menuitem" onclick={pickAddText}>
        <span class="eye" aria-hidden="true">T</span>
        <span class="lname">Add text…</span>
      </button>
    </div>
  {/if}
</div>

<style>
  /* Positioned by the parent .canvas-chip-dock (a flex row anchored
     bottom-left of the canvas host); the chip itself is just a relative
     box so its popover anchors to it. */
  .canvas-layers-chip {
    position: relative;
  }
  .chip-trigger {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    min-height: 2rem;
    padding: 0 0.55rem;
    border-radius: 1rem;
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text);
    opacity: 0.85;
    cursor: pointer;
    transition:
      opacity 0.12s,
      color 0.12s;
  }
  .chip-trigger:hover,
  .chip-trigger:focus-visible {
    opacity: 1;
    color: var(--text-strong);
  }
  .chip-glyph {
    font-size: 0.95rem;
    line-height: 1;
  }
  .chip-count {
    font-size: 0.78rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .chip-menu {
    position: absolute;
    left: 0;
    bottom: calc(100% + 0.35rem);
    min-width: 13rem;
    max-width: min(18rem, 80vw);
    padding: 0.3rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 6px 22px rgb(0 0 0 / 35%);
  }
  .layer-scroll {
    max-height: 40vh;
    overflow-y: auto;
  }
  .menu-item {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    min-height: 44px;
    padding: 0 0.5rem;
    background: none;
    border: none;
    border-radius: 5px;
    color: var(--text);
    font-size: 0.85rem;
    text-align: left;
    cursor: pointer;
  }
  .menu-item:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    color: var(--text-strong);
  }
  .menu-item.layer.off .lname {
    color: var(--text-muted);
  }
  .menu-item .dot {
    width: 0.7rem;
    height: 0.7rem;
    border-radius: 50%;
    flex: 0 0 auto;
    border: 1px solid var(--border);
  }
  .menu-item .lname {
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .menu-item .lcount {
    color: var(--text-muted);
    font-size: 0.75rem;
    font-variant-numeric: tabular-nums;
  }
  .menu-item .eye {
    flex: 0 0 auto;
    width: 1.1em;
    text-align: center;
    color: var(--text-muted);
  }
  .menu-item.master {
    font-weight: 600;
  }
  .menu-sep {
    height: 1px;
    margin: 0.3rem 0.2rem;
    background: var(--border);
  }
  .menu-empty {
    padding: 0.4rem 0.6rem;
    color: var(--text-muted);
    font-size: 0.82rem;
  }
</style>
