<script lang="ts">
  /// Settings dialog. Per-installation user preferences (theme, language,
  /// cutting-preview defaults, performance caps). Persisted to localStorage
  /// under `ivac.settings`; not part of .vc-project. Mirrors the modal
  /// style used by MachineDialog / ToolLibraryDialog.
  ///
  /// Unlike Machine / Tools we commit changes live: theme switching needs
  /// to apply immediately, and there's no "Cancel" button — Escape or the
  /// close button just dismisses the dialog.
  import { project, type AppSettings } from '../state/project.svelte';
  import { layout } from '../state/layout.svelte';
  import { t, type MsgKey } from '../i18n';
  import Modal from './Modal.svelte';

  interface Props {
    open: boolean;
    onClose: () => void;
    /// Render as a tab panel: no Modal wrapper, no header/Done (edits
    /// apply live, so there's nothing to confirm).
    embedded?: boolean;
  }
  let { open, onClose, embedded = false }: Props = $props();

  /// Embedded (Settings tab) layout: the section headers become a
  /// vertical group nav on the left and only the active group's
  /// content shows on the right — the full stacked form overwhelms.
  /// Modal mode (unused since the tab landed) keeps the stacked list.
  const GROUPS = [
    { id: 'view', labelKey: 'settings.group.view' },
    { id: 'appearance', labelKey: 'settings.group.appearance' },
    { id: 'preview', labelKey: 'settings.group.preview' },
    { id: 'performance', labelKey: 'settings.group.performance' },
    { id: 'safety', labelKey: 'settings.group.safety' },
    { id: 'sources', labelKey: 'settings.group.sources' },
    { id: 'snap', labelKey: 'settings.group.snap' },
  ] as const satisfies ReadonlyArray<{ id: string; labelKey: MsgKey }>;
  type GroupId = (typeof GROUPS)[number]['id'];
  let activeGroup = $state<GroupId>('appearance');
  /// On a phone the side-nav + single-group layout squishes; show every
  /// group in one scrolling column instead (the nav is hidden via CSS).
  /// Only the desktop tab keeps the nav-filtered single-group view.
  function groupHidden(id: GroupId): boolean {
    return embedded && !layout.isNarrow && activeGroup !== id;
  }

  function update<K extends keyof AppSettings>(key: K, value: AppSettings[K]) {
    project.updateSettings({ [key]: value } as Partial<AppSettings>);
  }

  /// Object-snap toggles. `key` indexes AppSettings.osnap; label/help go
  /// through t() in the template so they translate live.
  const SNAP_OPTIONS = [
    { key: 'endpoint', labelKey: 'settings.snap.endpoint', helpKey: 'settings.snap.endpoint.help' },
    { key: 'midpoint', labelKey: 'settings.snap.midpoint', helpKey: 'settings.snap.midpoint.help' },
    {
      key: 'intersection',
      labelKey: 'settings.snap.intersection',
      helpKey: 'settings.snap.intersection.help',
    },
    { key: 'center', labelKey: 'settings.snap.center', helpKey: 'settings.snap.center.help' },
    { key: 'grid', labelKey: 'settings.snap.grid', helpKey: 'settings.snap.grid.help' },
  ] as const satisfies ReadonlyArray<{
    key: 'endpoint' | 'midpoint' | 'intersection' | 'center' | 'grid';
    labelKey: MsgKey;
    helpKey: MsgKey;
  }>;

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
    const cur = THEMES.indexOf(project.data.settings.theme as (typeof THEMES)[number]);
    let next: number;
    if (e.key === 'ArrowLeft' || e.key === 'ArrowUp')
      next = (cur - 1 + THEMES.length) % THEMES.length;
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

{#snippet shell()}
  {#if !embedded}
    <header>
      <h2 id="settings-title">{t('settings.title')}</h2>
      <button class="dlg-close" onclick={onClose} aria-label={t('common.close')}>×</button>
    </header>
  {/if}

  <div class="body">
    <section class:group-hidden={groupHidden('view')}>
      <h3>{t('settings.group.view')}</h3>
      <div class="grid">
        <label
          >{t('settings.view.preview_style')}
          <select
            value={project.data.settings.previewMode}
            onchange={(e) =>
              update(
                'previewMode',
                (e.currentTarget as HTMLSelectElement).value as AppSettings['previewMode'],
              )}
          >
            <option value="both">{t('settings.view.preview_style.both')}</option>
            <option value="wireframe">{t('settings.view.preview_style.wireframe')}</option>
            <option value="solid">{t('settings.view.preview_style.solid')}</option>
          </select>
        </label>
        <label class="check">
          <input
            type="checkbox"
            checked={project.data.settings.showStockBox}
            onchange={(e) => update('showStockBox', (e.currentTarget as HTMLInputElement).checked)}
          />
          <span>{t('settings.view.show_stock_box')}</span>
        </label>
      </div>
    </section>

    <section class:group-hidden={groupHidden('appearance')}>
      <h3>{t('settings.group.appearance')}</h3>
      <div class="grid">
        <label
          >{t('settings.appearance.theme')}
          <div
            class="seg"
            role="radiogroup"
            aria-label={t('settings.appearance.theme')}
            tabindex="-1"
            onkeydown={onThemeKey}
          >
            <button
              role="radio"
              aria-checked={project.data.settings.theme === 'auto'}
              tabindex={project.data.settings.theme === 'auto' ? 0 : -1}
              class:active={project.data.settings.theme === 'auto'}
              onclick={() => update('theme', 'auto')}
              type="button">{t('settings.appearance.theme.auto')}</button
            >
            <button
              role="radio"
              aria-checked={project.data.settings.theme === 'light'}
              tabindex={project.data.settings.theme === 'light' ? 0 : -1}
              class:active={project.data.settings.theme === 'light'}
              onclick={() => update('theme', 'light')}
              type="button">{t('settings.appearance.theme.light')}</button
            >
            <button
              role="radio"
              aria-checked={project.data.settings.theme === 'dark'}
              tabindex={project.data.settings.theme === 'dark' ? 0 : -1}
              class:active={project.data.settings.theme === 'dark'}
              onclick={() => update('theme', 'dark')}
              type="button">{t('settings.appearance.theme.dark')}</button
            >
          </div>
        </label>

        <label
          >{t('settings.appearance.language')}
          <!-- Language endonyms (English / Deutsch) are shown in their own
               language by convention and stay untranslated; only the label,
               the Automatic option, and the hint go through t(). -->
          <select
            value={project.data.settings.language}
            onchange={(e) =>
              update(
                'language',
                (e.currentTarget as HTMLSelectElement).value as AppSettings['language'],
              )}
          >
            <option value="auto">{t('settings.appearance.language.auto')}</option>
            <option value="en">English</option>
            <option value="de">Deutsch</option>
          </select>
        </label>
      </div>
      <p class="hint">{t('settings.appearance.language.help')}</p>
    </section>

    <section class:group-hidden={groupHidden('preview')}>
      <h3>{t('settings.group.preview')}</h3>
      <p class="hint">{t('settings.preview.intro')}</p>
      <div class="grid">
        <label
          >{t('settings.preview.solid_color')}
          <div class="color">
            <input
              type="color"
              value={project.data.settings.solidColor}
              oninput={(e) => update('solidColor', (e.currentTarget as HTMLInputElement).value)}
            />
            <input
              type="text"
              class="hex"
              value={project.data.settings.solidColor}
              oninput={(e) => update('solidColor', (e.currentTarget as HTMLInputElement).value)}
            />
          </div>
        </label>

        <label
          >{t('settings.preview.solid_opacity')}
          <div class="slider-row">
            <input
              type="range"
              min="0.1"
              max="1"
              step="0.05"
              value={project.data.settings.solidOpacity}
              onchange={(e) =>
                update(
                  'solidOpacity',
                  toNumber(
                    (e.currentTarget as HTMLInputElement).value,
                    project.data.settings.solidOpacity,
                    0.1,
                    1,
                  ),
                )}
            />
            <span class="num">{project.data.settings.solidOpacity.toFixed(2)}</span>
          </div>
        </label>

        <label
          >{t('settings.preview.edge_color')}
          <div class="color">
            <input
              type="color"
              value={project.data.settings.edgeColor}
              oninput={(e) => update('edgeColor', (e.currentTarget as HTMLInputElement).value)}
            />
            <input
              type="text"
              class="hex"
              value={project.data.settings.edgeColor}
              oninput={(e) => update('edgeColor', (e.currentTarget as HTMLInputElement).value)}
            />
          </div>
        </label>

        <label
          >{t('settings.preview.edge_opacity')}
          <div class="slider-row">
            <input
              type="range"
              min="0"
              max="1"
              step="0.05"
              value={project.data.settings.edgeOpacity}
              onchange={(e) =>
                update(
                  'edgeOpacity',
                  toNumber(
                    (e.currentTarget as HTMLInputElement).value,
                    project.data.settings.edgeOpacity,
                    0,
                    1,
                  ),
                )}
            />
            <span class="num">{project.data.settings.edgeOpacity.toFixed(2)}</span>
          </div>
        </label>

        <label
          >{t('settings.preview.line_width')}
          <div class="slider-row">
            <input
              type="range"
              min="0.5"
              max="6"
              step="0.5"
              value={project.data.settings.previewLineWidth}
              onchange={(e) =>
                update(
                  'previewLineWidth',
                  toNumber(
                    (e.currentTarget as HTMLInputElement).value,
                    project.data.settings.previewLineWidth,
                    0.5,
                    6,
                  ),
                )}
            />
            <span class="num">{project.data.settings.previewLineWidth.toFixed(1)} px</span>
          </div>
        </label>

        <label
          >{t('settings.preview.arrow_density')}
          <div class="slider-row">
            <input
              type="range"
              min="0"
              max="3"
              step="0.25"
              value={project.data.settings.toolMoveArrowDensity}
              onchange={(e) =>
                update(
                  'toolMoveArrowDensity',
                  toNumber(
                    (e.currentTarget as HTMLInputElement).value,
                    project.data.settings.toolMoveArrowDensity,
                    0,
                    3,
                  ),
                )}
            />
            <span class="num"
              >{project.data.settings.toolMoveArrowDensity === 0
                ? t('settings.preview.arrow_off')
                : `${project.data.settings.toolMoveArrowDensity.toFixed(2)}×`}</span
            >
          </div>
        </label>

        <label
          >{t('settings.preview.cell_resolution')}
          <select
            value={project.data.settings.cellResolutionMode}
            onchange={(e) =>
              update(
                'cellResolutionMode',
                (e.currentTarget as HTMLSelectElement).value as AppSettings['cellResolutionMode'],
              )}
          >
            <option value="auto">{t('settings.preview.cell_resolution.auto')}</option>
            <option value="manual">{t('settings.preview.cell_resolution.manual')}</option>
          </select>
        </label>

        {#if project.data.settings.cellResolutionMode === 'manual'}
          <label
            >{t('settings.preview.cell_size')}
            <input
              type="number"
              min="0.01"
              max="5"
              step="0.05"
              title={t('settings.preview.cell_size.title')}
              value={project.data.settings.cellResolutionMm}
              onchange={(e) =>
                update(
                  'cellResolutionMm',
                  toNumber(
                    (e.currentTarget as HTMLInputElement).value,
                    project.data.settings.cellResolutionMm,
                    0.01,
                    5,
                  ),
                )}
            />
          </label>
        {/if}
      </div>
    </section>

    <section class:group-hidden={groupHidden('performance')}>
      <h3>{t('settings.group.performance')}</h3>
      <div class="grid">
        <label class="check">
          <input
            type="checkbox"
            checked={project.data.settings.solidPreviewByDefault}
            onchange={(e) =>
              update('solidPreviewByDefault', (e.currentTarget as HTMLInputElement).checked)}
          />
          <span>{t('settings.performance.solid_default')}</span>
        </label>

        <label
          >{t('settings.performance.max_sim_cells')}
          <input
            type="number"
            min="100000"
            step="100000"
            value={project.data.settings.maxSimulationCells}
            onchange={(e) =>
              update(
                'maxSimulationCells',
                Math.round(
                  toNumber(
                    (e.currentTarget as HTMLInputElement).value,
                    project.data.settings.maxSimulationCells,
                    100_000,
                  ),
                ),
              )}
          />
        </label>

        <label
          >{t('settings.performance.max_render_triangles')}
          <input
            type="number"
            min="100000"
            step="100000"
            value={project.data.settings.maxRenderTriangles}
            onchange={(e) =>
              update(
                'maxRenderTriangles',
                Math.round(
                  toNumber(
                    (e.currentTarget as HTMLInputElement).value,
                    project.data.settings.maxRenderTriangles,
                    100_000,
                  ),
                ),
              )}
          />
        </label>
      </div>
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- static, translator-authored markup -->
      <p class="hint">{@html t('settings.performance.caps_hint')}</p>

      <!-- Exact 3D rewind toggle. Default ON
             because the post-Generate `playhead = 1.0` hop means
             the user lands at the END-OF-PROGRAM state right
             after Generate — without exact rewind, dragging the
             scrubber back leaves the terrain stuck at end-state.
             Users on enormous programs can flip it off for
             responsive scrubbing at the price of time-accurate
             terrain. -->
      <label class="check">
        <input
          type="checkbox"
          checked={project.data.settings.exactSimRewind}
          onchange={(e) => update('exactSimRewind', (e.currentTarget as HTMLInputElement).checked)}
        />
        <span>{t('settings.performance.exact_rewind')}</span>
      </label>
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- static, translator-authored markup -->
      <p class="hint">{@html t('settings.performance.exact_rewind.hint')}</p>
    </section>

    <section class:group-hidden={groupHidden('safety')}>
      <h3>{t('settings.group.safety')}</h3>
      <div class="grid">
        <label class="check">
          <input
            type="checkbox"
            checked={project.data.settings.blockOnCriticalSimWarnings}
            onchange={(e) =>
              update('blockOnCriticalSimWarnings', (e.currentTarget as HTMLInputElement).checked)}
          />
          <span>{t('settings.safety.block_critical')}</span>
        </label>
        <label class="check" title={t('settings.safety.block_work_area.title')}>
          <input
            type="checkbox"
            checked={project.data.settings.blockOnWorkAreaViolation}
            onchange={(e) =>
              update('blockOnWorkAreaViolation', (e.currentTarget as HTMLInputElement).checked)}
          />
          <span>{t('settings.safety.block_work_area')}</span>
        </label>
        <label class="check">
          <input
            type="checkbox"
            checked={project.data.settings.autoRunSimOnSave}
            onchange={(e) =>
              update('autoRunSimOnSave', (e.currentTarget as HTMLInputElement).checked)}
          />
          <span>{t('settings.safety.auto_run_sim')}</span>
        </label>
        <label class="check" title={t('settings.safety.auto_regenerate.title')}>
          <input
            type="checkbox"
            checked={project.data.settings.autoRegenerate}
            onchange={(e) =>
              update('autoRegenerate', (e.currentTarget as HTMLInputElement).checked)}
          />
          <span>{t('settings.safety.auto_regenerate')}</span>
        </label>
      </div>
      <p class="hint">{t('settings.safety.hint')}</p>
    </section>

    <section class:group-hidden={groupHidden('sources')}>
      <h3>{t('settings.group.sources')}</h3>
      <div class="grid">
        <label class="check">
          <input
            type="checkbox"
            checked={project.data.settings.autoReloadSources}
            onchange={(e) =>
              update('autoReloadSources', (e.currentTarget as HTMLInputElement).checked)}
          />
          <span>{t('settings.sources.auto_reload')}</span>
        </label>
      </div>
      <p class="hint">{t('settings.sources.hint')}</p>
    </section>

    <section class:group-hidden={groupHidden('snap')}>
      <h3>{t('settings.group.snap')}</h3>
      <div class="grid">
        {#each SNAP_OPTIONS as o (o.key)}
          <label class="check" title={t(o.helpKey)}>
            <input
              type="checkbox"
              checked={!!project.data.settings.osnap?.[o.key]}
              onchange={(e) =>
                update('osnap', {
                  ...project.data.settings.osnap,
                  [o.key]: (e.currentTarget as HTMLInputElement).checked,
                })}
            />
            <span>{t(o.labelKey)}</span>
          </label>
        {/each}
        <label title={t('settings.snap.grid_step.title')}
          >{t('settings.snap.grid_step')}
          <input
            type="number"
            min="0.1"
            step="0.1"
            value={project.data.settings.osnap?.gridStepMm ?? 5}
            onchange={(e) =>
              update('osnap', {
                ...project.data.settings.osnap,
                gridStepMm: Math.max(
                  0.1,
                  toNumber(
                    (e.currentTarget as HTMLInputElement).value,
                    project.data.settings.osnap?.gridStepMm ?? 5,
                    0.1,
                  ),
                ),
              })}
          />
        </label>
      </div>
      <p class="hint">{t('settings.snap.hint')}</p>
    </section>
  </div>

  {#if !embedded}
    <footer>
      <button class="btn-primary" onclick={onClose} type="button">{t('common.done')}</button>
    </footer>
  {/if}
{/snippet}

{#if embedded}
  <section class="embedded-shell" class:narrow={layout.isNarrow}>
    <nav class="group-nav" aria-label={t('settings.group_nav.aria')}>
      {#each GROUPS as g (g.id)}
        <button
          type="button"
          class="group-tab"
          class:active={activeGroup === g.id}
          onclick={() => (activeGroup = g.id)}>{t(g.labelKey)}</button
        >
      {/each}
    </nav>
    <div class="group-content">{@render shell()}</div>
  </section>
{:else if open}
  <Modal
    {onClose}
    persistKey="settings"
    width="min(540px, 95vw)"
    draggable
    resizable
    ariaLabelledBy="settings-title"
  >
    {@render shell()}
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
  .embedded-shell {
    display: flex;
    flex-direction: row;
    flex: 1;
    min-height: 0;
    background: var(--bg-panel);
  }
  /* Phone: one scrolling column with every group shown — the side-nav +
     single-group split is too cramped and cut content off. */
  .embedded-shell.narrow {
    flex-direction: column;
  }
  .embedded-shell.narrow .group-nav {
    display: none;
  }
  .embedded-shell.narrow .group-content {
    max-width: none;
  }
  .group-nav {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    padding: 0.6rem 0.4rem;
    border-right: 1px solid var(--border);
    min-width: 10rem;
  }
  .group-tab {
    background: none;
    border: 1px solid transparent;
    border-radius: 4px;
    padding: 0.35rem 0.6rem;
    font-size: 0.8rem;
    color: var(--text-muted);
    text-align: left;
    cursor: pointer;
  }
  .group-tab:hover {
    color: var(--text);
  }
  .group-tab.active {
    background: var(--bg-elevated);
    border-color: var(--border);
    color: var(--text-strong);
  }
  .group-content {
    flex: 1;
    min-width: 0;
    overflow: auto;
    /* The modal constrains width; the tab shouldn't stretch forms
       across a 4k monitor. */
    max-width: 720px;
  }
  .group-content :global(section.group-hidden),
  section.group-hidden {
    display: none;
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
