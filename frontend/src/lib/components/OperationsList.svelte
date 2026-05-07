<script lang="ts">
  /// Operations list — the centerpiece of the new UX. Ordered list of
  /// CAM operations the program runs. Each row shows enabled-checkbox,
  /// op-kind icon, name, tool, and a status badge. Click selects the op
  /// (drives OpPropertiesPanel). Drag-handle reorders. + Add operation
  /// pops a kind picker.
  import { project, type OpEntry, type OpKind } from '../state/project.svelte';
  import OpPropertiesPanel from './OpPropertiesPanel.svelte';

  const KIND_LABEL: Record<OpKind, string> = {
    profile: 'Profile',
    pocket: 'Pocket',
    drill: 'Drill',
    thread: 'Thread',
    chamfer: 'Chamfer',
    engrave: 'Engrave',
    drag_knife: 'Drag-knife',
    helix: 'Helix',
  };
  const KIND_ICON: Record<OpKind, string> = {
    profile: '▢',
    pocket: '▣',
    drill: '◉',
    thread: '◎',
    chamfer: '◇',
    engrave: '✎',
    drag_knife: '✁',
    helix: '◎',
  };
  const ALL_KINDS: OpKind[] = [
    'profile', 'pocket', 'drill', 'thread', 'chamfer', 'engrave', 'drag_knife', 'helix',
  ];

  let pickerOpen = $state(false);
  let dragId = $state<number | null>(null);
  let dragOverId = $state<number | null>(null);

  function toolName(toolId: number): string {
    const t = project.tools.find((x) => x.id === toolId);
    return t ? t.name : `tool #${toolId}`;
  }

  function statusFor(op: OpEntry): { label: string; tone: 'ok' | 'warn' | 'bad'; reason: string } {
    if (!project.tools.find((t) => t.id === op.toolId)) {
      return { label: '✘', tone: 'bad', reason: `Tool #${op.toolId} is not in the project's tool library. Pick a tool in the operation properties.` };
    }
    if (!project.imported) {
      return { label: '⚠', tone: 'warn', reason: 'No drawing imported yet — open a DXF/SVG to apply this operation.' };
    }
    if (op.sourceObjects && op.sourceObjects.length > 0) {
      const have = project.imported.objects ?? [];
      const missing = op.sourceObjects.filter((id) => !have.includes(id));
      if (missing.length > 0) {
        return { label: '⚠', tone: 'warn', reason: `Source includes ${missing.length} object id(s) not present in the current import — they may have come from a different drawing.` };
      }
    }
    if (op.sourceLayers && op.sourceLayers.length > 0) {
      const knownLayers = new Set(project.imported.layers.map((l) => l.name));
      const missing = op.sourceLayers.filter((l) => !knownLayers.has(l));
      if (missing.length > 0) {
        return { label: '⚠', tone: 'warn', reason: `Source layer(s) "${missing.join(', ')}" not in this drawing.` };
      }
    }
    if (project.dirty) {
      return { label: '⚠', tone: 'warn', reason: 'Project changed since the last Generate — re-Generate to refresh this operation\'s gcode.' };
    }
    if (!project.generated) {
      return { label: '·', tone: 'warn', reason: 'Not generated yet — click Generate to produce this operation\'s gcode.' };
    }
    // Pipeline warnings tagged with this op's id (tool-fit, kind
    // mismatch, etc.) — escalate to the bad tone if a structural
    // problem (kind mismatch, impossible geometry); warn for fit issues.
    const opWarnings = (project.generated.warnings ?? []).filter((w) => w.op_id === op.id);
    if (opWarnings.length > 0) {
      const bad = opWarnings.find(
        (w) => w.kind === 'tool_kind_mismatch' || w.kind === 'tool_geometry_impossible',
      );
      const reason = opWarnings.map((w) => w.message).join('\n');
      return bad
        ? { label: '✘', tone: 'bad', reason }
        : { label: '⚠', tone: 'warn', reason };
    }
    return { label: '✓', tone: 'ok', reason: 'Up to date with the last Generate.' };
  }

  function selectOp(id: number) {
    project.selectedOpId = project.selectedOpId === id ? null : id;
  }

  function pick(kind: OpKind) {
    project.addOperation(kind);
    pickerOpen = false;
  }

  function onDragStart(e: DragEvent, id: number) {
    dragId = id;
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }
  function onDragOver(e: DragEvent, id: number) {
    if (dragId == null || dragId === id) return;
    e.preventDefault();
    dragOverId = id;
  }
  function onDrop(_e: DragEvent, id: number) {
    if (dragId == null) return;
    const targetIdx = project.operations.findIndex((o) => o.id === id);
    if (targetIdx >= 0) project.reorderOperation(dragId, targetIdx);
    dragId = null;
    dragOverId = null;
  }
  function onDragEnd() {
    dragId = null;
    dragOverId = null;
  }
</script>

<div class="ops">
  <header>
    <h3>Operations</h3>
    <button class="add" onclick={() => (pickerOpen = !pickerOpen)} title="Add operation">
      +
    </button>
  </header>

  {#if pickerOpen}
    <div class="picker" role="menu">
      {#each ALL_KINDS as k (k)}
        <button class="kind" role="menuitem" onclick={() => pick(k)}>
          <span class="ico" aria-hidden="true">{KIND_ICON[k]}</span>
          <span>{KIND_LABEL[k]}</span>
        </button>
      {/each}
    </div>
  {/if}

  {#if project.operations.length === 0}
    <p class="empty">No operations. Click <strong>+</strong> to add one.</p>
  {:else}
    <ul role="listbox">
      {#each project.operations as op (op.id)}
        {@const status = statusFor(op)}
        {@const selected = project.selectedOpId === op.id}
        {@const dragOver = dragOverId === op.id}
        <li class:selected class:drag-over={dragOver}>
          <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
          <div
            class="row"
            draggable="true"
            ondragstart={(e) => onDragStart(e, op.id)}
            ondragover={(e) => onDragOver(e, op.id)}
            ondrop={(e) => onDrop(e, op.id)}
            ondragend={onDragEnd}
            onclick={() => selectOp(op.id)}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') selectOp(op.id); }}
            role="option"
            tabindex="0"
            aria-selected={selected}
          >
            <span class="grip" title="Drag to reorder" aria-hidden="true">⋮⋮</span>
            <input
              type="checkbox"
              checked={op.enabled}
              onclick={(e) => e.stopPropagation()}
              onchange={(e) => project.updateOperation(op.id, { enabled: (e.currentTarget as HTMLInputElement).checked })}
            />
            <span class="caret" aria-hidden="true">{selected ? '▾' : '▸'}</span>
            <span class="ico" title={KIND_LABEL[op.kind]}>{KIND_ICON[op.kind]}</span>
            <span class="name">{op.name}</span>
            <span class="tool">{toolName(op.toolId)}</span>
            <span class="status {status.tone}" title={status.reason}>{status.label}</span>
            <button
              class="del"
              onclick={(e) => { e.stopPropagation(); project.removeOperation(op.id); }}
              title="Delete operation"
            >×</button>
          </div>
          {#if selected}
            <div class="props">
              <OpPropertiesPanel embedded />
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .ops {
    width: 100%;
    height: 100%;
    background: var(--bg-panel);
    color: var(--text);
    border-left: 1px solid var(--border);
    overflow-y: auto;
    padding: 0.55rem 0.6rem 0.8rem;
    box-sizing: border-box;
    min-width: 0;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.4rem;
  }
  h3 {
    margin: 0;
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
  }
  .add {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.05rem 0.5rem;
    font-size: 0.95rem;
    line-height: 1;
    cursor: pointer;
  }
  .picker {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.2rem;
    margin-bottom: 0.4rem;
    padding: 0.3rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 4px;
  }
  .kind {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    background: transparent;
    color: var(--text);
    border: 1px solid transparent;
    border-radius: 3px;
    padding: 0.2rem 0.4rem;
    font-size: 0.78rem;
    cursor: pointer;
    text-align: left;
  }
  .kind:hover {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    border-color: var(--accent);
  }
  .ico {
    font-size: 0.95rem;
    color: var(--accent-strong);
    width: 1rem;
    text-align: center;
  }
  .empty {
    color: var(--text-faint);
    font-size: 0.78rem;
    margin: 0.5rem 0;
  }
  ul {
    list-style: none;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 0.18rem;
  }
  li {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: var(--bg-elevated);
    font-size: 0.78rem;
  }
  li.selected {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
  }
  li.drag-over {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent) inset;
  }
  .row {
    display: grid;
    grid-template-columns: auto auto auto auto minmax(0, 1.4fr) minmax(0, 1fr) auto auto;
    gap: 0.3rem;
    align-items: center;
    padding: 0.25rem 0.35rem;
    cursor: pointer;
  }
  .grip {
    color: var(--text-faint);
    cursor: grab;
    font-size: 0.7rem;
    user-select: none;
    line-height: 1;
  }
  .caret {
    color: var(--text-muted);
    font-size: 0.7rem;
    width: 0.8rem;
    text-align: center;
  }
  .props {
    border-top: 1px solid var(--border);
    background: color-mix(in srgb, var(--accent) 4%, var(--bg-panel));
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-strong);
  }
  .tool {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
    font-size: 0.72rem;
  }
  .status {
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
    font-size: 0.78rem;
    min-width: 1.3rem;
    text-align: center;
  }
  .status.ok { color: var(--success); }
  .status.warn { color: var(--warn); }
  .status.bad { color: var(--error); }
  .del {
    background: transparent;
    color: var(--text-muted);
    border: 1px solid transparent;
    border-radius: 3px;
    padding: 0 0.35rem;
    cursor: pointer;
  }
  .del:hover {
    color: var(--error);
    border-color: var(--error);
  }
  input[type='checkbox'] {
    accent-color: var(--accent);
  }
</style>
