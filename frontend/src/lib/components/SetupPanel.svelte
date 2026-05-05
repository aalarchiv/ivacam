<script lang="ts">
  import { onMount } from 'svelte';
  import { defaultClient } from '../api/http';
  import { project } from '../state/project.svelte';
  import SchemaForm from './SchemaForm.svelte';

  const client = defaultClient();
  let loading = $state(false);
  let loadError = $state<string | null>(null);

  onMount(async () => {
    if (project.setupSchema) return;
    loading = true;
    try {
      const d = await client.defaults();
      project.setDefaults(d);
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  });

  function reset() {
    if (loading) return;
    loading = true;
    client
      .defaults()
      .then((d) => project.setDefaults(d))
      .finally(() => (loading = false));
  }
</script>

<aside class="setup">
  <header>
    <h3>Setup</h3>
    <button class="reset" onclick={reset} disabled={loading} title="Reset to defaults"
      >reset</button
    >
  </header>
  {#if loading && !project.setupSchema}
    <p class="hint">Loading setup tree…</p>
  {:else if loadError}
    <p class="error">{loadError}</p>
  {:else if project.setupSchema}
    <SchemaForm
      schema={project.setupSchema}
      definitions={project.setupDefinitions}
      value={project.setup as Record<string, unknown>}
      onChange={(next) => project.setSetup(next)}
    />
  {:else}
    <p class="hint">No setup schema available.</p>
  {/if}
</aside>

<style>
  .setup {
    width: 100%;
    height: 100%;
    background: var(--bg-panel);
    color: var(--text);
    border-left: 1px solid var(--border);
    overflow-y: auto;
    overflow-x: hidden;
    padding: 0.6rem 0.7rem 1rem;
    box-sizing: border-box;
    min-width: 0;
  }
  header {
    display: flex;
    align-items: baseline;
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
  .reset {
    background: transparent;
    color: var(--text-muted);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.1rem 0.4rem;
    font-size: 0.7rem;
    cursor: pointer;
  }
  .reset:hover {
    color: var(--text);
  }
  .reset:disabled {
    opacity: 0.5;
  }
  .hint {
    color: var(--text-muted);
    font-size: 0.78rem;
  }
  .error {
    color: var(--error);
    font-size: 0.78rem;
  }
</style>
