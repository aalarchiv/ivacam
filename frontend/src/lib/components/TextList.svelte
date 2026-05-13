<script lang="ts">
  /// Editable text-layer panel — phase 3 of the text-engraving rework.
  ///
  /// Lists every `project.textLayers` entry as a collapsible row. The
  /// active row expands an inline edit form (text content, font label,
  /// size, position, rotation, spacing, alignment, kind). Edits flow
  /// through `project.updateTextLayer` so they undo cleanly and the
  /// pipeline re-runs at Generate.
  ///
  /// Click the trash icon to delete a layer; any ops targeting the
  /// layer's synthetic geometry name (`__text_<id>`) are cascade-deleted
  /// in the same undo step (see project.removeTextLayer).

  import { project } from '../state/project.svelte';
  import type { TextLayer, TextLayerKind, TextAlignment } from '../state/project.svelte';

  interface Props {
    onAddText?: () => void;
  }
  let { onAddText }: Props = $props();

  let collapsed = $state(false);

  function isSelected(id: number): boolean {
    return project.selectedTextLayerId === id;
  }

  function select(id: number) {
    if (project.selectedTextLayerId === id) {
      project.selectedTextLayerId = null;
    } else {
      project.selectedTextLayerId = id;
      // Deselect any op so the properties pane shows the text form.
      project.selectedOpId = null;
    }
  }

  function patch(id: number, delta: Partial<TextLayer>) {
    project.updateTextLayer(id, delta);
  }

  function patchOrigin(id: number, axis: 'x' | 'y', value: number) {
    const cur = project.textLayers.find((t) => t.id === id);
    if (!cur) return;
    const origin = axis === 'x' ? { x: value, y: cur.origin.y } : { x: cur.origin.x, y: value };
    project.updateTextLayer(id, { origin });
  }

  function remove(id: number) {
    project.removeTextLayer(id);
  }

  function fontLabel(layer: TextLayer): string {
    if (layer.fontSource.kind === 'bundled') {
      const name = layer.fontSource.path.split('/').pop() ?? layer.fontSource.path;
      return name.replace(/\.[^.]+$/, '');
    }
    return layer.fontSource.filename;
  }

  function shortLabel(layer: TextLayer): string {
    const firstLine = layer.text.split(/\r?\n/, 1)[0] ?? '';
    const truncated = firstLine.length > 24 ? `${firstLine.slice(0, 24)}…` : firstLine;
    return `${layer.kind} — "${truncated}"`;
  }
</script>

<aside class="text-panel">
  <div class="group-head">
    <button
      class="caret-btn"
      onclick={() => (collapsed = !collapsed)}
      title={collapsed ? 'Expand text layers' : 'Collapse text layers'}
      aria-label="Toggle text panel">{collapsed ? '▸' : '▾'}</button
    >
    <span class="group-name">Text</span>
    <span class="group-count">{project.textLayers.length}</span>
    {#if onAddText}
      <button
        type="button"
        class="add-btn"
        onclick={() => onAddText?.()}
        title="Add text engraving (T)"
        aria-label="Add text"
      >
        + Add
      </button>
    {/if}
  </div>
  {#if !collapsed}
    <div class="group-body">
      {#if project.textLayers.length === 0}
        <p class="empty">
          No text yet. Click <strong>+ Add</strong> to create an editable text engraving.
        </p>
      {:else}
        <ul>
          {#each project.textLayers as layer (layer.id)}
            <li class="text-row" class:active={isSelected(layer.id)}>
              <div class="row-head">
                <button
                  type="button"
                  class="caret-btn"
                  onclick={() => select(layer.id)}
                  aria-expanded={isSelected(layer.id)}
                  aria-label={`Toggle edit form for ${layer.name}`}
                >
                  {isSelected(layer.id) ? '▾' : '▸'}
                </button>
                <button
                  type="button"
                  class="row-label"
                  onclick={() => select(layer.id)}
                  title={layer.name}
                >
                  <span class="kind-tag" class:single={layer.singleLine}>
                    {layer.kind}{layer.singleLine ? ' · 1L' : ''}
                  </span>
                  <span class="row-text">{shortLabel(layer)}</span>
                </button>
                <button
                  type="button"
                  class="del-btn"
                  onclick={() => remove(layer.id)}
                  title="Delete text layer (also deletes ops targeting it)"
                  aria-label="Delete text layer"
                >
                  ×
                </button>
              </div>
              {#if isSelected(layer.id)}
                <div class="edit-form">
                  <label class="full">
                    <span>Text</span>
                    {#if layer.kind === 'MTEXT'}
                      <textarea
                        rows="3"
                        value={layer.text}
                        oninput={(e) =>
                          patch(layer.id, { text: (e.currentTarget as HTMLTextAreaElement).value })}
                      ></textarea>
                    {:else}
                      <input
                        type="text"
                        value={layer.text}
                        oninput={(e) =>
                          patch(layer.id, { text: (e.currentTarget as HTMLInputElement).value })}
                      />
                    {/if}
                  </label>
                  <label>
                    <span>Kind</span>
                    <select
                      value={layer.kind}
                      onchange={(e) =>
                        patch(layer.id, {
                          kind: (e.currentTarget as HTMLSelectElement).value as TextLayerKind,
                        })}
                    >
                      <option value="TEXT">TEXT (single line)</option>
                      <option value="MTEXT">MTEXT (multi-line)</option>
                    </select>
                  </label>
                  <div class="field-pair">
                    <span class="field-label">Font</span>
                    <span class="readout" title={fontLabel(layer)}>{fontLabel(layer)}</span>
                  </div>
                  <label>
                    <span>Size (mm)</span>
                    <input
                      type="number"
                      step="0.5"
                      min="0.1"
                      value={layer.sizeMm}
                      oninput={(e) =>
                        patch(layer.id, {
                          sizeMm: parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        })}
                    />
                  </label>
                  <label>
                    <span>X (mm)</span>
                    <input
                      type="number"
                      step="0.5"
                      value={layer.origin.x}
                      oninput={(e) =>
                        patchOrigin(
                          layer.id,
                          'x',
                          parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        )}
                    />
                  </label>
                  <label>
                    <span>Y (mm)</span>
                    <input
                      type="number"
                      step="0.5"
                      value={layer.origin.y}
                      oninput={(e) =>
                        patchOrigin(
                          layer.id,
                          'y',
                          parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        )}
                    />
                  </label>
                  <label>
                    <span>Rotation (°)</span>
                    <input
                      type="number"
                      step="5"
                      value={layer.rotationDeg}
                      oninput={(e) =>
                        patch(layer.id, {
                          rotationDeg:
                            parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        })}
                    />
                  </label>
                  <label>
                    <span>Letter gap</span>
                    <input
                      type="number"
                      step="0.1"
                      value={layer.letterSpacingMm}
                      oninput={(e) =>
                        patch(layer.id, {
                          letterSpacingMm:
                            parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        })}
                    />
                  </label>
                  <label class:hidden={layer.kind !== 'MTEXT'}>
                    <span>Line spacing</span>
                    <input
                      type="number"
                      step="0.5"
                      min="0"
                      value={layer.lineSpacingMm}
                      oninput={(e) =>
                        patch(layer.id, {
                          lineSpacingMm:
                            parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        })}
                    />
                  </label>
                  <label>
                    <span>Align</span>
                    <select
                      value={layer.alignment}
                      onchange={(e) =>
                        patch(layer.id, {
                          alignment: (e.currentTarget as HTMLSelectElement)
                            .value as TextAlignment,
                        })}
                    >
                      <option value="left">left</option>
                      <option value="center">center</option>
                      <option value="right">right</option>
                    </select>
                  </label>
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</aside>

<style>
  .text-panel {
    width: 100%;
    background: var(--bg-panel);
    color: var(--text);
    padding: 0.4rem 0.6rem 0.5rem;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
    border-top: 1px solid var(--border);
  }
  .group-head {
    display: grid;
    grid-template-columns: auto 1fr auto auto;
    gap: 0.3rem;
    align-items: center;
    padding: 0.2rem 0.35rem;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: color-mix(in srgb, var(--accent) 6%, var(--bg-panel));
    font-size: 0.78rem;
  }
  .caret-btn {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    cursor: pointer;
    padding: 0 0.2rem;
    font-size: 0.85rem;
    line-height: 1;
  }
  .group-name {
    color: var(--text-strong);
    font-weight: 600;
  }
  .group-count {
    color: var(--text-muted);
    font-size: 0.72rem;
    padding: 0 0.3rem;
    background: var(--bg);
    border-radius: 10px;
    line-height: 1.4;
  }
  .add-btn {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    color: var(--text);
    border-radius: 3px;
    padding: 0.15rem 0.4rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .add-btn:hover {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .group-body {
    margin: 0.2rem 0 0 0.5rem;
    padding-left: 0.3rem;
    border-left: 2px solid color-mix(in srgb, var(--accent) 30%, transparent);
    max-height: 38vh;
    overflow-y: auto;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li.text-row {
    margin: 0.2rem 0;
  }
  .row-head {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 0.2rem;
  }
  .row-label {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    background: transparent;
    color: var(--text);
    border: 0;
    text-align: left;
    cursor: pointer;
    padding: 0.15rem 0.2rem;
    font: inherit;
    overflow: hidden;
  }
  .row-label:hover {
    color: var(--text-strong);
  }
  li.text-row.active .row-label {
    color: var(--text-strong);
  }
  .kind-tag {
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 2px;
    padding: 0 0.25rem;
    line-height: 1.3;
    font-variant-numeric: tabular-nums;
  }
  .kind-tag.single {
    color: var(--accent-strong);
    border-color: var(--accent);
  }
  .row-text {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.78rem;
  }
  .del-btn {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 1rem;
    line-height: 1;
    padding: 0 0.3rem;
  }
  .del-btn:hover {
    color: var(--error);
  }
  .empty {
    margin: 0.6rem 0.2rem;
    font-size: 0.72rem;
    color: var(--text-muted);
    line-height: 1.4;
  }
  .empty strong {
    color: var(--text-strong);
  }
  .edit-form {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.3rem 0.5rem;
    margin: 0.3rem 0 0.4rem;
    padding: 0.4rem 0.5rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 3px;
    font-size: 0.74rem;
  }
  .edit-form label,
  .edit-form .field-pair {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    color: var(--text-muted);
  }
  .edit-form .field-label {
    color: var(--text-muted);
  }
  .edit-form label.full {
    grid-column: 1 / -1;
  }
  .edit-form label.hidden {
    display: none;
  }
  .edit-form input,
  .edit-form textarea,
  .edit-form select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.3rem;
    font: inherit;
    width: 100%;
    box-sizing: border-box;
  }
  .edit-form textarea {
    resize: vertical;
    min-height: 3rem;
    font-family: inherit;
  }
  .edit-form .readout {
    padding: 0.18rem 0.3rem;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 3px;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
