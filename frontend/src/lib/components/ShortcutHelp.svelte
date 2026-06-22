<script lang="ts">
  /// Static keyboard / mouse shortcut overlay. Opened via the `?` button
  /// in the 2D canvas corner or the global `?` / F1 keybinding. Wraps
  /// the shared Modal so focus management + Esc-to-close + click-outside
  /// follow the same pattern as MachineDialog / ToolLibraryDialog /
  /// SettingsDialog / AddTextDialog.
  import Modal from './Modal.svelte';
  import { t } from '../i18n';

  interface Props {
    onClose: () => void;
    /// Render inline (Help tab column) instead of as a modal.
    embedded?: boolean;
  }
  let { onClose, embedded = false }: Props = $props();
</script>

{#snippet shell()}
  <div class="shortcut-grid">
    <header>
      <h2 id="shortcut-help-title">{t('shortcuts.title')}</h2>
      {#if !embedded}
        <button class="dlg-close" onclick={onClose} type="button" aria-label={t('common.close')}
          >×</button
        >
      {/if}
    </header>

    <div class="body">
      <section>
        <h3>{t('shortcuts.section.canvas2d')}</h3>
        <dl>
          <dt><kbd>drag</kbd></dt>
          <dd>{t('shortcuts.canvas2d.pan')}</dd>
          <dt><kbd>scroll</kbd></dt>
          <dd>{t('shortcuts.zoom')}</dd>
          <dt><kbd>click</kbd></dt>
          <dd>{t('shortcuts.canvas2d.select')}</dd>
          <dt><kbd>Shift</kbd> + <kbd>click</kbd></dt>
          <dd>{t('shortcuts.canvas2d.add_selection')}</dd>
          <dt><kbd>Ctrl</kbd> / <kbd>⌘</kbd> + <kbd>click</kbd></dt>
          <dd>{t('shortcuts.canvas2d.remove_selection')}</dd>
          <dt><kbd>right-click</kbd></dt>
          <dd>{t('shortcuts.canvas2d.context')}</dd>
        </dl>
      </section>

      <section>
        <h3>{t('shortcuts.section.view3d')}</h3>
        <dl>
          <dt><kbd>left-drag</kbd></dt>
          <dd>{t('shortcuts.view3d.orbit')}</dd>
          <dt><kbd>right-drag</kbd></dt>
          <dd>{t('shortcuts.view3d.pan')}</dd>
          <dt><kbd>scroll</kbd></dt>
          <dd>{t('shortcuts.zoom')}</dd>
        </dl>
      </section>

      <section>
        <h3>{t('shortcuts.section.global')}</h3>
        <dl>
          <dt><kbd>T</kbd></dt>
          <dd>{t('shortcuts.global.add_text')}</dd>
          <dt><kbd>Ctrl</kbd> / <kbd>⌘</kbd> + <kbd>Z</kbd></dt>
          <dd>{t('shortcuts.global.undo')}</dd>
          <dt><kbd>Ctrl</kbd> + <kbd>Y</kbd></dt>
          <dd>{t('shortcuts.global.redo')}</dd>
          <dt><kbd>Ctrl</kbd> / <kbd>⌘</kbd> + <kbd>Shift</kbd> + <kbd>Z</kbd></dt>
          <dd>{t('shortcuts.global.redo')}</dd>
          <dt><kbd>?</kbd> / <kbd>F1</kbd></dt>
          <dd>{t('shortcuts.global.show_help')}</dd>
          <dt><kbd>Esc</kbd></dt>
          <dd>{t('shortcuts.global.escape')}</dd>
        </dl>
      </section>

      <section>
        <h3>{t('shortcuts.section.touch')}</h3>
        <dl>
          <dt><kbd>pinch</kbd></dt>
          <dd>{t('shortcuts.touch.pinch')}</dd>
          <dt><kbd>two-finger drag</kbd></dt>
          <dd>{t('shortcuts.touch.two_finger')}</dd>
          <dt><kbd>one-finger drag</kbd></dt>
          <dd>{t('shortcuts.touch.one_finger')}</dd>
          <dt><kbd>tap</kbd></dt>
          <dd>{t('shortcuts.touch.tap')}</dd>
          <dt><kbd>⧉</kbd> then <kbd>tap</kbd></dt>
          <dd>{t('shortcuts.touch.multi_select')}</dd>
          <dt><kbd>long-press</kbd></dt>
          <dd>{t('shortcuts.touch.long_press')}</dd>
          <dt><kbd>⌖</kbd></dt>
          <dd>{t('shortcuts.touch.fit_view')}</dd>
          <dt><kbd>edge-swipe</kbd> ◂ ▸</dt>
          <dd>{t('shortcuts.touch.edge_swipe')}</dd>
          <dt><kbd>pull down</kbd></dt>
          <dd>{t('shortcuts.touch.pull_down')}</dd>
          <dt><kbd>bottom handles</kbd></dt>
          <dd>{t('shortcuts.touch.bottom_handles')}</dd>
        </dl>
      </section>
    </div>

    {#if !embedded}
      <footer>
        <button class="btn-primary" onclick={onClose} type="button">{t('common.done')}</button>
      </footer>
    {/if}
  </div>
{/snippet}

{#if embedded}
  <section class="embedded-col">{@render shell()}</section>
{:else}
  <Modal
    {onClose}
    persistKey="shortcuts"
    width="min(560px, 95vw)"
    draggable
    resizable
    ariaLabelledBy="shortcut-help-title"
  >
    {@render shell()}
  </Modal>
{/if}

<style>
  /* Modal sizing comes from Modal.svelte's width / max-height props.
     Drop the prior `height: 100%` — the parent has only `max-height`,
     so `100%` resolved against an unsized box and collapsed to content
     anyway. Let the grid size to content and rely on Modal's overflow. */
  .shortcut-grid {
    display: grid;
    grid-template-rows: auto auto auto;
  }
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
    margin-bottom: 0.9rem;
  }
  section:last-child {
    margin-bottom: 0;
  }
  h3 {
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
    margin: 0 0 0.4rem;
    padding-bottom: 0.2rem;
    border-bottom: 1px solid var(--border);
  }
  dl {
    margin: 0;
    display: grid;
    grid-template-columns: minmax(0, 13rem) minmax(0, 1fr);
    column-gap: 0.8rem;
    row-gap: 0.25rem;
    font-size: 0.8rem;
  }
  dt {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.25rem;
    color: var(--text-muted);
    min-width: 0;
  }
  dd {
    margin: 0;
    color: var(--text);
    min-width: 0;
  }
  kbd {
    background: var(--bg-input);
    color: var(--text-strong);
    border: 1px solid var(--border);
    border-bottom-width: 2px;
    border-radius: 3px;
    padding: 0.05rem 0.35rem;
    font-family: ui-monospace, monospace;
    font-size: 0.72rem;
    line-height: 1.4;
    white-space: nowrap;
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
