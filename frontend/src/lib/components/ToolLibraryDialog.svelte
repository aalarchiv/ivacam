<script lang="ts">
  /// Tool library dialog. Project-scoped table of every tool the user
  /// has configured; ops will reference an entry by id once UX-7 lands.
  /// Each row is editable in place; the modal commits/cancels as a
  /// single unit so the user can revert without touching project.tools.
  import {
    project,
    type ToolEntry,
    type ToolKind,
    type CoolantMode,
    type HolderShape,
  } from '../state/project.svelte';
  import Modal from './Modal.svelte';
  import * as fileOps from '../state/file_ops';

  interface Props {
    open: boolean;
    onClose: () => void;
  }
  let { open, onClose }: Props = $props();

  let draft = $state<ToolEntry[]>([]);
  /// Per-row UI flag — Holder sub-panel collapsed by default to keep the
  /// table compact. Stored as a Set of row ids so reorders / additions
  /// don't accidentally move the toggle to a different tool.
  let expanded = $state<Set<number>>(new Set());
  /// Tool id that flashes briefly when the dialog is opened with a focus
  /// request (the "edit this tool" link in OpPropertiesPanel).
  let highlightedId = $state<number | null>(null);
  let bodyEl = $state<HTMLDivElement | null>(null);
  /// Snapshot captured at open — dirty check compares a deep clone of
  /// the draft (via the deepEqual helper) so X / Esc / click-outside
  /// can prompt before silently discarding edits (audit-dh1n).
  ///
  /// 1xgj: was `JSON.stringify(newDraft)` vs `JSON.stringify(draft)`,
  /// which is sensitive to key-order shuffle on $state proxies.
  /// Likely benign in practice — object literal updates via `...t`
  /// preserve key order — but a deep-equal compare is robust to any
  /// future code path that rebuilds the row by destructuring.
  let pristine = $state<ToolEntry[]>([]);

  /// 1xgj: minimal recursive deep-equal. Handles primitives, arrays,
  /// and plain object records — which is everything we ever store in a
  /// ToolEntry (no Dates, Maps, Sets, class instances). Skips
  /// prototype-walking, getters, and circular detection on purpose:
  /// ToolEntry is a flat JSON shape so none of those apply.
  function deepEqual(a: unknown, b: unknown): boolean {
    if (a === b) return true;
    if (a === null || b === null) return false;
    if (typeof a !== 'object' || typeof b !== 'object') return false;
    if (Array.isArray(a) !== Array.isArray(b)) return false;
    if (Array.isArray(a) && Array.isArray(b)) {
      if (a.length !== b.length) return false;
      for (let i = 0; i < a.length; i++) {
        if (!deepEqual(a[i], b[i])) return false;
      }
      return true;
    }
    const ao = a as Record<string, unknown>;
    const bo = b as Record<string, unknown>;
    const aKeys = Object.keys(ao);
    const bKeys = Object.keys(bo);
    if (aKeys.length !== bKeys.length) return false;
    for (const k of aKeys) {
      if (!Object.prototype.hasOwnProperty.call(bo, k)) return false;
      if (!deepEqual(ao[k], bo[k])) return false;
    }
    return true;
  }

  $effect(() => {
    if (open) {
      // Build the pristine snapshot from a local before assigning the
      // $state proxies. Reading `draft` after we've just written it
      // makes the effect depend on draft, and the write reschedules
      // the effect — Svelte throws `effect_update_depth_exceeded`
      // after ~1000 self-runs, killing the reactivity scheduler for
      // the whole app (same root cause we just fixed in
      // MachineDialog).
      const newDraft = project.tools.map((t) => ({ ...t }));
      draft = newDraft;
      // k94n: tools whose kind has a REQUIRED kind-specific field
      // open by default so the user sees `dragoff` / `cornerRadiusMm`
      // / T-slot neck dims without hunting for them. Other kinds
      // start collapsed.
      expanded = new Set(
        newDraft.filter((t) => kindNeedsExpansion(t.kind)).map((t) => t.id),
      );
      // 1xgj: snapshot a deep clone so subsequent edits to nested
      // fields (e.g. `tool.holder.diameter_mm`) don't mutate the
      // pristine reference. structuredClone handles the nested
      // `holder` discriminated union correctly.
      pristine = structuredClone(newDraft) as ToolEntry[];
    }
  });

  /// Kinds whose kind-specific block in the expanded row is
  /// LOAD-BEARING — without those fields the gcode emitter falls
  /// back to defaults that almost always produce wrong output.
  /// Auto-expanding the row keeps the field visible (k94n).
  function kindNeedsExpansion(kind: ToolKind): boolean {
    return kind === 'drag_knife' || kind === 'bull_nose' || kind === 't_slot';
  }

  // 1xgj: was `JSON.stringify(draft) !== pristine` — fragile because
  // a Svelte 5 $state proxy could in principle iterate keys in a
  // different order than the source object, falsely flagging the
  // dialog as dirty. Deep-equal is invariant to key order.
  let isDirty = $derived.by(() => open && !deepEqual(draft, pristine));

  /// jkgj: numeric-field validation. Before, every input on a row used
  /// `parseFloat(...) || 0` with no `min` attribute, so 0 or a
  /// negative diameter / RPM / feed silently committed and the
  /// pipeline produced zero-rate gcode (zero_rate_emitted) or worse.
  /// These predicates classify each row as invalid so:
  ///   * the input gets `.invalid` (red border, same pattern as
  ///     `defaultStep`),
  ///   * OK is disabled while any row is broken.
  ///
  /// Per-field rule:
  ///   * diameter:  must be > 0 (mm) when the kind cuts at all
  ///   * speed:     must be ≥ 1 RPM when fieldApplies('speed')
  ///   * feedRate:  must be ≥ 1 mm/min (always required)
  ///   * plungeRate: must be ≥ 1 mm/min when fieldApplies('plunge')
  ///
  /// Disabled fields (drag-knife speed, laser plunge, etc.) are
  /// excluded — fieldApplies() already says they're not used.
  function diameterInvalid(t: ToolEntry): boolean {
    // HTML min="0.01" is the actual floor — flag anything below it as
    // invalid so the user gets the red border (the prior `> 0` accepted
    // 0.005 mm which is below the HTML constraint and gets clamped /
    // ignored downstream without surface feedback).
    return !(t.diameter >= 0.01);
  }
  function speedInvalid(t: ToolEntry): boolean {
    if (!fieldApplies('speed', t.kind)) return false;
    return !(t.speed >= 1);
  }
  function feedInvalid(t: ToolEntry): boolean {
    return !(t.feedRate >= 1);
  }
  function plungeInvalid(t: ToolEntry): boolean {
    if (!fieldApplies('plunge', t.kind)) return false;
    return !(t.plungeRate >= 1);
  }
  function rowInvalid(t: ToolEntry): boolean {
    return (
      diameterInvalid(t) ||
      speedInvalid(t) ||
      feedInvalid(t) ||
      plungeInvalid(t) ||
      (t.defaultStep !== undefined && t.defaultStep >= 0)
    );
  }
  let hasInvalidRow = $derived(draft.some(rowInvalid));

  /// Two-step close-on-dirty: first attempt arms `confirmingDiscard`
  /// so the footer swaps to a "Discard / Keep editing" pair; second
  /// click on Discard actually fires `onClose`. Replaces the prior
  /// `window.confirm` prompt, which silently returns false in some
  /// Tauri / WebKitGTK builds (audit-C10).
  let confirmingDiscard = $state(false);

  function close() {
    if (isDirty) {
      confirmingDiscard = true;
      return;
    }
    onClose();
  }

  function discardAndClose() {
    confirmingDiscard = false;
    onClose();
  }

  function cancelDiscard() {
    confirmingDiscard = false;
  }

  $effect(() => {
    const focusId = project.toolsDialogFocusId;
    if (!open || focusId == null) return;
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
    // jkgj: refuse to commit while any row has an invalid numeric
    // field. The OK button is also disabled in that state — this is
    // belt-and-braces so a keyboard / programmatic invocation can't
    // smuggle a zero-rate tool through.
    if (draft.some(rowInvalid)) return;
    // Deep-snapshot so the command system receives plain objects —
    // Svelte 5 `$state` proxies inside `draft[i]` can trip up the
    // `structuredClone` call inside replaceToolsCommand on some
    // production builds, which would silently abort and leave the
    // dialog open.
    if (draft.length > 0) {
      const snap = JSON.parse(JSON.stringify(draft)) as typeof draft;
      try {
        project.replaceTools(snap);
      } catch (e) {
        console.error('ToolLibraryDialog.commit: replaceTools failed', e);
      }
    }
    onClose();
  }

  function addTool() {
    const nextId = (draft.reduce((m, t) => Math.max(m, t.id), 0) || 0) + 1;
    draft = [
      ...draft,
      {
        id: nextId,
        name: `Tool #${nextId}`,
        kind: 'endmill',
        diameter: 3,
        flutes: 2,
        speed: 18000,
        plungeRate: 100,
        feedRate: 800,
        coolant: 'off',
      },
    ];
  }

  function removeAt(idx: number) {
    if (draft.length <= 1) return;
    draft = draft.filter((_, i) => i !== idx);
  }

  function updateField<K extends keyof ToolEntry>(idx: number, key: K, value: ToolEntry[K]) {
    draft = draft.map((t, i) => (i === idx ? { ...t, [key]: value } : t));
  }

  /// Per-kind default fill-in on `kind` change. Pre-populates the
  /// fields that newly APPLY to the target kind so the user doesn't
  /// see blank inputs when flipping endmill → drill (twist drills
  /// usually have 2 flutes and a 118° tip). Existing user-set values
  /// are preserved.
  function onKindChange(idx: number, kind: ToolKind) {
    let touchedId: number | null = null;
    draft = draft.map((t, i) => {
      if (i !== idx) return t;
      const next: ToolEntry = { ...t, kind };
      if (kind === 'drill') {
        if (next.flutes === 0 || next.flutes === undefined) next.flutes = 2;
        if (next.tipAngleDeg === undefined) next.tipAngleDeg = 118;
      }
      if ((kind === 'v_bit' || kind === 'engraver') && next.tipAngleDeg === undefined) {
        next.tipAngleDeg = 60;
      }
      touchedId = next.id;
      return next;
    });
    // k94n: open the expanded section so the new kind's required
    // kind-specific field is in view (e.g. dragoff for drag_knife).
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
    draft = draft.map((t, i) => (i === idx ? { ...t, ...patch } : t));
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
    draft = draft.map((t, i) => (i === idx ? { ...t, holder: next } : t));
  }

  function updateHolderField(idx: number, key: string, value: number) {
    const cur = draft[idx];
    if (!cur.holder) return;
    const updated = { ...cur.holder, [key]: value } as HolderShape;
    draft = draft.map((t, i) => (i === idx ? { ...t, holder: updated } : t));
  }

  const kindLabels: Record<ToolKind, string> = {
    endmill: 'Endmill',
    ball_nose: 'Ball-nose',
    v_bit: 'V-bit',
    engraver: 'Engraver',
    drag_knife: 'Drag-knife',
    drill: 'Drill',
    laser_beam: 'Laser',
    bull_nose: 'Bull-nose (radius)',
    compression: 'Compression',
    t_slot: 'T-slot',
    form_profile: 'Form / profile',
  };
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

  /// Whether a given main-row field is meaningful for the tool kind.
  /// Inapplicable fields are kept in the grid (so the row layout
  /// stays stable) but disabled with a tooltip explaining why —
  /// mirrors Estlcam's c_Tools (`_TP`) which hides per-type rows.
  function fieldApplies(field: string, kind: ToolKind): boolean {
    switch (field) {
      case 'flutes':
        // Drills HAVE flutes (twist drills typically 2; some 3-/4-flute);
        // Estlcam exposes the field for Bohrer-type tools too (_TP.Flutes).
        return !['drag_knife', 'laser_beam'].includes(kind);
      case 'tipDiameter':
        return ['v_bit', 'engraver'].includes(kind);
      case 'speed':
        return !['drag_knife', 'laser_beam'].includes(kind);
      case 'plunge':
        return !['drag_knife', 'laser_beam', 'drill'].includes(kind);
      case 'defaultStep':
        // Drill has its own peck-step in the expanded section, not
        // the generic Z step.
        return !['drag_knife', 'laser_beam', 'drill'].includes(kind);
      case 'tipAngleDeg':
        // V-bits / engravers use the apex angle for V-Carve depth math;
        // drills carry the conical-tip apex (typically 118°) for the
        // 3D-preview mesh + a future drill-into-stock collision model.
        return ['v_bit', 'engraver', 'drill'].includes(kind);
      case 'coolant':
        // Laser uses gas-assist (not implemented yet) — coolant
        // dropdown still applies as a generic "assist" toggle.
        return true;
      default:
        return true;
    }
  }

  function fieldReasonForKind(field: string, kind: ToolKind): string {
    const k = kindLabels[kind];
    if (field === 'flutes' && kind === 'drag_knife') return `Drag-knife doesn't cut by rotation.`;
    if (field === 'flutes' && kind === 'laser_beam') return `Laser has no cutting edges.`;
    if (field === 'flutes') return `Flutes not used for ${k.toLowerCase()}.`;
    if (field === 'tipDiameter') return `Tip ⌀ only applies to V-bits / engravers.`;
    if (field === 'speed' && kind === 'drag_knife') return `Drag-knife doesn't spin.`;
    if (field === 'speed' && kind === 'laser_beam')
      return `Laser uses power (set in machine config), not RPM.`;
    if (field === 'plunge' && kind === 'drag_knife') return `Drag-knife stays at cut depth.`;
    if (field === 'plunge' && kind === 'laser_beam') return `Laser cuts at constant Z.`;
    if (field === 'plunge' && kind === 'drill')
      return `Drill uses the cut feed as its plunge rate.`;
    if (field === 'defaultStep' && kind === 'drill')
      return `Drill uses the peck step in the expanded section, not the generic Z step.`;
    if (field === 'defaultStep' && kind === 'drag_knife') return `Drag-knife runs at fixed depth.`;
    if (field === 'defaultStep' && kind === 'laser_beam') return `Laser cuts at constant Z.`;
    if (field === 'tipAngleDeg')
      return `Tip angle drives V-Carve depth math (V-bits / engravers) and the drill-tip 3D preview.`;
    return '';
  }
</script>

{#if open}
  <Modal
    onClose={close}
    persistKey="tool-library"
    width="min(960px, 96vw)"
    draggable
    resizable
    ariaLabelledBy="tools-title"
  >
    <header>
      <h2 id="tools-title">Tool library</h2>
      <button class="dlg-close" onclick={close} aria-label="Close">×</button>
    </header>
    <div class="body" bind:this={bodyEl}>
      <div class="table">
        <div class="row head">
          <span>#</span>
          <span>Name</span>
          <span>Kind</span>
          <span>⌀ <span class="unit-hdr">mm</span></span>
          <span>tip ⌀ <span class="unit-hdr">mm</span></span>
          <span title="Full apex angle for V-bits / engravers — drives V-Carve depth.">tip ∠ <span class="unit-hdr">°</span></span>
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
                ? fieldReasonForKind('tipDiameter', tool.kind)
                : tool.tipDiameter !== undefined && tool.tipDiameter < 0
                  ? 'Tip ⌀ must be ≥ 0 mm — the V-Carve depth math (z = -(r - tip_r) / tan(angle / 2)) silently clamps negative values to 0, hiding the typo.'
                  : ''}
              onchange={(e) => {
                // wz0r: reject negative tip ⌀ — Rust setup_resolver.rs:669
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
                : fieldReasonForKind('tipAngleDeg', tool.kind)}
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
                : fieldReasonForKind('flutes', tool.kind)}
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
                ? fieldReasonForKind('speed', tool.kind)
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
                ? fieldReasonForKind('plunge', tool.kind)
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
                : fieldReasonForKind('defaultStep', tool.kind)}
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
                    title="q0kc: free shank length between the top of the cutting flutes and the bottom of the holder/collet (mm). Models reach-extension tooling where the collet doesn't grip right above the flutes. Empty / 0 = legacy behavior (collet sits directly on flutes)."
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
                    title="Free-text description (rt1.31). Doesn't affect any pipeline output."
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
                    title="Per-tool Z origin offset (rt1.30). For machines without auto tool-length probing. Pre-measure each tool's tip Z relative to a reference and record the delta. Positive = sticks out further; negative = shorter. A G92 Z<shift> line is emitted at program start and after each tool change. Empty / 0 = no shift."
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
                  disabled={tool.kind === 'drag_knife' || tool.kind === 'laser_beam'}
                  title={tool.kind === 'drag_knife'
                    ? `Drag-knife doesn't spin.`
                    : tool.kind === 'laser_beam'
                      ? `Laser has no spindle.`
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
                  title="Wirbeln helical-spiral overlay (3e5 / Estlcam Flooper). When enabled with an Extra width > 0, every cut move with this tool is subdivided and the cutter centerline spirals around the toolpath — bounded engagement at every point. Set Extra width to the spiral diameter (Estlcam Wirbelzusatzbreite), Stride to the path distance per revolution, Osc to a small Z-wobble for chip clearance."
                  >Wirbeln (helical overlay)</span
                >
              </div>
              <div class="holder-row">
                <label class="radio">
                  <input
                    type="checkbox"
                    checked={tool.wirbeln ?? false}
                    onchange={(e) =>
                      updateField(i, 'wirbeln', (e.currentTarget as HTMLInputElement).checked)}
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
                    value={tool.wirbelnExtraWidthMm ?? ''}
                    disabled={!tool.wirbeln}
                    title="Estlcam Wirbelzusatzbreite — diameter (mm) by which the spiral overlay widens the cut. Empty / 0 ⇒ overlay disabled (Wirbeln is a no-op)."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(
                        i,
                        'wirbelnExtraWidthMm',
                        v === '' ? undefined : parseFloat(v),
                      );
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
                    placeholder={((tool.wirbelnExtraWidthMm ?? 0) * 0.5).toFixed(2)}
                    value={tool.wirbelnStepoverMm ?? ''}
                    disabled={!tool.wirbeln}
                    title="Path distance per full spiral revolution. Empty = half the spiral radius (one-revolution overlap)."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'wirbelnStepoverMm', v === '' ? undefined : parseFloat(v));
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
                    value={tool.wirbelnOscMm ?? ''}
                    disabled={!tool.wirbeln}
                    title="Z ripple amplitude. The cutter dips up to 2·osc below the cut plane between revolutions, improving chip evacuation. Empty / 0 ⇒ flat (no Z motion from the overlay)."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'wirbelnOscMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
              </div>
              {#if tool.kind === 'drag_knife'}
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
                </div>
              {/if}
              {#if tool.kind === 'bull_nose'}
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
              {#if tool.kind === 't_slot'}
                <div class="holder-row pass-overrides">
                  <span
                    class="holder-label"
                    title="T-slot cutter geometry: the neck connects the shank to the wider cutting head."
                    >T-slot</span
                  >
                </div>
                <div class="holder-row">
                  <label>
                    <span>Neck ⌀ (mm)</span>
                    <input
                      type="number"
                      step="0.1"
                      min="0"
                      placeholder="—"
                      value={tool.tslotNeckDiameterMm ?? ''}
                      title="Diameter of the narrow neck above the cutting head. Must be smaller than the cutter ⌀ (otherwise it's a regular endmill)."
                      onchange={(e) => {
                        const v = (e.currentTarget as HTMLInputElement).value;
                        updateField(i, 'tslotNeckDiameterMm', v === '' ? undefined : parseFloat(v));
                      }}
                    />
                  </label>
                  <label>
                    <span>Neck length (mm)</span>
                    <input
                      type="number"
                      step="0.5"
                      min="0"
                      placeholder="—"
                      value={tool.tslotNeckLengthMm ?? ''}
                      title="Length of the neck section from the top of the cutting head up to where the shank begins."
                      onchange={(e) => {
                        const v = (e.currentTarget as HTMLInputElement).value;
                        updateField(i, 'tslotNeckLengthMm', v === '' ? undefined : parseFloat(v));
                      }}
                    />
                  </label>
                </div>
              {/if}
              {#if tool.kind === 'laser_beam'}
                <div class="holder-row pass-overrides">
                  <span
                    class="holder-label"
                    title="Laser-only fields (rt1.29). Honored when this tool fires the cut."
                    >Laser</span
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
                      title="mmu8: laser kerf width (mm) — the heightmap-side spot radius the sim carves at. Empty / 0 = the legacy 0.15 mm default in the Rust sim. Set to your measured kerf on the actual stock for accurate heightmap previews."
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
      </div>
      <button class="add" onclick={addTool}>+ Add tool</button>
    </div>
    <footer>
      {#if confirmingDiscard}
        <span class="discard-prompt">Discard unsaved changes?</span>
        <button class="btn-secondary" onclick={cancelDiscard}>Keep editing</button>
        <button class="btn-danger" onclick={discardAndClose}>Discard</button>
      {:else}
        <button
          class="btn-secondary"
          onclick={async () => {
            await fileOps.saveToolset();
          }}
          title="Save the current tool library to a .wiac-toolset.json file."
        >
          Save…
        </button>
        <button
          class="btn-secondary"
          onclick={async () => {
            await fileOps.loadToolset('replace');
            // Editor draft must follow the new tools so the dialog
            // doesn't keep showing stale entries.
            draft = project.tools.map((t) => ({ ...t }));
          }}
          title="Replace the current tools with the contents of a .wiac-toolset.json file."
        >
          Load (replace)…
        </button>
        <button
          class="btn-secondary"
          onclick={async () => {
            await fileOps.loadToolset('add');
            draft = project.tools.map((t) => ({ ...t }));
          }}
          title="Add tools from a .wiac-toolset.json file. Tools whose name already exists are skipped."
        >
          Load (add)…
        </button>
        <span class="sep"></span>
        {#if hasInvalidRow}
          <!-- jkgj: surface why OK is greyed out so the user knows
               which inputs need fixing. -->
          <span class="validation-msg" role="status"
            >Fix highlighted fields (⌀, RPM, feed, plunge must be &gt; 0).</span
          >
        {/if}
        <button class="btn-secondary" onclick={close}>Cancel</button>
        <button
          class="btn-primary"
          onclick={commit}
          disabled={hasInvalidRow}
          title={hasInvalidRow ? 'Fix the highlighted fields before saving.' : ''}>OK</button
        >
      {/if}
    </footer>
  </Modal>
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
  @keyframes wiac-tool-flash {
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
    animation: wiac-tool-flash 1.2s ease-in-out;
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
  /* jkgj: footer-side validation hint shown when an OK-disabling
     row is present. Same red palette as `.discard-prompt`, but
     keeps the action buttons aligned to the right by NOT setting
     `margin-right: auto` — we want this slot inline with the
     buttons, not pushed to the start. */
  .validation-msg {
    color: var(--danger);
    font-size: 0.78rem;
    align-self: center;
  }
</style>
