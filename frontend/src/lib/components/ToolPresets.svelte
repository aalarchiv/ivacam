<script lang="ts">
  import { onMount } from 'svelte';
  import { project } from '../state/project.svelte';
  import { _ } from 'svelte-i18n';

  type ToolConfig = Record<string, unknown>;
  const STORAGE_KEY = 'wiac.tools';

  let presets = $state<Record<string, ToolConfig>>({});
  let selected = $state<string>('');
  let newName = $state<string>('');

  onMount(() => {
    try {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (raw) presets = JSON.parse(raw);
    } catch {
      presets = {};
    }
  });

  function persist() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(presets));
    } catch {}
  }

  function loadPreset(name: string) {
    const p = presets[name];
    if (!p) return;
    project.setSetup({ ...project.setup, tool: { ...p } });
    selected = name;
  }

  function saveCurrent() {
    const name = newName.trim();
    if (!name) return;
    presets = { ...presets, [name]: { ...((project.setup as Record<string, unknown>).tool as ToolConfig) } };
    persist();
    selected = name;
    newName = '';
  }

  function deletePreset() {
    if (!selected || !(selected in presets)) return;
    const next = { ...presets };
    delete next[selected];
    presets = next;
    persist();
    selected = '';
  }

  const presetNames = $derived(Object.keys(presets).sort());
</script>

<div class="tools">
  <label class="row">
    <span class="lbl">{$_('tools.label')}</span>
    <select
      bind:value={selected}
      onchange={(e) => loadPreset((e.target as HTMLSelectElement).value)}
      title={$_('tools.load_hint')}
    >
      <option value="">{$_('tools.pick_preset')}</option>
      {#each presetNames as name}
        <option value={name}>{name}</option>
      {/each}
    </select>
    <button onclick={deletePreset} disabled={!selected} title={$_('tools.delete_hint')}>×</button>
  </label>
  <label class="row">
    <span class="lbl">{$_('tools.save_as')}</span>
    <input
      type="text"
      placeholder={$_('tools.placeholder')}
      bind:value={newName}
      onkeydown={(e) => e.key === 'Enter' && saveCurrent()}
    />
    <button onclick={saveCurrent} disabled={!newName.trim()} title={$_('tools.save_hint')}>{$_('tools.save_button')}</button>
  </label>
</div>

<style>
  .tools {
    display: grid;
    gap: 0.3rem;
    padding: 0.4rem 0;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0.5rem;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 4.5rem) minmax(0, 1fr) auto;
    gap: 0.4rem;
    align-items: center;
  }
  .lbl {
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  select,
  input {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.35rem;
    font-size: 0.78rem;
    min-width: 0;
  }
  button {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.15rem 0.55rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
</style>
