<script lang="ts">
  /// Machine Tooling sub-tab — stock the active machine from the shop
  /// inventory, like loading an automatic tool changer. Left: the
  /// inventory (capability-gated — a torch can't be stocked on a
  /// mill-only machine). Right: the machine's current loadout
  /// (= project.data.tools — exactly what op tool dropdowns offer and
  /// what the machine profile persists). Stock / remove are undoable
  /// project edits; the inventory itself is NEVER mutated here.
  import { project } from '../state/project.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { stockTool } from '../state/tool_inventory';
  import { t } from '../i18n';
  import {
    effectiveModes,
    KIND_DISPLAY_LABELS,
    MACHINE_MODE_NOUN,
    machineModesLabel,
    TOOL_COMPATIBLE_MODES,
    toolCompatibleWithAnyMode,
  } from '../state/tool_family';

  const inventory = $derived.by(() => {
    void workspace.version;
    return workspace.get().tool_inventory;
  });
  /// Quick name/kind filter over the inventory column.
  let filter = $state('');
  const shownInventory = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    if (q === '') return inventory;
    return inventory.filter((t) =>
      `${t.name} ${KIND_DISPLAY_LABELS[t.kind]}`.toLowerCase().includes(q),
    );
  });
  const stocked = $derived(project.data.tools);
  const machineModes = $derived(effectiveModes(project.data.machine));
  const machineLabel = $derived(machineModesLabel(machineModes));

  function alreadyStocked(id: number): boolean {
    const inv = inventory.find((t) => t.id === id);
    const st = stocked.find((t) => t.id === id);
    return inv != null && st != null && JSON.stringify(inv) === JSON.stringify(st);
  }

  function add(id: number) {
    const inv = inventory.find((t) => t.id === id);
    if (!inv) return;
    const copy = stockTool(inv, stocked);
    if (copy) project.addTool(copy);
  }

  /// Ops referencing the tool block removal — removing would leave
  /// dangling references the user then has to chase through warnings.
  function usedByOps(id: number): number {
    return project.data.operations.filter((o) => o.toolId === id).length;
  }

  function remove(id: number) {
    if (stocked.length <= 1 || usedByOps(id) > 0) return;
    project.removeTool(id);
  }
</script>

<div class="tooling">
  <section class="col">
    <h3 title={t('machinetool.shop_inventory.title')}>
      {t('machinetool.shop_inventory')}
    </h3>
    {#if inventory.length > 0}
      <input
        type="text"
        class="inv-filter"
        placeholder={t('machinetool.filter.placeholder')}
        bind:value={filter}
        title={t('machinetool.filter.title')}
      />
    {/if}
    {#if inventory.length === 0}
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- static, translator-authored markup -->
      <p class="hint">{@html t('machinetool.inventory_empty')}</p>
    {/if}
    <ul>
      {#each shownInventory as tool (tool.id)}
        {@const compatible = toolCompatibleWithAnyMode(tool.kind, machineModes)}
        {@const stockedAlready = alreadyStocked(tool.id)}
        <li class:incompatible={!compatible}>
          <span class="name">#{tool.id} {tool.name}</span>
          <span class="meta">{KIND_DISPLAY_LABELS[tool.kind]} · ⌀{tool.diameter}</span>
          <span class="chips">
            {#each TOOL_COMPATIBLE_MODES[tool.kind] as m (m)}
              <span class="cap-chip">{MACHINE_MODE_NOUN[m]}</span>
            {/each}
          </span>
          <button
            type="button"
            class="act"
            disabled={!compatible || stockedAlready}
            title={!compatible
              ? t('machinetool.incompatible_tool.title', {
                  kind: KIND_DISPLAY_LABELS[tool.kind].toLowerCase(),
                  machine: machineLabel,
                })
              : stockedAlready
                ? t('machinetool.already_stocked.title')
                : t('machinetool.add.title')}
            onclick={() => add(tool.id)}
            >{stockedAlready ? t('machinetool.stocked') : t('machinetool.add')}</button
          >
        </li>
      {/each}
    </ul>
  </section>
  <section class="col">
    <h3 title={t('machinetool.stocked_col.title')}>
      {t('machinetool.stocked_col', { machine: machineLabel })}
    </h3>
    <ul>
      {#each stocked as tool (tool.id)}
        {@const uses = usedByOps(tool.id)}
        <li class:incompatible={!toolCompatibleWithAnyMode(tool.kind, machineModes)}>
          <span class="name">#{tool.id} {tool.name}</span>
          <span class="meta">{KIND_DISPLAY_LABELS[tool.kind]} · ⌀{tool.diameter}</span>
          {#if !toolCompatibleWithAnyMode(tool.kind, machineModes)}
            <span class="warn" title={t('machinetool.incompatible.title')}
              >{t('machinetool.incompatible')}</span
            >
          {/if}
          <button
            type="button"
            class="act"
            disabled={stocked.length <= 1 || uses > 0}
            title={uses > 0
              ? uses === 1
                ? t('machinetool.remove_used.title.one', { n: uses })
                : t('machinetool.remove_used.title.other', { n: uses })
              : stocked.length <= 1
                ? t('machinetool.remove_last.title')
                : t('machinetool.remove.title')}
            onclick={() => remove(tool.id)}>{t('machinetool.remove')}</button
          >
        </li>
      {/each}
    </ul>
  </section>
</div>

<style>
  .tooling {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1rem;
    padding: 0.7rem;
    flex: 1;
    min-height: 0;
    overflow: auto;
    align-content: start;
  }
  .col h3 {
    font-size: 0.85rem;
    margin: 0 0 0.4rem;
    color: var(--text-strong);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  li {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-elevated);
    font-size: 0.8rem;
  }
  li.incompatible {
    opacity: 0.55;
  }
  .name {
    font-weight: 500;
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    color: var(--text-muted);
    font-size: 0.74rem;
    white-space: nowrap;
  }
  .chips {
    display: flex;
    gap: 0.25rem;
  }
  .cap-chip {
    padding: 0.05rem 0.4rem;
    border: 1px solid var(--border);
    border-radius: 9px;
    font-size: 0.68rem;
    color: var(--text);
  }
  .warn {
    color: var(--danger);
    font-size: 0.72rem;
  }
  .act {
    margin-left: 0.4rem;
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.15rem 0.5rem;
    font-size: 0.72rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .act:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .inv-filter {
    width: 100%;
    margin-bottom: 0.4rem;
    box-sizing: border-box;
  }
  .hint {
    font-size: 0.78rem;
    color: var(--text-muted);
  }
</style>
