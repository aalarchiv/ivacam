<script lang="ts">
  /// Tool library dialog. Project-scoped table of every tool the user
  /// has configured; ops will reference an entry by id once UX-7 lands.
  /// Each row is editable in place; the modal commits/cancels as a
  /// single unit so the user can revert without touching project.tools.
  import { project, type ToolEntry, type ToolKind, type CoolantMode } from '../state/project.svelte';

  interface Props {
    open: boolean;
    onClose: () => void;
  }
  let { open, onClose }: Props = $props();

  let draft = $state<ToolEntry[]>([]);

  $effect(() => {
    if (open) draft = project.tools.map((t) => ({ ...t }));
  });

  function commit() {
    if (draft.length > 0) project.tools = draft.map((t) => ({ ...t }));
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
            <span>coolant</span>
            <span></span>
          </div>
          {#each draft as tool, i (tool.id)}
            <div class="row">
              <span class="id">{tool.id}</span>
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
    grid-template-columns: 2.5rem minmax(0, 1.6fr) minmax(0, 1fr) 4.5rem 4.5rem 3.5rem 5rem 5rem 5rem minmax(0, 1fr) 2rem;
    gap: 0.3rem;
    align-items: center;
    font-size: 0.78rem;
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
