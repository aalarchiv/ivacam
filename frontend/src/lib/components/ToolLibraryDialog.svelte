<script lang="ts">
  /// Tool library dialog. Project-scoped table of every tool the user
  /// has configured; ops will reference an entry by id once UX-7 lands.
  /// Each row is editable in place; the modal commits/cancels as a
  /// single unit so the user can revert without touching project.data.tools.
  import {
    project,
    type ToolEntry,
    type ToolKind,
    type CoolantMode,
    type HolderShape,
  } from '../state/project.svelte';
  import { untrack } from 'svelte';
  import { t } from '../i18n';
  import Modal from './Modal.svelte';
  import { DialogDraft } from './dialog-draft.svelte';
  import * as fileOps from '../services/file_ops';
  import {
    attrApplies,
    effectiveModes,
    KIND_DISPLAY_LABELS,
    MACHINE_MODE_NOUN,
    machineModesLabel,
    TOOL_COMPATIBLE_MODES,
    toolCompatibleWithAnyMode,
  } from '../state/tool_family';
  import { workspace } from '../state/workspace.svelte';
  import { seedInventoryFromProject, syncStockedFromInventory } from '../state/tool_inventory';
  import { isAutoToolName, suggestToolName } from '../state/tool_naming';
  import ToolFormProfileEditor from './ToolFormProfileEditor.svelte';
  import {
    applyToolTableView,
    EMPTY_TOOL_VIEW,
    nextSortState,
    pageOfTool,
    paginateToolRows,
    type ToolSortKey,
    type ToolTableView,
  } from '../state/tool_table';
  import { defaultToolForMode } from '../state/tool_mode_defaults';
  import ToolCalibrationDialog from './ToolCalibrationDialog.svelte';
  import { effectiveDiameterHint, isCalibrationStale } from '../state/tool_wear';
  import {
    diameterInvalid,
    speedInvalid,
    feedInvalid,
    plungeInvalid,
    rowInvalid,
    fieldApplies,
    fieldDisabledReason,
    kindNeedsExpansion,
  } from '../state/tool_validation';

  interface Props {
    open: boolean;
    onClose: () => void;
    /// Render as a first-class tab panel instead of a modal: no Modal
    /// wrapper, no × / Cancel (the component stays mounted across tab
    /// switches, so an in-progress draft survives), footer becomes
    /// Apply / Revert.
    embedded?: boolean;
    /// Backing store: 'project' edits the working tool set of the
    /// current project/machine (the legacy modal behavior); 'inventory'
    /// edits the workspace-level SHOP inventory — every tool the user
    /// owns. Inventory commits propagate into same-id stocked copies in
    /// the project, so "the 6 mm endmill" stays one tool everywhere.
    source?: 'project' | 'inventory';
  }
  let { open, onClose, embedded = false, source = 'project' }: Props = $props();
  const active = $derived(open || embedded);
  const isInventory = $derived(source === 'inventory');
  /// The list this editor seeds from / commits to.
  const backingTools = $derived.by(() => {
    if (source === 'inventory') {
      void workspace.version;
      return workspace.get().tool_inventory;
    }
    return project.data.tools;
  });

  /// Draft / pristine / dirty / discard lifecycle lives in DialogDraft
  /// so X / Esc / click-outside can prompt before silently discarding
  /// edits. The `draft` alias keeps the table markup terse — row
  /// rebuilds always reassign `dd.draft`, never the alias.
  const dd = new DialogDraft<ToolEntry[]>();
  const draft = $derived(dd.draft ?? []);
  /// Per-row UI flag — Holder sub-panel collapsed by default to keep the
  /// table compact. Stored as a Set of row ids so reorders / additions
  /// don't accidentally move the toggle to a different tool.
  let expanded = $state<Set<number>>(new Set());
  /// Tool id that flashes briefly when the dialog is opened with a focus
  /// request (the "edit this tool" link in OpPropertiesPanel).
  let highlightedId = $state<number | null>(null);
  let bodyEl = $state<HTMLDivElement | null>(null);
  /// Mode filter: the default view shows only tools the machine's
  /// EFFECTIVE mode set (primary mode + capabilities — a combo
  /// mill+plasma machine keeps both halves visible) can run; the
  /// "N hidden — Show all" row reveals the rest. View-only — a mode
  /// switch never mutates the library.
  let showIncompatible = $state(false);
  /// Row index whose wear calibration dialog is open, or null.
  let calibratingIdx = $state<number | null>(null);
  function applyCalibration(idx: number, wearOffsetMm: number, dateIso: string) {
    dd.draft = draft.map((t, i) =>
      i === idx
        ? {
            ...t,
            wearOffsetMm: wearOffsetMm === 0 ? undefined : wearOffsetMm,
            lastCalibrated: dateIso,
          }
        : t,
    );
  }
  const machineModes = $derived(effectiveModes(project.data.machine));
  const incompatibleCount = $derived(
    isInventory ? 0 : draft.filter((t) => !toolCompatibleWithAnyMode(t.kind, machineModes)).length,
  );
  function rowVisible(tool: ToolEntry): boolean {
    if (isInventory) return true; // the shop inventory is machine-agnostic
    return showIncompatible || toolCompatibleWithAnyMode(tool.kind, machineModes);
  }

  // ── table view: sort / filter / pagination ─────────────────────────
  // View-only — never reorders the stored library. Rows are wrapped as
  // { tool, i } with i = the ORIGINAL draft index, so every edit
  // handler keeps mutating the right entry regardless of sort order.
  let view = $state<ToolTableView>({ ...EMPTY_TOOL_VIEW });
  let page = $state(0);
  const viewedRows = $derived(
    applyToolTableView(
      draft.map((tool, i) => ({ tool, i })).filter((r) => rowVisible(r.tool)),
      view,
    ),
  );
  const paged = $derived(paginateToolRows(viewedRows, page));
  const filtersActive = $derived(view.query.trim() !== '' || view.kind !== '' || view.mode !== '');
  function setSort(key: ToolSortKey) {
    const next = nextSortState(view, key);
    view.sortKey = next.sortKey;
    view.sortDir = next.sortDir;
  }
  function sortArrow(key: ToolSortKey): string {
    if (view.sortKey !== key) return '';
    return view.sortDir === 'asc' ? ' ▲' : ' ▼';
  }
  function clearFilters() {
    view.query = '';
    view.kind = '';
    view.mode = '';
    page = 0;
  }
  /// Make a specific tool visible: drop filters that would hide it and
  /// jump to its page under the current sort.
  function revealTool(id: number) {
    clearFilters();
    const rows = applyToolTableView(
      draft.map((tool, i) => ({ tool, i })).filter((r) => rowVisible(r.tool)),
      { ...view, query: '', kind: '', mode: '' },
    );
    page = pageOfTool(rows, id) ?? 0;
  }

  $effect(() => {
    if (!active) return;
    // Tracked deps: ONLY the backing store (deep snapshot) + the
    // project tools the inventory seeds from. Everything below runs
    // untracked — the previous version read dd.isDirty (which
    // deep-reads dd.draft) and then WROTE dd.draft via dd.open(), so
    // every clone write re-invalidated the effect: an infinite loop
    // that froze the whole app the moment the tab mounted.
    const backing = $state.snapshot(backingTools) as ToolEntry[];
    const projectTools = $state.snapshot(project.data.tools) as ToolEntry[];
    untrack(() => {
      // Embedded panels stay mounted, so external tool changes (undo,
      // stocking from the Machine tab) re-run this — refresh a CLEAN
      // draft to stay in sync, but never clobber in-progress edits.
      if (embedded && dd.isDirty) return;
      let tools = backing;
      if (isInventory && tools.length === 0 && projectTools.length > 0) {
        // First use of the shop inventory on an installation that
        // predates it: seed from the current project's tools so the
        // user starts from what they already configured. Deferred —
        // workspace.version is $state and must not bump synchronously
        // inside an effect body.
        const seeded = seedInventoryFromProject(projectTools);
        queueMicrotask(() => workspace.setToolInventory(seeded));
        tools = seeded;
      }
      dd.open(tools);
      showIncompatible = false;
      calibratingIdx = null;
      // Tools whose kind has a REQUIRED kind-specific field open by
      // default so the user sees `dragoff` / `cornerRadiusMm` / T-slot
      // neck dims without hunting for them. Other kinds start collapsed.
      expanded = new Set(tools.filter((t) => kindNeedsExpansion(t.kind)).map((t) => t.id));
    });
  });

  // Numeric-field validation, fieldApplies, and the per-kind
  // disabled-reason tooltips all live in lib/state/tool_validation.ts;
  // the dialog wires them in via the imports up top.
  let hasInvalidRow = $derived(draft.some(rowInvalid));

  /// Close protocol (dd.requestClose): the first attempt on a dirty
  /// draft arms the inline "Discard / Keep editing" footer pair; the
  /// second confirms. The inline bar replaces the prior `window.confirm`
  /// prompt, which silently returns false in some Tauri / WebKitGTK
  /// builds (audit-C10).
  function close() {
    if (dd.requestClose()) onClose();
  }

  $effect(() => {
    const focusId = project.sel.toolsDialogFocusId;
    if (!active || focusId == null) return;
    // The focus target may be mode-filtered out (an op still
    // referencing a mill tool on a plasma machine) — reveal it.
    const target = draft.find((t) => t.id === focusId);
    if (target && !rowVisible(target)) showIncompatible = true;
    if (target) revealTool(focusId);
    queueMicrotask(() => {
      const host = bodyEl;
      if (!host) return;
      const row = host.querySelector(`[data-tool-id="${focusId}"]`) as HTMLElement | null;
      if (row) row.scrollIntoView({ block: 'center', behavior: 'smooth' });
      highlightedId = focusId;
      window.setTimeout(() => {
        if (highlightedId === focusId) highlightedId = null;
      }, 1400);
    });
  });

  function commit() {
    // Refuse to commit while any row has an invalid numeric field. The
    // OK button is also disabled in that state — belt-and-braces so a
    // keyboard / programmatic invocation can't smuggle a zero-rate tool
    // through.
    if (draft.some(rowInvalid)) return;
    // Deep-snapshot so the command system receives plain objects —
    // Svelte 5 `$state` proxies inside `draft[i]` can trip up the
    // `structuredClone` call inside replaceToolsCommand on some
    // production builds, which would silently abort and leave the
    // dialog open.
    if (draft.length > 0) {
      const snap = JSON.parse(JSON.stringify(draft)) as typeof draft;
      try {
        if (isInventory) {
          workspace.setToolInventory(snap);
          // Propagate the edits into same-id stocked copies so the
          // machine's loadout (and its profile, via the mirror)
          // follows the inventory definition. One undoable step.
          const synced = syncStockedFromInventory(
            snap,
            JSON.parse(JSON.stringify(project.data.tools)) as typeof draft,
          );
          if (synced) project.replaceTools(synced);
        } else {
          project.replaceTools(snap);
        }
      } catch (e) {
        console.error('ToolLibraryDialog.commit: apply failed', e);
      }
    }
    // Embedded (tab) mode: Apply commits and stays — re-baseline the
    // draft instead of closing.
    if (embedded) dd.markClean();
    else onClose();
  }

  /// Embedded-mode Revert: drop the draft back to the committed tools.
  function revert() {
    dd.open(backingTools);
  }

  function addTool() {
    const nextId = (draft.reduce((m, t) => Math.max(m, t.id), 0) || 0) + 1;
    // Seed the PRIMARY mode's signature kind — a new tool on a plasma
    // machine starts as a torch, not an endmill the mode filter would
    // immediately hide.
    dd.draft = [...draft, defaultToolForMode(project.data.machine.mode, nextId)];
    revealTool(nextId);
  }

  function removeAt(idx: number) {
    if (draft.length <= 1) return;
    dd.draft = draft.filter((_, i) => i !== idx);
  }

  function updateField<K extends keyof ToolEntry>(idx: number, key: K, value: ToolEntry[K]) {
    dd.draft = draft.map((t, i) => {
      if (i !== idx) return t;
      // Auto-naming, editor-autocomplete style: while a row's name is
      // empty or still equals its own suggestion, setting edits keep
      // the name in sync ("3mm endmill" follows the diameter). A name
      // the user typed is never rewritten.
      const wasAuto = key !== 'name' && isAutoToolName(t);
      const next = { ...t, [key]: value };
      if (wasAuto) next.name = suggestToolName(next);
      return next;
    });
  }

  /// Per-kind default fill-in on `kind` change. Pre-populates the
  /// fields that newly APPLY to the target kind so the user doesn't
  /// see blank inputs when flipping endmill → drill (twist drills
  /// usually have 2 flutes and a 118° tip). Existing user-set values
  /// are preserved.
  function onKindChange(idx: number, kind: ToolKind) {
    let touchedId: number | null = null;
    dd.draft = draft.map((t, i) => {
      if (i !== idx) return t;
      const wasAuto = isAutoToolName(t);
      const next: ToolEntry = { ...t, kind };
      if (wasAuto) next.name = suggestToolName(next);
      if (kind === 'drill') {
        if (next.flutes === 0 || next.flutes === undefined) next.flutes = 2;
        if (next.tipAngleDeg === undefined) next.tipAngleDeg = 118;
      }
      if (
        (kind === 'v_bit' || kind === 'engraver' || kind === 'cone') &&
        next.tipAngleDeg === undefined
      ) {
        // Cone bits are commonly steeper than engraving V-bits; 30° is a
        // sensible cone default vs 60° for V/engrave.
        next.tipAngleDeg = kind === 'cone' ? 30 : 60;
      }
      if (kind === 'thread_mill') {
        // Thread mill: tipAngleDeg is the thread flank angle (60° metric
        // / 55° Whitworth); seed a 1 mm pitch and the metric flank.
        if (next.tipAngleDeg === undefined) next.tipAngleDeg = 60;
        if (next.threadPitchMm === undefined) next.threadPitchMm = 1.0;
      }
      touchedId = next.id;
      return next;
    });
    // Open the expanded section so the new kind's required kind-specific
    // field is in view (e.g. dragoff for drag_knife).
    if (touchedId != null && kindNeedsExpansion(kind)) {
      const next = new Set(expanded);
      next.add(touchedId);
      expanded = next;
    }
  }

  function toggleExpanded(id: number) {
    const next = new Set(expanded);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    expanded = next;
  }

  /// Pre-baked dimensions for common tool-holder taper sizes. Numbers are
  /// the bounding-cylinder + bounding-cone of the typical ER nut + collet
  /// stack mounted in a standard spindle. Conservative — actual hardware
  /// varies a few mm across vendors. Picking the right preset is the
  /// fastest way to populate the holder spec; users can always edit the
  /// fields after.
  ///
  ///  - ER11 / ER16 / ER20: bounding cone of nut+spindle for the named
  ///    collet size. Lengths are total stick-out from the spindle face.
  ///  - Direct shank: no holder above the shank — just sets the shank
  ///    diameter to the cutting diameter and clears the holder.
  ///  - No holder: clears every holder field, restoring legacy behavior.
  type Preset = {
    label: string;
    apply: (t: ToolEntry) => Partial<ToolEntry>;
  };
  const presets: Preset[] = [
    {
      label: 'ER11 (≤7 mm)',
      apply: (t) => ({
        fluteLengthMm: t.fluteLengthMm ?? 15,
        shankDiameterMm: t.shankDiameterMm ?? Math.min(t.diameter, 6),
        holder: {
          kind: 'cone',
          bottom_diameter_mm: 19,
          top_diameter_mm: 30,
          length_mm: 35,
        },
      }),
    },
    {
      label: 'ER16 (≤10 mm)',
      apply: (t) => ({
        fluteLengthMm: t.fluteLengthMm ?? 20,
        shankDiameterMm: t.shankDiameterMm ?? Math.min(t.diameter, 8),
        holder: {
          kind: 'cone',
          bottom_diameter_mm: 28,
          top_diameter_mm: 42,
          length_mm: 45,
        },
      }),
    },
    {
      label: 'ER20 (≤13 mm)',
      apply: (t) => ({
        fluteLengthMm: t.fluteLengthMm ?? 25,
        shankDiameterMm: t.shankDiameterMm ?? Math.min(t.diameter, 12),
        holder: {
          kind: 'cone',
          bottom_diameter_mm: 34,
          top_diameter_mm: 50,
          length_mm: 50,
        },
      }),
    },
    {
      label: 'Direct shank',
      apply: (t) => ({
        fluteLengthMm: t.fluteLengthMm ?? 15,
        shankDiameterMm: t.shankDiameterMm ?? t.diameter,
        holder: undefined,
      }),
    },
    {
      label: 'No holder',
      apply: () => ({
        fluteLengthMm: undefined,
        shankDiameterMm: undefined,
        holder: undefined,
      }),
    },
  ];

  function applyPreset(idx: number, label: string) {
    const p = presets.find((x) => x.label === label);
    if (!p) return;
    const cur = draft[idx];
    const patch = p.apply(cur);
    dd.draft = draft.map((t, i) => (i === idx ? { ...t, ...patch } : t));
  }

  type HolderKind = HolderShape['kind'] | 'none';
  function holderKind(t: ToolEntry): HolderKind {
    return t.holder?.kind ?? 'none';
  }

  function setHolderKind(idx: number, kind: HolderKind) {
    const cur = draft[idx];
    let next: HolderShape | undefined;
    switch (kind) {
      case 'none':
        next = undefined;
        break;
      case 'cylinder':
        next =
          cur.holder?.kind === 'cylinder'
            ? cur.holder
            : { kind: 'cylinder', diameter_mm: 20, length_mm: 30 };
        break;
      case 'cone':
        next =
          cur.holder?.kind === 'cone'
            ? cur.holder
            : { kind: 'cone', bottom_diameter_mm: 20, top_diameter_mm: 35, length_mm: 35 };
        break;
      case 'stepped':
        next =
          cur.holder?.kind === 'stepped'
            ? cur.holder
            : {
                kind: 'stepped',
                cylinder_diameter_mm: 20,
                cylinder_length_mm: 12,
                cone_top_diameter_mm: 35,
                cone_length_mm: 25,
              };
        break;
    }
    dd.draft = draft.map((t, i) => (i === idx ? { ...t, holder: next } : t));
  }

  function updateHolderField(idx: number, key: string, value: number) {
    const cur = draft[idx];
    if (!cur.holder) return;
    const updated = { ...cur.holder, [key]: value } as HolderShape;
    dd.draft = draft.map((t, i) => (i === idx ? { ...t, holder: updated } : t));
  }

  // Display labels for the kind dropdown live in tool_family.ts so the
  // dialog, the disabled-reason tooltips, and any other UI surface that
  // names a tool kind read from the same source.
  const kindLabels = KIND_DISPLAY_LABELS;
  const coolantLabels: Record<CoolantMode, () => string> = {
    off: () => t('tools.coolant.off'),
    mist: () => t('tools.coolant.mist'),
    flood: () => t('tools.coolant.flood'),
  };
  const kindOptions = Object.keys(kindLabels) as ToolKind[];
  const coolantOptions = Object.keys(coolantLabels) as CoolantMode[];
  const holderKindLabels: Record<HolderKind, () => string> = {
    none: () => t('tools.holder.kind.none'),
    cylinder: () => t('tools.holder.kind.cylinder'),
    cone: () => t('tools.holder.kind.cone'),
    stepped: () => t('tools.holder.kind.stepped'),
  };
  const holderKindOptions: HolderKind[] = ['none', 'cylinder', 'cone', 'stepped'];
</script>

{#snippet shell()}
  {#if !embedded}
    <header>
      <h2 id="tools-title">{t('tools.title')}</h2>
      <button class="dlg-close" onclick={close} aria-label={t('common.close')}>×</button>
    </header>
  {/if}
  <!-- Header-attached filters: text search + kind + machine
       capability. View-only — filtering never touches the library. -->
  <div class="table-actions">
    <!-- File actions left-aligned, matching the Project toolbar. In
         inventory mode they import/export the SHOP INVENTORY (via the
         draft — Apply persists); in project mode the working tool set. -->
    <button
      type="button"
      class="tc-file"
      onclick={async () => {
        if (isInventory) await fileOps.exportToolset(JSON.parse(JSON.stringify(draft)));
        else await fileOps.saveToolset();
      }}
      title={isInventory
        ? t('tools.file.save.inventory.title')
        : t('tools.file.save.project.title')}>{t('common.save_ellipsis')}</button
    >
    <button
      type="button"
      class="tc-file"
      onclick={async () => {
        if (isInventory) {
          const merged = await fileOps.importToolset('replace', draft);
          if (merged) dd.draft = merged;
        } else {
          await fileOps.loadToolset('replace');
          dd.draft = project.data.tools.map((t) => ({ ...t }));
        }
      }}
      title={isInventory
        ? t('tools.file.load_replace.inventory.title')
        : t('tools.file.load_replace.project.title')}>{t('tools.file.load_replace')}</button
    >
    <button
      type="button"
      class="tc-file"
      onclick={async () => {
        if (isInventory) {
          const merged = await fileOps.importToolset('add', draft);
          if (merged) dd.draft = merged;
        } else {
          await fileOps.loadToolset('add');
          dd.draft = project.data.tools.map((t) => ({ ...t }));
        }
      }}
      title={t('tools.file.load_add.title')}>{t('tools.file.load_add')}</button
    >
  </div>
  <div class="table-filters">
    <input
      type="text"
      class="tc-search"
      placeholder={t('tools.search.placeholder')}
      value={view.query}
      oninput={(e) => {
        view.query = (e.currentTarget as HTMLInputElement).value;
        page = 0;
      }}
      title={t('tools.search.title')}
    />
    <label class="tc-filter">
      <span>{t('tools.filter.kind')}</span>
      <select
        value={view.kind}
        onchange={(e) => {
          view.kind = (e.currentTarget as HTMLSelectElement).value as typeof view.kind;
          page = 0;
        }}
      >
        <option value="">{t('tools.filter.all')}</option>
        {#each kindOptions as k (k)}
          <option value={k}>{kindLabels[k]}</option>
        {/each}
      </select>
    </label>
    <label class="tc-filter" title={t('tools.filter.runs_on.title')}>
      <span>{t('tools.filter.runs_on')}</span>
      <select
        value={view.mode}
        onchange={(e) => {
          view.mode = (e.currentTarget as HTMLSelectElement).value as typeof view.mode;
          page = 0;
        }}
      >
        <option value="">{t('tools.filter.runs_on.any')}</option>
        <option value="mill">{t('tools.filter.runs_on.mill')}</option>
        <option value="laser">{t('tools.filter.runs_on.laser')}</option>
        <option value="drag">{t('tools.filter.runs_on.drag')}</option>
        <option value="plasma">{t('tools.filter.runs_on.plasma')}</option>
      </select>
    </label>
    {#if filtersActive}
      <button type="button" class="tc-clear" onclick={clearFilters}
        >{t('tools.filter.clear')}</button
      >
    {/if}
    <span class="tc-count"
      >{paged.total === draft.length
        ? draft.length === 1
          ? t('tools.count.one', { count: draft.length })
          : t('tools.count.many', { count: draft.length })
        : t('tools.count.filtered', { shown: paged.total, total: draft.length })}</span
    >
  </div>
  <div class="body" bind:this={bodyEl}>
    <div class="table">
      <div class="row head">
        <button
          class="sort-h"
          type="button"
          onclick={() => setSort('id')}
          title={t('tools.col.id.title')}>#{sortArrow('id')}</button
        >
        <button
          class="sort-h"
          type="button"
          onclick={() => setSort('name')}
          title={t('tools.col.name.title')}>{t('tools.col.name')}{sortArrow('name')}</button
        >
        <button
          class="sort-h"
          type="button"
          onclick={() => setSort('kind')}
          title={t('tools.col.kind.title')}>{t('tools.col.kind')}{sortArrow('kind')}</button
        >
        <button
          class="sort-h"
          type="button"
          onclick={() => setSort('diameter')}
          title={t('tools.col.diameter.title')}
          >⌀ <span class="unit-hdr">mm</span>{sortArrow('diameter')}</button
        >
        <span>{t('tools.col.tip_diameter')} <span class="unit-hdr">mm</span></span>
        <span title={t('tools.col.tip_angle.title')}
          >{t('tools.col.tip_angle')} <span class="unit-hdr">°</span></span
        >
        <button
          class="sort-h"
          type="button"
          onclick={() => setSort('flutes')}
          title={t('tools.col.flutes.title')}>{t('tools.col.flutes')}{sortArrow('flutes')}</button
        >
        <button
          class="sort-h"
          type="button"
          onclick={() => setSort('speed')}
          title={t('tools.col.speed.title')}
          >{t('tools.col.speed')} <span class="unit-hdr">RPM</span>{sortArrow('speed')}</button
        >
        <button
          class="sort-h"
          type="button"
          onclick={() => setSort('feedRate')}
          title={t('tools.col.feed.title')}
          >{t('tools.col.feed')} <span class="unit-hdr">mm/min</span>{sortArrow('feedRate')}</button
        >
        <button
          class="sort-h"
          type="button"
          onclick={() => setSort('plungeRate')}
          title={t('tools.col.plunge.title')}
          >{t('tools.col.plunge')} <span class="unit-hdr">mm/min</span>{sortArrow(
            'plungeRate',
          )}</button
        >
        <span title={t('tools.col.dflt_step.title')}
          >{t('tools.col.dflt_step')} <span class="unit-hdr">mm</span></span
        >
        <span>{t('tools.col.coolant')}</span>
        <span></span>
      </div>
      {#each paged.rows as { tool, i } (tool.id)}
        <div class="row" class:highlight={highlightedId === tool.id} data-tool-id={tool.id}>
          <span class="id">
            <button
              class="expand"
              type="button"
              aria-expanded={expanded.has(tool.id)}
              aria-label={expanded.has(tool.id)
                ? t('tools.row.expand.collapse.aria', { id: tool.id })
                : t('tools.row.expand.expand.aria', { id: tool.id })}
              title={expanded.has(tool.id)
                ? t('tools.row.expand.collapse.title')
                : t('tools.row.expand.expand.title')}
              onclick={() => toggleExpanded(tool.id)}
              >{expanded.has(tool.id) ? '▾' : '▸'} {tool.id}</button
            >
          </span>
          <input
            type="text"
            value={tool.name}
            placeholder={suggestToolName(tool)}
            title={t('tools.row.name.title')}
            oninput={(e) => updateField(i, 'name', (e.currentTarget as HTMLInputElement).value)}
          />
          <select
            value={tool.kind}
            onchange={(e) =>
              onKindChange(i, (e.currentTarget as HTMLSelectElement).value as ToolKind)}
          >
            {#each kindOptions as k (k)}
              <option value={k}>{kindLabels[k]}</option>
            {/each}
          </select>
          <input
            type="number"
            step="0.1"
            min="0.01"
            value={tool.diameter}
            class:invalid={diameterInvalid(tool)}
            title={diameterInvalid(tool) ? t('tools.row.diameter.invalid.title') : ''}
            onchange={(e) =>
              updateField(
                i,
                'diameter',
                parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
              )}
          />
          <input
            type="number"
            step="0.05"
            min="0"
            value={tool.tipDiameter ?? ''}
            placeholder={fieldApplies('tipDiameter', tool.kind) ? '—' : t('tools.field.na')}
            disabled={!fieldApplies('tipDiameter', tool.kind)}
            class:invalid={tool.tipDiameter !== undefined && tool.tipDiameter < 0}
            title={!fieldApplies('tipDiameter', tool.kind)
              ? fieldDisabledReason('tipDiameter', tool.kind)
              : tool.tipDiameter !== undefined && tool.tipDiameter < 0
                ? t('tools.row.tip_diameter.invalid.title')
                : ''}
            onchange={(e) => {
              // Reject negative tip ⌀ — Rust setup_resolver.rs:669
              // does .max(0.0) on this, so a typo like -0.5 silently
              // becomes 0 and the depth math changes without warning.
              // Treat any negative input as "unset" (same pattern as
              // defaultStep) so the user must enter a valid value.
              const v = (e.currentTarget as HTMLInputElement).value;
              if (v === '') {
                updateField(i, 'tipDiameter', undefined);
                return;
              }
              const n = parseFloat(v);
              updateField(i, 'tipDiameter', isNaN(n) || n < 0 ? undefined : n);
            }}
          />
          <input
            type="number"
            step="1"
            min="1"
            max="179"
            value={tool.tipAngleDeg ?? ''}
            placeholder={fieldApplies('tipAngleDeg', tool.kind) ? '60' : t('tools.field.na')}
            disabled={!fieldApplies('tipAngleDeg', tool.kind)}
            title={fieldApplies('tipAngleDeg', tool.kind)
              ? t('tools.row.tip_angle.title')
              : fieldDisabledReason('tipAngleDeg', tool.kind)}
            onchange={(e) => {
              const v = (e.currentTarget as HTMLInputElement).value;
              updateField(i, 'tipAngleDeg', v === '' ? undefined : parseFloat(v));
            }}
          />
          <input
            type="number"
            step="1"
            min="1"
            value={tool.flutes}
            disabled={!fieldApplies('flutes', tool.kind)}
            title={fieldApplies('flutes', tool.kind)
              ? ''
              : fieldDisabledReason('flutes', tool.kind)}
            onchange={(e) =>
              updateField(
                i,
                'flutes',
                parseInt((e.currentTarget as HTMLInputElement).value, 10) || 1,
              )}
          />
          <input
            type="number"
            step="500"
            min="1"
            value={tool.speed}
            disabled={!fieldApplies('speed', tool.kind)}
            class:invalid={speedInvalid(tool)}
            title={!fieldApplies('speed', tool.kind)
              ? fieldDisabledReason('speed', tool.kind)
              : speedInvalid(tool)
                ? t('tools.row.speed.invalid.title')
                : ''}
            onchange={(e) =>
              updateField(
                i,
                'speed',
                parseInt((e.currentTarget as HTMLInputElement).value, 10) || 0,
              )}
          />
          <input
            type="number"
            step="50"
            min="1"
            value={tool.feedRate}
            class:invalid={feedInvalid(tool)}
            title={feedInvalid(tool)
              ? t('tools.row.feed.invalid.title')
              : tool.kind === 'drill'
                ? t('tools.row.feed.drill.title')
                : ''}
            onchange={(e) =>
              updateField(
                i,
                'feedRate',
                parseInt((e.currentTarget as HTMLInputElement).value, 10) || 0,
              )}
          />
          <input
            type="number"
            step="50"
            min="1"
            value={tool.plungeRate}
            disabled={!fieldApplies('plunge', tool.kind)}
            class:invalid={plungeInvalid(tool)}
            title={!fieldApplies('plunge', tool.kind)
              ? fieldDisabledReason('plunge', tool.kind)
              : plungeInvalid(tool)
                ? t('tools.row.plunge.invalid.title')
                : ''}
            onchange={(e) =>
              updateField(
                i,
                'plungeRate',
                parseInt((e.currentTarget as HTMLInputElement).value, 10) || 0,
              )}
          />
          <input
            type="number"
            step="0.05"
            max="0"
            value={tool.defaultStep ?? ''}
            placeholder={fieldApplies('defaultStep', tool.kind) ? '—' : t('tools.field.na')}
            disabled={!fieldApplies('defaultStep', tool.kind)}
            title={fieldApplies('defaultStep', tool.kind)
              ? tool.defaultStep !== undefined && tool.defaultStep >= 0
                ? t('tools.row.dflt_step.invalid.title')
                : t('tools.row.dflt_step.title')
              : fieldDisabledReason('defaultStep', tool.kind)}
            class:invalid={tool.defaultStep !== undefined && tool.defaultStep >= 0}
            onchange={(e) => {
              const v = (e.currentTarget as HTMLInputElement).value;
              if (v === '') {
                updateField(i, 'defaultStep', undefined);
                return;
              }
              const n = parseFloat(v);
              updateField(i, 'defaultStep', isNaN(n) || n >= 0 ? undefined : n);
            }}
          />
          <select
            value={tool.coolant}
            onchange={(e) =>
              updateField(
                i,
                'coolant',
                (e.currentTarget as HTMLSelectElement).value as CoolantMode,
              )}
          >
            {#each coolantOptions as c (c)}
              <option value={c}>{coolantLabels[c]()}</option>
            {/each}
          </select>
          <button
            class="del"
            onclick={() => removeAt(i)}
            disabled={draft.length <= 1}
            title={draft.length <= 1 ? t('tools.row.delete.disabled') : t('tools.row.delete.title')}
            aria-label={draft.length <= 1
              ? t('tools.row.delete.disabled')
              : t('tools.row.delete.aria', { name: tool.name })}>×</button
          >
        </div>
        {#if expanded.has(tool.id)}
          <div class="holder-panel">
            <div class="holder-row">
              <span class="holder-label" title={t('tools.holder.runs_on.title')}
                >{t('tools.holder.runs_on')}</span
              >
              {#each TOOL_COMPATIBLE_MODES[tool.kind] as m (m)}
                <span class="cap-chip">{MACHINE_MODE_NOUN[m]}</span>
              {/each}
            </div>
            <div class="holder-row">
              <label>
                <span>{t('tools.holder.flute_length')}</span>
                <input
                  type="number"
                  step="0.5"
                  min="0"
                  placeholder="—"
                  value={tool.fluteLengthMm ?? ''}
                  title={t('tools.holder.flute_length.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'fluteLengthMm', v === '' ? undefined : parseFloat(v));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.holder.overall_length')}</span>
                <input
                  type="number"
                  step="0.5"
                  min="0"
                  placeholder="—"
                  value={tool.lengthMm ?? ''}
                  title={t('tools.holder.overall_length.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'lengthMm', v === '' ? undefined : parseFloat(v));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.holder.shank_diameter')}</span>
                <input
                  type="number"
                  step="0.1"
                  min="0"
                  placeholder={t('tools.holder.shank_diameter.placeholder')}
                  value={tool.shankDiameterMm ?? ''}
                  title={t('tools.holder.shank_diameter.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'shankDiameterMm', v === '' ? undefined : parseFloat(v));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.holder.stickout')}</span>
                <input
                  type="number"
                  step="0.5"
                  min="0"
                  placeholder="—"
                  value={tool.stickoutLengthMm ?? ''}
                  title={t('tools.holder.stickout.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'stickoutLengthMm', v === '' ? undefined : parseFloat(v));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.holder.preset')}</span>
                <select
                  title={t('tools.holder.preset.title')}
                  onchange={(e) => {
                    const sel = e.currentTarget as HTMLSelectElement;
                    if (sel.value) {
                      applyPreset(i, sel.value);
                      sel.value = '';
                    }
                  }}
                >
                  <option value="">{t('tools.holder.preset.apply')}</option>
                  {#each presets as p (p.label)}
                    <option value={p.label}>{p.label}</option>
                  {/each}
                </select>
              </label>
            </div>
            <div class="holder-row">
              <span class="holder-label">{t('tools.holder.label')}</span>
              {#each holderKindOptions as k (k)}
                <label class="radio">
                  <input
                    type="radio"
                    name="holder-kind-{tool.id}"
                    value={k}
                    checked={holderKind(tool) === k}
                    onchange={() => setHolderKind(i, k)}
                  />
                  <span>{holderKindLabels[k]()}</span>
                </label>
              {/each}
            </div>
            {#if tool.holder?.kind === 'cylinder'}
              <div class="holder-row">
                <label>
                  <span>{t('tools.holder.cyl.diameter')}</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    value={tool.holder.diameter_mm}
                    onchange={(e) =>
                      updateHolderField(
                        i,
                        'diameter_mm',
                        parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                      )}
                  />
                </label>
                <label>
                  <span>{t('tools.holder.length')}</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    value={tool.holder.length_mm}
                    onchange={(e) =>
                      updateHolderField(
                        i,
                        'length_mm',
                        parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                      )}
                  />
                </label>
              </div>
            {:else if tool.holder?.kind === 'cone'}
              <div class="holder-row">
                <label>
                  <span>{t('tools.holder.cone.bottom_diameter')}</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    value={tool.holder.bottom_diameter_mm}
                    onchange={(e) =>
                      updateHolderField(
                        i,
                        'bottom_diameter_mm',
                        parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                      )}
                  />
                </label>
                <label>
                  <span>{t('tools.holder.cone.top_diameter')}</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    value={tool.holder.top_diameter_mm}
                    onchange={(e) =>
                      updateHolderField(
                        i,
                        'top_diameter_mm',
                        parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                      )}
                  />
                </label>
                <label>
                  <span>{t('tools.holder.length')}</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    value={tool.holder.length_mm}
                    onchange={(e) =>
                      updateHolderField(
                        i,
                        'length_mm',
                        parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                      )}
                  />
                </label>
              </div>
            {:else if tool.holder?.kind === 'stepped'}
              <div class="holder-row">
                <label>
                  <span>{t('tools.holder.stepped.cyl_diameter')}</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    value={tool.holder.cylinder_diameter_mm}
                    onchange={(e) =>
                      updateHolderField(
                        i,
                        'cylinder_diameter_mm',
                        parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                      )}
                  />
                </label>
                <label>
                  <span>{t('tools.holder.stepped.cyl_length')}</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    value={tool.holder.cylinder_length_mm}
                    onchange={(e) =>
                      updateHolderField(
                        i,
                        'cylinder_length_mm',
                        parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                      )}
                  />
                </label>
                <label>
                  <span>{t('tools.holder.stepped.cone_top_diameter')}</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    value={tool.holder.cone_top_diameter_mm}
                    onchange={(e) =>
                      updateHolderField(
                        i,
                        'cone_top_diameter_mm',
                        parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                      )}
                  />
                </label>
                <label>
                  <span>{t('tools.holder.stepped.cone_length')}</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    value={tool.holder.cone_length_mm}
                    onchange={(e) =>
                      updateHolderField(
                        i,
                        'cone_length_mm',
                        parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                      )}
                  />
                </label>
              </div>
            {/if}
            <div class="holder-row pass-overrides">
              <span class="holder-label" title={t('tools.pass_overrides.title')}
                >{t('tools.pass_overrides')}</span
              >
            </div>
            <div class="holder-row">
              <label>
                <span>{t('tools.finish.rpm')}</span>
                <input
                  type="number"
                  step="500"
                  min="0"
                  placeholder={String(tool.speed)}
                  value={tool.speedFinish ?? ''}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'speedFinish', v === '' ? undefined : parseInt(v, 10));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.finish.feed')}</span>
                <input
                  type="number"
                  step="50"
                  min="0"
                  placeholder={String(tool.feedRate)}
                  value={tool.feedRateFinish ?? ''}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'feedRateFinish', v === '' ? undefined : parseInt(v, 10));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.finish.plunge')}</span>
                <input
                  type="number"
                  step="50"
                  min="0"
                  placeholder={String(tool.plungeRate)}
                  value={tool.plungeRateFinish ?? ''}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'plungeRateFinish', v === '' ? undefined : parseInt(v, 10));
                  }}
                />
              </label>
            </div>
            <div class="holder-row">
              <label>
                <span>{t('tools.drill.rpm')}</span>
                <input
                  type="number"
                  step="500"
                  min="0"
                  placeholder={String(tool.speed)}
                  value={tool.speedDrill ?? ''}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'speedDrill', v === '' ? undefined : parseInt(v, 10));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.drill.feed')}</span>
                <input
                  type="number"
                  step="50"
                  min="0"
                  placeholder={String(tool.feedRate)}
                  value={tool.feedRateDrill ?? ''}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'feedRateDrill', v === '' ? undefined : parseInt(v, 10));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.drill.plunge')}</span>
                <input
                  type="number"
                  step="50"
                  min="0"
                  placeholder={String(tool.plungeRate)}
                  value={tool.plungeRateDrill ?? ''}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'plungeRateDrill', v === '' ? undefined : parseInt(v, 10));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.drill.peck')}</span>
                <input
                  type="number"
                  step="0.1"
                  min="0"
                  placeholder="—"
                  value={tool.defaultPeckStepMm ?? ''}
                  title={t('tools.drill.peck.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'defaultPeckStepMm', v === '' ? undefined : parseFloat(v));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.drill.xy_overlap')}</span>
                <input
                  type="number"
                  step="0.05"
                  min="0.05"
                  max="0.95"
                  placeholder="0.5"
                  value={tool.defaultXyOverlap ?? ''}
                  title={t('tools.drill.xy_overlap.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    if (v === '') {
                      updateField(i, 'defaultXyOverlap', undefined);
                      return;
                    }
                    const n = parseFloat(v);
                    updateField(i, 'defaultXyOverlap', isNaN(n) ? undefined : n);
                  }}
                />
              </label>
            </div>
            <div class="holder-row">
              <label class="comment-row">
                <span>{t('tools.comment')}</span>
                <textarea
                  rows="2"
                  value={tool.comment ?? ''}
                  placeholder={t('tools.comment.placeholder')}
                  title={t('tools.comment.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLTextAreaElement).value;
                    updateField(i, 'comment', v === '' ? undefined : v);
                  }}
                ></textarea>
              </label>
            </div>
            <div class="holder-row">
              <label>
                <span>{t('tools.z_shift')}</span>
                <input
                  type="number"
                  step="0.01"
                  placeholder="—"
                  value={tool.zShiftMm ?? ''}
                  title={t('tools.z_shift.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    if (v === '') {
                      updateField(i, 'zShiftMm', undefined);
                      return;
                    }
                    const n = parseFloat(v);
                    updateField(i, 'zShiftMm', isNaN(n) || n === 0 ? undefined : n);
                  }}
                />
              </label>
              <label>
                <span>{t('tools.spindle_warmup')}</span>
                <input
                  type="number"
                  step="0.5"
                  min="0"
                  placeholder="1"
                  value={tool.pause ?? ''}
                  title={t('tools.spindle_warmup.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    if (v === '') {
                      updateField(i, 'pause', undefined);
                      return;
                    }
                    const n = parseFloat(v);
                    updateField(i, 'pause', isNaN(n) || n < 0 ? undefined : n);
                  }}
                />
              </label>
              <fieldset
                class="spindle-dir"
                disabled={tool.kind === 'drag_knife' ||
                  tool.kind === 'laser_beam' ||
                  tool.kind === 'plasma_torch'}
                title={tool.kind === 'drag_knife'
                  ? t('tools.spindle_dir.disabled.drag_knife')
                  : tool.kind === 'laser_beam'
                    ? t('tools.spindle_dir.disabled.laser')
                    : tool.kind === 'plasma_torch'
                      ? t('tools.spindle_dir.disabled.plasma')
                      : t('tools.spindle_dir.title')}
              >
                <legend>{t('tools.spindle_dir')}</legend>
                <label class="radio">
                  <input
                    type="radio"
                    name="spindle-dir-{tool.id}"
                    value="cw"
                    checked={(tool.spindleDirection ?? 'cw') === 'cw'}
                    onchange={() => updateField(i, 'spindleDirection', undefined)}
                  />
                  <span>{t('tools.spindle_dir.cw')}</span>
                </label>
                <label class="radio">
                  <input
                    type="radio"
                    name="spindle-dir-{tool.id}"
                    value="ccw"
                    checked={tool.spindleDirection === 'ccw'}
                    onchange={() => updateField(i, 'spindleDirection', 'ccw')}
                  />
                  <span>{t('tools.spindle_dir.ccw')}</span>
                </label>
              </fieldset>
            </div>
            <div class="holder-row pass-overrides">
              <span class="holder-label" title={t('tools.whirl.title')}>{t('tools.whirl')}</span>
            </div>
            <div class="holder-row">
              <label class="radio">
                <input
                  type="checkbox"
                  checked={tool.whirl ?? false}
                  onchange={(e) =>
                    updateField(i, 'whirl', (e.currentTarget as HTMLInputElement).checked)}
                />
                <span>{t('tools.whirl.enable')}</span>
              </label>
              <label>
                <span>{t('tools.whirl.extra_width')}</span>
                <input
                  type="number"
                  step="0.1"
                  min="0"
                  placeholder="0"
                  value={tool.whirlExtraWidthMm ?? ''}
                  disabled={!tool.whirl}
                  title={t('tools.whirl.extra_width.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'whirlExtraWidthMm', v === '' ? undefined : parseFloat(v));
                  }}
                />
              </label>
            </div>
            <div class="holder-row">
              <label>
                <span>{t('tools.whirl.stride')}</span>
                <input
                  type="number"
                  step="0.1"
                  min="0.05"
                  placeholder={((tool.whirlExtraWidthMm ?? 0) * 0.5).toFixed(2)}
                  value={tool.whirlStepoverMm ?? ''}
                  disabled={!tool.whirl}
                  title={t('tools.whirl.stride.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'whirlStepoverMm', v === '' ? undefined : parseFloat(v));
                  }}
                />
              </label>
              <label>
                <span>{t('tools.whirl.z_wobble')}</span>
                <input
                  type="number"
                  step="0.05"
                  min="0"
                  placeholder="0"
                  value={tool.whirlOscMm ?? ''}
                  disabled={!tool.whirl}
                  title={t('tools.whirl.z_wobble.title')}
                  onchange={(e) => {
                    const v = (e.currentTarget as HTMLInputElement).value;
                    updateField(i, 'whirlOscMm', v === '' ? undefined : parseFloat(v));
                  }}
                />
              </label>
            </div>
            {#if attrApplies('dragoff', tool.kind)}
              <div class="holder-row pass-overrides">
                <span class="holder-label" title={t('tools.drag.title')}>{t('tools.drag')}</span>
              </div>
              <div class="holder-row">
                <label>
                  <span>{t('tools.drag.offset')}</span>
                  <input
                    type="number"
                    step="0.05"
                    min="0"
                    placeholder="—"
                    value={tool.dragoff ?? ''}
                    title={t('tools.drag.offset.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'dragoff', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>{t('tools.drag.self_align')}</span>
                  <input
                    type="number"
                    step="1"
                    min="0"
                    max="60"
                    placeholder="30"
                    value={tool.dragKnifeSelfAlignAngleDeg ?? ''}
                    title={t('tools.drag.self_align.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(
                        i,
                        'dragKnifeSelfAlignAngleDeg',
                        v === '' ? undefined : parseFloat(v),
                      );
                    }}
                  />
                </label>
              </div>
            {/if}
            {#if attrApplies('compressionTransition', tool.kind)}
              <div class="holder-row pass-overrides">
                <span class="holder-label" title={t('tools.compression.title')}
                  >{t('tools.compression')}</span
                >
              </div>
              <div class="holder-row">
                <label>
                  <span>{t('tools.compression.transition')}</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    placeholder={t('tools.compression.transition.placeholder')}
                    value={tool.compressionTransitionMm ?? ''}
                    title={t('tools.compression.transition.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(
                        i,
                        'compressionTransitionMm',
                        v === '' ? undefined : parseFloat(v),
                      );
                    }}
                  />
                </label>
              </div>
            {/if}
            {#if attrApplies('threadPitch', tool.kind)}
              <div class="holder-row pass-overrides">
                <span class="holder-label" title={t('tools.thread.title')}>{t('tools.thread')}</span
                >
              </div>
              <div class="holder-row">
                <label>
                  <span>{t('tools.thread.pitch')}</span>
                  <input
                    type="number"
                    step="0.05"
                    min="0"
                    placeholder="—"
                    value={tool.threadPitchMm ?? ''}
                    title={t('tools.thread.pitch.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'threadPitchMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
              </div>
            {/if}
            {#if attrApplies('cornerRadius', tool.kind)}
              <div class="holder-row pass-overrides">
                <span class="holder-label" title={t('tools.bullnose.title')}
                  >{t('tools.bullnose')}</span
                >
              </div>
              <div class="holder-row">
                <label>
                  <span>{t('tools.bullnose.corner_radius')}</span>
                  <input
                    type="number"
                    step="0.05"
                    min="0"
                    placeholder="—"
                    value={tool.cornerRadiusMm ?? ''}
                    title={t('tools.bullnose.corner_radius.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'cornerRadiusMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
              </div>
            {/if}
            {#if attrApplies('formProfile', tool.kind)}
              <ToolFormProfileEditor
                rows={tool.formProfileMm ?? []}
                diameterMm={tool.diameter}
                onChange={(next) => updateField(i, 'formProfileMm', next)}
              />
            {/if}
            {#if attrApplies('wear', tool.kind)}
              <div class="holder-row pass-overrides">
                <span class="holder-label" title={t('tools.wear.title')}>{t('tools.wear')}</span>
              </div>
              <div class="holder-row">
                <label>
                  <span>{t('tools.wear.offset')}</span>
                  <input
                    type="number"
                    step="0.01"
                    placeholder="0"
                    value={tool.wearOffsetMm ?? ''}
                    title={t('tools.wear.offset.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      if (v === '') {
                        updateField(i, 'wearOffsetMm', undefined);
                        return;
                      }
                      const n = parseFloat(v);
                      updateField(i, 'wearOffsetMm', isNaN(n) || n === 0 ? undefined : n);
                    }}
                  />
                </label>
                <button
                  type="button"
                  class="profile-btn"
                  onclick={() => (calibratingIdx = i)}
                  title={t('tools.wear.calibrate.title')}>{t('tools.wear.calibrate')}</button
                >
                <span class="cal-status">
                  {#if tool.lastCalibrated}
                    {t('tools.wear.last_calibrated', { date: tool.lastCalibrated })}
                    {#if isCalibrationStale(tool.lastCalibrated, new Date())}
                      <span class="stale-chip" title={t('tools.wear.stale.title')}
                        >{t('tools.wear.stale')}</span
                      >
                    {/if}
                  {:else}
                    {t('tools.wear.never')}
                  {/if}
                </span>
                {#if (tool.wearOffsetMm ?? 0) !== 0}
                  <span class="eff-hint"
                    >{t('tools.wear.cuts_as', { diameter: effectiveDiameterHint(tool) })}</span
                  >
                {/if}
              </div>
            {/if}
            {#if attrApplies('laser', tool.kind)}
              <div class="holder-row pass-overrides">
                <span class="holder-label" title={t('tools.laser.title')}>{t('tools.laser')}</span>
              </div>
              <div class="holder-row">
                <label>
                  <span>{t('tools.laser.pierce_time')}</span>
                  <input
                    type="number"
                    step="0.05"
                    min="0"
                    placeholder="—"
                    value={tool.laserPierceSec ?? ''}
                    title={t('tools.laser.pierce_time.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'laserPierceSec', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>{t('tools.laser.lead_in')}</span>
                  <input
                    type="number"
                    step="0.1"
                    min="0"
                    placeholder="—"
                    value={tool.laserLeadInMm ?? ''}
                    title={t('tools.laser.lead_in.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'laserLeadInMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>{t('tools.laser.kerf')}</span>
                  <input
                    type="number"
                    step="0.01"
                    min="0"
                    placeholder="0.15"
                    value={tool.kerfMm ?? ''}
                    title={t('tools.laser.kerf.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      if (v === '') {
                        updateField(i, 'kerfMm', undefined);
                        return;
                      }
                      const n = parseFloat(v);
                      updateField(i, 'kerfMm', isNaN(n) || n <= 0 ? undefined : n);
                    }}
                  />
                </label>
              </div>
            {/if}
            {#if attrApplies('plasma', tool.kind)}
              <div class="holder-row pass-overrides">
                <span class="holder-label" title={t('tools.plasma.title')}>{t('tools.plasma')}</span
                >
              </div>
              <div class="holder-row">
                <label>
                  <span>{t('tools.plasma.pierce_height')}</span>
                  <input
                    type="number"
                    step="0.1"
                    min="0"
                    placeholder="3.8"
                    value={tool.pierceHeightMm ?? ''}
                    title={t('tools.plasma.pierce_height.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'pierceHeightMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>{t('tools.plasma.cut_height')}</span>
                  <input
                    type="number"
                    step="0.1"
                    min="0"
                    placeholder="1.5"
                    value={tool.cutHeightMm ?? ''}
                    title={t('tools.plasma.cut_height.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'cutHeightMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>{t('tools.plasma.pierce_delay')}</span>
                  <input
                    type="number"
                    step="0.1"
                    min="0"
                    placeholder="0.5"
                    value={tool.pierceDelaySec ?? ''}
                    title={t('tools.plasma.pierce_delay.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'pierceDelaySec', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>{t('tools.plasma.kerf')}</span>
                  <input
                    type="number"
                    step="0.1"
                    min="0"
                    placeholder="—"
                    value={tool.kerfMm ?? ''}
                    title={t('tools.plasma.kerf.title')}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      if (v === '') {
                        updateField(i, 'kerfMm', undefined);
                        return;
                      }
                      const n = parseFloat(v);
                      updateField(i, 'kerfMm', isNaN(n) || n <= 0 ? undefined : n);
                    }}
                  />
                </label>
              </div>
            {/if}
          </div>
        {/if}
      {/each}
      {#if paged.total === 0 && filtersActive}
        <div class="mode-filter-row">
          <span>{t('tools.no_match')}</span>
          <button type="button" class="btn-secondary" onclick={clearFilters}
            >{t('tools.clear_filters')}</button
          >
        </div>
      {/if}
      {#if !showIncompatible && incompatibleCount > 0}
        <div class="mode-filter-row">
          <span
            >{incompatibleCount === 1
              ? t('tools.hidden.one', {
                  count: incompatibleCount,
                  machine: machineModesLabel(machineModes),
                })
              : t('tools.hidden.many', {
                  count: incompatibleCount,
                  machine: machineModesLabel(machineModes),
                })}</span
          >
          <button
            type="button"
            class="btn-secondary"
            onclick={() => (showIncompatible = true)}
            title={t('tools.show_all.title')}>{t('tools.show_all')}</button
          >
        </div>
      {:else if showIncompatible && incompatibleCount > 0}
        <div class="mode-filter-row">
          <span
            >{t('tools.showing_all', {
              count: incompatibleCount,
              machine: machineModesLabel(machineModes),
            })}</span
          >
          <button type="button" class="btn-secondary" onclick={() => (showIncompatible = false)}
            >{t('tools.hide_incompatible')}</button
          >
        </div>
      {/if}
    </div>
    <button class="add" onclick={addTool}>{t('tools.add')}</button>
  </div>
  {#if paged.pageCount > 1}
    <div class="table-pager">
      <button type="button" disabled={paged.page === 0} onclick={() => (page = paged.page - 1)}
        >{t('tools.pager.prev')}</button
      >
      <span>{t('tools.pager.status', { page: paged.page + 1, total: paged.pageCount })}</span>
      <button
        type="button"
        disabled={paged.page >= paged.pageCount - 1}
        onclick={() => (page = paged.page + 1)}>{t('tools.pager.next')}</button
      >
    </div>
  {/if}
  <footer>
    {#if dd.confirmingDiscard}
      <span class="discard-prompt">{t('common.discard_unsaved')}</span>
      <button class="btn-secondary" onclick={() => dd.cancelDiscard()}
        >{t('common.keep_editing')}</button
      >
      <button class="btn-danger" onclick={close}>{t('common.discard')}</button>
    {:else}
      <span class="sep"></span>
      {#if hasInvalidRow}
        <!-- Surface why OK is greyed out so the user knows which inputs need fixing. -->
        <span class="validation-msg" role="status">{t('tools.validation_msg')}</span>
      {/if}
      {#if embedded}
        <button class="btn-secondary" onclick={revert} disabled={!dd.isDirty}
          >{t('common.revert')}</button
        >
        <button
          class="btn-primary"
          onclick={commit}
          disabled={hasInvalidRow || !dd.isDirty}
          title={hasInvalidRow ? t('tools.apply.invalid.title') : ''}>{t('common.apply')}</button
        >
      {:else}
        <button class="btn-secondary" onclick={close}>{t('common.cancel')}</button>
        <button
          class="btn-primary"
          onclick={commit}
          disabled={hasInvalidRow}
          title={hasInvalidRow ? t('tools.save.invalid.title') : ''}>{t('common.ok')}</button
        >
      {/if}
    {/if}
  </footer>
{/snippet}

{#if embedded}
  <section class="embedded-shell">{@render shell()}</section>
{:else if open}
  <Modal
    onClose={close}
    persistKey="tool-library"
    width="min(960px, 96vw)"
    draggable
    resizable
    ariaLabelledBy="tools-title"
  >
    {@render shell()}
  </Modal>
{/if}
{#if active && calibratingIdx != null && draft[calibratingIdx]}
  {@const calTool = draft[calibratingIdx]}
  <ToolCalibrationDialog
    open
    toolName={calTool.name}
    nominalDiameterMm={calTool.diameter}
    currentWearOffsetMm={calTool.wearOffsetMm ?? 0}
    onApply={(wear, date) => {
      if (calibratingIdx != null) applyCalibration(calibratingIdx, wear, date);
    }}
    onClose={() => (calibratingIdx = null)}
  />
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
    padding: 0.6rem 0.7rem;
    overflow: auto;
    min-width: 0;
    /* Bound the scroll container's height so its OWN horizontal
       scrollbar always sits at the fixed bottom edge of the visible
       body — reachable without first scrolling the (potentially long)
       tool list vertically. `flex: 1` claims the space between the
       sticky header/filters and the footer; the modal shell and the
       embedded shell are both flex columns, so this works in both. The
       wide `.table` (min-width: min-content) overflows horizontally
       here and `overflow-x: auto` turns it into the pinned scroller. */
    flex: 1;
    min-height: 0;
    /* Touch: allow horizontal panning of the wide rows (and vertical
       scroll of the list) without the browser claiming the gesture. */
    touch-action: pan-x pan-y;
    /* Firefox: a thicker-than-thin scrollbar with themed colours. */
    scrollbar-width: auto;
    scrollbar-color: var(--text-muted) var(--bg-elevated);
  }
  /* WebKit/Blink: give the horizontal scrollbar a clearly grabbable
     height with a contrasting thumb, so it reads as an interactive
     control on both pointer and touch. Theme-driven, so it tracks
     light/dark via the CSS vars. */
  .body::-webkit-scrollbar {
    height: 13px;
    width: 13px;
  }
  .body::-webkit-scrollbar-track {
    background: var(--bg-elevated);
    border-radius: 7px;
  }
  .body::-webkit-scrollbar-thumb {
    background: var(--text-muted);
    border: 3px solid var(--bg-elevated);
    border-radius: 7px;
  }
  .body::-webkit-scrollbar-thumb:hover {
    background: var(--text);
  }
  .body::-webkit-scrollbar-corner {
    background: var(--bg-elevated);
  }
  /* Tab-panel (embedded) shell — fills the main area; the body scrolls
     between the sticky header and footer. */
  .embedded-shell {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    background: var(--bg-panel);
  }
  .table {
    display: grid;
    gap: 0.2rem;
    /* Below ~720 px the 13-column tools grid (id, name, kind, diameter,
       reach, flutes, ∠, speed, feed, plunge, warmup, notes, trash) used
       to squash numeric inputs to unreadable widths because every column
       was fr-based. min-content forces the cells to their intrinsic
       width and the body's `overflow: auto` kicks in as a horizontal
       scroller — a clearly worse-than-fitting outcome only on tiny
       windows, but never an unreadable squash on the common 900-1200 px
       laptop sizes. */
    min-width: min-content;
  }
  .row {
    display: grid;
    grid-template-columns:
      2.5rem minmax(8rem, 1.6fr) minmax(6rem, 1fr)
      4.5rem 4.5rem 4rem 3.5rem 5rem 5rem 5rem 4.5rem minmax(6rem, 1fr) 2rem;
    gap: 0.3rem;
    align-items: center;
    font-size: 0.78rem;
  }
  input.invalid {
    border-color: var(--danger);
  }
  /* Disabled fields (per-kind n/a entries) fade visibly so users see
     they're not editable, without changing the row layout. */
  input:disabled,
  select:disabled {
    opacity: 0.4;
    background: transparent;
    color: var(--text-muted);
    cursor: not-allowed;
  }
  .row.head {
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 0.68rem;
    padding-bottom: 0.2rem;
    border-bottom: 1px solid var(--border);
    /* Sticky so unit headers (mm / ° / RPM / mm/min) stay visible while
       scrolling through a long tool library; without it they scroll
       off and leave rows context-free. `.body` is the scroll container. */
    position: sticky;
    top: 0;
    background: var(--bg-panel);
    z-index: var(--z-anchor);
  }
  .row.head .unit-hdr {
    color: var(--text-faint);
    font-size: 0.62rem;
    text-transform: none;
    letter-spacing: 0;
    margin-left: 0.2rem;
  }
  @keyframes ivac-tool-flash {
    0%,
    100% {
      background: transparent;
    }
    25%,
    75% {
      background: color-mix(in srgb, var(--accent) 22%, transparent);
    }
  }
  .row.highlight {
    border-radius: 3px;
    animation: ivac-tool-flash 1.2s ease-in-out;
  }
  .id {
    text-align: center;
    color: var(--text-faint);
    font-variant-numeric: tabular-nums;
  }
  .expand {
    background: transparent;
    border: 0;
    color: var(--text-faint);
    cursor: pointer;
    padding: 0;
    font-size: 0.78rem;
    font-variant-numeric: tabular-nums;
    width: 100%;
    text-align: center;
  }
  .holder-panel {
    grid-column: 1 / -1;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.45rem 0.6rem;
    margin: 0.25rem 0 0.5rem 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .holder-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.6rem;
  }
  .holder-row label {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    font-size: 0.7rem;
    color: var(--text-muted);
    min-width: 7rem;
  }
  .holder-row label span {
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .holder-row label.radio {
    flex-direction: row;
    align-items: center;
    color: var(--text);
    text-transform: none;
    letter-spacing: normal;
    font-size: 0.78rem;
    min-width: auto;
  }
  .holder-row label.radio span {
    text-transform: none;
    letter-spacing: normal;
  }
  .holder-row .holder-label {
    color: var(--text-muted);
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .holder-row fieldset.spindle-dir {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.1rem 0.4rem 0.15rem;
    margin: 0;
    min-width: 0;
  }
  .holder-row fieldset.spindle-dir legend {
    color: var(--text-muted);
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0 0.2rem;
  }
  .holder-row fieldset.spindle-dir label.radio {
    min-width: auto;
    flex-direction: row;
  }
  .holder-row fieldset.spindle-dir[disabled] {
    opacity: 0.4;
  }
  input,
  select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.32rem;
    font-size: 0.78rem;
    min-width: 0;
    width: 100%;
    box-sizing: border-box;
  }
  .del {
    background: transparent;
    color: var(--text-muted);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.05rem 0.4rem;
    cursor: pointer;
  }
  .del:disabled {
    opacity: 0.3;
    cursor: not-allowed;
  }
  /* Two header rows: file actions on the activated-tab surface, then
     the filter row in a lighter tone acting as a divider before the
     table. */
  .table-actions {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.45rem 0.7rem;
    background: var(--bg-panel);
    font-size: 0.78rem;
    flex-wrap: wrap;
  }
  .table-filters {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.35rem 0.7rem;
    background: var(--bg-elevated);
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    font-size: 0.78rem;
    flex-wrap: wrap;
  }
  .tc-file {
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.2rem 0.55rem;
    font-size: 0.74rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .tc-search {
    width: 14rem;
    max-width: 40vw;
  }
  .tc-filter {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    color: var(--text-muted);
  }
  .tc-clear {
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.15rem 0.5rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .tc-count {
    margin-left: auto;
    color: var(--text-muted);
  }
  /* Sortable column headers — tri-state (natural → ▲ → ▼). */
  .sort-h {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    font-weight: inherit;
    color: inherit;
    text-align: left;
    cursor: pointer;
    white-space: nowrap;
  }
  .sort-h:hover {
    color: var(--text-strong);
    text-decoration: underline;
  }
  /* Pager — appears only when the filtered set exceeds one page. */
  .table-pager {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.7rem;
    padding: 0.35rem 0.7rem;
    border-top: 1px solid var(--border);
    background: var(--bg-elevated);
    font-size: 0.78rem;
  }
  .table-pager button {
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.15rem 0.55rem;
    font-size: 0.74rem;
    cursor: pointer;
  }
  .table-pager button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  /* Comment gets a full-width row of its own. */
  .comment-row {
    flex: 1;
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
  }
  .comment-row textarea {
    flex: 1;
    resize: vertical;
  }
  /* Machine-capability chips on each tool row ("Runs on mill"). */
  .cap-chip {
    display: inline-block;
    padding: 0.05rem 0.45rem;
    border: 1px solid var(--border);
    border-radius: 9px;
    background: var(--bg-elevated);
    color: var(--text);
    font-size: 0.7rem;
    align-self: center;
  }
  /* Wear-calibration status line + stale chip. */
  .cal-status {
    color: var(--text-muted);
    font-size: 0.74rem;
    align-self: center;
  }
  .stale-chip {
    display: inline-block;
    margin-left: 0.35rem;
    padding: 0.05rem 0.4rem;
    border-radius: 3px;
    background: color-mix(in srgb, #e6a700 22%, var(--bg-elevated));
    color: var(--text-strong);
    font-size: 0.7rem;
  }
  .eff-hint {
    color: var(--text-muted);
    font-size: 0.74rem;
    font-style: italic;
    align-self: center;
  }
  /* Machine-mode filter banner — the "N tools hidden — Show all" /
     "Hide incompatible" row under the table. Muted: it's a view
     control, not a warning (the library itself is untouched). */
  .mode-filter-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.35rem 0.5rem;
    font-size: 0.78rem;
    color: var(--text-muted);
    border-top: 1px dashed var(--border);
  }
  .mode-filter-row button {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.15rem 0.5rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .add {
    margin-top: 0.5rem;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.25rem 0.6rem;
    font-size: 0.78rem;
    cursor: pointer;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
    padding: 0.5rem 0.7rem;
    border-top: 1px solid var(--border);
    background: var(--bg-elevated);
  }
  /* Footer-side validation hint shown when an OK-disabling row is
     present. Same red palette as `.discard-prompt`, but keeps the
     action buttons aligned to the right by NOT setting
     `margin-right: auto` — we want this slot inline with the buttons,
     not pushed to the start. */
  .validation-msg {
    color: var(--danger);
    font-size: 0.78rem;
    align-self: center;
  }
  /* Form-profile (z, r) sample editor + dovetail generator. */
  .profile-btn {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.2rem 0.5rem;
    font-size: 0.72rem;
    cursor: pointer;
    align-self: flex-end;
  }
</style>
