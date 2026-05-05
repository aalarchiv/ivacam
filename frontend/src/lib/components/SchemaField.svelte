<script lang="ts">
  // Single leaf field: number / string / enum / boolean. The container
  // SchemaForm handles object/group recursion.

  import type { JsonSchema } from '../api/client';
  import { resolveRef } from '../schema/resolve';

  interface Props {
    schema: JsonSchema;
    definitions: Record<string, JsonSchema>;
    value: unknown;
    label: string;
    onChange: (next: unknown) => void;
  }

  let { schema, definitions, value, label, onChange }: Props = $props();
  const resolved = $derived(resolveRef(schema, definitions));

  function asNumber(v: unknown): number {
    return typeof v === 'number' ? v : Number(v ?? 0);
  }
  function asString(v: unknown): string {
    return v == null ? '' : String(v);
  }
  function asBool(v: unknown): boolean {
    return Boolean(v);
  }
</script>

<div class="field">
  <label>
    <span class="name" title={resolved.description ?? ''}>{label}</span>
    {#if resolved.enum && resolved.enum.length > 0}
      <select
        value={asString(value)}
        onchange={(e) => onChange((e.currentTarget as HTMLSelectElement).value)}
      >
        {#each resolved.enum as opt (opt)}
          <option value={opt}>{opt}</option>
        {/each}
      </select>
    {:else if resolved.type === 'boolean'}
      <input
        type="checkbox"
        checked={asBool(value)}
        onchange={(e) => onChange((e.currentTarget as HTMLInputElement).checked)}
      />
    {:else if resolved.type === 'integer'}
      <input
        type="number"
        step="1"
        value={asNumber(value)}
        onchange={(e) => onChange(parseInt((e.currentTarget as HTMLInputElement).value, 10) || 0)}
      />
    {:else if resolved.type === 'number'}
      <input
        type="number"
        step="0.01"
        value={asNumber(value)}
        onchange={(e) => onChange(parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
      />
    {:else if resolved.type === 'string'}
      <input
        type="text"
        value={asString(value)}
        oninput={(e) => onChange((e.currentTarget as HTMLInputElement).value)}
      />
    {:else}
      <span class="unsupported">{JSON.stringify(value)}</span>
    {/if}
  </label>
  {#if resolved.description && resolved.type !== 'boolean'}
    <span class="hint">{resolved.description}</span>
  {/if}
</div>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    margin: 0.25rem 0;
  }
  label {
    display: grid;
    grid-template-columns: 11rem 1fr;
    align-items: center;
    gap: 0.5rem;
  }
  .name {
    font-size: 0.78rem;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  input,
  select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.35rem;
    font-size: 0.78rem;
    min-width: 6rem;
    width: 100%;
  }
  input[type='checkbox'] {
    width: auto;
    accent-color: var(--accent);
    justify-self: start;
  }
  .hint {
    grid-column: 2;
    font-size: 0.7rem;
    color: var(--text-muted);
    margin-left: 11.5rem;
  }
  .unsupported {
    font-family: monospace;
    color: var(--text-faint);
    font-size: 0.7rem;
  }
</style>
