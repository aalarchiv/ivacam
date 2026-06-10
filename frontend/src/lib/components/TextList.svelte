<script lang="ts">
  /// Editable text-layer panel — phase 3 of the text-engraving rework.
  ///
  /// Lists every `project.data.textLayers` entry as a collapsible row. The
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
  import { selectionOrigin } from '../canvas/selection-geometry';
  import { parseFiniteNumber } from '../cam/units';

  /// 245i: bottom-left of the current object selection's bbox, or null
  /// when nothing is selected. Drives the per-text "snap origin to
  /// selection" affordance — the on-demand equivalent of the placement
  /// AddTextDialog does at creation time.
  const selOrigin = $derived(
    selectionOrigin(project.transformedImport?.object_meta ?? [], project.sel.selectedObjects),
  );
  function snapOriginToSelection(id: number) {
    if (!selOrigin) return;
    project.updateTextLayer(id, { origin: { x: selOrigin.x, y: selOrigin.y } });
  }

  interface Props {
    /// Accordion-controlled (sidebar parent passes active + activate).
    active: boolean;
    onActivate: () => void;
    onAddText?: () => void;
  }
  let { active, onActivate, onAddText }: Props = $props();
  let collapsed = $derived(!active);

  function isSelected(id: number): boolean {
    return project.sel.selectedTextLayerId === id;
  }

  function select(id: number) {
    if (project.sel.selectedTextLayerId === id) {
      project.sel.selectedTextLayerId = null;
    } else {
      project.sel.selectedTextLayerId = id;
      // Deselect any op so the properties pane shows the text form.
      project.sel.selectedOpId = null;
    }
  }

  function patch(id: number, delta: Partial<TextLayer>) {
    project.updateTextLayer(id, delta);
  }

  /// Transient red-border marker, keyed `<layerId>:<field>`. Set when a
  /// numeric input parses to garbage / out-of-range; cleared on the next
  /// valid (or empty) entry. Mirrors StockPanel's single-shared invalid
  /// key — avoids `parseFloat(v) || 0`, which silently coerced a cleared
  /// or non-numeric field to 0 (an empty toolpath when it became a text
  /// size). See `parseFiniteNumber` in cam/units.ts.
  let invalidKey = $state<string | null>(null);

  /// Parse `raw` and apply it only when finite/in-range; an empty field
  /// keeps the prior value silently, garbage flashes the invalid cue.
  function commitNumber(
    id: number,
    field: string,
    raw: string,
    apply: (v: number) => void,
    opts: { min?: number; max?: number } = {},
  ) {
    const parsed = parseFiniteNumber(raw, opts);
    if (parsed.value == null) {
      invalidKey = parsed.invalid ? `${id}:${field}` : null;
      return;
    }
    invalidKey = null;
    apply(parsed.value);
  }

  /// Single text-input handler that auto-promotes between TEXT and
  /// MTEXT based on whether the value contains a newline. The user
  /// doesn't pick the kind any more — the field reacts.
  function onTextInput(layer: TextLayer, e: Event) {
    const value = (e.currentTarget as HTMLTextAreaElement).value;
    const isMulti = value.includes('\n');
    const nextKind: TextLayerKind = isMulti ? 'MTEXT' : 'TEXT';
    if (nextKind !== layer.kind) {
      project.updateTextLayer(layer.id, { text: value, kind: nextKind });
    } else {
      project.updateTextLayer(layer.id, { text: value });
    }
  }

  function patchOrigin(id: number, axis: 'x' | 'y', value: number) {
    const cur = project.data.textLayers.find((t) => t.id === id);
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
      onclick={onActivate}
      title={active ? 'Collapse text panel (return to previous panel)' : 'Expand text panel'}
      aria-label={active ? 'Collapse text panel' : 'Activate text panel'}
      >{active ? '▾' : '▸'}</button
    >
    <span class="group-name">Text</span>
    <span class="group-count">{project.data.textLayers.length}</span>
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
      {#if project.data.textLayers.length === 0}
        <p class="empty">
          No text yet. Click <strong>+ Add</strong> to create an editable text engraving.
        </p>
      {:else}
        <ul>
          {#each project.data.textLayers as layer (layer.id)}
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
                    <textarea
                      class:multiline={layer.text.includes('\n')}
                      rows={layer.text.includes('\n') ? 4 : 1}
                      value={layer.text}
                      oninput={(e) => onTextInput(layer, e)}
                    ></textarea>
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
                      class:invalid={invalidKey === `${layer.id}:size`}
                      value={layer.sizeMm}
                      oninput={(e) =>
                        commitNumber(
                          layer.id,
                          'size',
                          (e.currentTarget as HTMLInputElement).value,
                          (v) => patch(layer.id, { sizeMm: v }),
                          { min: 0.1 },
                        )}
                    />
                  </label>
                  <label>
                    <span>X (mm)</span>
                    <input
                      type="number"
                      step="0.5"
                      class:invalid={invalidKey === `${layer.id}:x`}
                      value={layer.origin.x}
                      oninput={(e) =>
                        commitNumber(
                          layer.id,
                          'x',
                          (e.currentTarget as HTMLInputElement).value,
                          (v) => patchOrigin(layer.id, 'x', v),
                        )}
                    />
                  </label>
                  <label>
                    <span>Y (mm)</span>
                    <input
                      type="number"
                      step="0.5"
                      class:invalid={invalidKey === `${layer.id}:y`}
                      value={layer.origin.y}
                      oninput={(e) =>
                        commitNumber(
                          layer.id,
                          'y',
                          (e.currentTarget as HTMLInputElement).value,
                          (v) => patchOrigin(layer.id, 'y', v),
                        )}
                    />
                  </label>
                  <!-- 245i: re-anchor the text origin to the bottom-left
                       of the current object selection's bbox — the
                       on-demand "source of text origin" control. -->
                  <button
                    type="button"
                    class="snap-origin"
                    onclick={() => snapOriginToSelection(layer.id)}
                    disabled={!selOrigin}
                    title={selOrigin
                      ? `Move origin to the bottom-left of the selection bbox (${selOrigin.x.toFixed(1)}, ${selOrigin.y.toFixed(1)})`
                      : 'Select geometry in the canvas first, then snap the text origin to its bottom-left corner'}
                  >
                    Snap origin to selection
                  </button>
                  <label>
                    <span>Rotation (°)</span>
                    <input
                      type="number"
                      step="5"
                      class:invalid={invalidKey === `${layer.id}:rot`}
                      value={layer.rotationDeg}
                      oninput={(e) =>
                        commitNumber(
                          layer.id,
                          'rot',
                          (e.currentTarget as HTMLInputElement).value,
                          (v) => patch(layer.id, { rotationDeg: v }),
                        )}
                    />
                  </label>
                  <label>
                    <span>Letter gap</span>
                    <input
                      type="number"
                      step="0.1"
                      class:invalid={invalidKey === `${layer.id}:gap`}
                      value={layer.letterSpacingMm}
                      oninput={(e) =>
                        commitNumber(
                          layer.id,
                          'gap',
                          (e.currentTarget as HTMLInputElement).value,
                          (v) => patch(layer.id, { letterSpacingMm: v }),
                        )}
                    />
                  </label>
                  <label title="Horizontal stretch (50–200 %). 100 % = natural font width.">
                    <span>Width %</span>
                    <input
                      type="number"
                      step="5"
                      min="50"
                      max="200"
                      class:invalid={invalidKey === `${layer.id}:width`}
                      value={Math.round(layer.widthScale * 100)}
                      oninput={(e) =>
                        commitNumber(
                          layer.id,
                          'width',
                          (e.currentTarget as HTMLInputElement).value,
                          (pct) => patch(layer.id, { widthScale: pct / 100 }),
                          { min: 50, max: 200 },
                        )}
                    />
                  </label>
                  <label class:hidden={layer.kind !== 'MTEXT'}>
                    <span>Line spacing</span>
                    <input
                      type="number"
                      step="0.5"
                      min="0"
                      class:invalid={invalidKey === `${layer.id}:line`}
                      value={layer.lineSpacingMm}
                      oninput={(e) =>
                        commitNumber(
                          layer.id,
                          'line',
                          (e.currentTarget as HTMLInputElement).value,
                          (v) => patch(layer.id, { lineSpacingMm: v }),
                          { min: 0 },
                        )}
                    />
                  </label>
                  <label>
                    <span>Align</span>
                    <select
                      value={layer.alignment}
                      onchange={(e) =>
                        patch(layer.id, {
                          alignment: (e.currentTarget as HTMLSelectElement).value as TextAlignment,
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
  /* Red-border cue for a rejected numeric entry. The global
     `:where(.field) input.invalid` rule doesn't reach here — these
     inputs sit in plain `<label>`s — so mirror it locally. */
  input.invalid {
    border-color: var(--danger);
  }
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
  /* Base `.group-head` / `.caret-btn` shapes live in app.css. */
  .group-head {
    grid-template-columns: auto 1fr auto auto;
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
    padding: 0 0.4rem;
    font-size: 0.72rem;
    line-height: 1.2;
    cursor: pointer;
    /* Don't push the row taller than the Stock / Layers headers. */
    min-height: 0;
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
    /* The sidebar accordion gives this host the active 1fr row and
       clips overflow on the host wrapper, so we don't need a second
       max-height cap here — see LayerList for the same fix. */
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
    /* WCAG ≥24×24 hit target — was padding: 0 0.3rem. */
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
    min-height: 1.6rem;
    font-family: inherit;
    overflow: hidden;
    white-space: nowrap;
  }
  .edit-form textarea.multiline {
    min-height: 4rem;
    overflow: auto;
    white-space: pre;
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
  /* 245i: full-width snap-origin button under the X/Y origin fields. */
  .edit-form .snap-origin {
    grid-column: 1 / -1;
    padding: 0.2rem 0.4rem;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .edit-form .snap-origin:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .edit-form .snap-origin:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
