<script lang="ts">
  /// Static keyboard / mouse shortcut overlay. Opened via the `?` button
  /// in the 2D canvas corner or the global `?` / F1 keybinding. Closes
  /// on Escape, click-outside, or via the close button.
  interface Props {
    onClose: () => void;
  }
  let { onClose }: Props = $props();

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      e.preventDefault();
      onClose();
    }
  }

  function onOverlayClick(e: MouseEvent) {
    if (e.target === e.currentTarget) onClose();
  }
</script>

<svelte:window onkeydown={onKeyDown} />

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div
  class="overlay"
  role="dialog"
  tabindex="-1"
  aria-modal="true"
  aria-labelledby="shortcut-help-title"
  onclick={onOverlayClick}
>
  <div class="modal">
    <header>
      <h2 id="shortcut-help-title">Keyboard &amp; mouse shortcuts</h2>
      <button class="close" onclick={onClose} type="button" aria-label="Close">×</button>
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
      <button class="primary" onclick={onClose} type="button">Done</button>
    </footer>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, black 50%, transparent);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 60;
  }
  .modal {
    width: min(560px, 95vw);
    max-height: 90vh;
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    box-shadow: 0 10px 40px rgba(0, 0, 0, 0.4);
    display: grid;
    grid-template-rows: auto 1fr auto;
    overflow: hidden;
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
  .close {
    background: transparent;
    color: var(--text-muted);
    border: 0;
    font-size: 1.2rem;
    cursor: pointer;
    padding: 0 0.3rem;
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
  .primary {
    background: var(--accent);
    color: white;
    border: 0;
    padding: 0.3rem 0.8rem;
    border-radius: 3px;
    cursor: pointer;
  }
</style>
