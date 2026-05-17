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
  /// Snapshot captured at open — dirty check compares stringified draft
  /// to this so X / Esc / click-outside can prompt before silently
  /// discarding edits (audit-dh1n).
  let pristine = $state<string>('');

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
      expanded = new Set();
      pristine = JSON.stringify(newDraft);
    }
  });

  let isDirty = $derived.by(() => open && JSON.stringify(draft) !== pristine);

  function close() {
    if (isDirty && !window.confirm('Discard unsaved Tool Library changes?')) return;
    onClose();
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
        return !['v_bit', 'engraver', 'drag_knife', 'laser_beam', 'drill'].includes(kind);
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
    return '';
  }
</script>

{#if open}
  <Modal onClose={close} persistKey="tool-library" modalClass="tools-modal">
    <header>
      <h2 id="tools-title">Tool library</h2>
      <button class="close" onclick={close} aria-label="Close">×</button>
    </header>
    <div class="body" bind:this={bodyEl}>
      <div class="table">
        <div class="row head">
          <span>#</span>
          <span>Name</span>
          <span>Kind</span>
          <span>⌀ <span class="unit-hdr">mm</span></span>
          <span>tip ⌀ <span class="unit-hdr">mm</span></span>
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
                updateField(i, 'kind', (e.currentTarget as HTMLSelectElement).value as ToolKind)}
            >
              {#each kindOptions as k (k)}
                <option value={k}>{kindLabels[k]}</option>
              {/each}
            </select>
            <input
              type="number"
              step="0.1"
              value={tool.diameter}
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
              value={tool.tipDiameter ?? ''}
              placeholder={fieldApplies('tipDiameter', tool.kind) ? '—' : 'n/a'}
              disabled={!fieldApplies('tipDiameter', tool.kind)}
              title={fieldApplies('tipDiameter', tool.kind)
                ? ''
                : fieldReasonForKind('tipDiameter', tool.kind)}
              onchange={(e) => {
                const v = (e.currentTarget as HTMLInputElement).value;
                updateField(i, 'tipDiameter', v === '' ? undefined : parseFloat(v));
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
              value={tool.speed}
              disabled={!fieldApplies('speed', tool.kind)}
              title={fieldApplies('speed', tool.kind) ? '' : fieldReasonForKind('speed', tool.kind)}
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
              value={tool.feedRate}
              title={tool.kind === 'drill' ? 'For drill, this is the plunge feed.' : ''}
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
              value={tool.plungeRate}
              disabled={!fieldApplies('plunge', tool.kind)}
              title={fieldApplies('plunge', tool.kind)
                ? ''
                : fieldReasonForKind('plunge', tool.kind)}
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
              </div>
              <div class="holder-row pass-overrides">
                <span
                  class="holder-label"
                  title="Automatic chip-thinning (rt1.25 / Estlcam Wirbeln). When checked, Pocket ops using this tool clamp the cascade step down to tool_radius/2 (or the user-set Step) — keeps the cutter from overloading on hard materials. Set the Step value to override the default half-radius rule."
                  >Chip-thinning (Estlcam: Wirbeln)</span
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
                  <span>Step (mm)</span>
                  <input
                    type="number"
                    step="0.1"
                    min="0.05"
                    placeholder={(tool.diameter * 0.25).toFixed(2)}
                    value={tool.wirbelnStepoverMm ?? ''}
                    disabled={!tool.wirbeln}
                    title="Cascade-step cap when Wirbeln is on. Empty = use tool_radius / 2 (the classic chip-thinning rule)."
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLInputElement).value;
                      updateField(i, 'wirbelnStepoverMm', v === '' ? undefined : parseFloat(v));
                    }}
                  />
                </label>
              </div>
              {#if tool.kind === 'v_bit' || tool.kind === 'engraver'}
                <div class="holder-row pass-overrides">
                  <span
                    class="holder-label"
                    title="V-bit / engraver fields. The cone math drives V-Carve depth and Chamfer width."
                    >V-bit</span
                  >
                </div>
                <div class="holder-row">
                  <label>
                    <span>Tip angle (°)</span>
                    <input
                      type="number"
                      step="1"
                      min="1"
                      max="179"
                      placeholder="60"
                      value={tool.tipAngleDeg ?? ''}
                      title="Full included angle of the V-bit point (degrees). Drives V-Carve depth (z = -R / tan(angle / 2)) and Chamfer depth. Common values: 30°, 45°, 60°, 90°."
                      onchange={(e) => {
                        const v = (e.currentTarget as HTMLInputElement).value;
                        updateField(i, 'tipAngleDeg', v === '' ? undefined : parseFloat(v));
                      }}
                    />
                  </label>
                </div>
              {/if}
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
                </div>
              {/if}
            </div>
          {/if}
        {/each}
      </div>
      <button class="add" onclick={addTool}>+ Add tool</button>
    </div>
    <footer>
      <button class="secondary" onclick={close}>Cancel</button>
      <button class="primary" onclick={commit}>OK</button>
    </footer>
  </Modal>
{/if}

<style>
  :global(.tools-modal) {
    width: min(960px, 96vw);
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
    padding: 0.6rem 0.7rem;
    overflow: auto;
    min-width: 0;
  }
  .table {
    display: grid;
    gap: 0.2rem;
  }
  .row {
    display: grid;
    grid-template-columns:
      2.5rem minmax(0, 1.6fr) minmax(0, 1fr)
      4.5rem 4.5rem 3.5rem 5rem 5rem 5rem 4.5rem minmax(0, 1fr) 2rem;
    gap: 0.3rem;
    align-items: center;
    font-size: 0.78rem;
  }
  input.invalid {
    border-color: var(--danger, #c44);
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
  .primary {
    background: var(--accent);
    color: white;
    border: 0;
    padding: 0.3rem 0.8rem;
    border-radius: 3px;
    cursor: pointer;
  }
  .secondary {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 0.3rem 0.8rem;
    border-radius: 3px;
    cursor: pointer;
  }
</style>
