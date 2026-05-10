<script lang="ts">
  import { project, type Fixture, type FixtureKind } from '../state/project.svelte';
  import { _ } from 'svelte-i18n';

  let visible = $derived(project.stock.visible);
  function patch(p: Partial<typeof project.stock>) {
    project.setStock(p);
  }

  let showAddMenu = $state(false);
  let fixturesExpanded = $state(true);

  /// Add a fixture using sensible defaults for the chosen shape. Centered
  /// on the imported geometry's bbox; falls back to (0, 0) if nothing
  /// imported yet.
  function addFixtureOf(kind: 'box' | 'cylinder' | 'polygon') {
    showAddMenu = false;
    const data = project.imported;
    const cx = data ? (data.bbox.min_x + data.bbox.max_x) * 0.5 : 0;
    const cy = data ? (data.bbox.min_y + data.bbox.max_y) * 0.5 : 0;
    let fkind: FixtureKind;
    if (kind === 'box') {
      fkind = { shape: 'box', width: 30, depth: 50 };
    } else if (kind === 'cylinder') {
      fkind = { shape: 'cylinder', radius: 6 };
    } else {
      fkind = {
        shape: 'polygon',
        vertices: [
          [-10, -10],
          [10, -10],
          [10, 10],
          [-10, 10],
        ],
      };
    }
    project.addFixture(fkind, [cx, cy], 0, 12);
  }

  function patchFixture(id: number, p: Partial<Fixture>) {
    project.updateFixture(id, p);
  }

  function patchKind(id: number, p: Partial<FixtureKind>) {
    const f = project.fixtures.find((x) => x.id === id);
    if (!f) return;
    const merged = { ...f.kind, ...p } as FixtureKind;
    project.updateFixture(id, { kind: merged });
  }
</script>

<div class="stock">
  <label class="row toggle">
    <input
      type="checkbox"
      checked={visible}
      onchange={(e) => patch({ visible: (e.currentTarget as HTMLInputElement).checked })}
    />
    <span>{$_('stock.title')}</span>
  </label>
  {#if visible}
    <div class="row">
      <span class="lbl">{$_('stock.mode')}</span>
      <select
        value={project.stock.mode}
        onchange={(e) =>
          patch({ mode: ((e.target as HTMLSelectElement).value === 'manual'
            ? 'manual'
            : 'auto') })}
      >
        <option value="auto">{$_('stock.auto')}</option>
        <option value="manual">{$_('stock.manual')}</option>
      </select>
    </div>
    {#if project.stock.mode === 'auto'}
      <div class="row">
        <span class="lbl">{$_('stock.margin')}</span>
        <input
          type="number"
          step="0.1"
          value={project.stock.margin}
          onchange={(e) => patch({ margin: parseFloat((e.target as HTMLInputElement).value) })}
        />
      </div>
    {:else}
      <div class="row">
        <span class="lbl">X (mm)</span>
        <input
          type="number"
          step="0.1"
          value={project.stock.customX}
          onchange={(e) => patch({ customX: parseFloat((e.target as HTMLInputElement).value) })}
        />
      </div>
      <div class="row">
        <span class="lbl">Y (mm)</span>
        <input
          type="number"
          step="0.1"
          value={project.stock.customY}
          onchange={(e) => patch({ customY: parseFloat((e.target as HTMLInputElement).value) })}
        />
      </div>
      <div class="row">
        <span class="lbl">{$_('stock.thickness')}</span>
        <input
          type="number"
          step="0.1"
          value={project.stock.thickness}
          onchange={(e) => patch({ thickness: parseFloat((e.target as HTMLInputElement).value) })}
        />
      </div>
    {/if}
  {/if}
</div>

<div class="fixtures">
  <button class="row toggle" type="button" onclick={() => (fixturesExpanded = !fixturesExpanded)}>
    <span class="caret">{fixturesExpanded ? '▾' : '▸'}</span>
    <span>Fixtures ({project.fixtures.length})</span>
  </button>
  {#if fixturesExpanded}
    <div class="add-row">
      <button type="button" onclick={() => (showAddMenu = !showAddMenu)} class="add-btn">
        + Add Fixture
      </button>
      {#if showAddMenu}
        <div class="add-menu">
          <button type="button" onclick={() => addFixtureOf('box')}>Box</button>
          <button type="button" onclick={() => addFixtureOf('cylinder')}>Cylinder</button>
          <button type="button" onclick={() => addFixtureOf('polygon')}>Polygon</button>
        </div>
      {/if}
    </div>
    {#each project.fixtures as f (f.id)}
      <div
        class="fix-row"
        class:selected={project.selectedFixtureId === f.id}
        onclick={() => project.selectFixture(f.id)}
        onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') project.selectFixture(f.id); }}
        role="button"
        tabindex="0"
      >
        <input
          type="text"
          class="fix-name"
          value={f.name}
          onclick={(e) => e.stopPropagation()}
          onchange={(e) => patchFixture(f.id, { name: (e.target as HTMLInputElement).value })}
        />
        <span class="fix-shape">{f.kind.shape}</span>
        <button
          type="button"
          class="del-btn"
          aria-label="Delete fixture"
          onclick={(e) => { e.stopPropagation(); project.removeFixture(f.id); }}
        >×</button>
      </div>
      {#if project.selectedFixtureId === f.id}
        <div class="fix-edit">
          <div class="row">
            <span class="lbl">X (mm)</span>
            <input
              type="number"
              step="0.1"
              value={f.origin[0]}
              onchange={(e) =>
                patchFixture(f.id, {
                  origin: [parseFloat((e.target as HTMLInputElement).value), f.origin[1]],
                })}
            />
          </div>
          <div class="row">
            <span class="lbl">Y (mm)</span>
            <input
              type="number"
              step="0.1"
              value={f.origin[1]}
              onchange={(e) =>
                patchFixture(f.id, {
                  origin: [f.origin[0], parseFloat((e.target as HTMLInputElement).value)],
                })}
            />
          </div>
          <div class="row">
            <span class="lbl">Z bot</span>
            <input
              type="number"
              step="0.1"
              value={f.z_bottom}
              onchange={(e) => patchFixture(f.id, { z_bottom: parseFloat((e.target as HTMLInputElement).value) })}
            />
          </div>
          <div class="row">
            <span class="lbl">Z top</span>
            <input
              type="number"
              step="0.1"
              value={f.z_top}
              onchange={(e) => patchFixture(f.id, { z_top: parseFloat((e.target as HTMLInputElement).value) })}
            />
          </div>
          {#if f.kind.shape === 'box'}
            <div class="row">
              <span class="lbl">W (mm)</span>
              <input
                type="number"
                step="0.1"
                value={f.kind.width}
                onchange={(e) => patchKind(f.id, { width: parseFloat((e.target as HTMLInputElement).value) })}
              />
            </div>
            <div class="row">
              <span class="lbl">D (mm)</span>
              <input
                type="number"
                step="0.1"
                value={f.kind.depth}
                onchange={(e) => patchKind(f.id, { depth: parseFloat((e.target as HTMLInputElement).value) })}
              />
            </div>
          {:else if f.kind.shape === 'cylinder'}
            <div class="row">
              <span class="lbl">R (mm)</span>
              <input
                type="number"
                step="0.1"
                value={f.kind.radius}
                onchange={(e) => patchKind(f.id, { radius: parseFloat((e.target as HTMLInputElement).value) })}
              />
            </div>
          {:else}
            <div class="poly-note">{f.kind.vertices.length} vertices</div>
          {/if}
        </div>
      {/if}
    {/each}
  {/if}
</div>

<style>
  .stock,
  .fixtures {
    display: grid;
    gap: 0.25rem;
    padding: 0.4rem 0;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0.5rem;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 4.5rem) minmax(0, 1fr);
    gap: 0.4rem;
    align-items: center;
  }
  .row.toggle {
    grid-template-columns: auto auto;
    justify-content: start;
    gap: 0.4rem;
    background: transparent;
    border: 0;
    color: inherit;
    cursor: pointer;
    text-align: left;
    padding: 0;
    font: inherit;
  }
  .lbl {
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  select,
  input[type='number'],
  input[type='text'] {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.35rem;
    font-size: 0.78rem;
    min-width: 0;
  }
  input[type='checkbox'] {
    accent-color: var(--accent);
  }
  .caret {
    font-size: 0.7rem;
    color: var(--text-muted);
  }
  .add-row {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .add-btn {
    background: var(--bg-input);
    border: 1px solid var(--border);
    color: var(--text);
    border-radius: 3px;
    padding: 0.25rem 0.5rem;
    font-size: 0.78rem;
    cursor: pointer;
    text-align: left;
  }
  .add-menu {
    display: grid;
    gap: 0.2rem;
    padding: 0.25rem;
    background: var(--bg-elevated, var(--bg-input));
    border: 1px solid var(--border);
    border-radius: 3px;
  }
  .add-menu button {
    background: transparent;
    border: 0;
    color: inherit;
    text-align: left;
    cursor: pointer;
    font-size: 0.78rem;
    padding: 0.2rem 0.4rem;
    border-radius: 2px;
  }
  .add-menu button:hover {
    background: var(--bg-input);
  }
  .fix-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    gap: 0.3rem;
    align-items: center;
    padding: 0.18rem 0.25rem;
    border-radius: 3px;
    cursor: pointer;
  }
  .fix-row.selected {
    background: color-mix(in srgb, var(--accent) 25%, transparent);
  }
  .fix-row:hover {
    background: var(--bg-input);
  }
  .fix-name {
    border: 0 !important;
    background: transparent !important;
    padding: 0 !important;
  }
  .fix-shape {
    font-size: 0.7rem;
    color: var(--text-muted);
  }
  .del-btn {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    cursor: pointer;
    padding: 0 0.3rem;
    font-size: 1rem;
    line-height: 1;
  }
  .del-btn:hover {
    color: var(--text-strong, #f0f0f0);
  }
  .fix-edit {
    display: grid;
    gap: 0.2rem;
    padding: 0.25rem 0.5rem;
    border-left: 2px solid var(--accent);
    margin-left: 0.25rem;
  }
  .poly-note {
    font-size: 0.72rem;
    color: var(--text-muted);
    padding: 0.2rem 0;
  }
</style>
