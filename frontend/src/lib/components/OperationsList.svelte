<script lang="ts">
  /// Operations list — the centerpiece of the new UX. Ordered list of
  /// CAM operations the program runs, organized into collapsible
  /// groups (rt1.21). Each row shows enabled-checkbox, op-kind icon,
  /// name, tool, and a status badge. Click selects the op (drives
  /// OpPropertiesPanel). Drag-handle reorders inside a group or
  /// drops onto a group header to move the op between groups.
  import { project, type OpEntry } from '../state/project.svelte';
  import OpPropertiesPanel from './OpPropertiesPanel.svelte';
  import OpKindPicker, {
    KIND_ICON,
    KIND_LABEL,
    PICKER_HELP,
    type PickerKind,
  } from './OpKindPicker.svelte';
  import { groupOperations, isGroupAllEnabled } from '../cam/op-grouping';

  let pickerOpen = $state(false);
  let dragId = $state<number | null>(null);
  let dragOverId = $state<number | null>(null);
  /// Group header the user is currently dragging an op over — drives
  /// the "drop here to move into this group" highlight. Empty string
  /// targets the implicit ungrouped bucket.
  let dragOverGroup = $state<string | null>(null);
  let collapsedGroups = $state<Set<string>>(new Set());
  /// When non-null, the group header with this name is in inline-
  /// rename mode (the header's text becomes an `<input>`).
  let renamingGroup = $state<string | null>(null);
  let renameDraft = $state('');
  /// When true, the "new group" inline input is shown at the bottom
  /// of the list. The user types a name and Enter to commit; the
  /// next-added op (or the currently-selected op) gets that group.
  let newGroupOpen = $state(false);
  let newGroupDraft = $state('');

  /// Local wrapper around the pure helper so the template can call
  /// it without re-deriving the type parameter.
  function groupedOperations() {
    return groupOperations(project.operations);
  }

  function isGroupCollapsed(name: string): boolean {
    return collapsedGroups.has(name);
  }

  function toggleGroupCollapsed(name: string) {
    const next = new Set(collapsedGroups);
    if (next.has(name)) next.delete(name);
    else next.add(name);
    collapsedGroups = next;
  }

  function startRenameGroup(name: string) {
    if (name === '') return; // can't rename the implicit ungrouped bucket
    renamingGroup = name;
    renameDraft = name;
  }

  function commitRename() {
    if (renamingGroup == null) return;
    const newName = renameDraft.trim();
    if (newName && newName !== renamingGroup) {
      project.renameGroup(renamingGroup, newName);
    }
    renamingGroup = null;
    renameDraft = '';
  }

  function cancelRename() {
    renamingGroup = null;
    renameDraft = '';
  }

  function commitNewGroup() {
    const name = newGroupDraft.trim();
    if (!name) {
      newGroupOpen = false;
      return;
    }
    // Apply to currently selected op; otherwise, no-op (the user can
    // drag an op onto the new header in a follow-up).
    if (project.selectedOpId != null) {
      project.setOpGroup(project.selectedOpId, name);
    } else {
      // No selection — at least show the empty group by mounting a
      // "ghost" bucket. We do this by adding the name to collapsedGroups
      // via a sentinel — no, simpler: just collapse it after creation
      // so the empty header is visible at the top until the user drags
      // something in.
      const next = new Set(collapsedGroups);
      next.delete(name);
      collapsedGroups = next;
      // Force the group to appear by stashing it in a "pending" set.
      pendingEmptyGroups = new Set(pendingEmptyGroups).add(name);
    }
    newGroupDraft = '';
    newGroupOpen = false;
  }

  /// Empty group placeholders — groups the user named via "+ New group"
  /// but hasn't moved any ops into yet. Rendered as a stub header so
  /// the user has a drop target for the next op. Cleared whenever
  /// the group gains a member.
  let pendingEmptyGroups = $state<Set<string>>(new Set());

  /// Svelte action: focus the element on mount. Replaces the
  /// `autofocus` attribute that svelte-check flags as an a11y
  /// no-no — for an inline rename / new-group input the focus is
  /// the explicit user-intended behavior.
  function focusOnMount(el: HTMLInputElement) {
    el.focus();
    el.select();
  }

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
      return { label: '⚠', tone: 'warn', reason: 'Project changed since the last Generate — re-Generate to refresh this operation\'s G-code.' };
    }
    if (!project.generated) {
      return { label: '·', tone: 'warn', reason: 'Not generated yet — click Generate to produce this operation\'s G-code.' };
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

  function pick(kind: PickerKind) {
    if (kind === 'pocket_outside') {
      addPocketOutside();
    } else {
      project.addOperation(kind);
    }
    pickerOpen = false;
  }

  /// Wrapper around addOperation('pocket') that pre-wires the
  /// SourceCombine::Difference + frame_shape params so the pipeline
  /// auto-derives the outer frame from the selection at generate time.
  function addPocketOutside() {
    if (project.selectedObjects.size === 0) return;
    const endmill = project.tools.find((t) => t.kind === 'endmill') ?? project.tools[0];
    const toolDiameter = endmill?.diameter ?? 3;
    // One Ctrl+Z reverts the whole "Pocket Outside" insert.
    project.history.beginTransaction('Add Pocket Outside');
    try {
      const op = project.addOperation('pocket');
      project.updateOperation(op.id, {
        name: 'Pocket Outside',
        toolId: endmill?.id ?? op.toolId,
        sourceLayers: null,
        sourceObjects: [...project.selectedObjects],
        sourceCombine: 'difference',
        frameShape: 'rectangle',
        framePaddingMm: 3 * toolDiameter,
        frameCornerRadiusMm: undefined,
      });
      project.history.commitTransaction();
    } catch (e) {
      project.history.cancelTransaction(project as unknown as never);
      throw e;
    }
  }

  function onDragStart(e: DragEvent, id: number) {
    dragId = id;
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }
  function onDragOver(e: DragEvent, id: number) {
    if (dragId == null || dragId === id) return;
    e.preventDefault();
    dragOverId = id;
    dragOverGroup = null;
  }
  function onDrop(_e: DragEvent, id: number) {
    if (dragId == null) return;
    // Same-group drop ⇒ pure reorder. Cross-group drop ⇒ move into
    // the target's group AND reposition next to it.
    const dragged = project.operations.find((o) => o.id === dragId);
    const target = project.operations.find((o) => o.id === id);
    if (dragged && target && (dragged.group ?? '') !== (target.group ?? '')) {
      project.setOpGroup(dragged.id, target.group);
    }
    const targetIdx = project.operations.findIndex((o) => o.id === id);
    if (targetIdx >= 0) project.reorderOperation(dragId, targetIdx);
    dragId = null;
    dragOverId = null;
    dragOverGroup = null;
  }
  function onDragEnd() {
    dragId = null;
    dragOverId = null;
    dragOverGroup = null;
  }
  /// Drag-over a group header: highlight the header and treat it as
  /// a "move into this group" target without reordering the dropped
  /// op within the group (it just goes to the end of the group's
  /// existing members).
  function onGroupDragOver(e: DragEvent, groupName: string) {
    if (dragId == null) return;
    e.preventDefault();
    dragOverGroup = groupName;
    dragOverId = null;
  }
  function onGroupDrop(_e: DragEvent, groupName: string) {
    if (dragId == null) return;
    const dragged = project.operations.find((o) => o.id === dragId);
    if (dragged && (dragged.group ?? '') !== groupName) {
      project.setOpGroup(dragged.id, groupName || undefined);
      // Drop the pendingEmptyGroups entry if the user just populated
      // a freshly-created group with its first member.
      if (groupName && pendingEmptyGroups.has(groupName)) {
        const next = new Set(pendingEmptyGroups);
        next.delete(groupName);
        pendingEmptyGroups = next;
      }
    }
    dragId = null;
    dragOverId = null;
    dragOverGroup = null;
  }
</script>

<div class="ops">
  <header>
    <h3>Operations</h3>
    <button
      class="add"
      onclick={() => (pickerOpen = !pickerOpen)}
      title="Add operation"
      aria-label="Add operation"
    >
      +
    </button>
  </header>

  {#if pickerOpen}
    <div class="picker-host">
      <OpKindPicker onPick={pick} />
    </div>
  {/if}

  {#if project.operations.length === 0}
    <div class="empty-card">
      <p class="empty-title">No operations yet</p>
      <p class="empty-sub">An operation tells the machine how to cut a region — pocket, contour, drill, engrave.</p>
      <button class="primary-cta" type="button" onclick={() => (pickerOpen = true)}>
        + Add operation
      </button>
    </div>
  {:else}
    {@const groups = groupedOperations()}
    {@const extraEmpty = [...pendingEmptyGroups].filter((g) => !groups.some((b) => b.name === g))}
    <ul role="listbox" class="groups-root">
      {#each [...extraEmpty.map((name) => ({ name, ops: [] as OpEntry[] })), ...groups] as bucket (bucket.name)}
        {@const collapsed = isGroupCollapsed(bucket.name)}
        {@const allEnabled = isGroupAllEnabled(bucket.ops)}
        {@const dragOverHere = dragOverGroup === bucket.name}
        {#if bucket.name !== '' || bucket.ops.length > 0 || groups.length > 1}
          <li class="group">
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
              class="group-head"
              class:drag-over={dragOverHere}
              ondragover={(e) => onGroupDragOver(e, bucket.name)}
              ondrop={(e) => onGroupDrop(e, bucket.name)}
            >
              <button
                class="caret-btn"
                onclick={() => toggleGroupCollapsed(bucket.name)}
                title={collapsed ? 'Expand group' : 'Collapse group'}
                aria-label="Toggle group {bucket.name || 'Default Ops Group'}"
              >{collapsed ? '▸' : '▾'}</button>
              {#if bucket.name !== ''}
                <input
                  type="checkbox"
                  checked={allEnabled}
                  title="Toggle every op in this group"
                  aria-label="Enable group {bucket.name}"
                  onclick={(e) => e.stopPropagation()}
                  onchange={(e) => project.setGroupEnabled(bucket.name, (e.currentTarget as HTMLInputElement).checked)}
                />
              {/if}
              {#if renamingGroup === bucket.name}
                <input
                  class="group-name-input"
                  type="text"
                  bind:value={renameDraft}
                  use:focusOnMount
                  onkeydown={(e) => {
                    if (e.key === 'Enter') commitRename();
                    else if (e.key === 'Escape') cancelRename();
                  }}
                  onblur={commitRename}
                />
              {:else}
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <span
                  class="group-name"
                  class:ungrouped={bucket.name === ''}
                  ondblclick={() => startRenameGroup(bucket.name)}
                  title={bucket.name === '' ? 'Ungrouped operations' : 'Double-click to rename'}
                >{bucket.name || 'Default Ops Group'}</span>
              {/if}
              <span class="group-count">{bucket.ops.length}</span>
              {#if bucket.name !== ''}
                <button
                  class="group-action"
                  onclick={() => startRenameGroup(bucket.name)}
                  title="Rename group"
                  aria-label="Rename group {bucket.name}"
                >✎</button>
                <button
                  class="group-action"
                  onclick={() => project.dissolveGroup(bucket.name)}
                  title="Dissolve group (members become ungrouped)"
                  aria-label="Dissolve group {bucket.name}"
                >×</button>
              {/if}
            </div>
            {#if !collapsed}
              <ul class="group-body" role="listbox">
                {#if bucket.ops.length === 0}
                  <li class="empty-group">
                    Empty group. Drop an op here or drag to populate.
                  </li>
                {/if}
                {#each bucket.ops as op (op.id)}
                  {@const status = statusFor(op)}
                  {@const selected = project.selectedOpId === op.id}
                  {@const dragOver = dragOverId === op.id}
                  <li class:selected class:drag-over={dragOver}>
                    <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
                    <div
                      class="row"
                      ondragover={(e) => onDragOver(e, op.id)}
                      ondrop={(e) => onDrop(e, op.id)}
                      onclick={() => selectOp(op.id)}
                      onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') selectOp(op.id); }}
                      role="option"
                      tabindex="0"
                      aria-selected={selected}
                    >
                      <!-- Only the grip initiates a drag. Putting
                           draggable=true on the row body hijacks
                           mousedown on buttons / checkboxes (the
                           browser starts a drag instead of firing
                           click), so duplicate / delete / enable
                           appear dead. -->
                      <!-- svelte-ignore a11y_no_static_element_interactions -->
                      <span
                        class="grip"
                        draggable="true"
                        ondragstart={(e) => onDragStart(e, op.id)}
                        ondragend={onDragEnd}
                        title="Drag to reorder or move between groups"
                        aria-hidden="true"
                      >⋮⋮</span>
                      <input
                        type="checkbox"
                        checked={op.enabled}
                        onclick={(e) => e.stopPropagation()}
                        onchange={(e) => project.updateOperation(op.id, { enabled: (e.currentTarget as HTMLInputElement).checked })}
                      />
                      <span class="caret" aria-hidden="true">{selected ? '▾' : '▸'}</span>
                      <span
                        class="ico"
                        title={`${KIND_LABEL[op.kind]} — ${PICKER_HELP[op.kind]}`}
                        aria-label={`${KIND_LABEL[op.kind]} — ${PICKER_HELP[op.kind]}`}
                      >{KIND_ICON[op.kind]}</span>
                      <span class="name">{op.name}</span>
                      <span class="tool">{toolName(op.toolId)}</span>
                      <span class="status {status.tone}" title={status.reason}>{status.label}</span>
                      <button
                        class="dup"
                        onclick={(e) => { e.stopPropagation(); project.duplicateOperation(op.id); }}
                        title="Duplicate"
                        aria-label={`Duplicate operation ${op.name}`}
                      >⎘</button>
                      <button
                        class="del"
                        onclick={(e) => { e.stopPropagation(); project.removeOperation(op.id); }}
                        title="Delete operation"
                        aria-label={`Delete operation ${op.name}`}
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
          </li>
        {/if}
      {/each}
    </ul>
    <div class="below-list">
      {#if newGroupOpen}
        <div class="new-group">
          <input
            type="text"
            placeholder="Group name"
            bind:value={newGroupDraft}
            use:focusOnMount
            onkeydown={(e) => {
              if (e.key === 'Enter') commitNewGroup();
              else if (e.key === 'Escape') { newGroupOpen = false; newGroupDraft = ''; }
            }}
            onblur={commitNewGroup}
          />
        </div>
      {:else}
        <button class="new-group-btn" onclick={() => (newGroupOpen = true)}>+ New group</button>
      {/if}
    </div>
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
  .picker-host {
    margin-bottom: 0.4rem;
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
  .empty-card {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0.35rem;
    padding: 0.75rem 0.7rem;
    margin: 0.4rem 0;
    border: 1px dashed var(--border);
    border-radius: 5px;
    background: color-mix(in srgb, var(--accent) 4%, var(--bg-panel));
    text-align: center;
  }
  .empty-title {
    margin: 0;
    color: var(--text-strong);
    font-size: 0.85rem;
    font-weight: 600;
  }
  .empty-sub {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.74rem;
    line-height: 1.3;
  }
  .primary-cta {
    margin-top: 0.4rem;
    background: var(--accent);
    color: #fff;
    border: 0;
    padding: 0.4rem 0.7rem;
    border-radius: 4px;
    font-size: 0.85rem;
    font-weight: 600;
    cursor: pointer;
  }
  .primary-cta:hover {
    background: var(--accent-strong);
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
    grid-template-columns: auto auto auto auto minmax(0, 1.4fr) minmax(0, 1fr) auto auto auto;
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
    padding: 0.25rem 0.15rem;
    border-radius: 2px;
  }
  .grip:hover {
    background: var(--bg);
    color: var(--text-muted);
  }
  .grip:active {
    cursor: grabbing;
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
  .del,
  .dup {
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
  .dup:hover {
    color: var(--accent-strong);
    border-color: var(--accent);
  }
  input[type='checkbox'] {
    accent-color: var(--accent);
  }
  /* rt1.21: group headers + bodies. */
  .groups-root {
    gap: 0.35rem;
  }
  .group {
    display: flex;
    flex-direction: column;
    background: transparent;
    border: 0;
  }
  .group-head {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr) auto auto auto;
    gap: 0.3rem;
    align-items: center;
    padding: 0.2rem 0.35rem;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: color-mix(in srgb, var(--accent) 6%, var(--bg-panel));
    font-size: 0.78rem;
  }
  .group-head.drag-over {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent) inset;
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
    cursor: text;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .group-name.ungrouped {
    color: var(--text-muted);
    font-weight: 500;
    font-style: italic;
    cursor: default;
  }
  .group-name-input {
    font-size: 0.82rem;
    padding: 0.1rem 0.3rem;
    background: var(--bg);
    border: 1px solid var(--accent);
    border-radius: 2px;
    color: var(--text-strong);
  }
  .group-count {
    color: var(--text-muted);
    font-size: 0.72rem;
    padding: 0 0.3rem;
    background: var(--bg);
    border-radius: 10px;
    line-height: 1.4;
  }
  .group-action {
    background: transparent;
    color: var(--text-muted);
    border: 1px solid transparent;
    border-radius: 3px;
    padding: 0 0.3rem;
    cursor: pointer;
    font-size: 0.78rem;
  }
  .group-action:hover {
    color: var(--accent-strong);
    border-color: var(--accent);
  }
  .group-body {
    list-style: none;
    padding: 0;
    margin: 0.15rem 0 0 0.5rem;
    display: grid;
    gap: 0.15rem;
    border-left: 2px solid color-mix(in srgb, var(--accent) 30%, transparent);
    padding-left: 0.3rem;
  }
  .empty-group {
    color: var(--text-faint);
    font-size: 0.74rem;
    font-style: italic;
    padding: 0.3rem 0.4rem;
    text-align: center;
  }
  .below-list {
    margin-top: 0.5rem;
  }
  .new-group-btn {
    background: transparent;
    color: var(--text-muted);
    border: 1px dashed var(--border);
    border-radius: 3px;
    padding: 0.2rem 0.6rem;
    font-size: 0.78rem;
    cursor: pointer;
    width: 100%;
  }
  .new-group-btn:hover {
    color: var(--accent-strong);
    border-color: var(--accent);
  }
  .new-group input {
    width: 100%;
    padding: 0.25rem 0.4rem;
    font-size: 0.85rem;
    border: 1px solid var(--accent);
    border-radius: 3px;
    background: var(--bg);
    color: var(--text-strong);
  }
</style>
