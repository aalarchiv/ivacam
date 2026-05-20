
<script lang="ts">
  /// Tabs op-properties fieldset. Shown only when op.kind is one of the
  /// closed-contour kinds (profile / pocket — the only ones that emit
  /// tabs).
  ///
  /// Owns the tab-mode picker (off / auto / manual / mixed), tab
  /// width/height/type inputs, ramp-angle subsection, and the
  /// disconnected-tab clear-up affordance.
  ///
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import { _ } from 'svelte-i18n';
  import {
    project,
    type OpField,
    type OpFieldValue,
    type PocketOp,
    type ProfileOp,
  } from '../../state/project.svelte';

  interface Props {
    op: ProfileOp | PocketOp;
    /// Kind-aware patch (OpField + OpFieldValue) so calls like
    /// `patch('tabMode', { kind: 'auto', count: 4 })` type-check
    /// without each section redeclaring a per-variant signature.
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();

  /// Count tab placements whose object id is no longer reachable from
  /// this op's source. "Reachable" means the import still carries an
  /// object with that id AND the op's source filter would include it.
  function disconnectedTabCount(o: ProfileOp | PocketOp): number {
    const placements = o.tabPlacements ?? [];
    if (placements.length === 0) return 0;
    const imp = project.transformedImport;
    if (!imp) return 0;
    const liveIds = new Set<number>(imp.objects ?? []);
    const so = o.sourceObjects;
    const allowed = (id: number) =>
      liveIds.has(id) && (!so || so.length === 0 || so.includes(id));
    return placements.filter((p) => !allowed(p.objectId)).length;
  }

  /// One-click strip of disconnected placements. Single
  /// updateOperation call so it lands as one undoable history entry.
  function clearDisconnectedTabs(o: ProfileOp | PocketOp) {
    const imp = project.transformedImport;
    if (!imp) return;
    const liveIds = new Set<number>(imp.objects ?? []);
    const so = o.sourceObjects;
    const allowed = (id: number) =>
      liveIds.has(id) && (!so || so.length === 0 || so.includes(id));
    const next = (o.tabPlacements ?? []).filter((p) => allowed(p.objectId));
    project.updateOperation(o.id, { tabPlacements: next });
  }
</script>

<fieldset>
  <legend>Tabs</legend>
  <div
    class="row"
    title="How tab positions are sourced for this op. Off ignores tabs entirely. Auto evenly spaces N tabs on each closed contour. Manual lets you click on the 2D canvas to place individual tabs. Mixed combines both."
  >
    <span>Mode</span>
    <div class="segmented">
      {#each ['off', 'auto', 'manual', 'mixed'] as mk (mk)}
        <button
          type="button"
          class:active={(op.tabMode?.kind ?? 'off') === mk}
          onclick={() => {
            if (mk === 'off') {
              patch('tabMode', { kind: 'off' });
              patch('tabsActive', false);
            } else if (mk === 'auto') {
              const count =
                op.tabMode?.kind === 'auto'
                  ? op.tabMode.count
                  : op.tabMode?.kind === 'mixed'
                    ? op.tabMode.auto_count
                    : 4;
              patch('tabMode', { kind: 'auto', count });
              patch('tabsActive', true);
            } else if (mk === 'manual') {
              patch('tabMode', { kind: 'manual' });
              patch('tabsActive', true);
            } else {
              const auto_count =
                op.tabMode?.kind === 'auto'
                  ? op.tabMode.count
                  : op.tabMode?.kind === 'mixed'
                    ? op.tabMode.auto_count
                    : 4;
              patch('tabMode', { kind: 'mixed', auto_count });
              patch('tabsActive', true);
            }
          }}>{mk}</button
        >
      {/each}
    </div>
  </div>
  {#if op.tabMode?.kind === 'auto' || op.tabMode?.kind === 'mixed'}
    <label
      class="row"
      title="Number of tabs to auto-place evenly around each closed contour."
    >
      <span>Count</span>
      <div class="num-cell">
        <input
          type="number"
          min="1"
          step="1"
          value={op.tabMode.kind === 'auto' ? op.tabMode.count : op.tabMode.auto_count}
          onchange={(e) => {
            const n = Math.max(
              1,
              parseInt((e.currentTarget as HTMLInputElement).value, 10) || 1,
            );
            if (op.tabMode?.kind === 'auto') patch('tabMode', { kind: 'auto', count: n });
            else if (op.tabMode?.kind === 'mixed')
              patch('tabMode', { kind: 'mixed', auto_count: n });
          }}
        />
      </div>
    </label>
  {/if}
  {#if op.tabMode?.kind === 'manual' || op.tabMode?.kind === 'mixed'}
    <p
      class="hint"
      title="Click on a closed contour in the 2D canvas to place a tab. Click on an existing tab to remove it."
    >
      Click the 2D canvas to add or remove tabs.
      {#if op.tabPlacements && op.tabPlacements.length > 0}
        ({op.tabPlacements.length} placed)
      {/if}
    </p>
    {@const disconnected = disconnectedTabCount(op)}
    {#if disconnected > 0}
      <p
        class="hint warn"
        title="These tabs reference objects that are no longer in this op's source set (either removed from the import or no longer selected). The pipeline silently drops them; clear them out to keep the data tidy."
      >
        <strong>{disconnected}</strong> tab{disconnected === 1 ? '' : 's'} disconnected
        <button type="button" class="reset-link" onclick={() => clearDisconnectedTabs(op)}
          >clear</button
        >
      </p>
    {/if}
  {/if}
  <label class="row" title="Width of each bridge along the cut path. Default 10 mm.">
    <span>Width</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.5"
        min="0.1"
        value={op.tabWidth ?? 10}
        onchange={(e) => {
          const v = parseFloat((e.currentTarget as HTMLInputElement).value);
          if (!isNaN(v) && v > 0) patch('tabWidth', v);
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label class="row" title="Z clearance the cutter lifts to over each tab. Default 1 mm.">
    <span>Height</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.1"
        min="0.1"
        value={op.tabHeight ?? 1}
        onchange={(e) => {
          const v = parseFloat((e.currentTarget as HTMLInputElement).value);
          if (!isNaN(v) && v > 0) patch('tabHeight', v);
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label class="row" title={$_('op.help.tab_type.' + (op.tabType ?? 'rectangle'))}>
    <span>Type</span>
    <select
      value={op.tabType ?? 'rectangle'}
      onchange={(e) => {
        const v = (e.currentTarget as HTMLSelectElement).value as 'rectangle' | 'ramp';
        patch('tabType', v);
        if (v === 'ramp' && op?.tabRampAngleDeg === undefined) {
          patch('tabRampAngleDeg', 30);
        }
      }}
    >
      <option value="rectangle" title={$_('op.help.tab_type.rectangle')}>rectangle</option>
      <option value="ramp" title={$_('op.help.tab_type.ramp')}>ramp</option>
    </select>
  </label>
  {#if op.tabType === 'ramp'}
    <details class="subsection" open>
      <summary>{$_('op.section.tab_ramp')}</summary>
      <label
        class="row"
        title="Ramp angle in degrees. 30° (default) gives a 1:√3 slope. Smaller = gentler, longer ramps; larger = steeper, more like a Rectangle tab. Horizontal ramp length = tabs.height / tan(angle)."
      >
        <span>Ramp angle</span>
        <div class="num-cell">
          <input
            type="number"
            step="1"
            min="1"
            max="89"
            value={op.tabRampAngleDeg ?? 30}
            onchange={(e) => {
              const v = parseFloat((e.currentTarget as HTMLInputElement).value);
              if (!isNaN(v)) patch('tabRampAngleDeg', Math.max(1, Math.min(89, v)));
            }}
          />
          <span class="unit">°</span>
        </div>
      </label>
    </details>
  {/if}
</fieldset>
