<script lang="ts">
  /// Operation properties panel — bound to project.selectedOpId. Shows
  /// the kind-specific parameters of the selected op plus a tool picker
  /// fed from project.tools. Edits are pushed straight back through
  /// project.updateOperation, so the operation list updates instantly.

  import {
    project,
    isContourOp,
    type OpEntry,
    type OpField,
    type OpFieldValue,
    type ProfileOp,
    type PocketOp,
    type ProfileOffset,
    type PocketStrategy,
    type SourceCombine,
    type CutDirection,
    type DrillCycle,
    type FrameShape,
  } from '../state/project.svelte';
  import { defaultClient } from '../api/http';
  import type { HelixRadiusResponse } from '../api/types';
  import { _ } from 'svelte-i18n';
  import VCarveSection from './op_properties/VCarveSection.svelte';
  import ChamferSection from './op_properties/ChamferSection.svelte';
  import ThreadSection from './op_properties/ThreadSection.svelte';
  import PatternSection from './op_properties/PatternSection.svelte';
  import DrillSection from './op_properties/DrillSection.svelte';
  import TabsSection from './op_properties/TabsSection.svelte';
  import ProfileSection from './op_properties/ProfileSection.svelte';
  import PocketSection from './op_properties/PocketSection.svelte';

  const apiClient = defaultClient();
  const HELIX_PREVIEW_DEBOUNCE_MS = 300;

  interface Props {
    /// True when rendered inline under an OperationsList row (drops the
    /// outer aside chrome + the standalone "Properties" header).
    embedded?: boolean;
  }
  let { embedded = false }: Props = $props();

  const op = $derived<OpEntry | null>(
    project.selectedOpId == null
      ? null
      : (project.operations.find((o) => o.id === project.selectedOpId) ?? null),
  );

  /// Resolve the assigned tool's defaultStep for the current op so the
  /// Step / pass input can fall back to it. null when no assignment.
  const toolDefaultStep = $derived<number | null>(
    op == null ? null : (project.tools.find((t) => t.id === op.toolId)?.defaultStep ?? null),
  );
  /// Tool defaults that the per-op feed / plunge fields inherit when
  /// unset. Placeholders below show these as concrete numbers (audit-bv6).
  const toolFeedRate = $derived<number | null>(
    op == null ? null : (project.tools.find((t) => t.id === op.toolId)?.feedRate ?? null),
  );
  const toolPlungeRate = $derived<number | null>(
    op == null ? null : (project.tools.find((t) => t.id === op.toolId)?.plungeRate ?? null),
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
    if (op?.plunge?.kind === 'helix' && op.plunge.radius_mm != null) {
      lastManualHelixRadius = op.plunge.radius_mm;
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
    op == null ? null : (project.tools.find((t) => t.id === op.toolId)?.diameter ?? null),
  );
  const helixAutoActive = $derived(
    op != null && op.plunge != null && op.plunge.kind === 'helix' && op.plunge.radius_mm === null,
  );
  const helixHasGeometry = $derived(
    project.imported != null && (project.imported.segments?.length ?? 0) > 0,
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
    const segments = project.imported?.segments ?? [];
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
          if (project.selectedOpId !== opIdAtStart) return;
          helixPreview = resp;
          helixPreviewLoading = false;
        })
        .catch(() => {
          if (project.selectedOpId !== opIdAtStart) return;
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
  {#if !embedded}
    <h3>Properties</h3>
  {/if}

  {#if !op}
    <p class="empty" class:embedded-empty={embedded}>Select an operation in the list to edit it.</p>
  {:else}
    <label class="row">
      <span>Name</span>
      <input
        type="text"
        value={op.name}
        oninput={(e) => patch('name', (e.currentTarget as HTMLInputElement).value)}
      />
    </label>

    <label class="row">
      <span>Tool</span>
      <div class="tool-cell">
        <select
          value={op.toolId}
          onchange={(e) =>
            patch('toolId', parseInt((e.currentTarget as HTMLSelectElement).value, 10))}
        >
          {#each project.tools as t (t.id)}
            <option value={t.id}>#{t.id} {t.name} ({t.diameter}mm)</option>
          {/each}
        </select>
        <button
          type="button"
          class="tool-edit"
          title="Edit this tool in the Tool library"
          aria-label="Edit this tool in the Tool library"
          onclick={(e) => {
            e.stopPropagation();
            project.toolsDialogFocusId = op.toolId;
          }}>⚙</button
        >
      </div>
    </label>

    {#if op.kind === 'pocket' || op.kind === 'drill'}
      <label
        class="row"
        title={op.kind === 'pocket'
          ? 'Optional finish tool (rt1.33). When different from the rough tool, the pipeline runs the bulk cascade with the rough tool, emits a T<n> M6 toolchange, then walks the wall ring with this smaller / sharper finish tool at its finish-set feed/speed. Empty = single-tool (the rough tool also defines the wall).'
          : 'Stufenfase chamfer cutter (rt1.20). Used only when Chamfer width is set below — after the drill cycle the pipeline emits a toolchange to this V-bit, then walks the hole rim at the chamfer depth. Empty = chamfer with the drill tool itself.'}
      >
        <span>Finish tool</span>
        <div class="tool-cell">
          <select
            value={op.finishToolId ?? ''}
            onchange={(e) => {
              const raw = (e.currentTarget as HTMLSelectElement).value;
              patch('finishToolId', raw === '' ? undefined : parseInt(raw, 10));
            }}
          >
            <option value="">— same as rough —</option>
            {#each project.tools as t (t.id)}
              <option value={t.id} disabled={t.id === op.toolId}
                >#{t.id} {t.name} ({t.diameter}mm)</option
              >
            {/each}
          </select>
        </div>
      </label>
    {/if}

    <fieldset>
      <legend>Source</legend>
      <label class="row">
        <span>Mode</span>
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
          <option value="_all_">all imported geometry</option>
          <option value="_layer_">specific layer(s)</option>
          <option value="_objects_">selected objects</option>
        </select>
      </label>
      {#if op.sourceLayers !== null && (op.sourceObjects?.length ?? 0) === 0}
        <label class="row">
          <span>Layer</span>
          <select
            value={op.sourceLayers[0] ?? ''}
            onchange={(e) => patch('sourceLayers', [(e.currentTarget as HTMLSelectElement).value])}
          >
            <option value="">— pick a layer —</option>
            {#if project.imported}
              {#each project.imported.layers.filter((l) => l.segment_count > 0) as layer (layer.name)}
                <option value={layer.name}>"{layer.name}"</option>
              {/each}
            {/if}
          </select>
        </label>
      {:else if op.sourceObjects && op.sourceObjects.length > 0}
        <p class="hint">{op.sourceObjects.length} object(s) selected</p>
      {:else if op.sourceLayers === null}
        <p class="hint">runs on every chain in the import</p>
      {/if}
      <!-- Drill picks a single XY per selected object (POINT / circle
           center / bbox center) and emits a drill cycle there. The
           area-based boolean modes (union / difference / intersection
           / xor) have no effect — each object gets its own hole no
           matter what. Hide the Combine selector for Drill to stop
           promising a knob that does nothing. -->
      {#if op.kind !== 'drill' && ((op.sourceObjects?.length ?? 0) > 1 || (op.sourceLayers !== null && op.sourceLayers.length > 0))}
        <label class="row" title={$_('op.help.combine.' + (op.sourceCombine ?? 'auto'))}>
          <span>Combine</span>
          <select
            value={op.sourceCombine ?? 'auto'}
            onchange={(e) =>
              patch('sourceCombine', (e.currentTarget as HTMLSelectElement).value as SourceCombine)}
          >
            <option value="auto" title={$_('op.help.combine.auto')}>auto (containment)</option>
            <option value="union" title={$_('op.help.combine.union')}>union</option>
            <option value="difference" title={$_('op.help.combine.difference')}>difference</option>
            <option value="intersection" title={$_('op.help.combine.intersection')}
              >intersection</option
            >
            <option value="xor" title={$_('op.help.combine.xor')}>xor</option>
            <option value="none" title={$_('op.help.combine.none')}>none (per object)</option>
          </select>
        </label>
      {/if}
      <button
        class="from-selection"
        class:ghost={project.selectedObjects.size === 0}
        type="button"
        disabled={project.selectedObjects.size === 0}
        aria-label={project.selectedObjects.size === 0
          ? 'Select one or more objects in the 2D canvas first to enable this.'
          : `Set sources from ${project.selectedObjects.size} selected`}
        title={project.selectedObjects.size === 0
          ? 'Select one or more objects in the 2D canvas first to enable this.'
          : 'Use the chains currently highlighted in the 2D pane'}
        onclick={() => {
          patch('sourceLayers', null);
          patch('sourceObjects', [...project.selectedObjects]);
        }}
        >{project.selectedObjects.size === 0
          ? 'Set sources from selection'
          : `Set sources from ${project.selectedObjects.size} selected`}</button
      >
    </fieldset>

    <fieldset>
      <legend>Cut</legend>
      <label class="row">
        <span>Final depth</span>
        <div class="num-cell">
          <input
            type="number"
            step="0.1"
            value={op.depth}
            onchange={(e) =>
              patch('depth', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
          />
          <span class="unit">mm</span>
        </div>
      </label>
      <label class="row">
        <span>Start depth</span>
        <div class="num-cell">
          <input
            type="number"
            step="0.1"
            value={op.startDepth}
            onchange={(e) =>
              patch('startDepth', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
          />
          <span class="unit">mm</span>
        </div>
      </label>
      <label class="row">
        <span>Step / pass</span>
        <div class="step-cell">
          <input
            type="number"
            step="0.1"
            value={op.step ?? ''}
            placeholder={stepInheriting && toolDefaultStep !== null && toolDefaultStep < 0
              ? `${toolDefaultStep} (from tool)`
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
              title="Clear the override and inherit the tool's default Z step."
              onclick={() => patch('step', null)}>reset to inherit</button
            >
          {/if}
        </div>
      </label>
      {#if stepMissing}
        <p class="step-error">Step required (set per-op or in the tool library).</p>
      {/if}
      {#if isContourOp(op)}
        <label
          class="row"
          title="Optional smaller step for the FINAL Z pass — gives a thin finishing pass at the bottom for cleaner surface. Same sign as Step (negative). Empty = same as Step."
        >
          <span>Finish step</span>
          <div class="num-cell">
            <input
              type="number"
              step="0.05"
              placeholder="same as step"
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
        <label
          class="row"
          title="Material left UNCUT on the walls by the roughing pass. A dedicated finish ring walks the actual boundary at the tool's finish-set feed/speed to remove it. Empty / 0 = no allowance (roughing reaches the wall in one pass)."
        >
          <span>XY finish stock</span>
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
          project.pickMode?.kind === 'approach-point' && project.pickMode.opId === op.id}
        <div
          class="row"
          title="Anfahrpunkt (rt1.26): user-picked XY where the cutter enters each closed ring. Each closed offset's start vertex is rotated to the segment closest to this point — plunge/lead-in then happens there instead of an auto-picked vertex. Empty = auto."
        >
          <span>Approach point</span>
          <div class="num-cell num-cell-pair">
            <input
              type="number"
              step="0.1"
              placeholder="X"
              aria-label="Approach point X"
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
              aria-label="Approach point Y"
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
                ? 'Picking — click in canvas, ESC to finalize'
                : 'Pick the approach point by clicking in the 2D canvas (Shift = disable snap)'}
              onclick={() => {
                project.pickMode = pickActive
                  ? null
                  : { kind: 'approach-point', opId: op.id };
              }}
            >{pickActive ? 'picking…' : 'pick'}</button>
            {#if op.approachPoint}
              <button
                type="button"
                class="reset-link"
                title="Clear approach point (auto-pick)"
                onclick={() => patch('approachPoint', undefined)}>clear</button
              >
            {/if}
          </div>
        </div>
      {/if}
      <label
        class="row"
        title="Cut past the nominal depth by this many mm. Useful for through-cuts on edge-clamped sheet so the cutter clears the bottom. 0 = no extension."
      >
        <span>Through depth</span>
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
      <label
        class="row"
        title="Explicit comma-separated list of Z depths (negative numbers, e.g. -0.5, -1.5, -3). When non-empty, overrides Step / Finish step / Through depth. Empty = use the step-down loop."
      >
        <span>Depth list</span>
        <div class="num-cell">
          <input
            type="text"
            placeholder="e.g. -0.5, -1.5, -3"
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
        <label
          class="row"
          title={$_('op.help.cut_direction.' + (op.cutDirection ?? 'conventional'))}
        >
          <span>Direction</span>
          <select
            value={op.cutDirection ?? 'conventional'}
            onchange={(e) =>
              patch('cutDirection', (e.currentTarget as HTMLSelectElement).value as CutDirection)}
          >
            <option value="conventional" title={$_('op.help.cut_direction.conventional')}
              >conventional</option
            >
            <option value="climb" title={$_('op.help.cut_direction.climb')}>climb</option>
          </select>
        </label>
        <label
          class="row"
          title={$_('op.help.cut_direction.' + (op.finishCutDirection ?? 'conventional'))}
        >
          <span>Finish dir</span>
          <select
            value={op.finishCutDirection ?? 'conventional'}
            onchange={(e) =>
              patch(
                'finishCutDirection',
                (e.currentTarget as HTMLSelectElement).value as CutDirection,
              )}
          >
            <option value="conventional" title={$_('op.help.cut_direction.conventional')}
              >conventional</option
            >
            <option value="climb" title={$_('op.help.cut_direction.climb')}>climb</option>
          </select>
        </label>
        <label class="row" title={$_('op.help.plunge.' + (op.plunge?.kind ?? 'direct'))}>
          <span>Plunge</span>
          <select
            value={op.plunge?.kind ?? 'direct'}
            onchange={(e) => {
              const v = (e.currentTarget as HTMLSelectElement).value;
              if (v === 'ramp') {
                patch('plunge', {
                  kind: 'ramp',
                  angle_deg: op.plunge && op.plunge.kind === 'ramp' ? op.plunge.angle_deg : 3,
                });
              } else if (v === 'helix') {
                // Sane default helix radius: 1.5 × tool radius, fallback 3mm.
                const tool = project.tools.find((t) => t.id === op?.toolId);
                const defaultRadius = tool ? Math.max(0.1, tool.diameter * 0.75) : 3;
                patch('plunge', {
                  kind: 'helix',
                  angle_deg: op.plunge && op.plunge.kind === 'helix' ? op.plunge.angle_deg : 3,
                  radius_mm:
                    op.plunge && op.plunge.kind === 'helix' ? op.plunge.radius_mm : defaultRadius,
                });
              } else {
                patch('plunge', { kind: 'direct' });
              }
            }}
          >
            <option value="direct" title={$_('op.help.plunge.direct')}>direct</option>
            <option value="ramp" title={$_('op.help.plunge.ramp')}>ramp</option>
            <option value="helix" title={$_('op.help.plunge.helix')}>helix</option>
          </select>
        </label>
        {#if op.plunge && op.plunge.kind === 'ramp'}
          <label
            class="row"
            title="Ramp angle in degrees. 1°–5° is gentle, 10°+ is aggressive. The ramp's horizontal length is step / tan(angle)."
          >
            <span>Ramp angle</span>
            <div class="num-cell">
              <input
                type="number"
                step="0.5"
                min="0.5"
                max="45"
                value={op.plunge.angle_deg}
                onchange={(e) => {
                  const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                  if (!isNaN(v))
                    patch('plunge', { kind: 'ramp', angle_deg: Math.max(0.5, Math.min(45, v)) });
                }}
              />
              <span class="unit">°</span>
            </div>
          </label>
        {:else if op.plunge && op.plunge.kind === 'helix'}
          <details class="subsection" open>
            <summary>{$_('op.section.helix')}</summary>
            <label
              class="row"
              title="Helix descent angle in degrees. 1°–5° is gentle, 10°+ is aggressive. Each revolution drops Z by 2π·radius·tan(angle)."
            >
              <span>Helix angle</span>
              <div class="num-cell">
                <input
                  type="number"
                  step="0.5"
                  min="0.5"
                  max="45"
                  value={op.plunge.angle_deg}
                  onchange={(e) => {
                    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                    if (!isNaN(v) && op.plunge && op.plunge.kind === 'helix')
                      patch('plunge', {
                        kind: 'helix',
                        angle_deg: Math.max(0.5, Math.min(45, v)),
                        radius_mm: op.plunge.radius_mm,
                      });
                  }}
                />
                <span class="unit">°</span>
              </div>
            </label>
            <label
              class="row"
              title="Auto-fit the helix circle to the largest inscribed circle inside the pocket boundary. Falls back to ramp when no helix circle fits."
            >
              <span>Auto-fit helix</span>
              <input
                type="checkbox"
                checked={op.plunge.radius_mm === null}
                onchange={(e) => {
                  const checked = (e.currentTarget as HTMLInputElement).checked;
                  if (op.plunge && op.plunge.kind === 'helix') {
                    patch('plunge', {
                      kind: 'helix',
                      angle_deg: op.plunge.angle_deg,
                      radius_mm: checked ? null : lastManualHelixRadius,
                    });
                  }
                }}
              />
            </label>
            {#if op.plunge.radius_mm === null}
              <div
                class="row"
                title="Auto-fit picks the helix radius from the pocket geometry. The detected value previews here before generation; the final fit re-runs at gcode time."
              >
                <span>Helix radius</span>
                {#if helixPreview?.radius_mm != null}
                  <em class="placeholder"
                    >Auto (detected: {helixPreview.radius_mm.toFixed(1)} mm)</em
                  >
                {:else if helixPreview && helixPreview.radius_mm == null}
                  <em class="placeholder"
                    >Auto (no fit — will Ramp instead{helixPreview.fallback_reason
                      ? `: ${helixPreview.fallback_reason}`
                      : ''})</em
                  >
                {:else if helixPreviewLoading}
                  <em class="placeholder">Auto (will fit at generation)</em>
                {:else}
                  <em class="placeholder">Auto (will fit at generation)</em>
                {/if}
              </div>
            {:else}
              <label
                class="row"
                title="Helix radius in mm. Should be ≥ tool radius; sane default is 1.5 × tool radius. Larger = more clearance, more material removed by the spiral."
              >
                <span>Helix radius</span>
                <div class="num-cell">
                  <input
                    type="number"
                    step="0.1"
                    min="0.1"
                    max="50"
                    value={op.plunge.radius_mm}
                    onchange={(e) => {
                      const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                      if (!isNaN(v) && op.plunge && op.plunge.kind === 'helix')
                        patch('plunge', {
                          kind: 'helix',
                          angle_deg: op.plunge.angle_deg,
                          radius_mm: Math.max(0.1, Math.min(50, v)),
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
      <TabsSection {op} {patch} />
    {/if}

    {#if op.kind === 'profile'}
      <ProfileSection {op} {patch} />
    {:else if op.kind === 'pocket'}
      <PocketSection {op} {patch} />
    {/if}

    {#if op.kind === 'drill'}
      <DrillSection {op} {patch} />
    {/if}

    {#if op.kind === 'profile' || op.kind === 'pocket' || op.kind === 'engrave' || op.kind === 'drag_knife'}
      <fieldset>
        <legend>Feeds (overrides)</legend>
        <label
          class="row"
          title="Override the tool's feed rate (mm/min) for this operation only. Leave empty to use the tool default."
        >
          <span>Feed rate</span>
          <div class="num-cell">
            <input
              type="number"
              step="50"
              min="0"
              placeholder={toolFeedRate != null ? String(toolFeedRate) : 'tool default'}
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
                title="Clear override and inherit from the tool's feed rate.">reset</button
              >
            {/if}
          </div>
        </label>
        <label
          class="row"
          title="Override the tool's plunge rate (mm/min) for Z descents in this operation. Leave empty to use the tool default."
        >
          <span>Plunge rate</span>
          <div class="num-cell">
            <input
              type="number"
              step="10"
              min="0"
              placeholder={toolPlungeRate != null ? String(toolPlungeRate) : 'tool default'}
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
                title="Clear override and inherit from the tool's plunge rate.">reset</button
              >
            {/if}
          </div>
        </label>
        <label
          class="row"
          title="Slow the feed at sharp Line→Line corners by this fraction. 0 = no reduction (default). 0.5 = half feed at corners. Most useful for zigzag pocket fills with their many 180° turns."
        >
          <span>Corner slow</span>
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
            <span class="unit" title="Unitless fraction between 0 and 1.">fraction</span>
          </div>
        </label>
      </fieldset>
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

    <!-- Standalone helix op was removed (audit-sue): users get helical
         plunge by adding a Pocket and setting Plunge → Helix in the
         Cut section. The OpKind 'helix' value is no longer in the
         union so this branch is unreachable; kept as a comment for
         the eventual standalone-helix-emitter feature reintroduction. -->


    <PatternSection {op} {patch} />
  {/if}
</aside>

<style>
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
  :global(.props h3) {
    margin: 0 0 0.4rem 0;
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
  }
  :global(.props .empty) {
    color: var(--text-faint);
    font-size: 0.78rem;
  }
  :global(.props .empty.embedded-empty) {
    text-align: center;
    font-size: 0.72rem;
    opacity: 0.7;
    margin: 0.4rem 0;
  }
  :global(.props .placeholder) {
    color: var(--text-faint);
    font-size: 0.78rem;
    font-style: italic;
  }
  :global(.props .row) {
    display: grid;
    grid-template-columns: minmax(0, 6.5rem) minmax(0, 1fr);
    gap: 0.5rem;
    align-items: center;
    margin: 0.2rem 0;
    font-size: 0.78rem;
  }
  :global(.props fieldset) {
    border: 1px solid var(--border);
    border-radius: 3px;
    margin: 0.4rem 0;
    padding: 0.3rem 0.5rem 0.4rem;
    background: var(--bg-elevated);
  }
  :global(.props legend) {
    font-size: 0.7rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0 0.3rem;
  }
  :global(.props input),
  :global(.props select) {
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
  :global(.props .hint) {
    margin: 0.2rem 0 0;
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  :global(.props .hint.warn) {
    color: var(--warn, #b86f00);
    background: var(--warn-bg, rgba(184, 111, 0, 0.08));
    border-left: 2px solid var(--warn, #b86f00);
    padding: 0.15rem 0.4rem;
    border-radius: 2px;
  }
  :global(.props .num) {
    font-variant-numeric: tabular-nums;
    font-size: 0.78rem;
    color: var(--text-muted);
    min-width: 3em;
    text-align: right;
  }
  :global(.props .from-selection) {
    margin-top: 0.3rem;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.25rem 0.6rem;
    font-size: 0.74rem;
    cursor: pointer;
    width: 100%;
  }
  :global(.props .from-selection:disabled) {
    cursor: not-allowed;
  }
  :global(.props .from-selection.ghost) {
    opacity: 0.5;
    border-style: dashed;
  }
  :global(.props .warn-chip) {
    margin: 0.2rem 0;
    padding: 0.2rem 0.4rem;
    border-radius: 3px;
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
    border: 1px solid var(--warn);
    font-size: 0.72rem;
  }
  :global(.props .step-cell) {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    min-width: 0;
  }
  :global(.props .step-cell input) {
    flex: 1 1 auto;
    min-width: 0;
  }
  :global(.props .num-cell) {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    min-width: 0;
  }
  :global(.props .num-cell input) {
    flex: 1 1 auto;
    min-width: 0;
  }
  :global(.props .num-cell-pair input) {
    flex: 1 1 0;
    min-width: 0;
  }
  :global(.props .segmented) {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 3px;
    overflow: hidden;
    background: var(--bg-elevated);
  }
  :global(.props .segmented button) {
    background: transparent;
    color: var(--text);
    border: 0;
    border-left: 1px solid var(--border);
    padding: 0.2rem 0.5rem;
    font-size: 0.7rem;
    text-transform: capitalize;
    cursor: pointer;
  }
  :global(.props .segmented button:first-child) {
    border-left: 0;
  }
  :global(.props .segmented button.active) {
    background: color-mix(in srgb, var(--accent) 30%, transparent);
    color: var(--text-strong);
  }
  :global(.props .segmented button:hover:not(.active)) {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  :global(.props .unit) {
    font-size: 0.7rem;
    color: var(--text-muted);
    margin-left: 0.25rem;
    white-space: nowrap;
    flex: 0 0 auto;
  }
  :global(.props .range-cell) {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    min-width: 0;
  }
  :global(.props .range-cell input[type='range']) {
    flex: 1 1 auto;
    min-width: 0;
    padding: 0;
  }
  :global(.props .range-min),
  :global(.props .range-max) {
    font-size: 0.68rem;
    color: var(--text-faint);
    flex: 0 0 auto;
    white-space: nowrap;
  }
  :global(.props .tool-cell) {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    min-width: 0;
  }
  :global(.props .tool-cell select) {
    flex: 1 1 auto;
    min-width: 0;
  }
  :global(.props .tool-edit) {
    background: var(--bg-elevated);
    color: var(--text-muted);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0 0.4rem;
    font-size: 0.9rem;
    line-height: 1.4;
    cursor: pointer;
    flex: 0 0 auto;
  }
  :global(.props .tool-edit:hover) {
    color: var(--accent-strong);
    border-color: var(--accent);
  }
  :global(.props input.inherit::placeholder) {
    font-style: italic;
    color: var(--text-faint);
  }
  :global(.props input.invalid) {
    border-color: var(--danger, #c44);
  }
  :global(.props .reset-link) {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    font-size: 0.7rem;
    text-decoration: underline;
    cursor: pointer;
    padding: 0;
    white-space: nowrap;
  }
  :global(.props .reset-link.pick-active) {
    color: var(--accent);
    font-weight: 600;
    text-decoration: none;
  }
  :global(.props .step-error) {
    margin: 0.1rem 0 0.2rem;
    padding: 0.15rem 0.4rem;
    background: color-mix(in srgb, var(--danger, #c44) 18%, transparent);
    color: var(--danger, #c44);
    border: 1px solid var(--danger, #c44);
    border-radius: 3px;
    font-size: 0.72rem;
    width: max-content;
  }
  :global(.props .subsection) {
    margin: 0.3rem 0 0.1rem;
    border-top: 1px solid var(--border);
    padding-top: 0.2rem;
  }
  :global(.props .subsection > summary) {
    cursor: pointer;
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
    padding: 0.15rem 0;
    list-style: none;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    user-select: none;
  }
  :global(.props .subsection > summary::-webkit-details-marker) {
    display: none;
  }
  :global(.props .subsection > summary::before) {
    content: '▸';
    font-size: 0.6rem;
    transition: transform 0.12s ease;
    color: var(--text-faint);
  }
  :global(.props .subsection[open] > summary::before) {
    transform: rotate(90deg);
  }
</style>
