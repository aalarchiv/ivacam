<script lang="ts">
  /// Settings dialog. Per-installation user preferences (theme, language,
  /// cutting-preview defaults, performance caps). Persisted to localStorage
  /// under `wiac.settings`; not part of .vc-project. Mirrors the modal
  /// style used by MachineDialog / ToolLibraryDialog.
  ///
  /// Unlike Machine / Tools we commit changes live: theme switching needs
  /// to apply immediately, and there's no "Cancel" button — Escape or the
  /// close button just dismisses the dialog.
  import { project, type AppSettings } from '../state/project.svelte';
  import Modal from './Modal.svelte';

  interface Props {
    open: boolean;
    onClose: () => void;
  }
  let { open, onClose }: Props = $props();

  function update<K extends keyof AppSettings>(key: K, value: AppSettings[K]) {
    project.updateSettings({ [key]: value } as Partial<AppSettings>);
  }

  // Coerce a number input: keep current value if the user typed garbage.
  function toNumber(raw: string, fallback: number, min?: number, max?: number): number {
    const n = Number.parseFloat(raw);
    if (!Number.isFinite(n)) return fallback;
    let v = n;
    if (typeof min === 'number' && v < min) v = min;
    if (typeof max === 'number' && v > max) v = max;
    return v;
  }

  /// WAI-ARIA radiogroup arrow-key nav for the Theme segmented buttons.
  /// ArrowLeft/Up moves to previous, ArrowRight/Down to next, Home/End
  /// jump to ends. Selection moves with focus per the radiogroup pattern.
  const THEMES: Array<'auto' | 'light' | 'dark'> = ['auto', 'light', 'dark'];
  function onThemeKey(e: KeyboardEvent) {
    const cur = THEMES.indexOf(project.settings.theme as (typeof THEMES)[number]);
    let next = cur;
    if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') next = (cur - 1 + THEMES.length) % THEMES.length;
    else if (e.key === 'ArrowRight' || e.key === 'ArrowDown') next = (cur + 1) % THEMES.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = THEMES.length - 1;
    else return;
    e.preventDefault();
    update('theme', THEMES[next]);
    queueMicrotask(() => {
      (e.currentTarget as HTMLElement | null)
        ?.querySelector<HTMLElement>(`[role="radio"][aria-checked="true"]`)
        ?.focus();
    });
  }
</script>

{#if open}
  <Modal
    {onClose}
    persistKey="settings"
    width="min(540px, 95vw)"
    draggable
    resizable
    ariaLabelledBy="settings-title"
  >
    <header>
      <h2 id="settings-title">Settings</h2>
      <button class="dlg-close" onclick={onClose} aria-label="Close">×</button>
    </header>

    <div class="body">
      <section>
        <h3>Appearance</h3>
        <div class="grid">
          <label
            >Theme
            <div
              class="seg"
              role="radiogroup"
              aria-label="Theme"
              tabindex="-1"
              onkeydown={onThemeKey}
            >
              <button
                role="radio"
                aria-checked={project.settings.theme === 'auto'}
                tabindex={project.settings.theme === 'auto' ? 0 : -1}
                class:active={project.settings.theme === 'auto'}
                onclick={() => update('theme', 'auto')}
                type="button">Auto</button
              >
              <button
                role="radio"
                aria-checked={project.settings.theme === 'light'}
                tabindex={project.settings.theme === 'light' ? 0 : -1}
                class:active={project.settings.theme === 'light'}
                onclick={() => update('theme', 'light')}
                type="button">Light</button
              >
              <button
                role="radio"
                aria-checked={project.settings.theme === 'dark'}
                tabindex={project.settings.theme === 'dark' ? 0 : -1}
                class:active={project.settings.theme === 'dark'}
                onclick={() => update('theme', 'dark')}
                type="button">Dark</button
              >
            </div>
          </label>

        </div>
      </section>

      <section>
        <h3>View</h3>
        <div class="grid">
          <label class="check">
            <input
              type="checkbox"
              checked={project.settings.showStockBox}
              onchange={(e) =>
                update('showStockBox', (e.currentTarget as HTMLInputElement).checked)}
            />
            <span>Show stock outline in 3D</span>
          </label>
        </div>
      </section>

      <section>
        <h3>Cutting preview</h3>
        <p class="hint">
          How the 3D viewport renders the simulated stock once cutting preview lands. These values
          are stored now so the eventual renderer picks them up automatically.
        </p>
        <div class="grid">
          <label
            >Default mode
            <select
              value={project.settings.previewMode}
              onchange={(e) =>
                update(
                  'previewMode',
                  (e.currentTarget as HTMLSelectElement).value as AppSettings['previewMode'],
                )}
            >
              <option value="wireframe">Wireframe</option>
              <option value="solid">Solid</option>
              <option value="both">Both</option>
            </select>
          </label>

          <label
            >Solid color
            <div class="color">
              <input
                type="color"
                value={project.settings.solidColor}
                oninput={(e) => update('solidColor', (e.currentTarget as HTMLInputElement).value)}
              />
              <input
                type="text"
                class="hex"
                value={project.settings.solidColor}
                oninput={(e) => update('solidColor', (e.currentTarget as HTMLInputElement).value)}
              />
            </div>
          </label>

          <label
            >Solid opacity
            <div class="slider-row">
              <input
                type="range"
                min="0.1"
                max="1"
                step="0.05"
                value={project.settings.solidOpacity}
                onchange={(e) =>
                  update(
                    'solidOpacity',
                    toNumber(
                      (e.currentTarget as HTMLInputElement).value,
                      project.settings.solidOpacity,
                      0.1,
                      1,
                    ),
                  )}
              />
              <span class="num">{project.settings.solidOpacity.toFixed(2)}</span>
            </div>
          </label>

          <label
            >Edge color
            <div class="color">
              <input
                type="color"
                value={project.settings.edgeColor}
                oninput={(e) => update('edgeColor', (e.currentTarget as HTMLInputElement).value)}
              />
              <input
                type="text"
                class="hex"
                value={project.settings.edgeColor}
                oninput={(e) => update('edgeColor', (e.currentTarget as HTMLInputElement).value)}
              />
            </div>
          </label>

          <label
            >Edge opacity
            <div class="slider-row">
              <input
                type="range"
                min="0"
                max="1"
                step="0.05"
                value={project.settings.edgeOpacity}
                onchange={(e) =>
                  update(
                    'edgeOpacity',
                    toNumber(
                      (e.currentTarget as HTMLInputElement).value,
                      project.settings.edgeOpacity,
                      0,
                      1,
                    ),
                  )}
              />
              <span class="num">{project.settings.edgeOpacity.toFixed(2)}</span>
            </div>
          </label>

          <label
            >Cell resolution
            <select
              value={project.settings.cellResolutionMode}
              onchange={(e) =>
                update(
                  'cellResolutionMode',
                  (e.currentTarget as HTMLSelectElement).value as AppSettings['cellResolutionMode'],
                )}
            >
              <option value="auto">Auto (tool diameter / 15)</option>
              <option value="manual">Manual</option>
            </select>
          </label>

          {#if project.settings.cellResolutionMode === 'manual'}
            <label
              >Cell size (mm)
              <input
                type="number"
                min="0.01"
                max="5"
                step="0.05"
                title="Voxel resolution for the sim heightmap. Below 0.05 mm explodes RAM; above ~2 mm loses tab + sliver detail. Cap is 5 mm to keep the sim sane."
                value={project.settings.cellResolutionMm}
                onchange={(e) =>
                  update(
                    'cellResolutionMm',
                    toNumber(
                      (e.currentTarget as HTMLInputElement).value,
                      project.settings.cellResolutionMm,
                      0.01,
                      5,
                    ),
                  )}
              />
            </label>
          {/if}
        </div>
      </section>

      <section>
        <h3>Performance</h3>
        <div class="grid">
          <label class="check">
            <input
              type="checkbox"
              checked={project.settings.solidPreviewByDefault}
              onchange={(e) =>
                update('solidPreviewByDefault', (e.currentTarget as HTMLInputElement).checked)}
            />
            <span>Enable solid preview by default</span>
          </label>

          <label
            >Max simulation cells
            <input
              type="number"
              min="100000"
              step="100000"
              value={project.settings.maxSimulationCells}
              onchange={(e) =>
                update(
                  'maxSimulationCells',
                  Math.round(
                    toNumber(
                      (e.currentTarget as HTMLInputElement).value,
                      project.settings.maxSimulationCells,
                      100_000,
                    ),
                  ),
                )}
            />
          </label>

          <label
            >Max render triangles
            <input
              type="number"
              min="100000"
              step="100000"
              value={project.settings.maxRenderTriangles}
              onchange={(e) =>
                update(
                  'maxRenderTriangles',
                  Math.round(
                    toNumber(
                      (e.currentTarget as HTMLInputElement).value,
                      project.settings.maxRenderTriangles,
                      100_000,
                    ),
                  ),
                )}
            />
          </label>
        </div>
        <p class="hint">
          <b>Max simulation cells</b> caps the WASM heightmap grid (and so the simulation accuracy).
          <b>Max render triangles</b> caps the 3D-sim preview's mesh size: the renderer automatically
          drops to a coarser LOD level when zoomed out or when simulation cells exceed the budget.
          Simulation accuracy is preserved; only the rendered mesh degrades. Stepped voxel mesh ≈
          6 triangles per cell.
        </p>
      </section>

      <section>
        <h3>Sim safety</h3>
        <div class="grid">
          <label class="check">
            <input
              type="checkbox"
              checked={project.settings.blockOnCriticalSimWarnings}
              onchange={(e) =>
                update('blockOnCriticalSimWarnings', (e.currentTarget as HTMLInputElement).checked)}
            />
            <span>Block G-code generation on critical sim warnings</span>
          </label>
          <label class="check">
            <input
              type="checkbox"
              checked={project.settings.autoRunSimOnSave}
              onchange={(e) =>
                update('autoRunSimOnSave', (e.currentTarget as HTMLInputElement).checked)}
            />
            <span>Auto-run sim on every project save</span>
          </label>
          <label class="check" title="Debounces ~1.5 s after the last edit, then runs Generate G-code. Off by default so power users on big projects keep manual control.">
            <input
              type="checkbox"
              checked={project.settings.autoRegenerate}
              onchange={(e) =>
                update('autoRegenerate', (e.currentTarget as HTMLInputElement).checked)}
            />
            <span>Auto-regenerate G-code after edits</span>
          </label>
        </div>
        <p class="hint">
          "Critical" warnings are collisions and rapids cutting through material. With the block
          enabled, fixing them — or disabling the safety check — is required before downloading
          G-code.
        </p>
      </section>

      <section>
        <h3>Source files</h3>
        <div class="grid">
          <label class="check">
            <input
              type="checkbox"
              checked={project.settings.autoReloadSources}
              onchange={(e) =>
                update('autoReloadSources', (e.currentTarget as HTMLInputElement).checked)}
            />
            <span>Auto-reload imported DXF / SVG when changed externally</span>
          </label>
        </div>
        <p class="hint">
          Desktop only. Watches the source file backing the current import and re-runs it when the
          CAD app saves a new version (one undoable step). Disable to get a "Reload?" toast instead.
          Network and OneDrive-synced paths can drop events silently — manually re-import if a save
          doesn't show up within a few seconds.
        </p>
      </section>

      <section>
        <h3>Snap to</h3>
        <div class="grid">
          {#each [
            { key: 'endpoint', label: 'Endpoint', help: 'Snap to segment endpoints.' },
            { key: 'midpoint', label: 'Midpoint', help: 'Snap to the midpoint of each segment.' },
            { key: 'intersection', label: 'Intersection', help: 'Snap to line / arc crossings.' },
            { key: 'center', label: 'Center', help: 'Snap to circle / arc centers.' },
            { key: 'grid', label: 'Grid', help: 'Snap to integer multiples of the grid step below.' },
          ] as o (o.key)}
            <label class="check" title={o.help}>
              <input
                type="checkbox"
                checked={!!project.settings.osnap?.[o.key as 'endpoint' | 'midpoint' | 'intersection' | 'center' | 'grid']}
                onchange={(e) =>
                  update('osnap', {
                    ...project.settings.osnap,
                    [o.key]: (e.currentTarget as HTMLInputElement).checked,
                  })}
              />
              <span>{o.label}</span>
            </label>
          {/each}
          <label
            title="Grid step (mm) when 'Grid' snap is on. Cursor latches to integer multiples of this offset from the project origin."
            >Grid step
            <input
              type="number"
              min="0.1"
              step="0.1"
              value={project.settings.osnap?.gridStepMm ?? 5}
              onchange={(e) =>
                update('osnap', {
                  ...project.settings.osnap,
                  gridStepMm: Math.max(
                    0.1,
                    toNumber(
                      (e.currentTarget as HTMLInputElement).value,
                      project.settings.osnap?.gridStepMm ?? 5,
                      0.1,
                    ),
                  ),
                })}
            />
          </label>
        </div>
        <p class="hint">
          Object snap on the 2D canvas. Endpoint / midpoint / intersection / center latch the cursor
          to existing geometry features; Grid latches to abstract grid spots independent of the
          drawing.
        </p>
      </section>
    </div>

    <footer>
      <button class="btn-primary" onclick={onClose} type="button">Done</button>
    </footer>
  </Modal>
{/if}

<style>
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.7rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
  }
  h2 {
    font-size: 0.95rem;
    margin: 0;
    color: var(--text-strong);
  }
  .body {
    padding: 0.7rem 0.9rem;
    overflow: auto;
  }
  section {
    margin-bottom: 1rem;
  }
  section:last-child {
    margin-bottom: 0;
  }
  h3 {
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
    margin: 0 0 0.5rem;
    padding-bottom: 0.25rem;
    border-bottom: 1px solid var(--border);
  }
  .hint {
    font-size: 0.72rem;
    color: var(--text-faint);
    margin: 0 0 0.5rem;
    line-height: 1.35;
  }
  .grid {
    display: grid;
    gap: 0.5rem;
  }
  label {
    display: grid;
    grid-template-columns: minmax(0, 11rem) minmax(0, 1fr);
    align-items: center;
    gap: 0.6rem;
    font-size: 0.8rem;
  }
  label.check {
    grid-template-columns: auto 1fr;
    gap: 0.4rem;
  }
  input[type='number'],
  input[type='text'],
  select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.2rem 0.4rem;
    font-size: 0.8rem;
    min-width: 0;
  }
  input[type='checkbox'] {
    accent-color: var(--accent);
  }
  input[type='range'] {
    accent-color: var(--accent);
    flex: 1;
    min-width: 0;
  }
  input[type='color'] {
    width: 2.2rem;
    height: 1.6rem;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: 3px;
    cursor: pointer;
    padding: 0;
  }
  .color {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    min-width: 0;
  }
  .color .hex {
    flex: 1;
    font-family: ui-monospace, monospace;
    font-size: 0.75rem;
  }
  .slider-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    min-width: 0;
  }
  .slider-row .num {
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
    font-size: 0.75rem;
    min-width: 2.5rem;
    text-align: right;
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 4px;
    overflow: hidden;
  }
  .seg button {
    background: var(--bg-elevated);
    color: var(--text-muted);
    border: 0;
    padding: 0.25rem 0.6rem;
    font-size: 0.75rem;
    cursor: pointer;
  }
  .seg button.active {
    background: var(--accent);
    color: white;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
    padding: 0.5rem 0.7rem;
    border-top: 1px solid var(--border);
    background: var(--bg-elevated);
  }
</style>
