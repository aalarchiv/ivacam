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

  // schemars renders Option<T> as `type: [T, "null"]`. Detect the array
  // form and pull out the underlying scalar so the rest of the component
  // can render it as a normal editable input — blank value clears to null.
  function pickScalarType(t: unknown): string | undefined {
    if (typeof t === 'string') return t;
    if (Array.isArray(t)) {
      const v = t.find((x) => typeof x === 'string' && x !== 'null');
      return typeof v === 'string' ? v : undefined;
    }
    return undefined;
  }
  const effectiveType = $derived(pickScalarType(resolved.type));
  const isNullable = $derived(Array.isArray(resolved.type) && resolved.type.includes('null'));
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
    {:else if effectiveType === 'boolean'}
      <input
        type="checkbox"
        checked={asBool(value)}
        onchange={(e) => onChange((e.currentTarget as HTMLInputElement).checked)}
      />
    {:else if effectiveType === 'integer'}
      <input
        type="number"
        step="1"
        value={value == null ? '' : asNumber(value)}
        placeholder={isNullable ? 'unset' : ''}
        onchange={(e) => {
          const raw = (e.currentTarget as HTMLInputElement).value;
          if (raw === '' && isNullable) onChange(null);
          else onChange(parseInt(raw, 10) || 0);
        }}
      />
    {:else if effectiveType === 'number'}
      <input
        type="number"
        step="0.01"
        value={value == null ? '' : asNumber(value)}
        placeholder={isNullable ? 'unset' : ''}
        onchange={(e) => {
          const raw = (e.currentTarget as HTMLInputElement).value;
          if (raw === '' && isNullable) onChange(null);
          else onChange(parseFloat(raw) || 0);
        }}
      />
    {:else if effectiveType === 'string'}
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
    min-width: 0;
  }
  label {
    display: grid;
    grid-template-columns: minmax(0, 7.5rem) minmax(0, 1fr);
    align-items: center;
    gap: 0.5rem;
  }
  .name {
    font-size: 0.78rem;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  input,
  select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.35rem;
    font-size: 0.78rem;
    min-width: 0;
    width: 100%;
    box-sizing: border-box;
  }
  input[type='checkbox'] {
    width: auto;
    accent-color: var(--accent);
    justify-self: start;
  }
  .hint {
    font-size: 0.7rem;
    color: var(--text-muted);
    line-height: 1.3;
    word-break: break-word;
    margin-top: 0.05rem;
    /* Subtly indented so the hint visually pairs with its field. */
    padding-left: 0.2rem;
  }
  .unsupported {
    font-family: monospace;
    color: var(--text-faint);
    font-size: 0.7rem;
  }
</style>
