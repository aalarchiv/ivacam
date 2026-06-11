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
    type FormProfileSample,
  } from '../state/project.svelte';
  import { untrack } from 'svelte';
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
  }

  function removeAt(idx: number) {
    if (draft.length <= 1) return;
    dd.draft = draft.filter((_, i) => i !== idx);
  }

  function updateField<K extends keyof ToolEntry>(idx: number, key: K, value: ToolEntry[K]) {
    dd.draft = draft.map((t, i) => (i === idx ? { ...t, [key]: value } : t));
  }

  // ─────────────────────────── form-profile editor ───────────────────
  // The (z, r) sample table is the source of truth (stored on the
  // tool). The dovetail inputs below are a transient generator that
  // fills the table; they're keyed by tool id and not persisted —
  // re-deriving them from the table would be ambiguous for cove/ogee.
  let dovetailDraft = $state<Record<number, { diaMm: number; angleDeg: number; heightMm: number }>>(
    {},
  );
  function dovetailParamsFor(id: number) {
    return dovetailDraft[id] ?? { diaMm: 12.7, angleDeg: 14, heightMm: 9.5 };
  }
  function setDovetailParam(id: number, key: 'diaMm' | 'angleDeg' | 'heightMm', v: number) {
    dovetailDraft = { ...dovetailDraft, [id]: { ...dovetailParamsFor(id), [key]: v } };
  }
  const round3 = (v: number) => Math.round(v * 1000) / 1000;
  // A dovetail bit is widest at the bottom (z=0) and narrows upward as
  // the angled flank rises toward the neck: r(z) = D/2 − z·tan(angle).
  // Clamp the neck radius to ≥0 so an over-deep height can't invert it.
  function generateDovetail(idx: number, id: number) {
    const { diaMm, angleDeg, heightMm } = dovetailParamsFor(id);
    const rBottom = Math.max(diaMm / 2, 0);
    const h = Math.max(heightMm, 0);
    const rTop = Math.max(rBottom - h * Math.tan((angleDeg * Math.PI) / 180), 0);
    const samples: FormProfileSample[] = [
      { zMm: 0, rMm: round3(rBottom) },
      { zMm: round3(h), rMm: round3(rTop) },
    ];
    updateField(idx, 'formProfileMm', samples);
  }
  // T-slot preset — the former dedicated kind, now a form-profile.
  // A wide cutting disk at the tip (headDia) of height headThickness,
  // then a narrow neck (neckDia) up to the top of the neck. Transient
  // generator inputs, keyed by tool id like the dovetail ones.
  let tslotDraft = $state<
    Record<number, { headDiaMm: number; headThickMm: number; neckDiaMm: number; neckLenMm: number }>
  >({});
  function tslotParamsFor(id: number) {
    return tslotDraft[id] ?? { headDiaMm: 12.7, headThickMm: 3, neckDiaMm: 6, neckLenMm: 6 };
  }
  function setTslotParam(
    id: number,
    key: 'headDiaMm' | 'headThickMm' | 'neckDiaMm' | 'neckLenMm',
    v: number,
  ) {
    tslotDraft = { ...tslotDraft, [id]: { ...tslotParamsFor(id), [key]: v } };
  }
  function generateTslot(idx: number, id: number) {
    const { headDiaMm, headThickMm, neckDiaMm, neckLenMm } = tslotParamsFor(id);
    const rHead = Math.max(headDiaMm / 2, 0);
    const rNeck = Math.max(Math.min(neckDiaMm / 2, rHead), 0);
    const hHead = Math.max(headThickMm, 0);
    const samples: FormProfileSample[] = [
      { zMm: 0, rMm: round3(rHead) },
      { zMm: round3(hHead), rMm: round3(rHead) },
      { zMm: round3(hHead), rMm: round3(rNeck) },
      { zMm: round3(hHead + Math.max(neckLenMm, 0)), rMm: round3(rNeck) },
    ];
    updateField(idx, 'formProfileMm', samples);
  }
  function addProfileRow(idx: number, tool: ToolEntry) {
    const rows = tool.formProfileMm ?? [];
    const last = rows[rows.length - 1];
    const next: FormProfileSample = last
      ? { zMm: round3(last.zMm + 1), rMm: last.rMm }
      : { zMm: 0, rMm: round3((tool.diameter ?? 0) / 2) };
    updateField(idx, 'formProfileMm', [...rows, next]);
  }
  function updateProfileRow(
    idx: number,
    tool: ToolEntry,
    row: number,
    key: 'zMm' | 'rMm',
    v: number,
  ) {
    const rows = (tool.formProfileMm ?? []).map((s, r) => (r === row ? { ...s, [key]: v } : s));
    updateField(idx, 'formProfileMm', rows);
  }
  function removeProfileRow(idx: number, tool: ToolEntry, row: number) {
    const rows = (tool.formProfileMm ?? []).filter((_, r) => r !== row);
    updateField(idx, 'formProfileMm', rows);
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
      const next: ToolEntry = { ...t, kind };
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
  const coolantLabels: Record<CoolantMode, string> = {
    off: 'Off',
    mist: 'Mist',
    flood: 'Flood',
  };
  const kindOptions = Object.keys(kindLabels) as ToolKind[];
  const coolantOptions = Object.keys(coolantLabels) as CoolantMode[];
  const holderKindLabels: Record<HolderKind, string> = {
    none: 'None',
    cylinder: 'Cylinder',
    cone: 'Cone',
    stepped: 'Stepped',
  };
  const holderKindOptions: HolderKind[] = ['none', 'cylinder', 'cone', 'stepped'];
</script>

{#snippet shell()}
  <header>
    <h2 id="tools-title">{isInventory ? 'Tool library — shop inventory' : 'Tool library'}</h2>
    {#if !embedded}
      <button class="dlg-close" onclick={close} aria-label="Close">×</button>
    {/if}
  </header>
  <div class="body" bind:this={bodyEl}>
    <div class="table">
      <div class="row head">
        <span>#</span>
        <span>Name</span>
        <span>Kind</span>
        <span>⌀ <span class="unit-hdr">mm</span></span>
        <span>tip ⌀ <span class="unit-hdr">mm</span></span>
        <span title="Full apex angle for V-bits / engravers — drives V-Carve depth."
          >tip ∠ <span class="unit-hdr">°</span></span
        >
        <span>flutes</span>
        <span>speed <span class="unit-hdr">RPM</span></span>
        <span>feed <span class="unit-hdr">mm/min</span></span>
        <span>plunge <span class="unit-hdr">mm/min</span></span>
        <span
          title="Default Z step (depth-per-pass) for operations using this tool. Negative number, mm. Empty = no default — every operation must set its own."
          >dflt step <span class="unit-hdr">mm</span></span
        >
        <span>coolant</span>
        <span></span>
      </div>
      {#each draft as tool, i (tool.id)}
        {#if rowVisible(tool)}
          <div class="row" class:highlight={highlightedId === tool.id} data-tool-id={tool.id}>
            <span class="id">
              <button
                class="expand"
                type="button"
                aria-expanded={expanded.has(tool.id)}
                aria-label={expanded.has(tool.id)
                  ? `Collapse holder section for tool ${tool.id}`
                  : `Expand holder section for tool ${tool.id}`}
                title={expanded.has(tool.id) ? 'Collapse holder section' : 'Expand holder section'}
                onclick={() => toggleExpanded(tool.id)}
                >{expanded.has(tool.id) ? '▾' : '▸'} {tool.id}</button
              >
            </span>
            <input
              type="text"
              value={tool.name}
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
              title={diameterInvalid(tool)
                ? 'Tool ⌀ must be greater than 0 mm — zero / negative values produce no toolpath.'
                : ''}
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
              placeholder={fieldApplies('tipDiameter', tool.kind) ? '—' : 'n/a'}
              disabled={!fieldApplies('tipDiameter', tool.kind)}
              class:invalid={tool.tipDiameter !== undefined && tool.tipDiameter < 0}
              title={!fieldApplies('tipDiameter', tool.kind)
                ? fieldDisabledReason('tipDiameter', tool.kind)
                : tool.tipDiameter !== undefined && tool.tipDiameter < 0
                  ? 'Tip ⌀ must be ≥ 0 mm — the V-Carve depth math (z = -(r - tip_r) / tan(angle / 2)) silently clamps negative values to 0, hiding the typo.'
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
              placeholder={fieldApplies('tipAngleDeg', tool.kind) ? '60' : 'n/a'}
              disabled={!fieldApplies('tipAngleDeg', tool.kind)}
              title={fieldApplies('tipAngleDeg', tool.kind)
                ? 'Full apex angle of the V cone in degrees. Drives V-Carve depth (z = -(r - tip_r) / tan(angle / 2)) and Chamfer width. Common values: 30°, 45°, 60°, 90°.'
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
                  ? 'Spindle speed must be ≥ 1 RPM — zero / negative values emit no S word and the controller may refuse.'
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
                ? 'Feed rate must be ≥ 1 mm/min — zero / negative values emit no F word and the controller stalls.'
                : tool.kind === 'drill'
                  ? 'For drill, this is the plunge feed.'
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
                  ? 'Plunge rate must be ≥ 1 mm/min — zero / negative values cause the controller to refuse the move.'
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
              placeholder={fieldApplies('defaultStep', tool.kind) ? '—' : 'n/a'}
              disabled={!fieldApplies('defaultStep', tool.kind)}
              title={fieldApplies('defaultStep', tool.kind)
                ? tool.defaultStep !== undefined && tool.defaultStep >= 0
                  ? 'Default step must be NEGATIVE (mm down per pass) — values ≥ 0 are ignored and treated as unset.'
                  : "Operations using this tool inherit this when they don't specify their own. Negative number, mm."
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
                <option value={c}>{coolantLabels[c]}</option>
              {/each}
            </select>
            <button
              class="del"
              onclick={() => removeAt(i)}
              disabled={draft.length <= 1}
              title={draft.length <= 1 ? 'At least one tool must remain' : 'Delete tool'}
              aria-label={draft.length <= 1
                ? 'At least one tool must remain'
                : `Delete tool ${tool.name}`}>×</button
            >
          </div>
          {#if expanded.has(tool.id)}
            <div class="holder-panel">
              <div class="holder-row">
                <span
                  class="holder-label"
                  title="Machine modes this tool kind can physically run on — a machine stocks only compatible tools."
                  >Runs on</span
                >
                {#each TOOL_COMPATIBLE_MODES[tool.kind] as m (m)}
                  <span class="cap-chip">{MACHINE_MODE_NOUN[m]}</span>
                {/each}
              </div>
              <div class="holder-row">
                <label>
                  <span>Flute length (mm)</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    placeholder="—"
                    value={tool.fluteLengthMm ?? ''}
                    title="Length of the cutting flutes from the tip up. Sets the height above the tip where the shank starts. Empty = treat the entire tool as cutting (no holder check)."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'fluteLengthMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>Overall length (mm)</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    placeholder="—"
                    value={tool.lengthMm ?? ''}
                    title="Overall / usable tool length (mm), tip to where the shank enters the collet. Display + 3D-preview only — it does NOT change the G-code (reach is driven by flute length + stickout + holder). Sets the preview tool's total height. Empty = diameter-derived heuristic."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'lengthMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>Shank ⌀ (mm)</span>
                  <input
                    type="number"
                    step="0.1"
                    min="0"
                    placeholder="= cutting ⌀"
                    value={tool.shankDiameterMm ?? ''}
                    title="Shank diameter above the cutting flutes. Empty = same as the cutting diameter (parallel-shank bit)."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'shankDiameterMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>Stickout (mm)</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    placeholder="—"
                    value={tool.stickoutLengthMm ?? ''}
                    title="Free shank length between the top of the cutting flutes and the bottom of the holder/collet (mm). Models reach-extension tooling where the collet doesn't grip right above the flutes. Empty / 0 = legacy behavior (collet sits directly on flutes)."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'stickoutLengthMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>Preset</span>
                  <select
                    title="Apply common ER-style holder presets — fills in flute length, shank, and holder fields with conservative ballpark values."
                    onchange={(e) => {
                      const sel = e.currentTarget as HTMLSelectElement;
                      if (sel.value) {
                        applyPreset(i, sel.value);
                        sel.value = '';
                      }
                    }}
                  >
                    <option value="">Apply…</option>
                    {#each presets as p (p.label)}
                      <option value={p.label}>{p.label}</option>
                    {/each}
                  </select>
                </label>
              </div>
              <div class="holder-row">
                <span class="holder-label">Holder</span>
                {#each holderKindOptions as k (k)}
                  <label class="radio">
                    <input
                      type="radio"
                      name="holder-kind-{tool.id}"
                      value={k}
                      checked={holderKind(tool) === k}
                      onchange={() => setHolderKind(i, k)}
                    />
                    <span>{holderKindLabels[k]}</span>
                  </label>
                {/each}
              </div>
              {#if tool.holder?.kind === 'cylinder'}
                <div class="holder-row">
                  <label>
                    <span>⌀ (mm)</span>
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
                    <span>Length (mm)</span>
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
                    <span>Bottom ⌀ (mm)</span>
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
                    <span>Top ⌀ (mm)</span>
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
                    <span>Length (mm)</span>
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
                    <span>Cyl ⌀ (mm)</span>
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
                    <span>Cyl length (mm)</span>
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
                    <span>Cone top ⌀ (mm)</span>
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
                    <span>Cone length (mm)</span>
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
                <span
                  class="holder-label"
                  title="Per-tool overrides for the wall-defining (finish) pass of a Pocket op and for Drill ops. Empty = inherit the main speed/feed/plunge values."
                  >Pass overrides</span
                >
              </div>
              <div class="holder-row">
                <label>
                  <span>Finish RPM</span>
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
                  <span>Finish feed (mm/min)</span>
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
                  <span>Finish plunge (mm/min)</span>
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
                  <span>Drill RPM</span>
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
                  <span>Drill feed (mm/min)</span>
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
                  <span>Drill plunge (mm/min)</span>
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
                  <span>Default peck (mm)</span>
                  <input
                    type="number"
                    step="0.1"
                    min="0"
                    placeholder="—"
                    value={tool.defaultPeckStepMm ?? ''}
                    title="Default peck step for Peck / ChipBreak drill cycles whose op leaves peck_step_mm at 0. Empty = the op must specify its own."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'defaultPeckStepMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>Default XY overlap (0..1)</span>
                  <input
                    type="number"
                    step="0.05"
                    min="0.05"
                    max="0.95"
                    placeholder="0.5"
                    value={tool.defaultXyOverlap ?? ''}
                    title="Per-tool default XY overlap for pocket / cascade ops (dr5). Empty = fall through to the global 0.5. Clamped to 0.05–0.95 at generate time."
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
                <label class="comment-row">
                  <span>Comment</span>
                  <textarea
                    rows="2"
                    value={tool.comment ?? ''}
                    placeholder="Notes about this tool — material, vendor, sharpening date, etc. Appears as the tooltip on the tool dropdown in op properties."
                    title="Free-text description. Doesn't affect any pipeline output."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLTextAreaElement).value;
                      updateField(i, 'comment', v === '' ? undefined : v);
                    }}
                  ></textarea>
                </label>
                <label>
                  <span>Z shift (mm)</span>
                  <input
                    type="number"
                    step="0.01"
                    placeholder="—"
                    value={tool.zShiftMm ?? ''}
                    title="Per-tool Z origin offset. For machines without auto tool-length probing. Pre-measure each tool's tip Z relative to a reference and record the delta. Positive = sticks out further; negative = shorter. A G92 Z<shift> line is emitted at program start and after each tool change. Empty / 0 = no shift."
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
                  <span>Spindle warmup (s)</span>
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    placeholder="1"
                    value={tool.pause ?? ''}
                    title="Dwell (seconds) emitted as G4 P<n> after every M3 / M4 so the spindle reaches commanded RPM before the cut starts. Critical on machines without spindle-at-speed feedback. Empty = default (1 s)."
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
                    ? `Drag-knife doesn't spin.`
                    : tool.kind === 'laser_beam'
                      ? `Laser has no spindle.`
                      : tool.kind === 'plasma_torch'
                        ? `Plasma torch has no spindle.`
                        : 'z1y0: spindle direction the post commands when this tool is selected. CW (M3) is the default; CCW (M4) is for left-hand cutters / reverse-thread / mirror-helix tooling.'}
                >
                  <legend>Spindle dir</legend>
                  <label class="radio">
                    <input
                      type="radio"
                      name="spindle-dir-{tool.id}"
                      value="cw"
                      checked={(tool.spindleDirection ?? 'cw') === 'cw'}
                      onchange={() => updateField(i, 'spindleDirection', undefined)}
                    />
                    <span>CW (M3)</span>
                  </label>
                  <label class="radio">
                    <input
                      type="radio"
                      name="spindle-dir-{tool.id}"
                      value="ccw"
                      checked={tool.spindleDirection === 'ccw'}
                      onchange={() => updateField(i, 'spindleDirection', 'ccw')}
                    />
                    <span>CCW (M4)</span>
                  </label>
                </fieldset>
              </div>
              <div class="holder-row pass-overrides">
                <span
                  class="holder-label"
                  title="Whirling (orbital milling) overlay. When enabled with an Extra width > 0, every cut move with this tool is subdivided and the cutter orbits in a helical spiral around the toolpath — keeping radial engagement bounded at every point, for chip-thinning on hard material. This is milling whirling, not lathe thread-whirling; to cut threads use a Thread operation with a thread-mill tool. Set Extra width to the orbit diameter, Stride to the path distance per revolution, Z-wobble to a small dip for chip clearance."
                  >Whirling</span
                >
              </div>
              <div class="holder-row">
                <label class="radio">
                  <input
                    type="checkbox"
                    checked={tool.whirl ?? false}
                    onchange={(e) =>
                      updateField(i, 'whirl', (e.currentTarget as HTMLInputElement).checked)}
                  />
                  <span>Enable</span>
                </label>
                <label>
                  <span>Extra width (mm)</span>
                  <input
                    type="number"
                    step="0.1"
                    min="0"
                    placeholder="0"
                    value={tool.whirlExtraWidthMm ?? ''}
                    disabled={!tool.whirl}
                    title="Diameter (mm) by which the whirling orbit widens the cut. Empty / 0 ⇒ overlay disabled (whirling is a no-op)."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'whirlExtraWidthMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
              </div>
              <div class="holder-row">
                <label>
                  <span>Stride (mm)</span>
                  <input
                    type="number"
                    step="0.1"
                    min="0.05"
                    placeholder={((tool.whirlExtraWidthMm ?? 0) * 0.5).toFixed(2)}
                    value={tool.whirlStepoverMm ?? ''}
                    disabled={!tool.whirl}
                    title="Path distance per full spiral revolution. Empty = half the spiral radius (one-revolution overlap)."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'whirlStepoverMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
                <label>
                  <span>Z-wobble (mm)</span>
                  <input
                    type="number"
                    step="0.05"
                    min="0"
                    placeholder="0"
                    value={tool.whirlOscMm ?? ''}
                    disabled={!tool.whirl}
                    title="Z ripple amplitude. The cutter dips up to 2·osc below the cut plane between revolutions, improving chip evacuation. Empty / 0 ⇒ flat (no Z motion from the overlay)."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'whirlOscMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
              </div>
              {#if attrApplies('dragoff', tool.kind)}
                <div class="holder-row pass-overrides">
                  <span
                    class="holder-label"
                    title="Drag-knife fields. The drag offset drives the pivot-arc compensation at corners."
                    >Drag-knife</span
                  >
                </div>
                <div class="holder-row">
                  <label>
                    <span>Drag offset (mm)</span>
                    <input
                      type="number"
                      step="0.05"
                      min="0"
                      placeholder="—"
                      value={tool.dragoff ?? ''}
                      title="Distance from the spindle axis to the cutting tip. The post emits a pivot arc at each corner so the blade trails through cleanly. 0 ⇒ no compensation (true tangent knife)."
                      onchange={(e) => {
                        const v = (e.currentTarget as HTMLInputElement).value;
                        updateField(i, 'dragoff', v === '' ? undefined : parseFloat(v));
                      }}
                    />
                  </label>
                  <label>
                    <span>Self-align angle (°)</span>
                    <input
                      type="number"
                      step="1"
                      min="0"
                      max="60"
                      placeholder="30"
                      value={tool.dragKnifeSelfAlignAngleDeg ?? ''}
                      title="Corners with a tangent change below this angle skip the explicit swivel arc — real drag knives self-align below ~30° via the trailing offset. Set to 0 to force a swivel at every corner (legacy behaviour). Empty = 30° default."
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
                  <span
                    class="holder-label"
                    title="Compression / up-down cutter: down-cut flutes on the upper part, up-cut on the lower, meeting at the transition height — clean edges on both faces of sheet stock."
                    >Compression</span
                  >
                </div>
                <div class="holder-row">
                  <label>
                    <span>Transition height (mm)</span>
                    <input
                      type="number"
                      step="0.5"
                      min="0"
                      placeholder="flute midpoint"
                      value={tool.compressionTransitionMm ?? ''}
                      title="Height above the tip where the down-cut flutes flip to up-cut. Set to your stock thickness so the flip lands at the bottom face. Display + preview only in v1 — it does NOT change the cut cross-section. Empty = the preview assumes the flute midpoint."
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
                  <span
                    class="holder-label"
                    title="Single-point thread mill: cuts threads by helical interpolation. The tip ∠ in the main row is the thread flank angle (60° metric / 55° Whitworth)."
                    >Thread mill</span
                  >
                </div>
                <div class="holder-row">
                  <label>
                    <span>Pitch (mm)</span>
                    <input
                      type="number"
                      step="0.05"
                      min="0"
                      placeholder="—"
                      value={tool.threadPitchMm ?? ''}
                      title="Thread pitch (mm) — the axial advance per orbit. e.g. 1.0 for M6×1, 1.5 for M10×1.5. Drives the helical Z-advance of the Thread op."
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
                  <span
                    class="holder-label"
                    title="Bull-nose endmill: corner radius rounds the cutter's bottom edge."
                    >Bull-nose</span
                  >
                </div>
                <div class="holder-row">
                  <label>
                    <span>Corner radius (mm)</span>
                    <input
                      type="number"
                      step="0.05"
                      min="0"
                      placeholder="—"
                      value={tool.cornerRadiusMm ?? ''}
                      title="Radius of the rounded corner where the flat bottom meets the side. Set to 0 (or empty) for a square endmill; set to half the diameter for a ball-nose."
                      onchange={(e) => {
                        const v = (e.currentTarget as HTMLInputElement).value;
                        updateField(i, 'cornerRadiusMm', v === '' ? undefined : parseFloat(v));
                      }}
                    />
                  </label>
                </div>
              {/if}
              {#if attrApplies('formProfile', tool.kind)}
                {@const dt = dovetailParamsFor(tool.id)}
                {@const ts = tslotParamsFor(tool.id)}
                {@const rows = tool.formProfileMm ?? []}
                <div class="holder-row pass-overrides">
                  <span
                    class="holder-label"
                    title="Form / profile cutter cross-section (cove / ogee / dovetail / T-slot / custom). The (z, r) table — height above the tip vs radius — drives the simulator's cut shape. Needs ≥2 rows; otherwise the sim falls back to a tip→diameter taper. Use a preset below or edit rows directly."
                    >Form profile</span
                  >
                </div>
                <div class="holder-row dovetail-gen">
                  <label>
                    <span>Dovetail ⌀ (mm)</span>
                    <input
                      type="number"
                      step="0.1"
                      min="0"
                      value={dt.diaMm}
                      title="Widest cutting diameter (at the bottom face) of a dovetail bit."
                      onchange={(e) =>
                        setDovetailParam(
                          tool.id,
                          'diaMm',
                          parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        )}
                    />
                  </label>
                  <label>
                    <span>Angle (°)</span>
                    <input
                      type="number"
                      step="1"
                      min="0"
                      max="89"
                      value={dt.angleDeg}
                      title="Flank angle from the tool axis. The radius narrows by tan(angle) per mm of rise. 7°–14° typical."
                      onchange={(e) =>
                        setDovetailParam(
                          tool.id,
                          'angleDeg',
                          parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        )}
                    />
                  </label>
                  <label>
                    <span>Cut height (mm)</span>
                    <input
                      type="number"
                      step="0.5"
                      min="0"
                      value={dt.heightMm}
                      title="Flute / cutting height — how tall the angled profile is from the bottom face up to the neck."
                      onchange={(e) =>
                        setDovetailParam(
                          tool.id,
                          'heightMm',
                          parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        )}
                    />
                  </label>
                  <button
                    type="button"
                    class="profile-btn"
                    title="Overwrite the sample table below with a 2-row dovetail profile generated from these inputs."
                    onclick={() => generateDovetail(i, tool.id)}>Generate dovetail</button
                  >
                </div>
                <div class="holder-row dovetail-gen">
                  <label>
                    <span>T-slot head ⌀ (mm)</span>
                    <input
                      type="number"
                      step="0.1"
                      min="0"
                      value={ts.headDiaMm}
                      title="Widest cutting-disk diameter at the tip of a T-slot / keyway cutter."
                      onchange={(e) =>
                        setTslotParam(
                          tool.id,
                          'headDiaMm',
                          parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        )}
                    />
                  </label>
                  <label>
                    <span>Head thick (mm)</span>
                    <input
                      type="number"
                      step="0.5"
                      min="0"
                      value={ts.headThickMm}
                      title="Height of the cutting disk (how tall the wide undercut head is)."
                      onchange={(e) =>
                        setTslotParam(
                          tool.id,
                          'headThickMm',
                          parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        )}
                    />
                  </label>
                  <label>
                    <span>Neck ⌀ (mm)</span>
                    <input
                      type="number"
                      step="0.1"
                      min="0"
                      value={ts.neckDiaMm}
                      title="Diameter of the narrow neck above the head — must be smaller than the head ⌀."
                      onchange={(e) =>
                        setTslotParam(
                          tool.id,
                          'neckDiaMm',
                          parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        )}
                    />
                  </label>
                  <label>
                    <span>Neck length (mm)</span>
                    <input
                      type="number"
                      step="0.5"
                      min="0"
                      value={ts.neckLenMm}
                      title="Length of the narrow neck above the head, up to where the shank begins."
                      onchange={(e) =>
                        setTslotParam(
                          tool.id,
                          'neckLenMm',
                          parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                        )}
                    />
                  </label>
                  <button
                    type="button"
                    class="profile-btn"
                    title="Overwrite the sample table below with a 4-row T-slot profile (wide disk → narrow neck) generated from these inputs."
                    onclick={() => generateTslot(i, tool.id)}>Generate T-slot</button
                  >
                </div>
                <div class="profile-table">
                  <div class="profile-table-head">
                    <span>z above tip (mm)</span>
                    <span>radius (mm)</span>
                    <span></span>
                  </div>
                  {#each rows as row, r (r)}
                    <div class="profile-row">
                      <input
                        type="number"
                        step="0.1"
                        min="0"
                        value={row.zMm}
                        aria-label="z above tip (mm)"
                        onchange={(e) =>
                          updateProfileRow(
                            i,
                            tool,
                            r,
                            'zMm',
                            parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                          )}
                      />
                      <input
                        type="number"
                        step="0.1"
                        min="0"
                        value={row.rMm}
                        aria-label="radius (mm)"
                        onchange={(e) =>
                          updateProfileRow(
                            i,
                            tool,
                            r,
                            'rMm',
                            parseFloat((e.currentTarget as HTMLInputElement).value) || 0,
                          )}
                      />
                      <button
                        type="button"
                        class="profile-btn del"
                        title="Delete this sample row"
                        onclick={() => removeProfileRow(i, tool, r)}>✕</button
                      >
                    </div>
                  {/each}
                  <div class="profile-actions">
                    <button type="button" class="profile-btn" onclick={() => addProfileRow(i, tool)}
                      >+ Add row</button
                    >
                    {#if rows.length < 2}
                      <span class="profile-hint"
                        >Add at least 2 rows (tip → top) for the sim to carve the real profile.</span
                      >
                    {/if}
                  </div>
                </div>
              {/if}
              {#if attrApplies('wear', tool.kind)}
                <div class="holder-row pass-overrides">
                  <span
                    class="holder-label"
                    title="Wear compensation. Toolpaths cut at the nominal diameter minus this offset — the bit's TRUE cutting diameter after wear or a regrind. The nominal diameter above stays what's printed on the bit."
                    >Wear</span
                  >
                </div>
                <div class="holder-row">
                  <label>
                    <span>Wear offset (mm)</span>
                    <input
                      type="number"
                      step="0.01"
                      placeholder="0"
                      value={tool.wearOffsetMm ?? ''}
                      title="Difference between the bit's nominal and measured cutting diameter (positive = worn smaller). Empty / 0 = cut at the nominal diameter. Run Calibrate to measure it."
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
                    title="Measure the bit's true cutting diameter with a slot test cut and store the wear offset."
                    >Calibrate…</button
                  >
                  <span class="cal-status">
                    {#if tool.lastCalibrated}
                      Last calibrated {tool.lastCalibrated}
                      {#if isCalibrationStale(tool.lastCalibrated, new Date())}
                        <span
                          class="stale-chip"
                          title="This measurement is more than 90 days old — bits keep wearing; re-run the calibration."
                          >Stale calibration</span
                        >
                      {/if}
                    {:else}
                      Never calibrated
                    {/if}
                  </span>
                  {#if (tool.wearOffsetMm ?? 0) !== 0}
                    <span class="eff-hint">cuts as {effectiveDiameterHint(tool)}</span>
                  {/if}
                </div>
              {/if}
              {#if attrApplies('laser', tool.kind)}
                <div class="holder-row pass-overrides">
                  <span
                    class="holder-label"
                    title="Laser-only fields. Honored when this tool fires the cut.">Laser</span
                  >
                </div>
                <div class="holder-row">
                  <label>
                    <span>Pierce time (s)</span>
                    <input
                      type="number"
                      step="0.05"
                      min="0"
                      placeholder="—"
                      value={tool.laserPierceSec ?? ''}
                      title="Seconds the beam dwells at the entry point with laser ON before the cut begins. Critical for piercing thick acrylic / wood — without it the first millimeter is gouged. Emitted as a G4 P<sec> between rapid-to-entry and plunge."
                      onchange={(e) => {
                        const v = (e.currentTarget as HTMLInputElement).value;
                        updateField(i, 'laserPierceSec', v === '' ? undefined : parseFloat(v));
                      }}
                    />
                  </label>
                  <label>
                    <span>Lead-in (mm)</span>
                    <input
                      type="number"
                      step="0.1"
                      min="0"
                      placeholder="—"
                      value={tool.laserLeadInMm ?? ''}
                      title="Per-tool lead-in distance (mm) the laser head travels before reaching the cut path, to reduce edge entry burn. Used as the lead-in length for laser ops that don't override leads themselves."
                      onchange={(e) => {
                        const v = (e.currentTarget as HTMLInputElement).value;
                        updateField(i, 'laserLeadInMm', v === '' ? undefined : parseFloat(v));
                      }}
                    />
                  </label>
                  <label>
                    <span>Kerf (mm)</span>
                    <input
                      type="number"
                      step="0.01"
                      min="0"
                      placeholder="0.15"
                      value={tool.kerfMm ?? ''}
                      title="Laser kerf width (mm) — the heightmap-side spot radius the sim carves at. Empty / 0 = the legacy 0.15 mm default in the Rust sim. Set to your measured kerf on the actual stock for accurate heightmap previews."
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
                  <span
                    class="holder-label"
                    title="Plasma torch entry sequence. The torch pierces at the pierce height, dwells the pierce delay, then drops to the cut height for the cut."
                    >Plasma</span
                  >
                </div>
                <div class="holder-row">
                  <label>
                    <span>Pierce height (mm)</span>
                    <input
                      type="number"
                      step="0.1"
                      min="0"
                      placeholder="3.8"
                      value={tool.pierceHeightMm ?? ''}
                      title="Height above the stock where the arc is established before dropping to the cut height. Too low and the torch sticks to the slag; too high and the arc drops out. Typical 3–5 mm for 1–3 mm steel. Empty = 3.8 mm default."
                      onchange={(e) => {
                        const v = (e.currentTarget as HTMLInputElement).value;
                        updateField(i, 'pierceHeightMm', v === '' ? undefined : parseFloat(v));
                      }}
                    />
                  </label>
                  <label>
                    <span>Cut height (mm)</span>
                    <input
                      type="number"
                      step="0.1"
                      min="0"
                      placeholder="1.5"
                      value={tool.cutHeightMm ?? ''}
                      title="Height above the stock the torch drops to for the actual cut (below the pierce height). Typical 1.5–2.5 mm for thin steel. Empty = 1.5 mm default."
                      onchange={(e) => {
                        const v = (e.currentTarget as HTMLInputElement).value;
                        updateField(i, 'cutHeightMm', v === '' ? undefined : parseFloat(v));
                      }}
                    />
                  </label>
                  <label>
                    <span>Pierce delay (s)</span>
                    <input
                      type="number"
                      step="0.1"
                      min="0"
                      placeholder="0.5"
                      value={tool.pierceDelaySec ?? ''}
                      title="Seconds the torch dwells at the pierce height before dropping to the cut height — long enough to pierce the stock, short enough not to undercut the rim. Typical 0.4 s for 1 mm steel, up to ~1.5 s for 6 mm. Empty = 0.5 s default."
                      onchange={(e) => {
                        const v = (e.currentTarget as HTMLInputElement).value;
                        updateField(i, 'pierceDelaySec', v === '' ? undefined : parseFloat(v));
                      }}
                    />
                  </label>
                  <label>
                    <span>Kerf (mm)</span>
                    <input
                      type="number"
                      step="0.1"
                      min="0"
                      placeholder="—"
                      value={tool.kerfMm ?? ''}
                      title="Plasma cut width (kerf). The toolpath is offset by kerf/2 so the cut edge lands on the geometry — the same compensation a milling cutter gets from its diameter. Measure on your actual stock/amperage. Empty = no kerf compensation (cut on the nominal path)."
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
        {/if}
      {/each}
      {#if !showIncompatible && incompatibleCount > 0}
        <div class="mode-filter-row">
          <span
            >{incompatibleCount}
            {incompatibleCount === 1 ? 'tool' : 'tools'} hidden (incompatible with a {machineModesLabel(
              machineModes,
            )} machine)</span
          >
          <button
            type="button"
            class="btn-secondary"
            onclick={() => (showIncompatible = true)}
            title="Show every tool in the library, including ones the machine's mode/capabilities can't run. The library is never modified by a mode switch — this only changes the view."
            >Show all</button
          >
        </div>
      {:else if showIncompatible && incompatibleCount > 0}
        <div class="mode-filter-row">
          <span
            >Showing all tools — {incompatibleCount} can't run on a {machineModesLabel(
              machineModes,
            )} machine</span
          >
          <button type="button" class="btn-secondary" onclick={() => (showIncompatible = false)}
            >Hide incompatible</button
          >
        </div>
      {/if}
    </div>
    <button class="add" onclick={addTool}>+ Add tool</button>
  </div>
  <footer>
    {#if dd.confirmingDiscard}
      <span class="discard-prompt">Discard unsaved changes?</span>
      <button class="btn-secondary" onclick={() => dd.cancelDiscard()}>Keep editing</button>
      <button class="btn-danger" onclick={close}>Discard</button>
    {:else}
      {#if !isInventory}
        <button
          class="btn-secondary"
          onclick={async () => {
            await fileOps.saveToolset();
          }}
          title="Save the current tool library to a .ivac-toolset.json file."
        >
          Save…
        </button>
      {/if}
      {#if !isInventory}
        <button
          class="btn-secondary"
          onclick={async () => {
            await fileOps.loadToolset('replace');
            // Editor draft must follow the new tools so the dialog
            // doesn't keep showing stale entries. Assigned directly
            // (not dd.open) so the pristine snapshot from open is
            // kept — a load counts as an unsaved change.
            dd.draft = project.data.tools.map((t) => ({ ...t }));
          }}
          title="Replace the current tools with the contents of a .ivac-toolset.json file."
        >
          Load (replace)…
        </button>
      {/if}
      {#if !isInventory}
        <button
          class="btn-secondary"
          onclick={async () => {
            await fileOps.loadToolset('add');
            dd.draft = project.data.tools.map((t) => ({ ...t }));
          }}
          title="Add tools from a .ivac-toolset.json file. Tools whose name already exists are skipped."
        >
          Load (add)…
        </button>
      {/if}
      <span class="sep"></span>
      {#if hasInvalidRow}
        <!-- Surface why OK is greyed out so the user knows which inputs need fixing. -->
        <span class="validation-msg" role="status"
          >Fix highlighted fields (⌀, RPM, feed, plunge must be &gt; 0).</span
        >
      {/if}
      {#if embedded}
        <button class="btn-secondary" onclick={revert} disabled={!dd.isDirty}>Revert</button>
        <button
          class="btn-primary"
          onclick={commit}
          disabled={hasInvalidRow || !dd.isDirty}
          title={hasInvalidRow ? 'Fix the highlighted fields before applying.' : ''}>Apply</button
        >
      {:else}
        <button class="btn-secondary" onclick={close}>Cancel</button>
        <button
          class="btn-primary"
          onclick={commit}
          disabled={hasInvalidRow}
          title={hasInvalidRow ? 'Fix the highlighted fields before saving.' : ''}>OK</button
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
  .embedded-shell .body {
    flex: 1;
    min-height: 0;
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
       scrolling through a long tool library; was previously scrolling
       off and leaving rows context-free. `.body` is the scroll container. */
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
  .profile-btn.del {
    padding: 0.2rem 0.4rem;
    color: var(--text-muted);
  }
  .profile-table {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin-top: 0.3rem;
  }
  .profile-table-head,
  .profile-row {
    display: grid;
    grid-template-columns: 8rem 8rem 2rem;
    gap: 0.4rem;
    align-items: center;
  }
  .profile-table-head span {
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
  }
  .profile-row input {
    width: 100%;
  }
  .profile-actions {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 0.2rem;
  }
  .profile-hint {
    font-size: 0.7rem;
    color: var(--text-muted);
  }
</style>
