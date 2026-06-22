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
  import {
    project,
    type OpField,
    type OpFieldValue,
    type PocketOp,
    type ProfileOp,
  } from '../../state/project.svelte';
  import { t } from '../../i18n';

  interface Props {
    op: ProfileOp | PocketOp;
    /// Kind-aware patch (OpField + OpFieldValue) so calls like
    /// `patch('tabMode', { kind: 'auto', count: 4 })` type-check
    /// without each section redeclaring a per-variant signature.
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();

  /// Invalid-feedback flags for Width / Height — surface the user's
  /// typo with a red border instead of silently rejecting the change.
  /// Reset on a successful commit.
  let tabWidthInvalid = $state(false);
  let tabHeightInvalid = $state(false);

  const TAB_TYPE_HELP: Record<string, string> = {
    rectangle: t('ops.tabs.type.rectangle.help'),
    ramp: t('ops.tabs.type.ramp.help'),
  };

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
    const allowed = (id: number) => liveIds.has(id) && (!so || so.length === 0 || so.includes(id));
    return placements.filter((p) => !allowed(p.objectId)).length;
  }

  /// One-click strip of disconnected placements. Single
  /// updateOperation call so it lands as one undoable history entry.
  function clearDisconnectedTabs(o: ProfileOp | PocketOp) {
    const imp = project.transformedImport;
    if (!imp) return;
    const liveIds = new Set<number>(imp.objects ?? []);
    const so = o.sourceObjects;
    const allowed = (id: number) => liveIds.has(id) && (!so || so.length === 0 || so.includes(id));
    const next = (o.tabPlacements ?? []).filter((p) => allowed(p.objectId));
    project.updateOperation(o.id, { tabPlacements: next });
  }
</script>

<fieldset>
  <legend>{t('ops.tabs.legend')}</legend>
  <div class="row" title={t('ops.tabs.mode.help')}>
    <span>{t('ops.tabs.mode.label')}</span>
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
                    ? op.tabMode.autoCount
                    : 4;
              patch('tabMode', { kind: 'auto', count });
              patch('tabsActive', true);
            } else if (mk === 'manual') {
              patch('tabMode', { kind: 'manual' });
              patch('tabsActive', true);
            } else {
              const autoCount =
                op.tabMode?.kind === 'auto'
                  ? op.tabMode.count
                  : op.tabMode?.kind === 'mixed'
                    ? op.tabMode.autoCount
                    : 4;
              patch('tabMode', { kind: 'mixed', autoCount });
              patch('tabsActive', true);
            }
          }}>{mk}</button
        >
      {/each}
    </div>
  </div>
  {#if op.tabMode?.kind === 'auto' || op.tabMode?.kind === 'mixed'}
    <label class="row" title={t('ops.tabs.count.help')}>
      <span>{t('ops.tabs.count.label')}</span>
      <div class="num-cell">
        <input
          type="number"
          min="1"
          step="1"
          value={op.tabMode.kind === 'auto' ? op.tabMode.count : op.tabMode.autoCount}
          onchange={(e) => {
            const n = Math.max(1, parseInt((e.currentTarget as HTMLInputElement).value, 10) || 1);
            if (op.tabMode?.kind === 'auto') patch('tabMode', { kind: 'auto', count: n });
            else if (op.tabMode?.kind === 'mixed')
              patch('tabMode', { kind: 'mixed', autoCount: n });
          }}
        />
      </div>
    </label>
  {/if}
  {#if op.tabMode?.kind === 'manual' || op.tabMode?.kind === 'mixed'}
    <p class="hint" title={t('ops.tabs.manual.help')}>
      {t('ops.tabs.manual.hint')}
      {#if op.tabPlacements && op.tabPlacements.length > 0}
        {t('ops.tabs.manual.placed', { count: op.tabPlacements.length })}
      {/if}
    </p>
    {@const disconnected = disconnectedTabCount(op)}
    {#if disconnected > 0}
      <p class="hint warn" title={t('ops.tabs.disconnected.help')}>
        <strong>{disconnected}</strong>
        {t('ops.tabs.disconnected.hint', { count: disconnected })}
        <button type="button" class="reset-link" onclick={() => clearDisconnectedTabs(op)}
          >{t('ops.tabs.clear')}</button
        >
      </p>
    {/if}
  {/if}
  <label class="row" title={t('ops.tabs.width.help')}>
    <span>{t('ops.tabs.width.label')}</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.5"
        min="0.1"
        value={op.tabWidth ?? 10}
        class:invalid={tabWidthInvalid}
        onchange={(e) => {
          const v = parseFloat((e.currentTarget as HTMLInputElement).value);
          if (!isNaN(v) && v >= 0.1) {
            patch('tabWidth', v);
            tabWidthInvalid = false;
          } else {
            tabWidthInvalid = true;
          }
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label class="row" title={t('ops.tabs.height.help')}>
    <span>{t('ops.tabs.height.label')}</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.1"
        min="0.1"
        value={op.tabHeight ?? 1}
        class:invalid={tabHeightInvalid}
        onchange={(e) => {
          const v = parseFloat((e.currentTarget as HTMLInputElement).value);
          if (!isNaN(v) && v >= 0.1) {
            patch('tabHeight', v);
            tabHeightInvalid = false;
          } else {
            tabHeightInvalid = true;
          }
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label class="row" title={TAB_TYPE_HELP[op.tabType ?? 'rectangle']}>
    <span>{t('ops.tabs.type.label')}</span>
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
      <option value="rectangle" title={TAB_TYPE_HELP.rectangle}
        >{t('ops.tabs.type.rectangle')}</option
      >
      <option value="ramp" title={TAB_TYPE_HELP.ramp}>{t('ops.tabs.type.ramp')}</option>
    </select>
  </label>
  {#if op.tabType === 'ramp'}
    <details class="subsection" open>
      <summary>{t('ops.tabs.ramp.summary')}</summary>
      <label class="row" title={t('ops.tabs.ramp_angle.help')}>
        <span>{t('ops.tabs.ramp_angle.label')}</span>
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
