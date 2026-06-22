<script lang="ts">
  /// Operation properties panel — bound to project.sel.selectedOpId. Shows
  /// the kind-specific parameters of the selected op plus a tool picker
  /// fed from project.data.tools. Edits are pushed straight back through
  /// project.updateOperation, so the operation list updates instantly.

  import {
    project,
    isContourOp,
    type OpEntry,
    type OpField,
    type OpFieldValue,
    type SourceCombine,
    type CutDirection,
  } from '../state/project.svelte';
  import { defaultClient } from '../api/http';
  import type { HelixRadiusResponse } from '../api/types';
  import { prettyOpKind } from '../state/project-types';
  import { formatLength } from '../cam/units';
  import { formatExpectedToolKinds, isToolKindAcceptable } from '../state/op_tool_constraint';
  import { effectiveModes, machineModesLabel } from '../state/tool_family';
  import { effectiveDiameterHint } from '../state/tool_wear';
  import { partitionToolsForModes } from '../state/tool_picker';
  import { t } from '../i18n';
  import VCarveSection from './op_properties/VCarveSection.svelte';
  import ChamferSection from './op_properties/ChamferSection.svelte';
  import ThreadSection from './op_properties/ThreadSection.svelte';
  import PatternSection from './op_properties/PatternSection.svelte';
  import DrillSection from './op_properties/DrillSection.svelte';
  import TabsSection from './op_properties/TabsSection.svelte';
  import ProfileSection from './op_properties/ProfileSection.svelte';
  import PocketSection from './op_properties/PocketSection.svelte';
  import ReliefMillSection from './op_properties/ReliefMillSection.svelte';
  import RasterEngraveSection from './op_properties/RasterEngraveSection.svelte';
  import PauseSection from './op_properties/PauseSection.svelte';
  import HomingSection from './op_properties/HomingSection.svelte';
  import ProbeSection from './op_properties/ProbeSection.svelte';
  import GcodeIncludeSection from './op_properties/GcodeIncludeSection.svelte';
  import CycleMarkerSection from './op_properties/CycleMarkerSection.svelte';
  // Shared op-property styling lives in a plain CSS module so we
  // don't need 53 :global(.props X) rules in the scoped style block
  // below. Vite static-imports this once; the .props prefix keeps the
  // selectors namespaced.
  import './op_properties/op-properties.css';

  const apiClient = defaultClient();
  const HELIX_PREVIEW_DEBOUNCE_MS = 300;

  /// Help text for the per-op dropdowns. Each map keys an enum value to a
  /// message key; resolved through t() at the use site so it translates
  /// live. Keep these grouped near the top so future translators have one
  /// obvious target.
  const COMBINE_HELP = {
    auto: 'opprops.combine.auto.help',
    union: 'opprops.combine.union.help',
    difference: 'opprops.combine.difference.help',
    intersection: 'opprops.combine.intersection.help',
    xor: 'opprops.combine.xor.help',
    none: 'opprops.combine.none.help',
  } as const;
  const CUT_DIRECTION_HELP = {
    conventional: 'opprops.direction.conventional.help',
    climb: 'opprops.direction.climb.help',
  } as const;
  const PLUNGE_HELP = {
    direct: 'opprops.plunge.direct.help',
    ramp: 'opprops.plunge.ramp.help',
    helix: 'opprops.plunge.helix.help',
  } as const;

  interface Props {
    /// True when rendered inline under an OperationsList row (drops the
    /// outer aside chrome + the standalone "Properties" header).
    embedded?: boolean;
  }
  let { embedded = false }: Props = $props();

  const op = $derived<OpEntry | null>(
    project.sel.selectedOpId == null
      ? null
      : (project.data.operations.find((o) => o.id === project.sel.selectedOpId) ?? null),
  );

  /// Library split for the tool pickers: tools the machine's effective
  /// mode set can run first; the rest stay selectable under a labelled
  /// "incompatible" group so a machine-mode switch never strands an op
  /// on an invisible tool.
  const machineModes = $derived(effectiveModes(project.data.machine));
  const toolParts = $derived(partitionToolsForModes(project.data.tools, machineModes));

  /// Resolve the assigned tool's defaultStep for the current op so the
  /// Step / pass input can fall back to it. null when no assignment.
  const toolDefaultStep = $derived<number | null>(
    op == null ? null : (project.data.tools.find((t) => t.id === op.toolId)?.defaultStep ?? null),
  );
  /// Tool defaults that the per-op feed / plunge fields inherit when
  /// unset. Placeholders below show these as concrete numbers.
  const toolFeedRate = $derived<number | null>(
    op == null ? null : (project.data.tools.find((t) => t.id === op.toolId)?.feedRate ?? null),
  );
  const toolPlungeRate = $derived<number | null>(
    op == null ? null : (project.data.tools.find((t) => t.id === op.toolId)?.plungeRate ?? null),
  );
  const stepInheriting = $derived(op != null && (op.step === null || op.step === undefined));
  const stepMissing = $derived(
    stepInheriting && (toolDefaultStep === null || toolDefaultStep >= 0),
  );

  /// Kind-aware patch helper. `OpField` is the union of every field
  /// name across every OpEntry variant (so `'xyOverlap'` /
  /// `'chamferWidthMm'` etc. type-check), and `OpFieldValue<K>`
  /// picks the right value type for whichever variant carries that
  /// field. Runtime safety (rejecting wrong-kind writes) lives in
  /// `project.updateOperation`.
  function patch<K extends OpField>(key: K, value: OpFieldValue<K>) {
    if (op) project.updateOperation(op.id, { [key]: value } as Partial<OpEntry>);
  }

  // Remembers the last manual radius the user typed so toggling Auto
  // off restores it instead of jumping back to the default.
  let lastManualHelixRadius = $state<number>(3);
  $effect(() => {
    if (op?.plunge?.kind === 'helix' && op.plunge.radiusMm != null) {
      lastManualHelixRadius = op.plunge.radiusMm;
    }
  });

  // Auto-fit helix preview: when the checkbox is on, the panel shows
  // "Auto (detected: X mm)" — the same inscribed-circle search the
  // generator runs at gcode time, surfaced ahead of Generate so the user
  // can sanity-check before kicking off a full run.
  // Debounced 300ms so rapid selection / tool edits don't spam the
  // transport; the computation is fast (medial-axis on a small
  // polygon), so any value in the 100-500ms range works — 300 keeps the
  // UI feeling instant without thrashing.
  let helixPreview = $state<HelixRadiusResponse | null>(null);
  let helixPreviewLoading = $state(false);

  const helixToolDiameter = $derived<number | null>(
    op == null ? null : (project.data.tools.find((t) => t.id === op.toolId)?.diameter ?? null),
  );
  const helixAutoActive = $derived(
    op != null && op.plunge != null && op.plunge.kind === 'helix' && op.plunge.radiusMm === null,
  );
  const helixHasGeometry = $derived(
    project.transformedImport != null && (project.transformedImport.segments?.length ?? 0) > 0,
  );
  const helixHasSelection = $derived(op != null && (op.sourceObjects?.length ?? 0) > 0);

  $effect(() => {
    if (!helixAutoActive || !helixHasGeometry || !helixHasSelection || helixToolDiameter == null) {
      helixPreview = null;
      helixPreviewLoading = false;
      return;
    }
    // Capture everything we need at effect entry — once the effect
    // body returns, the async callbacks below run outside any
    // reactive scope and must not re-read `op` (it could be null by
    // then). The id captured here is what we'll compare against in
    // the .then to detect "user selected a different op while the
    // request was in flight".
    const opIdAtStart = op?.id;
    const segments = project.transformedImport?.segments ?? [];
    const objectIds = op?.sourceObjects ?? [];
    const toolD = helixToolDiameter;
    helixPreviewLoading = true;
    const timer = window.setTimeout(() => {
      apiClient
        .computeHelixRadius({
          segments,
          object_ids: objectIds,
          tool_diameter_mm: toolD,
        })
        .then((resp) => {
          if (project.sel.selectedOpId !== opIdAtStart) return;
          helixPreview = resp;
          helixPreviewLoading = false;
        })
        .catch(() => {
          if (project.sel.selectedOpId !== opIdAtStart) return;
          helixPreview = null;
          helixPreviewLoading = false;
        });
    }, HELIX_PREVIEW_DEBOUNCE_MS);
    return () => {
      window.clearTimeout(timer);
    };
  });
</script>

<aside class="props" class:embedded>
  {#snippet toolOptions(disabledId: number | null)}
    <!-- Mode-compatible tools first; incompatible ones stay selectable
         under a labelled group (visible-and-explained, never hidden). -->
    {#each toolParts.compatible as t (t.id)}
      <option value={t.id} disabled={disabledId === t.id} title={t.comment ?? ''}
        >#{t.id} {t.name} ({formatLength(t.diameter, project.data.machine.unit)})</option
      >
    {/each}
    {#if toolParts.incompatible.length > 0}
      <optgroup
        label={t('opprops.tool.incompatible_group', { modes: machineModesLabel(machineModes) })}
      >
        {#each toolParts.incompatible as t (t.id)}
          <option value={t.id} disabled={disabledId === t.id} title={t.comment ?? ''}
            >#{t.id} {t.name} ({formatLength(t.diameter, project.data.machine.unit)})</option
          >
        {/each}
      </optgroup>
    {/if}
  {/snippet}
  {#if !embedded}
    <h3>{t('opprops.title')}</h3>
  {/if}

  {#if !op}
    <p class="empty" class:embedded-empty={embedded}>{t('opprops.empty')}</p>
  {:else if op.kind === 'pause'}
    <!-- Program-only kinds delegate to a dedicated *Section, same as
         the geometry kinds — one rule for where a kind's panel lives. -->
    <PauseSection {op} {patch} />
  {:else if op.kind === 'homing'}
    <HomingSection {op} {patch} />
  {:else if op.kind === 'probe'}
    <ProbeSection {op} {patch} />
  {:else if op.kind === 'gcode_include'}
    <GcodeIncludeSection {op} {patch} />
  {:else if op.kind === 'cycle_marker'}
    <CycleMarkerSection {op} {patch} />
  {:else if op.kind === 'relief_mill'}
    <!-- Relief surfacing follows an image-derived Z-surface, not
         source geometry — name + tool + the relief section only. -->
    <label class="row">
      <span>{t('opprops.name')}</span>
      <input
        type="text"
        value={op.name}
        oninput={(e) => patch('name', (e.currentTarget as HTMLInputElement).value)}
      />
    </label>
    {@const reliefTool = project.data.tools.find((t) => t.id === op.toolId)}
    {#if reliefTool != null && !isToolKindAcceptable(op.kind, reliefTool.kind)}
      <p
        class="warn-chip"
        title={t('opprops.tool.mismatch.title', { kinds: formatExpectedToolKinds(op.kind) })}
      >
        {t('opprops.tool.mismatch', {
          op: prettyOpKind(op.kind),
          kinds: formatExpectedToolKinds(op.kind),
        })}
      </p>
    {/if}
    <label class="row" title={t('opprops.tool.title.relief')}>
      <span>{t('opprops.tool')}</span>
      <div class="tool-cell">
        <select
          value={op.toolId}
          onchange={(e) =>
            patch('toolId', parseInt((e.currentTarget as HTMLSelectElement).value, 10))}
        >
          {@render toolOptions(null)}
        </select>
        <button
          type="button"
          class="tool-edit"
          title={t('opprops.tool.edit')}
          aria-label={t('opprops.tool.edit')}
          onclick={(e) => {
            e.stopPropagation();
            project.sel.toolsDialogFocusId = op.toolId;
          }}>⚙</button
        >
      </div>
    </label>
    <ReliefMillSection {op} {patch} />
  {:else if op.kind === 'raster_engrave'}
    <!-- Laser raster engraving follows an image-derived power
         field, not source geometry — name + tool + the raster section. -->
    <label class="row">
      <span>{t('opprops.name')}</span>
      <input
        type="text"
        value={op.name}
        oninput={(e) => patch('name', (e.currentTarget as HTMLInputElement).value)}
      />
    </label>
    {@const rasterTool = project.data.tools.find((t) => t.id === op.toolId)}
    {#if rasterTool != null && !isToolKindAcceptable(op.kind, rasterTool.kind)}
      <p
        class="warn-chip"
        title={t('opprops.tool.mismatch.title', { kinds: formatExpectedToolKinds(op.kind) })}
      >
        {t('opprops.tool.mismatch', {
          op: prettyOpKind(op.kind),
          kinds: formatExpectedToolKinds(op.kind),
        })}
      </p>
    {/if}
    <label class="row" title={t('opprops.tool.title.raster')}>
      <span>{t('opprops.tool')}</span>
      <div class="tool-cell">
        <select
          value={op.toolId}
          onchange={(e) =>
            patch('toolId', parseInt((e.currentTarget as HTMLSelectElement).value, 10))}
        >
          {@render toolOptions(null)}
        </select>
        <button
          type="button"
          class="tool-edit"
          title={t('opprops.tool.edit')}
          aria-label={t('opprops.tool.edit')}
          onclick={(e) => {
            e.stopPropagation();
            project.sel.toolsDialogFocusId = op.toolId;
          }}>⚙</button
        >
      </div>
    </label>
    <RasterEngraveSection {op} {patch} />
  {:else}
    <label class="row">
      <span>{t('opprops.name')}</span>
      <input
        type="text"
        value={op.name}
        oninput={(e) => patch('name', (e.currentTarget as HTMLInputElement).value)}
      />
    </label>

    {@const selectedTool = project.data.tools.find((t) => t.id === op.toolId)}
    {@const toolKindOk = selectedTool == null || isToolKindAcceptable(op.kind, selectedTool.kind)}
    {#if selectedTool != null && !toolKindOk}
      <p
        class="warn-chip"
        title={t('opprops.tool.mismatch.title', { kinds: formatExpectedToolKinds(op.kind) })}
      >
        {t('opprops.tool.mismatch', {
          op: prettyOpKind(op.kind),
          kinds: formatExpectedToolKinds(op.kind),
        })}
      </p>
    {/if}
    <label
      class="row"
      title={selectedTool?.comment ? selectedTool.comment : t('opprops.tool.title.default')}
    >
      <span>{t('opprops.tool')}</span>
      <div class="tool-cell">
        <select
          value={op.toolId}
          onchange={(e) =>
            patch('toolId', parseInt((e.currentTarget as HTMLSelectElement).value, 10))}
        >
          {@render toolOptions(null)}
        </select>
        <button
          type="button"
          class="tool-edit"
          title={t('opprops.tool.edit')}
          aria-label={t('opprops.tool.edit')}
          onclick={(e) => {
            e.stopPropagation();
            project.sel.toolsDialogFocusId = op.toolId;
          }}>⚙</button
        >
      </div>
    </label>
    {#if selectedTool != null && (selectedTool.wearOffsetMm ?? 0) !== 0}
      <p class="wear-hint" title={t('opprops.wear.title')}>
        {t('opprops.wear.effective_diameter', { value: effectiveDiameterHint(selectedTool) })}
      </p>
    {/if}

    {#if op.kind === 'pocket' || op.kind === 'drill'}
      <label
        class="row"
        title={op.kind === 'pocket'
          ? t('opprops.finish_tool.title.pocket')
          : t('opprops.finish_tool.title.drill')}
      >
        <span>{t('opprops.finish_tool')}</span>
        <div class="tool-cell">
          <select
            value={op.finishToolId ?? ''}
            onchange={(e) => {
              const raw = (e.currentTarget as HTMLSelectElement).value;
              patch('finishToolId', raw === '' ? undefined : parseInt(raw, 10));
            }}
          >
            <option value="">{t('opprops.finish_tool.same_as_rough')}</option>
            {@render toolOptions(op.toolId)}
          </select>
        </div>
      </label>
    {/if}

    <fieldset>
      <legend>{t('opprops.source')}</legend>
      <label class="row">
        <span>{t('opprops.source.mode')}</span>
        <select
          value={op.sourceObjects && op.sourceObjects.length > 0
            ? '_objects_'
            : op.sourceLayers === null
              ? '_all_'
              : '_layer_'}
          onchange={(e) => {
            const v = (e.currentTarget as HTMLSelectElement).value;
            if (v === '_all_') {
              patch('sourceLayers', null);
              patch('sourceObjects', undefined);
            } else if (v === '_layer_') {
              patch('sourceObjects', undefined);
              if (op && op.sourceLayers === null) patch('sourceLayers', []);
            } else {
              patch('sourceLayers', null);
              if (op && (op.sourceObjects?.length ?? 0) === 0) patch('sourceObjects', []);
            }
          }}
        >
          <option value="_all_">{t('opprops.source.mode.all')}</option>
          <option value="_layer_">{t('opprops.source.mode.layer')}</option>
          <option value="_objects_">{t('opprops.source.mode.objects')}</option>
        </select>
      </label>
      {#if op.sourceLayers !== null && (op.sourceObjects?.length ?? 0) === 0}
        <label class="row">
          <span>{t('opprops.source.layer')}</span>
          <select
            value={op.sourceLayers[0] ?? ''}
            onchange={(e) => patch('sourceLayers', [(e.currentTarget as HTMLSelectElement).value])}
          >
            <option value="">{t('opprops.source.layer.pick')}</option>
            {#if project.transformedImport}
              {#each project.transformedImport.layers.filter((l) => l.segment_count > 0) as layer (layer.name)}
                <option value={layer.name}>"{layer.name}"</option>
              {/each}
            {/if}
            <!-- Text layers are selectable sources too. Each
                 TextLayer's rendered geometry lives on the synthetic
                 layer `__text_<id>` (the same value AddTextDialog seeds
                 the engrave op with), so listing it here makes the text
                 source visible + re-selectable instead of rendering blank
                 and getting clobbered on the next edit. -->
            {#if project.data.textLayers.length > 0}
              <optgroup label={t('opprops.source.text_group')}>
                {#each project.data.textLayers as t (t.id)}
                  <option value={`__text_${t.id}`}>{t.name}</option>
                {/each}
              </optgroup>
            {/if}
          </select>
        </label>
      {:else if op.sourceObjects && op.sourceObjects.length > 0}
        <p class="hint">
          {t('opprops.source.objects_selected', { count: op.sourceObjects.length })}
        </p>
      {:else if op.sourceLayers === null}
        <p class="hint">{t('opprops.source.all_chains')}</p>
      {/if}
      <!-- Drill picks a single XY per selected object (POINT / circle
           center / bbox center) and emits a drill cycle there. The
           area-based boolean modes (union / difference / intersection
           / xor) have no effect — each object gets its own hole no
           matter what. Hide the Combine selector for Drill to stop
           promising a knob that does nothing. -->
      {#if op.kind !== 'drill' && ((op.sourceObjects?.length ?? 0) > 1 || (op.sourceLayers !== null && op.sourceLayers.length > 0))}
        <label class="row" title={t(COMBINE_HELP[op.sourceCombine ?? 'auto'])}>
          <span>{t('opprops.combine')}</span>
          <select
            value={op.sourceCombine ?? 'auto'}
            onchange={(e) =>
              patch('sourceCombine', (e.currentTarget as HTMLSelectElement).value as SourceCombine)}
          >
            <option value="auto" title={t(COMBINE_HELP.auto)}>{t('opprops.combine.auto')}</option>
            <option value="union" title={t(COMBINE_HELP.union)}>{t('opprops.combine.union')}</option
            >
            <option value="difference" title={t(COMBINE_HELP.difference)}
              >{t('opprops.combine.difference')}</option
            >
            <option value="intersection" title={t(COMBINE_HELP.intersection)}
              >{t('opprops.combine.intersection')}</option
            >
            <option value="xor" title={t(COMBINE_HELP.xor)}>{t('opprops.combine.xor')}</option>
            <option value="none" title={t(COMBINE_HELP.none)}>{t('opprops.combine.none')}</option>
          </select>
        </label>
      {/if}
      <button
        class="from-selection"
        class:ghost={project.sel.selectedObjects.size === 0}
        type="button"
        disabled={project.sel.selectedObjects.size === 0}
        aria-label={project.sel.selectedObjects.size === 0
          ? t('opprops.from_selection.disabled_aria')
          : t('opprops.from_selection.count', { count: project.sel.selectedObjects.size })}
        title={project.sel.selectedObjects.size === 0
          ? t('opprops.from_selection.disabled_aria')
          : t('opprops.from_selection.title')}
        onclick={() => {
          patch('sourceLayers', null);
          patch('sourceObjects', [...project.sel.selectedObjects]);
        }}
        >{project.sel.selectedObjects.size === 0
          ? t('opprops.from_selection')
          : t('opprops.from_selection.count', { count: project.sel.selectedObjects.size })}</button
      >
    </fieldset>

    <fieldset>
      <legend>{t('opprops.cut')}</legend>
      <label class="row">
        <span>{t('opprops.cut.final_depth')}</span>
        <div class="num-cell">
          <input
            type="number"
            step="0.1"
            value={op.depth}
            onchange={(e) => {
              const raw = (e.currentTarget as HTMLInputElement).value;
              const v = parseFloat(raw);
              // Reject NaN — the prior `|| 0` silently snapped a typo
              // to depth=0 with no UI cue.
              if (Number.isFinite(v)) patch('depth', v);
            }}
          />
          <span class="unit">mm</span>
        </div>
      </label>
      <label class="row">
        <span>{t('opprops.cut.start_depth')}</span>
        <div class="num-cell">
          <input
            type="number"
            step="0.1"
            value={op.startDepth}
            onchange={(e) => {
              const raw = (e.currentTarget as HTMLInputElement).value;
              const v = parseFloat(raw);
              if (Number.isFinite(v)) patch('startDepth', v);
            }}
          />
          <span class="unit">mm</span>
        </div>
      </label>
      <label class="row">
        <span>{t('opprops.cut.step')}</span>
        <div class="step-cell">
          <input
            type="number"
            step="0.1"
            value={op.step ?? ''}
            placeholder={stepInheriting && toolDefaultStep !== null && toolDefaultStep < 0
              ? t('opprops.cut.step.from_tool', { value: toolDefaultStep })
              : '—'}
            class:inherit={stepInheriting && toolDefaultStep !== null && toolDefaultStep < 0}
            class:invalid={stepMissing}
            onchange={(e) => {
              const v = (e.currentTarget as HTMLInputElement).value;
              if (v === '') {
                patch('step', null);
                return;
              }
              const n = parseFloat(v);
              patch('step', isNaN(n) ? null : n);
            }}
          />
          <span class="unit">mm</span>
          {#if !stepInheriting}
            <button
              type="button"
              class="reset-link"
              title={t('opprops.cut.step.reset.title')}
              onclick={() => patch('step', null)}>{t('opprops.cut.step.reset')}</button
            >
          {/if}
        </div>
      </label>
      {#if stepMissing}
        <p class="step-error">{t('opprops.cut.step.required')}</p>
      {/if}
      {#if isContourOp(op)}
        <label class="row" title={t('opprops.cut.finish_step.title')}>
          <span>{t('opprops.cut.finish_step')}</span>
          <div class="num-cell">
            <input
              type="number"
              step="0.05"
              placeholder={t('opprops.cut.finish_step.placeholder')}
              value={op.finishStep ?? ''}
              onchange={(e) => {
                const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                patch('finishStep', isNaN(v) ? undefined : v);
              }}
            />
            <span class="unit">mm</span>
          </div>
        </label>
      {/if}
      {#if op.kind === 'pocket'}
        <label class="row" title={t('opprops.cut.xy_finish_stock.title')}>
          <span>{t('opprops.cut.xy_finish_stock')}</span>
          <div class="num-cell">
            <input
              type="number"
              step="0.05"
              min="0"
              placeholder="0"
              value={op.finishXyAllowanceMm ?? ''}
              onchange={(e) => {
                const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                patch('finishXyAllowanceMm', isNaN(v) || v <= 0 ? undefined : v);
              }}
            />
            <span class="unit">mm</span>
          </div>
        </label>
      {/if}

      {#if op.kind === 'pocket' || op.kind === 'profile'}
        {@const pickActive =
          project.sel.pickMode?.kind === 'approach-point' && project.sel.pickMode.opId === op.id}
        <div class="row" title={t('opprops.cut.approach_point.title')}>
          <span>{t('opprops.cut.approach_point')}</span>
          <div class="num-cell num-cell-pair">
            <input
              type="number"
              step="0.1"
              placeholder="X"
              aria-label={t('opprops.cut.approach_point.x_aria')}
              value={op.approachPoint?.[0] ?? ''}
              onchange={(e) => {
                const xs = (e.currentTarget as HTMLInputElement).value;
                if (xs === '') {
                  patch('approachPoint', undefined);
                  return;
                }
                const x = parseFloat(xs);
                const y = op.approachPoint?.[1] ?? 0;
                if (!isNaN(x)) patch('approachPoint', [x, y]);
              }}
            />
            <input
              type="number"
              step="0.1"
              placeholder="Y"
              aria-label={t('opprops.cut.approach_point.y_aria')}
              value={op.approachPoint?.[1] ?? ''}
              onchange={(e) => {
                const ys = (e.currentTarget as HTMLInputElement).value;
                if (ys === '') {
                  patch('approachPoint', undefined);
                  return;
                }
                const y = parseFloat(ys);
                const x = op.approachPoint?.[0] ?? 0;
                if (!isNaN(y)) patch('approachPoint', [x, y]);
              }}
            />
            <button
              type="button"
              class="reset-link"
              class:pick-active={pickActive}
              title={pickActive
                ? t('opprops.cut.approach_point.picking.title')
                : t('opprops.cut.approach_point.pick.title')}
              onclick={() => {
                project.sel.pickMode = pickActive ? null : { kind: 'approach-point', opId: op.id };
              }}
              >{pickActive
                ? t('opprops.cut.approach_point.picking')
                : t('opprops.cut.approach_point.pick')}</button
            >
            {#if op.approachPoint}
              <button
                type="button"
                class="reset-link"
                title={t('opprops.cut.approach_point.clear.title')}
                onclick={() => patch('approachPoint', undefined)}
                >{t('opprops.cut.approach_point.clear')}</button
              >
            {/if}
          </div>
        </div>
      {/if}
      <label class="row" title={t('opprops.cut.through_depth.title')}>
        <span>{t('opprops.cut.through_depth')}</span>
        <div class="num-cell">
          <input
            type="number"
            step="0.1"
            min="0"
            value={op.throughDepth ?? 0}
            onchange={(e) => {
              const v = parseFloat((e.currentTarget as HTMLInputElement).value);
              patch('throughDepth', isNaN(v) || v <= 0 ? undefined : v);
            }}
          />
          <span class="unit">mm</span>
        </div>
      </label>
      <label class="row" title={t('opprops.cut.depth_list.title')}>
        <span>{t('opprops.cut.depth_list')}</span>
        <div class="num-cell">
          <input
            type="text"
            placeholder={t('opprops.cut.depth_list.placeholder')}
            value={op.depthList ? op.depthList.join(', ') : ''}
            onchange={(e) => {
              const text = (e.currentTarget as HTMLInputElement).value.trim();
              if (text === '') {
                patch('depthList', undefined);
                return;
              }
              const parts = text
                .split(',')
                .map((s) => parseFloat(s.trim()))
                .filter((n) => !isNaN(n));
              patch('depthList', parts.length > 0 ? parts : undefined);
            }}
          />
          <span class="unit">mm</span>
        </div>
      </label>
      {#if op.kind === 'profile' || op.kind === 'pocket'}
        <label class="row" title={t(CUT_DIRECTION_HELP[op.cutDirection ?? 'conventional'])}>
          <span>{t('opprops.cut.direction')}</span>
          <select
            value={op.cutDirection ?? 'conventional'}
            onchange={(e) =>
              patch('cutDirection', (e.currentTarget as HTMLSelectElement).value as CutDirection)}
          >
            <option value="conventional" title={t(CUT_DIRECTION_HELP.conventional)}
              >{t('opprops.direction.conventional')}</option
            >
            <option value="climb" title={t(CUT_DIRECTION_HELP.climb)}
              >{t('opprops.direction.climb')}</option
            >
          </select>
        </label>
        <label class="row" title={t(CUT_DIRECTION_HELP[op.finishCutDirection ?? 'conventional'])}>
          <span>{t('opprops.cut.finish_direction')}</span>
          <select
            value={op.finishCutDirection ?? 'conventional'}
            onchange={(e) =>
              patch(
                'finishCutDirection',
                (e.currentTarget as HTMLSelectElement).value as CutDirection,
              )}
          >
            <option value="conventional" title={t(CUT_DIRECTION_HELP.conventional)}
              >{t('opprops.direction.conventional')}</option
            >
            <option value="climb" title={t(CUT_DIRECTION_HELP.climb)}
              >{t('opprops.direction.climb')}</option
            >
          </select>
        </label>
        <label class="row" title={t(PLUNGE_HELP[op.plunge?.kind ?? 'direct'])}>
          <span>{t('opprops.cut.plunge')}</span>
          <select
            value={op.plunge?.kind ?? 'direct'}
            onchange={(e) => {
              const v = (e.currentTarget as HTMLSelectElement).value;
              if (v === 'ramp') {
                patch('plunge', {
                  kind: 'ramp',
                  angleDeg: op.plunge && op.plunge.kind === 'ramp' ? op.plunge.angleDeg : 3,
                });
              } else if (v === 'helix') {
                // Sane default helix radius: 1.5 × tool radius, fallback 3mm.
                const tool = project.data.tools.find((t) => t.id === op?.toolId);
                const defaultRadius = tool ? Math.max(0.1, tool.diameter * 0.75) : 3;
                patch('plunge', {
                  kind: 'helix',
                  angleDeg: op.plunge && op.plunge.kind === 'helix' ? op.plunge.angleDeg : 3,
                  radiusMm:
                    op.plunge && op.plunge.kind === 'helix' ? op.plunge.radiusMm : defaultRadius,
                });
              } else {
                patch('plunge', { kind: 'direct' });
              }
            }}
          >
            <option value="direct" title={t(PLUNGE_HELP.direct)}
              >{t('opprops.plunge.direct')}</option
            >
            <option value="ramp" title={t(PLUNGE_HELP.ramp)}>{t('opprops.plunge.ramp')}</option>
            <option value="helix" title={t(PLUNGE_HELP.helix)}>{t('opprops.plunge.helix')}</option>
          </select>
        </label>
        {#if op.plunge && op.plunge.kind === 'ramp'}
          <label class="row" title={t('opprops.ramp_angle.title')}>
            <span>{t('opprops.ramp_angle')}</span>
            <div class="num-cell">
              <input
                type="number"
                step="0.5"
                min="0.5"
                max="45"
                value={op.plunge.angleDeg}
                onchange={(e) => {
                  const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                  if (!isNaN(v))
                    patch('plunge', { kind: 'ramp', angleDeg: Math.max(0.5, Math.min(45, v)) });
                }}
              />
              <span class="unit">°</span>
            </div>
          </label>
        {:else if op.plunge && op.plunge.kind === 'helix'}
          <details class="subsection" open>
            <summary>{t('opprops.helix')}</summary>
            <label class="row" title={t('opprops.helix_angle.title')}>
              <span>{t('opprops.helix_angle')}</span>
              <div class="num-cell">
                <input
                  type="number"
                  step="0.5"
                  min="0.5"
                  max="45"
                  value={op.plunge.angleDeg}
                  onchange={(e) => {
                    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                    if (!isNaN(v) && op.plunge && op.plunge.kind === 'helix')
                      patch('plunge', {
                        kind: 'helix',
                        angleDeg: Math.max(0.5, Math.min(45, v)),
                        radiusMm: op.plunge.radiusMm,
                      });
                  }}
                />
                <span class="unit">°</span>
              </div>
            </label>
            <label class="row" title={t('opprops.helix_autofit.title')}>
              <span>{t('opprops.helix_autofit')}</span>
              <input
                type="checkbox"
                checked={op.plunge.radiusMm === null}
                onchange={(e) => {
                  const checked = (e.currentTarget as HTMLInputElement).checked;
                  if (op.plunge && op.plunge.kind === 'helix') {
                    patch('plunge', {
                      kind: 'helix',
                      angleDeg: op.plunge.angleDeg,
                      radiusMm: checked ? null : lastManualHelixRadius,
                    });
                  }
                }}
              />
            </label>
            {#if op.plunge.radiusMm === null}
              <div class="row" title={t('opprops.helix_radius.auto.title')}>
                <span>{t('opprops.helix_radius')}</span>
                {#if helixPreview?.radius_mm != null}
                  <em class="placeholder"
                    >{t('opprops.helix_radius.auto_detected', {
                      value: helixPreview.radius_mm.toFixed(1),
                    })}</em
                  >
                {:else if helixPreview && helixPreview.radius_mm == null}
                  <em class="placeholder"
                    >{helixPreview.fallback_reason
                      ? t('opprops.helix_radius.auto_no_fit_reason', {
                          reason: helixPreview.fallback_reason,
                        })
                      : t('opprops.helix_radius.auto_no_fit')}</em
                  >
                {:else if helixPreviewLoading}
                  <em class="placeholder">{t('opprops.helix_radius.auto_pending')}</em>
                {:else}
                  <em class="placeholder">{t('opprops.helix_radius.auto_pending')}</em>
                {/if}
              </div>
            {:else}
              <label class="row" title={t('opprops.helix_radius.manual.title')}>
                <span>{t('opprops.helix_radius')}</span>
                <div class="num-cell">
                  <input
                    type="number"
                    step="0.1"
                    min="0.1"
                    max="50"
                    value={op.plunge.radiusMm}
                    onchange={(e) => {
                      const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                      if (!isNaN(v) && op.plunge && op.plunge.kind === 'helix')
                        patch('plunge', {
                          kind: 'helix',
                          angleDeg: op.plunge.angleDeg,
                          radiusMm: Math.max(0.1, Math.min(50, v)),
                        });
                    }}
                  />
                  <span class="unit">mm</span>
                </div>
              </label>
            {/if}
          </details>
        {/if}
      {/if}
    </fieldset>

    {#if op.kind === 'profile' || op.kind === 'pocket'}
      <details
        class="optional-section"
        open={(op.tabMode?.kind ?? 'off') !== 'off' ||
          (op.tabPlacements && op.tabPlacements.length > 0)}
      >
        <summary>
          {t('opprops.tabs')}
          <span class="opt-summary"
            >{op.tabMode?.kind === 'off' || !op.tabMode
              ? t('opprops.tabs.off')
              : op.tabMode.kind === 'manual'
                ? t('opprops.tabs.manual', { count: op.tabPlacements?.length ?? 0 })
                : op.tabMode.kind === 'auto'
                  ? t('opprops.tabs.auto', { count: op.tabMode.count })
                  : t('opprops.tabs.auto_manual', {
                      auto: op.tabMode.autoCount,
                      manual: op.tabPlacements?.length ?? 0,
                    })}</span
          >
        </summary>
        <TabsSection {op} {patch} />
      </details>
    {/if}

    {#if op.kind === 'profile'}
      <ProfileSection {op} {patch} />
    {:else if op.kind === 'pocket'}
      <PocketSection {op} {patch} />
    {/if}

    {#if op.kind === 'drill'}
      <DrillSection {op} {patch} />
    {/if}

    {#if op.kind === 'profile' || op.kind === 'pocket' || op.kind === 'engrave' || op.kind === 'drag_knife' || op.kind === 't_slot' || op.kind === 'dovetail'}
      <details
        class="optional-section"
        open={op.feedRateOverride !== undefined ||
          op.plungeRateOverride !== undefined ||
          (op.cornerFeedReduction ?? 0) > 0}
      >
        <summary>
          {t('opprops.feeds')}
          <span class="opt-summary"
            >{op.feedRateOverride !== undefined ||
            op.plungeRateOverride !== undefined ||
            (op.cornerFeedReduction ?? 0) > 0
              ? t('opprops.feeds.custom')
              : t('opprops.feeds.tool_defaults')}</span
          >
        </summary>
        <fieldset class="optional-fieldset">
          <legend>{t('opprops.feeds')}</legend>
          <label class="row" title={t('opprops.feeds.feed_rate.title')}>
            <span>{t('opprops.feeds.feed_rate')}</span>
            <div class="num-cell">
              <input
                type="number"
                step="50"
                min="0"
                placeholder={toolFeedRate != null
                  ? String(toolFeedRate)
                  : t('opprops.feeds.tool_default')}
                value={op.feedRateOverride ?? ''}
                onchange={(e) => {
                  const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
                  patch('feedRateOverride', isNaN(v) || v <= 0 ? undefined : v);
                }}
              />
              <span class="unit">mm/min</span>
              {#if op.feedRateOverride !== undefined}
                <button
                  type="button"
                  class="reset-link"
                  onclick={() => patch('feedRateOverride', undefined)}
                  title={t('opprops.feeds.feed_rate.reset.title')}
                  >{t('opprops.feeds.reset')}</button
                >
              {/if}
            </div>
          </label>
          <label class="row" title={t('opprops.feeds.plunge_rate.title')}>
            <span>{t('opprops.feeds.plunge_rate')}</span>
            <div class="num-cell">
              <input
                type="number"
                step="10"
                min="0"
                placeholder={toolPlungeRate != null
                  ? String(toolPlungeRate)
                  : t('opprops.feeds.tool_default')}
                value={op.plungeRateOverride ?? ''}
                onchange={(e) => {
                  const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
                  patch('plungeRateOverride', isNaN(v) || v <= 0 ? undefined : v);
                }}
              />
              <span class="unit">mm/min</span>
              {#if op.plungeRateOverride !== undefined}
                <button
                  type="button"
                  class="reset-link"
                  onclick={() => patch('plungeRateOverride', undefined)}
                  title={t('opprops.feeds.plunge_rate.reset.title')}
                  >{t('opprops.feeds.reset')}</button
                >
              {/if}
            </div>
          </label>
          <label class="row" title={t('opprops.feeds.corner_slow.title')}>
            <span>{t('opprops.feeds.corner_slow')}</span>
            <div class="num-cell">
              <input
                type="number"
                step="0.05"
                min="0"
                max="0.95"
                value={op.cornerFeedReduction ?? 0}
                onchange={(e) => {
                  const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                  patch('cornerFeedReduction', isNaN(v) ? 0 : Math.max(0, Math.min(0.95, v)));
                }}
              />
              <span class="unit" title={t('opprops.feeds.fraction.title')}
                >{t('opprops.feeds.fraction')}</span
              >
            </div>
          </label>
        </fieldset>
      </details>
    {/if}

    {#if op.kind === 'vcarve'}
      <VCarveSection {op} {patch} />
    {/if}

    {#if op.kind === 'chamfer'}
      <ChamferSection {op} {patch} />
    {/if}

    {#if op.kind === 'thread'}
      <ThreadSection {op} {patch} />
    {/if}

    <!-- No standalone helix op: users get helical plunge by adding a
         Pocket and setting Plunge → Helix in the Cut section. The
         OpKind 'helix' value is not in the union so this branch is
         unreachable; kept as a comment for the eventual
         standalone-helix-emitter feature. -->

    <!-- Group label. Consecutive enabled ops sharing the same
         value emit `; === GROUP: <name> ===` only ONCE at the entry —
         useful for marking rough / finish / drill / chamfer phases in
         the G-code stream. Collapsed by default; empty = no group. -->
    <details class="optional-section" open={op.group != null && op.group !== ''}>
      <summary>
        {t('opprops.group')}
        <span class="opt-summary">{op.group ? op.group : t('opprops.group.none')}</span>
      </summary>
      <label class="row" title={t('opprops.group.label.title')}>
        <span>{t('opprops.group.label')}</span>
        <input
          type="text"
          value={op.group ?? ''}
          placeholder={t('opprops.group.label.placeholder')}
          oninput={(e) => patch('group', (e.currentTarget as HTMLInputElement).value)}
        />
      </label>
      <!-- Pin this op's position when the project-level "Group ops
           by tool" reorder is on. A pinned op is a fixed barrier. -->
      <label class="row" title={t('opprops.group.pin_order.title')}>
        <span>{t('opprops.group.pin_order')}</span>
        <input
          type="checkbox"
          checked={op.pinOrder ?? false}
          onchange={(e) => patch('pinOrder', (e.currentTarget as HTMLInputElement).checked)}
        />
      </label>
    </details>

    {#if op.kind === 'drill'}
      <details class="optional-section" open={op.pattern !== undefined && op.pattern !== null}>
        <summary>
          {t('opprops.pattern')}
          <span class="opt-summary"
            >{op.pattern == null
              ? t('opprops.pattern.single')
              : op.pattern.kind === 'linear'
                ? t('opprops.pattern.linear', { count: op.pattern.count })
                : op.pattern.kind === 'grid'
                  ? t('opprops.pattern.grid', { x: op.pattern.countX, y: op.pattern.countY })
                  : t('opprops.pattern.polar', { count: op.pattern.count })}</span
          >
        </summary>
        <PatternSection {op} {patch} />
      </details>
    {/if}
  {/if}
</aside>

<style>
  /* Top-margin spacer for the pause-op explanatory paragraph (was an
     inline `style=""`; pulled into a class so CSP-strict deployments
     don't break and we don't ship a single-purpose inline rule). */
  :global(.hint.hint-pause) {
    margin-top: 0.5rem;
  }
  .props {
    width: 100%;
    height: 100%;
    background: var(--bg-panel);
    color: var(--text);
    border-left: 1px solid var(--border);
    overflow-y: auto;
    padding: 0.6rem 0.7rem 1rem;
    box-sizing: border-box;
    min-width: 0;
  }
  .props.embedded {
    height: auto;
    border-left: 0;
    background: transparent;
    padding: 0.4rem 0.5rem 0.6rem 1.6rem;
  }
</style>
