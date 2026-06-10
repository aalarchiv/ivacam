<script lang="ts">
  /// Imported-drawing layer list. Restyled (audit follow-up) to match
  /// the OperationsList group-header pattern: caret-to-collapse, a
  /// "show / hide all" master checkbox, a count chip, and a body that
  /// the user can fold away when they're not adjusting visibility.
  /// Same visual language as op groups so the sidebar reads as one
  /// coherent panel.
  import { isIdentityFileTransform, project } from '../state/project.svelte';

  interface Props {
    /// Accordion-controlled: parent passes `active` and `onActivate`;
    /// the panel renders its body iff `active` and clicking the caret
    /// asks the parent to activate (no internal collapsed state).
    active: boolean;
    onActivate: () => void;
    onOpenFileClick?: () => void;
    /// Add Text entry point — phase B of the stock-first rework. When
    /// provided, the panel header sprouts an Add+ dropdown that offers
    /// both creation paths in one place.
    onAddTextClick?: () => void;
    /// Startup reopen affordance — replaces the old top-of-window
    /// reopen banner. When set, the empty card offers a "Reopen <name>"
    /// button beside the regular Open file CTA so the no-drawing state
    /// is the single coherent surface for getting a project loaded.
    reopenPrompt?: { path: string; filename: string } | null;
    onReopenAccept?: () => void;
    onReopenDismiss?: () => void;
  }
  let {
    active,
    onActivate,
    onOpenFileClick,
    onAddTextClick,
    reopenPrompt = null,
    onReopenAccept,
    onReopenDismiss,
  }: Props = $props();
  let collapsed = $derived(!active);
  /// Add+ dropdown visibility. Closes on any item click and on the
  /// next window click outside the panel (svelte:window onclick handler
  /// below).
  let addMenuOpen = $state(false);
  function toggleAddMenu() {
    addMenuOpen = !addMenuOpen;
  }
  function closeAddMenu() {
    addMenuOpen = false;
  }
  function clickAddFile() {
    closeAddMenu();
    onOpenFileClick?.();
  }
  function clickAddText() {
    closeAddMenu();
    onAddTextClick?.();
  }
  function onWindowClick(e: MouseEvent) {
    if (!addMenuOpen) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest('.add-menu')) return;
    closeAddMenu();
  }
  function onWindowKey(e: KeyboardEvent) {
    if (e.key === 'Escape' && addMenuOpen) {
      e.preventDefault();
      closeAddMenu();
    }
  }
  /// Arrow-key nav across the "+ Add" disclosure. The disclosure has
  /// just two items so keyboard support is small but matches the
  /// pattern used by the menubar dropdowns.
  function onAddMenuKey(e: KeyboardEvent) {
    const root = e.currentTarget as HTMLElement;
    const items = Array.from(
      root.querySelectorAll<HTMLElement>('button[role="menuitem"]:not(:disabled)'),
    );
    if (items.length === 0) return;
    const active = document.activeElement as HTMLElement | null;
    const idx = active ? items.indexOf(active) : -1;
    let next: number;
    if (e.key === 'ArrowDown') next = idx < 0 ? 0 : (idx + 1) % items.length;
    else if (e.key === 'ArrowUp') next = idx <= 0 ? items.length - 1 : idx - 1;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = items.length - 1;
    else return;
    e.preventDefault();
    items[next]?.focus();
  }
  function focusFirstAddMenuItem(node: HTMLElement) {
    queueMicrotask(() => {
      const first = node.querySelector<HTMLElement>('button[role="menuitem"]:not(:disabled)');
      first?.focus();
    });
  }

  const ACI: Record<number, string> = {
    1: '#ff0000',
    2: '#ffff00',
    3: '#00ff00',
    4: '#00ffff',
    5: '#0000ff',
    6: '#ff00ff',
  };
  // ACI 7 / 256 = BYLAYER white (paper-color). Theme-tracked.
  function swatch(c: number): string {
    if (c === 7 || c === 256) return 'var(--text-strong)';
    if (c === 8) return 'var(--text-muted)';
    return ACI[c] ?? 'var(--text-faint)';
  }

  /// Usable layers across ALL imports. When several drawings share a
  /// layer name (e.g. both have "0") the counts sum; visibility toggles
  /// apply across imports.
  let usableLayers = $derived.by(() => {
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

  let allVisible = $derived(
    usableLayers.length > 0 && usableLayers.every((l) => project.data.visibleLayers.has(l.name)),
  );

  function setAllVisible(on: boolean) {
    for (const l of usableLayers) {
      const has = project.data.visibleLayers.has(l.name);
      if (has !== on) project.toggleLayer(l.name);
    }
  }

  /// Per-import transform foldout state — keyed by ImportEntry.id so
  /// adding / removing imports doesn't shift the open/closed flags.
  let openTransforms = $state<Set<number>>(new Set());
  function toggleTransform(id: number) {
    const next = new Set(openTransforms);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    openTransforms = next;
  }
</script>

<svelte:window onclick={onWindowClick} onkeydown={onWindowKey} />

<aside class="layers">
  <div class="group-head">
    <button
      class="caret-btn"
      onclick={onActivate}
      title={active ? 'Collapse layers (return to previous panel)' : 'Expand layers'}
      aria-label={active ? 'Collapse layers panel' : 'Activate layers panel'}
      >{active ? '▾' : '▸'}</button
    >
    {#if usableLayers.length > 0}
      <input
        type="checkbox"
        checked={allVisible}
        title="Show / hide every layer"
        aria-label="Toggle all layers"
        onclick={(e) => e.stopPropagation()}
        onchange={(e) => setAllVisible((e.currentTarget as HTMLInputElement).checked)}
      />
    {/if}
    {#if project.data.imports.length === 1 && usableLayers.length > 0}
      <span class="filename" title={project.data.imports[0].source.filename}>
        {project.data.imports[0].source.filename}
      </span>
      <span class="file-stats" title="Segments · layers">
        {project.data.imports[0].source.segments.length} seg · {usableLayers.length} layer{usableLayers.length ===
        1
          ? ''
          : 's'}
      </span>
    {:else if project.data.imports.length > 1}
      <span class="group-name">Drawings</span>
      <span class="group-count">{project.data.imports.length}</span>
    {:else}
      <span class="group-name">Layers</span>
      <span class="group-count">{usableLayers.length}</span>
    {/if}
    {#if onOpenFileClick || onAddTextClick}
      <div class="add-menu" class:open={addMenuOpen}>
        <button
          type="button"
          class="add-btn"
          onclick={toggleAddMenu}
          aria-haspopup="menu"
          aria-expanded={addMenuOpen}
          title="Add a layer — open a drawing or add text geometry"
        >
          + Add
        </button>
        {#if addMenuOpen}
          <div
            class="add-dropdown"
            role="menu"
            tabindex="-1"
            onmouseleave={closeAddMenu}
            onkeydown={onAddMenuKey}
            use:focusFirstAddMenuItem
          >
            {#if onOpenFileClick}
              <button
                type="button"
                role="menuitem"
                class="add-item"
                onclick={clickAddFile}
                title="Open a DXF or SVG file"
              >
                <span class="label">Open drawing file…</span>
              </button>
            {/if}
            {#if onAddTextClick}
              <button
                type="button"
                role="menuitem"
                class="add-item"
                onclick={clickAddText}
                title="Add editable text geometry"
              >
                <span class="label">Add text…</span>
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </div>
  {#if !collapsed}
    <div class="group-body">
      {#each project.data.imports as entry (entry.id)}
        {@const xfActive = !isIdentityFileTransform(entry.fileTransform)}
        {@const transformOpen = openTransforms.has(entry.id)}
        <div class="import-card">
          {#if project.data.imports.length > 1}
            <div class="import-head">
              <span class="import-filename" title={entry.source.filename}
                >{entry.source.filename}</span
              >
              <span class="import-stats"
                >{entry.source.segments.length} seg · {entry.source.layers.length} layer</span
              >
              <button
                type="button"
                class="import-remove"
                onclick={() => project.removeImport(entry.id)}
                title="Remove this drawing from the project"
                aria-label={`Remove ${entry.source.filename}`}>×</button
              >
            </div>
          {/if}
          <div class="xform" class:xform-active={xfActive}>
            <div class="xform-head-row">
              <button
                type="button"
                class="xform-head"
                onclick={() => toggleTransform(entry.id)}
                aria-expanded={transformOpen}
                title="Layout convenience: translate / rotate / scale / mirror this drawing. Pivot for rotate / scale / mirror is the drawing's original bbox center."
              >
                <span class="xform-caret">{transformOpen ? '▾' : '▸'}</span>
                <span class="xform-label">File transform</span>
                {#if xfActive}
                  <span class="xform-dot" aria-label="Transform is active"></span>
                {/if}
              </button>
              {#if transformOpen && xfActive}
                <button
                  type="button"
                  class="xform-reset"
                  onclick={() => project.resetFileTransformForImport(entry.id)}
                  title="Reset to identity (no transform)"
                >
                  Reset
                </button>
              {/if}
            </div>
            {#if transformOpen}
              <div class="xform-body">
                <label title="Move this drawing by this many mm in X. Positive = right.">
                  <span>X</span>
                  <span class="xform-field">
                    <input
                      type="number"
                      step="1"
                      value={entry.fileTransform.translate.x}
                      oninput={(e) => {
                        const v = (e.target as HTMLInputElement).valueAsNumber;
                        if (Number.isFinite(v))
                          project.patchFileTransformForImport(entry.id, { translate: { x: v } });
                      }}
                    />
                    <span class="xform-unit">mm</span>
                  </span>
                </label>
                <label title="Move this drawing by this many mm in Y. Positive = up.">
                  <span>Y</span>
                  <span class="xform-field">
                    <input
                      type="number"
                      step="1"
                      value={entry.fileTransform.translate.y}
                      oninput={(e) => {
                        const v = (e.target as HTMLInputElement).valueAsNumber;
                        if (Number.isFinite(v))
                          project.patchFileTransformForImport(entry.id, { translate: { y: v } });
                      }}
                    />
                    <span class="xform-unit">mm</span>
                  </span>
                </label>
                <label
                  title="Rotate around the drawing's original bbox center. Positive = counter-clockwise."
                >
                  <span>Rotate</span>
                  <span class="xform-field">
                    <input
                      type="number"
                      step="5"
                      value={entry.fileTransform.rotateDeg}
                      oninput={(e) => {
                        const v = (e.target as HTMLInputElement).valueAsNumber;
                        if (Number.isFinite(v))
                          project.patchFileTransformForImport(entry.id, { rotateDeg: v });
                      }}
                    />
                    <span class="xform-unit">°</span>
                  </span>
                </label>
                <label
                  title="Uniform scale around the bbox center. 1 = no scale, 2 = twice as big, 0.5 = half size."
                >
                  <span>Scale</span>
                  <span class="xform-field">
                    <input
                      type="number"
                      step="0.1"
                      min="0.001"
                      value={entry.fileTransform.scale}
                      oninput={(e) => {
                        const v = (e.target as HTMLInputElement).valueAsNumber;
                        if (Number.isFinite(v) && v > 0)
                          project.patchFileTransformForImport(entry.id, { scale: v });
                      }}
                    />
                    <span class="xform-unit">×</span>
                  </span>
                </label>
                <label
                  class="xform-check"
                  title="Mirror across the horizontal axis through the bbox center (flip top↔bottom). Negates arc bulges so curvature stays valid."
                >
                  <input
                    type="checkbox"
                    checked={entry.fileTransform.mirrorX}
                    onchange={(e) =>
                      project.patchFileTransformForImport(entry.id, {
                        mirrorX: (e.currentTarget as HTMLInputElement).checked,
                      })}
                  />
                  Mirror X (flip vertical)
                </label>
                <label
                  class="xform-check"
                  title="Mirror across the vertical axis through the bbox center (flip left↔right). Negates arc bulges."
                >
                  <input
                    type="checkbox"
                    checked={entry.fileTransform.mirrorY}
                    onchange={(e) =>
                      project.patchFileTransformForImport(entry.id, {
                        mirrorY: (e.currentTarget as HTMLInputElement).checked,
                      })}
                  />
                  Mirror Y (flip horizontal)
                </label>
              </div>
            {/if}
          </div>
        </div>
      {/each}
      {#if usableLayers.length > 0}
        <ul>
          {#each usableLayers as layer (layer.name)}
            <li class="layer-row">
              <label class="layer-label">
                <input
                  type="checkbox"
                  checked={project.data.visibleLayers.has(layer.name)}
                  onchange={() => project.toggleLayer(layer.name)}
                  title="Active = included in pipeline + visible on canvas. Uncheck to deactivate."
                />
                <span class="swatch" style:background={swatch(layer.color)}></span>
                <span class="name">{layer.name}</span>
                <span class="count">{layer.segment_count}</span>
              </label>
              <button
                type="button"
                class="del-btn"
                onclick={() => project.removeImportedLayer(layer.name)}
                title="Delete this layer (drops every segment on it)"
                aria-label={`Delete layer ${layer.name}`}
              >
                ×
              </button>
            </li>
          {/each}
        </ul>
      {:else}
        <div class="empty-card">
          <p class="empty-title">No drawing yet</p>
          <p class="empty-sub">Import a DXF or SVG to see its layers here.</p>
          {#if onOpenFileClick}
            <button class="primary-cta" type="button" onclick={onOpenFileClick}>
              + Open file
            </button>
          {/if}
          {#if reopenPrompt}
            <div class="reopen">
              <span class="reopen-text">
                Reopen <strong>{reopenPrompt.filename}</strong>?
              </span>
              <div class="reopen-actions">
                <button class="reopen-accept" type="button" onclick={() => onReopenAccept?.()}>
                  Reopen
                </button>
                <button class="reopen-dismiss" type="button" onclick={() => onReopenDismiss?.()}>
                  Dismiss
                </button>
              </div>
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</aside>

<style>
  .layers {
    width: 100%;
    background: var(--bg-panel);
    color: var(--text);
    border-left: 1px solid var(--border);
    padding: 0.4rem 0.6rem 0.5rem;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }
  /* Header mirrors OperationsList's group-head for visual parity.
     Five-column layout: caret · all-visible toggle · filename / label ·
     count / file-stats · Add+ menu. */
  /* Base shape lives in app.css `.group-head`; only the per-panel
     grid (5 columns) is local. */
  .group-head {
    grid-template-columns: auto auto minmax(0, 1fr) auto auto;
  }
  .add-menu {
    position: relative;
  }
  .add-menu.open {
    /* When open, lift the entire `.add-menu` into its own stacking
       context so the absolutely-positioned dropdown can paint above
       later sidebar rows (Text panel, Operations, etc.) instead of
       being painted under by their DOM-order. */
    z-index: var(--z-dropdown);
  }
  .add-btn {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    color: var(--text);
    border-radius: 3px;
    padding: 0 0.4rem;
    font-size: 0.7rem;
    cursor: pointer;
    line-height: 1.2;
    /* Don't push the row taller than the Stock / Text headers. */
    min-height: 0;
  }
  .add-btn:hover {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .add-menu.open .add-btn {
    background: var(--bg-elevated);
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .add-dropdown {
    position: absolute;
    top: 100%;
    right: 0;
    min-width: 200px;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px var(--shadow-modal);
    padding: 0.2rem;
    z-index: var(--z-dropdown);
    display: flex;
    flex-direction: column;
    gap: 0.05rem;
    margin-top: 4px;
  }
  .add-item {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: transparent;
    color: var(--text);
    border: 0;
    padding: 0.3rem 0.55rem;
    font-size: 0.78rem;
    border-radius: 3px;
    cursor: pointer;
    text-align: left;
    width: 100%;
  }
  .add-item:hover {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .add-item .label {
    white-space: nowrap;
  }
  /* `.caret-btn` shape is in app.css. */
  .group-name {
    color: var(--text-strong);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .group-count {
    color: var(--text-muted);
    font-size: 0.72rem;
    padding: 0 0.3rem;
    background: var(--bg);
    border-radius: 10px;
    line-height: 1.4;
  }
  .filename {
    color: var(--text-strong);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .file-stats {
    color: var(--text-muted);
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .group-body {
    margin: 0.2rem 0 0 0.5rem;
    padding-left: 0.3rem;
    border-left: 2px solid color-mix(in srgb, var(--accent) 30%, transparent);
    /* Sidebar accordion (App.svelte) already gives the active layers
       host `minmax(0, 1fr)` and clips overflow on its host wrapper.
       The prior `max-height: 28vh` capped this region INSIDE that
       1fr row and created a nested second scrollbar that wasted
       vertical space on tall windows. Let the host own scrolling. */
    overflow-y: auto;
  }
  /* Per-import card. When the project has multiple drawings the head
     row identifies each one and offers a remove button; single-import
     projects skip the head row entirely. */
  .import-card {
    margin: 0.1rem 0 0.4rem;
  }
  .import-head {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    align-items: center;
    gap: 0.4rem;
    padding: 0.18rem 0.2rem 0.15rem;
    font-size: 0.72rem;
  }
  .import-filename {
    color: var(--text-strong);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .import-stats {
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .import-remove {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    font-size: 1rem;
    line-height: 1;
    padding: 0 0.3rem;
    cursor: pointer;
  }
  .import-remove:hover {
    color: var(--error);
  }
  /* File-transform foldout. Lives inside each import-card. */
  .xform {
    margin: 0.05rem 0 0.3rem;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: color-mix(in srgb, var(--bg-elevated) 50%, var(--bg-panel));
  }
  .xform-active {
    border-color: color-mix(in srgb, var(--accent) 50%, var(--border));
    background: color-mix(in srgb, var(--accent) 6%, var(--bg-panel));
  }
  .xform-head-row {
    display: flex;
    align-items: center;
    width: 100%;
  }
  .xform-head {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    flex: 1;
    background: transparent;
    border: 0;
    color: var(--text);
    padding: 0.18rem 0.35rem;
    cursor: pointer;
    text-align: left;
    font-size: 0.74rem;
  }
  .xform-caret {
    color: var(--text-muted);
    font-size: 0.85rem;
    line-height: 1;
  }
  .xform-label {
    flex: 1;
    color: var(--text-strong);
    font-weight: 600;
  }
  .xform-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
  }
  .xform-reset {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    color: var(--text);
    border-radius: 3px;
    padding: 0.05rem 0.4rem;
    font-size: 0.7rem;
    cursor: pointer;
  }
  .xform-reset:hover {
    border-color: var(--accent);
  }
  .xform-body {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.25rem 0.4rem;
    padding: 0.25rem 0.4rem 0.4rem;
  }
  .xform-body label {
    display: grid;
    grid-template-columns: 4rem 1fr;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.74rem;
    cursor: default;
  }
  .xform-body label.xform-check {
    grid-column: 1 / -1;
    grid-template-columns: auto 1fr;
    cursor: pointer;
  }
  .xform-field {
    display: inline-flex;
    align-items: center;
    gap: 0.2rem;
    min-width: 0;
  }
  .xform-field input[type='number'] {
    flex: 1;
    min-width: 0;
    width: 100%;
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.1rem 0.3rem;
    font-size: 0.74rem;
    font-family: inherit;
  }
  .xform-unit {
    color: var(--text-muted);
    font-size: 0.7rem;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    margin: 0.18rem 0;
  }
  li.layer-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: 0.2rem;
  }
  label,
  .layer-label {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.82rem;
    cursor: pointer;
    min-width: 0;
  }
  .del-btn {
    /* WCAG-sized hit target (≥24×24) — was `padding: 0 0.3rem` which
       gave ~16-20 px depending on the glyph. Min-width / -height enforce
       the floor; centering keeps the × visually unchanged. */
    background: transparent;
    border: 0;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 1rem;
    line-height: 1;
    padding: 0;
    min-width: 24px;
    min-height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 3px;
  }
  .del-btn:hover {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 12%, transparent);
  }
  input[type='checkbox'] {
    accent-color: var(--accent);
  }
  .swatch {
    width: 10px;
    height: 10px;
    border-radius: 2px;
    display: inline-block;
    border: 1px solid var(--border);
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-variant-numeric: tabular-nums;
    color: var(--text-faint);
    font-size: 0.72rem;
  }
  /* Empty-state card matches OperationsList's pattern for visual
     consistency across the sidebar (audit 0ki0). */
  .empty-card {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0.35rem;
    padding: 0.6rem;
    margin: 0.4rem 0;
    border: 1px dashed var(--border);
    border-radius: 5px;
    background: color-mix(in srgb, var(--accent) 4%, var(--bg-panel));
    text-align: center;
  }
  .empty-title {
    margin: 0;
    color: var(--text-strong);
    font-size: 0.82rem;
    font-weight: 600;
  }
  .empty-sub {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.72rem;
    line-height: 1.3;
  }
  .primary-cta {
    margin-top: 0.3rem;
    background: var(--accent);
    color: #fff;
    border: 0;
    padding: 0.35rem 0.7rem;
    border-radius: 4px;
    font-size: 0.82rem;
    font-weight: 600;
    cursor: pointer;
  }
  .primary-cta:hover {
    background: var(--accent-strong);
  }
  /* Startup reopen-last-project affordance. Lives below the Open file
     CTA so the no-drawing state surfaces both ways to get a project
     loaded in one place. */
  .reopen {
    margin-top: 0.45rem;
    padding-top: 0.45rem;
    border-top: 1px dashed var(--border);
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.72rem;
    color: var(--text-muted);
    text-align: left;
  }
  .reopen-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .reopen-text strong {
    color: var(--text-strong);
    font-weight: 600;
  }
  .reopen-actions {
    display: flex;
    gap: 0.3rem;
  }
  .reopen-accept,
  .reopen-dismiss {
    flex: 1;
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text);
    padding: 0.2rem 0.5rem;
    border-radius: 3px;
    cursor: pointer;
    font-size: 0.74rem;
  }
  .reopen-accept {
    background: var(--accent);
    color: #fff;
    border-color: var(--accent);
  }
  .reopen-accept:hover {
    background: var(--accent-strong);
  }
  .reopen-dismiss:hover {
    background: var(--bg-input);
  }
</style>
