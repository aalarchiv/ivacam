<script lang="ts">
  // Recursive nested-object renderer. Object groups become <details> so the
  // user can collapse rarely-touched sections.

  import type { JsonSchema } from '../api/client';
  import { resolveRef, getAt, setAt } from '../schema/resolve';
  import SchemaField from './SchemaField.svelte';
  import Self from './SchemaForm.svelte';

  interface Props {
    schema: JsonSchema;
    definitions: Record<string, JsonSchema>;
    value: Record<string, unknown>;
    path?: string[];
    label?: string;
    rootValue?: Record<string, unknown>;
    onChange: (next: Record<string, unknown>) => void;
  }

  let {
    schema,
    definitions,
    value,
    path = [],
    label,
    rootValue,
    onChange,
  }: Props = $props();

  const resolved = $derived(resolveRef(schema, definitions));
  const isObject = $derived(
    resolved.type === 'object' || (resolved.properties && Object.keys(resolved.properties).length > 0),
  );
  const root = $derived(rootValue ?? value);

  function update(key: string, child: unknown) {
    const nextRoot = setAt(root, [...path, key], child);
    onChange(nextRoot);
  }
</script>

{#if isObject && resolved.properties}
  {#if path.length === 0}
    <div class="root">
      {#each Object.entries(resolved.properties) as [key, sub] (key)}
        {@const subResolved = resolveRef(sub, definitions)}
        {@const childValue = (value?.[key] ?? {}) as Record<string, unknown>}
        {#if subResolved.type === 'object' || subResolved.properties}
          <details open>
            <summary>{key}</summary>
            <div class="group">
              <Self
                schema={sub}
                {definitions}
                value={childValue}
                path={[...path, key]}
                label={key}
                rootValue={root}
                onChange={onChange}
              />
            </div>
          </details>
        {:else}
          <SchemaField
            schema={sub}
            {definitions}
            value={getAt(root, [...path, key])}
            label={key}
            onChange={(v) => update(key, v)}
          />
        {/if}
      {/each}
    </div>
  {:else}
    {#each Object.entries(resolved.properties) as [key, sub] (key)}
      {@const subResolved = resolveRef(sub, definitions)}
      {#if subResolved.type === 'object' || subResolved.properties}
        <details>
          <summary>{key}</summary>
          <div class="group">
            <Self
              schema={sub}
              {definitions}
              value={(value?.[key] ?? {}) as Record<string, unknown>}
              path={[...path, key]}
              label={key}
              rootValue={root}
              onChange={onChange}
            />
          </div>
        </details>
      {:else}
        <SchemaField
          schema={sub}
          {definitions}
          value={getAt(root, [...path, key])}
          label={key}
          onChange={(v) => update(key, v)}
        />
      {/if}
    {/each}
  {/if}
{:else}
  <SchemaField
    schema={resolved}
    {definitions}
    value={getAt(root, path)}
    label={label ?? path[path.length - 1] ?? ''}
    onChange={(v) => onChange(setAt(root, path, v))}
  />
{/if}

<style>
  .root {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    min-width: 0;
  }
  details {
    margin: 0.15rem 0 0.4rem 0;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-elevated);
    overflow: hidden; /* keeps deeply-nested fields from popping the panel width */
    min-width: 0;
  }
  summary {
    padding: 0.3rem 0.55rem;
    font-size: 0.75rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    cursor: pointer;
    list-style: none;
  }
  summary::before {
    content: '▸ ';
    color: var(--text-faint);
  }
  details[open] > summary::before {
    content: '▾ ';
  }
  .group {
    padding: 0.25rem 0.55rem 0.5rem;
    min-width: 0;
  }
</style>
