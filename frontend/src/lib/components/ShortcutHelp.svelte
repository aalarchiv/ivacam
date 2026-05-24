<script lang="ts">
  /// Static keyboard / mouse shortcut overlay. Opened via the `?` button
  /// in the 2D canvas corner or the global `?` / F1 keybinding. Wraps
  /// the shared Modal so focus management + Esc-to-close + click-outside
  /// follow the same pattern as MachineDialog / ToolLibraryDialog /
  /// SettingsDialog / AddTextDialog.
  import Modal from './Modal.svelte';

  interface Props {
    onClose: () => void;
  }
  let { onClose }: Props = $props();
</script>

<Modal {onClose} width="min(560px, 95vw)" maxHeight="90vh" ariaLabelledBy="shortcut-help-title">
 <div class="shortcut-grid">
  <header>
    <h2 id="shortcut-help-title">Keyboard &amp; mouse shortcuts</h2>
    <button class="dlg-close" onclick={onClose} type="button" aria-label="Close">×</button>
  </header>

  <div class="body">
    <section>
      <h3>2D Canvas</h3>
      <dl>
        <dt><kbd>drag</kbd></dt>
        <dd>Pan view</dd>
        <dt><kbd>scroll</kbd></dt>
        <dd>Zoom in / out</dd>
        <dt><kbd>click</kbd></dt>
        <dd>Select object</dd>
        <dt><kbd>Shift</kbd> + <kbd>click</kbd></dt>
        <dd>Add to selection</dd>
        <dt><kbd>Ctrl</kbd> / <kbd>⌘</kbd> + <kbd>click</kbd></dt>
        <dd>Remove from selection</dd>
        <dt><kbd>right-click</kbd></dt>
        <dd>Context menu / deselect</dd>
      </dl>
    </section>

    <section>
      <h3>3D View</h3>
      <dl>
        <dt><kbd>left-drag</kbd></dt>
        <dd>Orbit</dd>
        <dt><kbd>right-drag</kbd></dt>
        <dd>Pan</dd>
        <dt><kbd>scroll</kbd></dt>
        <dd>Zoom in / out</dd>
      </dl>
    </section>

    <section>
      <h3>Global</h3>
      <dl>
        <dt><kbd>T</kbd></dt>
        <dd>Add Text</dd>
        <dt><kbd>Ctrl</kbd> / <kbd>⌘</kbd> + <kbd>Z</kbd></dt>
        <dd>Undo</dd>
        <dt><kbd>Ctrl</kbd> + <kbd>Y</kbd></dt>
        <dd>Redo</dd>
        <dt><kbd>Ctrl</kbd> / <kbd>⌘</kbd> + <kbd>Shift</kbd> + <kbd>Z</kbd></dt>
        <dd>Redo</dd>
        <dt><kbd>?</kbd> / <kbd>F1</kbd></dt>
        <dd>Show this help</dd>
        <dt><kbd>Esc</kbd></dt>
        <dd>Cancel mode / clear selection / close menu</dd>
      </dl>
    </section>
  </div>

  <footer>
    <button class="btn-primary" onclick={onClose} type="button">Done</button>
  </footer>
 </div>
</Modal>

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
