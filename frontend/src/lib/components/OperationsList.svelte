<script lang="ts">
  /// Operations list — ordered flat list of CAM operations the program
  /// runs. Each row shows an enabled checkbox, kind icon, name, tool,
  /// status badge, duplicate, and delete affordance. Click selects the
  /// op (drives the inline OpPropertiesPanel). Drag the grip to
  /// reorder.
  import { project, type OpEntry } from '../state/project.svelte';
  import { opSourceCss } from '../state/op-color';
  import { warningFocus } from '../state/warning-focus.svelte';
  import { isProgramOnlyOp } from '../state/op_types';
  import OpPropertiesPanel from './OpPropertiesPanel.svelte';
  import OpKindPicker, {
    KIND_ICON,
    KIND_LABEL,
    PICKER_HELP,
    type PickerKind,
  } from './OpKindPicker.svelte';

  interface Props {
    /// Accordion-controlled by the sidebar parent. `active` =
    /// expanded; clicking the header asks the parent to activate.
    active: boolean;
    onActivate: () => void;
  }
  let { active, onActivate }: Props = $props();

  let pickerOpen = $state(false);
  let dragId = $state<number | null>(null);
  let dragOverId = $state<number | null>(null);

  function toolName(toolId: number): string {
    const t = project.tools.find((x) => x.id === toolId);
    return t ? t.name : `tool #${toolId}`;
  }

  /// Per-render Set lookups for source-id / layer validity. Lifted out
  /// of statusFor() so a 5000-object DXF doesn't re-build them N times.
  /// Reads `transformedImport` so all imports' objects and layers are
  /// visible (combineImports namespaces object ids; layers union by name).
  let importedObjectsSet = $derived(
    project.transformedImport?.objects
      ? new Set(project.transformedImport.objects)
      : new Set<number>(),
  );
  let importedLayersSet = $derived(
    project.transformedImport?.layers
      ? new Set(project.transformedImport.layers.map((l) => l.name))
      : new Set<string>(),
  );

  /// eb8.7: orphan-source diagnostics for an op. Returns the object ids
  /// or layer names that the op points at but that the current import
  /// doesn't carry. Used by both the status chip and the inline
  /// 'Re-pick' shortcut on the row.
  function orphansFor(op: OpEntry): { objectIds: number[]; layers: string[] } {
    const objectIds = op.sourceObjects?.filter((id) => !importedObjectsSet.has(id)) ?? [];
    const layers = op.sourceLayers?.filter((l) => !importedLayersSet.has(l)) ?? [];
    return { objectIds, layers };
  }

  /// Replace this op's sourceObjects with the currently-selected
  /// objects. Convenience for the orphan recovery flow: after a source
  /// re-import that changed object ids, the user selects the new
  /// equivalents and clicks 'Re-pick'.
  function repickFromSelection(opId: number) {
    if (project.selectedObjects.size === 0) return;
    project.updateOperation(opId, {
      sourceObjects: [...project.selectedObjects],
      sourceLayers: null,
    });
  }

  function statusFor(op: OpEntry): { label: string; tone: 'ok' | 'warn' | 'bad'; reason: string } {
    // gseb: program-only ops (Pause, Homing, Probe, CycleMarker,
    // GcodeInclude) carry no tool / source / geometry by design.
    // They emit a small fixed gcode sequence (M0, G28, G38.2, a
    // comment marker, or an included file) and never touch a
    // spindle / cutter. The standard validation chain below would
    // mark them red for 'Tool #0 missing'. Short-circuit to OK so
    // the row reads as a deliberate program-flow building block
    // instead of a config error. Pre-gseb only the Pause branch
    // short-circuited; the other four kinds incorrectly tripped
    // the missing-tool red X (rt1.34 / 8n4k / rxm9 follow-up).
    if (op.kind === 'pause') {
      return {
        label: '✓',
        tone: 'ok',
        reason: 'Pause op — emits M0 at this slot. Operator presses Cycle Start to resume.',
      };
    }
    if (op.kind === 'homing') {
      return {
        label: '✓',
        tone: 'ok',
        reason:
          'Homing op — emits G28 at this slot. The controller seeks the home switches; no cutter required.',
      };
    }
    if (op.kind === 'probe') {
      return {
        label: '✓',
        tone: 'ok',
        reason:
          'Probe op — emits a G38.2 probe move at this slot. Touch probe required at the spindle; no cutter.',
      };
    }
    if (op.kind === 'cycle_marker') {
      return {
        label: '✓',
        tone: 'ok',
        reason:
          'Marker op — emits a comment-only `; --- <label> ---` line so pendants / G-code viewers can jump between sections. No motion, no cutter.',
      };
    }
    if (op.kind === 'gcode_include') {
      return {
        label: '✓',
        tone: 'ok',
        reason:
          'G-code include op — splices an external file into the program. The file may carry its own toolchange; no project tool required at this slot.',
      };
    }
    if (!project.tools.find((t) => t.id === op.toolId)) {
      return {
        label: '✘',
        tone: 'bad',
        reason: `Tool #${op.toolId} is not in the project's tool library. Pick a tool in the operation properties.`,
      };
    }
    if (!project.transformedImport) {
      return {
        label: '⚠',
        tone: 'warn',
        reason: 'No drawing imported yet — open a DXF/SVG to apply this operation.',
      };
    }
    if (op.sourceObjects && op.sourceObjects.length > 0) {
      const missing = op.sourceObjects.filter((id) => !importedObjectsSet.has(id));
      if (missing.length > 0) {
        return {
          label: '⚠',
          tone: 'warn',
          reason: `Source includes ${missing.length} object id(s) not present in the current import — they may have come from a different drawing.`,
        };
      }
    }
    if (op.sourceLayers && op.sourceLayers.length > 0) {
      const missing = op.sourceLayers.filter((l) => !importedLayersSet.has(l));
      if (missing.length > 0) {
        return {
          label: '⚠',
          tone: 'warn',
          reason: `Source layer(s) "${missing.join(', ')}" not in this drawing.`,
        };
      }
    }
    if (project.dirty) {
      return {
        label: '⚠',
        tone: 'warn',
        reason:
          "Project changed since the last Generate — re-Generate to refresh this operation's G-code.",
      };
    }
    if (!project.generated) {
      return {
        label: '·',
        tone: 'warn',
        reason: "Not generated yet — click Generate to produce this operation's G-code.",
      };
    }
    const opWarnings = (project.generated.warnings ?? []).filter((w) => w.op_id === op.id);
    if (opWarnings.length > 0) {
      const bad = opWarnings.find(
        (w) => w.kind === 'tool_kind_mismatch' || w.kind === 'tool_geometry_impossible',
      );
      const reason = opWarnings.map((w) => w.message).join('\n');
      return bad ? { label: '✘', tone: 'bad', reason } : { label: '⚠', tone: 'warn', reason };
    }
    return { label: '✓', tone: 'ok', reason: 'Up to date with the last Generate.' };
  }

  /// 4kzy: true when this op has at least one warning that's listed in
  /// the warnings panel (pipeline `generated.warnings` keyed by op_id),
  /// so the status badge can act as a "show it in the panel" button.
  /// Status tones from non-panel reasons (missing tool, orphan source)
  /// stay a plain tooltip — there's nothing to reveal in the panel.
  function opHasPanelWarning(op: OpEntry): boolean {
    return (project.generated?.warnings ?? []).some((w) => w.op_id === op.id);
  }

  function selectOp(id: number) {
    const wasSelected = project.selectedOpId === id;
    project.selectedOpId = wasSelected ? null : id;
    if (!wasSelected) {
      // gucf: highlight the op's source objects on the canvas so the
      // user can see what this op will cut. Only act when sourceObjects
      // is explicitly set — when undefined the op runs over "all" or
      // "all in these layers", and overwriting the canvas selection
      // with that potentially-huge set would surprise mid-edit.
      const op = project.operations.find((o) => o.id === id);
      if (op?.sourceObjects && op.sourceObjects.length > 0) {
        project.selectObjects(op.sourceObjects, 'replace');
      }
      queueMicrotask(() => {
        const row = document.querySelector(`[data-op-row-id="${id}"]`) as HTMLElement | null;
        row?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
      });
    }
  }

  function pick(kind: PickerKind) {
    if (kind === 'pocket_outside') {
      addPocketOutside();
    } else {
      project.addOperation(kind);
    }
    pickerOpen = false;
  }

  /// Pocket-Outside is the only kind whose default params need more
  /// than `addOperation` provides — pre-wire the SourceCombine +
  /// frame_shape so the pipeline auto-derives a rectangular frame from
  /// the current selection at generate time.
  function addPocketOutside() {
    if (project.selectedObjects.size === 0) return;
    const endmill = project.tools.find((t) => t.kind === 'endmill') ?? project.tools[0];
    const toolDiameter = endmill?.diameter ?? 3;
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
      project.cancelTransaction();
      throw e;
    }
  }

  function onDragStart(e: DragEvent, id: number) {
    dragId = id;
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = 'move';
      // WebKit requires a setData call on dragstart for `drop` to fire.
      try {
        e.dataTransfer.setData('text/plain', String(id));
      } catch {
        /* old engines may throw inside drag handlers; harmless */
      }
    }
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

  /// Listbox arrow-key nav with Alt-Up/Down to reorder rows (the
  /// drag-grip's mouse-only counterpart, audit xc3a). Roving tabindex
  /// on each row: only the selected op is in the tab order.
  function onListKey(e: KeyboardEvent) {
    const ops = project.operations;
    if (ops.length === 0) return;
    const curIdx = Math.max(
      0,
      ops.findIndex((o) => o.id === project.selectedOpId),
    );
    // Alt + Arrow: reorder — keyboard counterpart to drag-grip. Checked
    // BEFORE the bare-arrow navigation branches below, because both keys
    // overlap and the bare branches would otherwise swallow the event.
    if (e.altKey && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      const dir = e.key === 'ArrowDown' ? 1 : -1;
      const dest = Math.max(0, Math.min(ops.length - 1, curIdx + dir));
      if (dest !== curIdx) {
        project.reorderOperation(ops[curIdx].id, dest);
        e.preventDefault();
      }
      return;
    }
    let nextIdx = curIdx;
    if (e.key === 'ArrowDown') nextIdx = Math.min(ops.length - 1, curIdx + 1);
    else if (e.key === 'ArrowUp') nextIdx = Math.max(0, curIdx - 1);
    else if (e.key === 'Home') nextIdx = 0;
    else if (e.key === 'End') nextIdx = ops.length - 1;
    else return;
    e.preventDefault();
    selectOp(ops[nextIdx].id);
    // Move focus onto the newly-selected row to match the tabindex flip.
    queueMicrotask(() => {
      (e.currentTarget as HTMLElement | null)
        ?.querySelector<HTMLElement>(`[data-op-row-id="${ops[nextIdx].id}"] [role="option"]`)
        ?.focus();
    });
  }
</script>

<div class="ops" class:collapsed={!active}>
  <!-- audit xc3a: dropped nested `role="button" tabindex="0"` on the
       header — the caret + add buttons inside are already focusable, so
       the wrapper-button created duplicate Tab stops + an Enter shortcut
       that overlapped with the caret's own activate behavior.
       audit o1od: shape now matches TextList/Stock .group-head — grid
       layout with accent-tinted bg + border so the three accordion
       panels read as one design language. -->
  <!-- a11y note (hmrc): the wrapper div is a mouse-convenience click
       area for the whole header row. Keyboard activation lives on the
       inner .caret-btn (focusable <button> that calls onActivate); this
       wrapper is intentionally not in the Tab sequence — per the audit-
       xc3a comment above — so it doesn't create a duplicate Tab stop or
       Enter shortcut that overlaps with the caret's own activation. -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="group-head" onclick={onActivate}>
    <button
      class="caret-btn"
      onclick={(e) => {
        e.stopPropagation();
        onActivate();
      }}
      title={active ? 'Collapse operations (return to previous panel)' : 'Expand operations panel'}
      aria-label={active ? 'Collapse operations panel' : 'Activate operations panel'}
    >
      {active ? '▾' : '▸'}
    </button>
    <span class="group-name">Operations</span>
    <span class="group-count" title="Number of operations">{project.operations.length}</span>
    <button
      class="add-btn"
      onclick={(e) => {
        e.stopPropagation();
        if (!active) onActivate();
        pickerOpen = !pickerOpen;
      }}
      title="Add operation"
      aria-label="Add operation"
    >
      +
    </button>
  </div>

  {#if active}
    {#if pickerOpen}
      <div class="picker-host">
        <OpKindPicker onPick={pick} />
      </div>
    {/if}

    {#if project.operations.length === 0}
      <div class="empty-card">
        <p class="empty-title">No operations yet</p>
        <p class="empty-sub">
          An operation tells the machine how to cut a region — pocket, contour, drill, engrave.
        </p>
        <button class="primary-cta" type="button" onclick={() => (pickerOpen = true)}>
          + Add operation
        </button>
      </div>
    {:else}
      <ul role="listbox" class="ops-list" tabindex="-1" onkeydown={onListKey}>
        {#each project.operations as op (op.id)}
          {@const status = statusFor(op)}
          {@const selected = project.selectedOpId === op.id}
          {@const dragOver = dragOverId === op.id}
          {@const orphans = orphansFor(op)}
          {@const hasOrphans = orphans.objectIds.length > 0 || orphans.layers.length > 0}
          <li
            class:selected
            class:drag-over={dragOver}
            class:op-disabled={!op.enabled}
            data-op-row-id={op.id}
          >
            <div
              class="row"
              ondragover={(e) => onDragOver(e, op.id)}
              ondrop={(e) => onDrop(e, op.id)}
              onclick={() => selectOp(op.id)}
              onkeydown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') selectOp(op.id);
              }}
              role="option"
              tabindex={selected ? 0 : -1}
              aria-selected={selected}
            >
              <span
                class="grip"
                draggable="true"
                ondragstart={(e) => onDragStart(e, op.id)}
                ondragend={onDragEnd}
                title="Drag to reorder · Alt+Up / Alt+Down with the row focused does the same from the keyboard"
                aria-hidden="true">⋮⋮</span
              >
              <input
                type="checkbox"
                checked={op.enabled}
                onclick={(e) => e.stopPropagation()}
                onchange={(e) =>
                  project.updateOperation(op.id, {
                    enabled: (e.currentTarget as HTMLInputElement).checked,
                  })}
              />
              <span class="caret" aria-hidden="true">{selected ? '▾' : '▸'}</span>
              <span
                class="ico"
                title={`${KIND_LABEL[op.kind]} — ${PICKER_HELP[op.kind]}`}
                aria-label={`${KIND_LABEL[op.kind]} — ${PICKER_HELP[op.kind]}`}
                style:color={op.kind === 'pause' ? null : opSourceCss(op.id, selected)}
                >{KIND_ICON[op.kind]}</span
              >
              <span class="name">{op.name}</span>
              <span class="tool"
                >{#if isProgramOnlyOp(op.kind)}
                  <!-- gseb: program-only ops carry no cutter. Render
                       a dash with the kind label so the row reads as
                       a deliberate program-flow building block instead
                       of an unconfigured cutting op. -->
                  — {KIND_LABEL[op.kind]?.toLowerCase() ?? op.kind} —
                {:else}
                  {toolName(op.toolId)}
                {/if}</span
              >
              {#if opHasPanelWarning(op)}
                <button
                  type="button"
                  class="status {status.tone} status-btn"
                  title={`${status.reason}\n\n(click to show in the warnings panel)`}
                  aria-label={`Show warnings for ${op.name} in the warnings panel`}
                  onclick={(e) => {
                    e.stopPropagation();
                    warningFocus.focus(op.id);
                  }}>{status.label}</button
                >
              {:else}
                <span class="status {status.tone}" title={status.reason}>{status.label}</span>
              {/if}
              {#if hasOrphans}
                <button
                  class="repick"
                  disabled={project.selectedObjects.size === 0}
                  onclick={(e) => {
                    e.stopPropagation();
                    repickFromSelection(op.id);
                  }}
                  title={project.selectedObjects.size === 0
                    ? `Source references ${orphans.objectIds.length} object id(s)${orphans.layers.length ? ` + ${orphans.layers.length} layer(s)` : ''} that no longer exist. Select objects on the canvas, then click Re-pick to attach them to this op.`
                    : `Replace this op's source with the ${project.selectedObjects.size} currently-selected object${project.selectedObjects.size === 1 ? '' : 's'}.`}
                  aria-label={`Re-pick source for operation ${op.name}`}
                >
                  Re-pick
                </button>
              {/if}
              <button
                class="dup"
                onclick={(e) => {
                  e.stopPropagation();
                  project.duplicateOperation(op.id);
                }}
                title="Duplicate"
                aria-label={`Duplicate operation ${op.name}`}>⎘</button
              >
              <button
                class="del"
                onclick={(e) => {
                  e.stopPropagation();
                  project.removeOperation(op.id);
                }}
                title="Delete operation"
                aria-label={`Delete operation ${op.name}`}>×</button
              >
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
  /* When collapsed by the accordion, the panel shrinks to its header
     strip — no body padding, no scroll bar. */
  .ops.collapsed {
    overflow-y: visible;
    padding: 0.25rem 0.6rem;
    height: auto;
  }
  .ops.collapsed .group-head {
    margin-bottom: 0;
  }
  /* Base shape lives in app.css `.group-head` / `.caret-btn`; only the
     per-panel grid + bottom margin are local. */
  .group-head {
    grid-template-columns: auto 1fr auto auto;
    margin-bottom: 0.4rem;
  }
  .group-name {
    color: var(--text-strong);
    font-weight: 600;
  }
  .group-count {
    color: var(--text-muted);
    font-size: 0.72rem;
    padding: 0 0.3rem;
    background: var(--bg-app);
    border-radius: 10px;
    line-height: 1.4;
  }
  .add-btn {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.05rem 0.5rem;
    font-size: 0.95rem;
    line-height: 1;
    cursor: pointer;
  }
  .add-btn:hover {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    border-color: var(--accent);
    color: var(--text-strong);
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
    font-size: 0.82rem;
    font-weight: 600;
  }
  .empty-sub {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.72rem;
    line-height: 1.3;
  }
  .primary-cta {
    margin-top: 0.3rem;
    background: var(--accent);
    color: #fff;
    border: 0;
    padding: 0.35rem 0.7rem;
    border-radius: 4px;
    font-size: 0.82rem;
    font-weight: 600;
    cursor: pointer;
  }
  .primary-cta:hover {
    background: var(--accent-strong);
  }
  ul.ops-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }
  li {
    margin: 0;
  }
  li.op-disabled .row {
    opacity: 0.55;
  }
  .row {
    display: grid;
    grid-template-columns: auto auto auto auto minmax(0, 1fr) minmax(0, auto) auto auto auto;
    align-items: center;
    gap: 0.35rem;
    padding: 0.25rem 0.4rem;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: var(--bg-elevated);
    cursor: pointer;
    font-size: 0.78rem;
  }
  .row:hover {
    background: color-mix(in srgb, var(--accent) 8%, var(--bg-elevated));
  }
  li.selected > .row {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-elevated));
    color: var(--text-strong);
  }
  li.drag-over > .row {
    border-top: 2px solid var(--accent);
  }
  .grip {
    cursor: grab;
    color: var(--text-muted);
    font-size: 0.85rem;
    line-height: 0.8;
    padding: 0 0.15rem;
  }
  .grip:hover {
    color: var(--text);
  }
  .caret {
    color: var(--text-muted);
    width: 0.8rem;
    font-size: 0.7rem;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tool {
    color: var(--text-muted);
    font-size: 0.72rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .status {
    width: 1rem;
    text-align: center;
    line-height: 1;
    font-size: 0.85rem;
    font-weight: 600;
  }
  .status.ok {
    color: var(--success);
  }
  .status.warn {
    color: var(--warn);
  }
  .status.bad {
    color: var(--error);
  }
  /* 4kzy: status badge as a button when it links to a panel warning. */
  .status-btn {
    background: transparent;
    border: 0;
    padding: 0;
    cursor: pointer;
  }
  .status-btn:hover {
    filter: brightness(1.25);
  }
  .status-btn:focus-visible {
    outline: 1px solid var(--accent);
    outline-offset: 1px;
    border-radius: 2px;
  }
  .dup,
  .del {
    /* WCAG ≥24×24 hit target — was padding: 0 0.25rem on a 0.9 rem
       glyph, ~14-18 px tall depending on row gap. */
    background: transparent;
    border: 0;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 0.9rem;
    line-height: 1;
    padding: 0;
    min-width: 24px;
    min-height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 3px;
  }
  .dup:hover {
    color: var(--text);
    background: var(--bg-elevated);
  }
  .del:hover {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 12%, transparent);
  }
  /* eb8.7: inline Re-pick shortcut on rows whose source references
     objects / layers that no longer exist in the current import.
     Disabled state surfaces a tooltip telling the user to select
     objects first. */
  .repick {
    background: color-mix(in srgb, var(--warn) 22%, transparent);
    color: var(--text-strong);
    border: 1px solid color-mix(in srgb, var(--warn) 50%, var(--border));
    border-radius: 3px;
    padding: 0.05rem 0.4rem;
    font-size: 0.7rem;
    line-height: 1.4;
    cursor: pointer;
    white-space: nowrap;
  }
  .repick:hover:not(:disabled) {
    background: color-mix(in srgb, var(--warn) 35%, transparent);
  }
  .repick:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .props {
    margin: 0.2rem 0 0.4rem 0.5rem;
    padding-left: 0.3rem;
    border-left: 2px solid color-mix(in srgb, var(--accent) 35%, transparent);
  }
  input[type='checkbox'] {
    accent-color: var(--accent);
  }
</style>
