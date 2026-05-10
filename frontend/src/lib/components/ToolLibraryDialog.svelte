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

  $effect(() => {
    if (open) {
      draft = project.tools.map((t) => ({ ...t }));
      expanded = new Set();
    }
  });

  function commit() {
    if (draft.length > 0) project.replaceTools(draft.map((t) => ({ ...t })));
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
    ball_nose: 'Ball nose',
    v_bit: 'V-bit',
    engraver: 'Engraver',
    drag_knife: 'Drag knife',
    drill: 'Drill',
    laser_beam: 'Laser',
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
</script>

{#if open}
  <div class="overlay" role="dialog" aria-modal="true" aria-labelledby="tools-title">
    <div class="modal">
      <header>
        <h2 id="tools-title">Tool library</h2>
        <button class="close" onclick={onClose} aria-label="Close">×</button>
      </header>
      <div class="body">
        <div class="table">
          <div class="row head">
            <span>#</span>
            <span>Name</span>
            <span>Kind</span>
            <span>⌀ mm</span>
            <span>tip</span>
            <span>flutes</span>
            <span>speed</span>
            <span>feed</span>
            <span>plunge</span>
            <span title="Default Z step (depth-per-pass) for ops using this tool. Negative number, mm. Empty = no default — every op must set its own.">dflt step</span>
            <span>coolant</span>
            <span></span>
          </div>
          {#each draft as tool, i (tool.id)}
            <div class="row">
              <span class="id">
                <button
                  class="expand"
                  type="button"
                  aria-expanded={expanded.has(tool.id)}
                  title={expanded.has(tool.id) ? 'Collapse holder section' : 'Expand holder section'}
                  onclick={() => toggleExpanded(tool.id)}
                >{expanded.has(tool.id) ? '▾' : '▸'} {tool.id}</button>
              </span>
              <input
                type="text"
                value={tool.name}
                oninput={(e) => updateField(i, 'name', (e.currentTarget as HTMLInputElement).value)}
              />
              <select
                value={tool.kind}
                onchange={(e) => updateField(i, 'kind', (e.currentTarget as HTMLSelectElement).value as ToolKind)}
              >
                {#each kindOptions as k (k)}
                  <option value={k}>{kindLabels[k]}</option>
                {/each}
              </select>
              <input
                type="number"
                step="0.1"
                value={tool.diameter}
                onchange={(e) => updateField(i, 'diameter', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
              />
              <input
                type="number"
                step="0.05"
                value={tool.tipDiameter ?? ''}
                placeholder="—"
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
                onchange={(e) => updateField(i, 'flutes', parseInt((e.currentTarget as HTMLInputElement).value, 10) || 1)}
              />
              <input
                type="number"
                step="500"
                value={tool.speed}
                onchange={(e) => updateField(i, 'speed', parseInt((e.currentTarget as HTMLInputElement).value, 10) || 0)}
              />
              <input
                type="number"
                step="50"
                value={tool.feedRate}
                onchange={(e) => updateField(i, 'feedRate', parseInt((e.currentTarget as HTMLInputElement).value, 10) || 0)}
              />
              <input
                type="number"
                step="50"
                value={tool.plungeRate}
                onchange={(e) => updateField(i, 'plungeRate', parseInt((e.currentTarget as HTMLInputElement).value, 10) || 0)}
              />
              <input
                type="number"
                step="0.05"
                max="0"
                value={tool.defaultStep ?? ''}
                placeholder="—"
                title="Operations using this tool inherit this when they don't specify their own. Negative number, mm."
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
                onchange={(e) => updateField(i, 'coolant', (e.currentTarget as HTMLSelectElement).value as CoolantMode)}
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
              >×</button>
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
                        onchange={(e) => updateHolderField(i, 'diameter_mm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
                      />
                    </label>
                    <label>
                      <span>Length (mm)</span>
                      <input
                        type="number"
                        step="0.5"
                        min="0"
                        value={tool.holder.length_mm}
                        onchange={(e) => updateHolderField(i, 'length_mm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
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
                        onchange={(e) => updateHolderField(i, 'bottom_diameter_mm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
                      />
                    </label>
                    <label>
                      <span>Top ⌀ (mm)</span>
                      <input
                        type="number"
                        step="0.5"
                        min="0"
                        value={tool.holder.top_diameter_mm}
                        onchange={(e) => updateHolderField(i, 'top_diameter_mm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
                      />
                    </label>
                    <label>
                      <span>Length (mm)</span>
                      <input
                        type="number"
                        step="0.5"
                        min="0"
                        value={tool.holder.length_mm}
                        onchange={(e) => updateHolderField(i, 'length_mm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
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
                        onchange={(e) => updateHolderField(i, 'cylinder_diameter_mm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
                      />
                    </label>
                    <label>
                      <span>Cyl length (mm)</span>
                      <input
                        type="number"
                        step="0.5"
                        min="0"
                        value={tool.holder.cylinder_length_mm}
                        onchange={(e) => updateHolderField(i, 'cylinder_length_mm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
                      />
                    </label>
                    <label>
                      <span>Cone top ⌀ (mm)</span>
                      <input
                        type="number"
                        step="0.5"
                        min="0"
                        value={tool.holder.cone_top_diameter_mm}
                        onchange={(e) => updateHolderField(i, 'cone_top_diameter_mm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
                      />
                    </label>
                    <label>
                      <span>Cone length (mm)</span>
                      <input
                        type="number"
                        step="0.5"
                        min="0"
                        value={tool.holder.cone_length_mm}
                        onchange={(e) => updateHolderField(i, 'cone_length_mm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
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
        <button class="secondary" onclick={onClose}>Cancel</button>
        <button class="primary" onclick={commit}>OK</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, black 50%, transparent);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 50;
  }
  .modal {
    width: min(960px, 96vw);
    max-height: 88vh;
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
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
    grid-template-columns: 2.5rem minmax(0, 1.6fr) minmax(0, 1fr) 4.5rem 4.5rem 3.5rem 5rem 5rem 5rem 4.5rem minmax(0, 1fr) 2rem;
    gap: 0.3rem;
    align-items: center;
    font-size: 0.78rem;
  }
  input.invalid {
    border-color: var(--danger, #c44);
  }
  .row.head {
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 0.68rem;
    padding-bottom: 0.2rem;
    border-bottom: 1px solid var(--border);
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
