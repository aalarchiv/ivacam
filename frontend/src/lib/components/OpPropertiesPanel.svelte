<script lang="ts">
  /// Operation properties panel — bound to project.selectedOpId. Shows
  /// the kind-specific parameters of the selected op plus a tool picker
  /// fed from project.tools. Edits are pushed straight back through
  /// project.updateOperation, so the operation list updates instantly.

  import {
    project,
    type OpEntry,
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
      : project.operations.find((o) => o.id === project.selectedOpId) ?? null,
  );

  /// rt1.10 / zed: count tab placements whose objectId is no longer
  /// reachable from this op's source. "Reachable" means the import
  /// still carries an object with that id AND the op's source filter
  /// would include it.
  function disconnectedTabCount(op: OpEntry): number {
    const placements = op.tabPlacements ?? [];
    if (placements.length === 0) return 0;
    const imp = project.imported;
    if (!imp) return 0;
    const liveIds = new Set<number>(imp.objects ?? []);
    const so = op.sourceObjects;
    const allowed = (id: number) =>
      liveIds.has(id) && (!so || so.length === 0 || so.includes(id));
    return placements.filter((p) => !allowed(p.objectId)).length;
  }

  /// One-click strip of disconnected placements. Single
  /// updateOperation call so it lands as one undoable history entry.
  function clearDisconnectedTabs(op: OpEntry) {
    const imp = project.imported;
    if (!imp) return;
    const liveIds = new Set<number>(imp.objects ?? []);
    const so = op.sourceObjects;
    const allowed = (id: number) =>
      liveIds.has(id) && (!so || so.length === 0 || so.includes(id));
    const next = (op.tabPlacements ?? []).filter((p) => allowed(p.objectId));
    project.updateOperation(op.id, { tabPlacements: next });
  }

  /// Resolve the assigned tool's defaultStep for the current op so the
  /// Step / pass input can fall back to it. null when no assignment.
  const toolDefaultStep = $derived<number | null>(
    op == null
      ? null
      : project.tools.find((t) => t.id === op.toolId)?.defaultStep ?? null,
  );
  const stepInheriting = $derived(op != null && (op.step === null || op.step === undefined));
  const stepMissing = $derived(
    stepInheriting && (toolDefaultStep === null || toolDefaultStep >= 0),
  );

  function patch<K extends keyof OpEntry>(key: K, value: OpEntry[K]) {
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
    op == null ? null : project.tools.find((t) => t.id === op.toolId)?.diameter ?? null,
  );
  const helixAutoActive = $derived(
    op != null
      && op.plunge != null
      && op.plunge.kind === 'helix'
      && op.plunge.radius_mm === null,
  );
  const helixHasGeometry = $derived(
    project.imported != null && (project.imported.segments?.length ?? 0) > 0,
  );
  const helixHasSelection = $derived(
    op != null && (op.sourceObjects?.length ?? 0) > 0,
  );

  $effect(() => {
    if (!helixAutoActive || !helixHasGeometry || !helixHasSelection || helixToolDiameter == null) {
      helixPreview = null;
      helixPreviewLoading = false;
      return;
    }
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
          if (op?.id !== opIdAtStart) return;
          helixPreview = resp;
          helixPreviewLoading = false;
        })
        .catch(() => {
          if (op?.id !== opIdAtStart) return;
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
    <p class="empty" class:embedded-empty={embedded}>
      Select an operation in the list to edit it.
    </p>
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
          onchange={(e) => patch('toolId', parseInt((e.currentTarget as HTMLSelectElement).value, 10))}
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
          }}
        >⚙</button>
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
              if (op && (op.sourceObjects?.length ?? 0) === 0)
                patch('sourceObjects', []);
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
      {#if (op.sourceObjects?.length ?? 0) > 1 || (op.sourceLayers !== null && op.sourceLayers.length > 0)}
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
            <option value="intersection" title={$_('op.help.combine.intersection')}>intersection</option>
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
          : `Set sources from ${project.selectedObjects.size} selected`}</button>
    </fieldset>

    <fieldset>
      <legend>Cut</legend>
      <label class="row">
        <span>Final depth</span>
        <div class="num-cell">
          <input
            type="number" step="0.1" value={op.depth}
            onchange={(e) => patch('depth', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
          />
          <span class="unit">mm</span>
        </div>
      </label>
      <label class="row">
        <span>Start depth</span>
        <div class="num-cell">
          <input
            type="number" step="0.1" value={op.startDepth}
            onchange={(e) => patch('startDepth', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
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
              onclick={() => patch('step', null)}
            >reset to inherit</button>
          {/if}
        </div>
      </label>
      {#if stepMissing}
        <p class="step-error">Step required (set per-op or in the tool library).</p>
      {/if}
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
        <div
          class="row"
          title="Anfahrpunkt (rt1.26): user-picked XY where the cutter enters each closed ring. Each closed offset's start vertex is rotated to the segment closest to this point — plunge/lead-in then happens there instead of an auto-picked vertex. Empty = auto. (Click-pick UI is a follow-up.)"
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
            {#if op.approachPoint}
              <button
                type="button"
                class="reset-link"
                title="Clear approach point (auto-pick)"
                onclick={() => patch('approachPoint', undefined)}
              >clear</button>
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
        <label class="row" title={$_('op.help.cut_direction.' + (op.cutDirection ?? 'conventional'))}>
          <span>Direction</span>
          <select
            value={op.cutDirection ?? 'conventional'}
            onchange={(e) =>
              patch('cutDirection', (e.currentTarget as HTMLSelectElement).value as CutDirection)}
          >
            <option value="conventional" title={$_('op.help.cut_direction.conventional')}>conventional</option>
            <option value="climb" title={$_('op.help.cut_direction.climb')}>climb</option>
          </select>
        </label>
        <label class="row" title={$_('op.help.cut_direction.' + (op.finishCutDirection ?? 'conventional'))}>
          <span>Finish dir</span>
          <select
            value={op.finishCutDirection ?? 'conventional'}
            onchange={(e) =>
              patch('finishCutDirection', (e.currentTarget as HTMLSelectElement).value as CutDirection)}
          >
            <option value="conventional" title={$_('op.help.cut_direction.conventional')}>conventional</option>
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
                  radius_mm: op.plunge && op.plunge.kind === 'helix' ? op.plunge.radius_mm : defaultRadius,
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
          <label class="row" title="Ramp angle in degrees. 1°–5° is gentle, 10°+ is aggressive. The ramp's horizontal length is step / tan(angle).">
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
            <label class="row" title="Helix descent angle in degrees. 1°–5° is gentle, 10°+ is aggressive. Each revolution drops Z by 2π·radius·tan(angle).">
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
            <label class="row" title="Auto-fit the helix circle to the largest inscribed circle inside the pocket boundary. Falls back to ramp when no helix circle fits.">
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
              <div class="row" title="Auto-fit picks the helix radius from the pocket geometry. The detected value previews here before generation; the final fit re-runs at gcode time.">
                <span>Helix radius</span>
                {#if helixPreview?.radius_mm != null}
                  <em class="placeholder">Auto (detected: {helixPreview.radius_mm.toFixed(1)} mm)</em>
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
              <label class="row" title="Helix radius in mm. Should be ≥ tool radius; sane default is 1.5 × tool radius. Larger = more clearance, more material removed by the spiral.">
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
                }}
              >{mk}</button>
            {/each}
          </div>
        </div>
        {#if op.tabMode?.kind === 'auto' || op.tabMode?.kind === 'mixed'}
          <label class="row" title="Number of tabs to auto-place evenly around each closed contour.">
            <span>Count</span>
            <div class="num-cell">
              <input
                type="number"
                min="1"
                step="1"
                value={op.tabMode.kind === 'auto' ? op.tabMode.count : op.tabMode.auto_count}
                onchange={(e) => {
                  const n = Math.max(1, parseInt((e.currentTarget as HTMLInputElement).value, 10) || 1);
                  if (op.tabMode?.kind === 'auto') patch('tabMode', { kind: 'auto', count: n });
                  else if (op.tabMode?.kind === 'mixed')
                    patch('tabMode', { kind: 'mixed', auto_count: n });
                }}
              />
            </div>
          </label>
        {/if}
        {#if op.tabMode?.kind === 'manual' || op.tabMode?.kind === 'mixed'}
          <p class="hint" title="Click on a closed contour in the 2D canvas to place a tab. Click on an existing tab to remove it.">
            Click the 2D canvas to add or remove tabs.
            {#if op.tabPlacements && op.tabPlacements.length > 0}
              ({op.tabPlacements.length} placed)
            {/if}
          </p>
          {@const disconnected = disconnectedTabCount(op)}
          {#if disconnected > 0}
            <p class="hint warn" title="These tabs reference objects that are no longer in this op's source set (either removed from the import or no longer selected). The pipeline silently drops them; clear them out to keep the data tidy.">
              <strong>{disconnected}</strong> tab{disconnected === 1 ? '' : 's'} disconnected
              <button
                type="button"
                class="reset-link"
                onclick={() => clearDisconnectedTabs(op)}
              >clear</button>
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
                    if (!isNaN(v))
                      patch('tabRampAngleDeg', Math.max(1, Math.min(89, v)));
                  }}
                />
                <span class="unit">°</span>
              </div>
            </label>
          </details>
        {/if}
      </fieldset>
    {/if}

    {#if op.kind === 'profile'}
      <fieldset>
        <legend>Profile</legend>
        <label class="row">
          <span>Tool offset</span>
          <select
            value={op.offset}
            onchange={(e) => patch('offset', (e.currentTarget as HTMLSelectElement).value as ProfileOffset)}
          >
            <option value="outside">outside</option>
            <option value="inside">inside</option>
            <option value="on">on path</option>
          </select>
        </label>
      </fieldset>

      <fieldset>
        <legend>Leads</legend>
        <label
          class="row"
          title="Lead-IN style. Off: rapid + plunge directly to the contour start. Straight: rapid to a point perpendicular to the start, then linear into the contour. Arc: tangent quarter-arc roll-on so the cutter eases into the cut without dwelling at the start."
        >
          <span>Lead in</span>
          <select
            value={op.leadInKind ?? 'off'}
            onchange={(e) =>
              patch('leadInKind', (e.currentTarget as HTMLSelectElement).value as 'off' | 'straight' | 'arc')}
          >
            <option value="off">off</option>
            <option value="straight">straight</option>
            <option value="arc">arc (roll-on)</option>
          </select>
        </label>
        {#if op.leadInKind && op.leadInKind !== 'off'}
          <label
            class="row"
            title={op.leadInKind === 'arc'
              ? 'Roll-on arc RADIUS (mm). The arc is a quarter-circle tangent to the contour at the entry point.'
              : 'Straight-line LENGTH (mm) of the perpendicular hop into the contour.'}
          >
            <span>{op.leadInKind === 'arc' ? 'Radius' : 'Length'}</span>
            <div class="num-cell">
              <input
                type="number"
                step="0.5"
                min="0"
                value={op.leadIn ?? 5}
                onchange={(e) => {
                  const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                  patch('leadIn', isNaN(v) || v < 0 ? 0 : v);
                }}
              />
              <span class="unit">mm</span>
            </div>
          </label>
        {/if}
        <label
          class="row"
          title="Lead-OUT style. Mirror of lead-in: how the cutter departs the contour at the END of the cut path. Arc gives a tangent roll-off; Straight a perpendicular exit; Off ends the cut at the contour end with a vertical retract."
        >
          <span>Lead out</span>
          <select
            value={op.leadOutKind ?? 'off'}
            onchange={(e) =>
              patch('leadOutKind', (e.currentTarget as HTMLSelectElement).value as 'off' | 'straight' | 'arc')}
          >
            <option value="off">off</option>
            <option value="straight">straight</option>
            <option value="arc">arc (roll-off)</option>
          </select>
        </label>
        {#if op.leadOutKind && op.leadOutKind !== 'off'}
          <label
            class="row"
            title={op.leadOutKind === 'arc'
              ? 'Roll-off arc RADIUS (mm). Quarter-circle tangent to the contour at the exit point.'
              : 'Straight-line LENGTH (mm) of the perpendicular exit from the contour.'}
          >
            <span>{op.leadOutKind === 'arc' ? 'Radius' : 'Length'}</span>
            <div class="num-cell">
              <input
                type="number"
                step="0.5"
                min="0"
                value={op.leadOut ?? 5}
                onchange={(e) => {
                  const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                  patch('leadOut', isNaN(v) || v < 0 ? 0 : v);
                }}
              />
              <span class="unit">mm</span>
            </div>
          </label>
        {/if}
      </fieldset>
    {:else if op.kind === 'pocket'}
      {#if op.frameShape != null}
        {@const opTool = project.tools.find((tt) => tt.id === op.toolId)}
        <fieldset>
          <legend>Frame</legend>
          <details class="subsection" open>
            <summary>{$_('op.section.frame')}</summary>
            <label class="row" title="Shape of the synthetic frame the pipeline derives from your selection at generate time.">
              <span>Shape</span>
              <select
                value={op.frameShape}
                onchange={(e) =>
                  patch('frameShape', (e.currentTarget as HTMLSelectElement).value as FrameShape)}
              >
                <option value="rectangle">rectangle</option>
                <option value="rounded_rectangle">rounded rectangle</option>
              </select>
            </label>
            <label class="row" title="Padding (mm) added on every side of the selection bbox to size the frame. Default is 3 × tool diameter; once you type a value it stays manual.">
              <span>Padding</span>
              <div class="num-cell">
                <input
                  type="number"
                  step="0.5"
                  min="0"
                  value={op.framePaddingMm ?? (opTool ? opTool.diameter * 3 : 9)}
                  onchange={(e) => {
                    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                    patch('framePaddingMm', isNaN(v) || v < 0 ? 0 : v);
                  }}
                />
                <span class="unit">mm</span>
              </div>
            </label>
            {#if op.frameShape === 'rounded_rectangle'}
              <label class="row" title="Corner radius (mm) for the rounded rectangle. Empty = same as padding.">
                <span>Corner radius</span>
                <div class="num-cell">
                  <input
                    type="number"
                    step="0.5"
                    min="0"
                    placeholder="same as padding"
                    value={op.frameCornerRadiusMm ?? ''}
                    onchange={(e) => {
                      const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                      patch('frameCornerRadiusMm', isNaN(v) || v < 0 ? undefined : v);
                    }}
                  />
                  <span class="unit">mm</span>
                </div>
              </label>
            {/if}
          </details>
        </fieldset>
      {/if}
      <fieldset>
        <legend>Pocket</legend>
        <label class="row">
          <span>Strategy</span>
          <select
            value={op.pocketStrategy ?? 'cascade'}
            onchange={(e) => {
              const v = (e.currentTarget as HTMLSelectElement).value as PocketStrategy;
              const patches: Partial<OpEntry> = { pocketStrategy: v };
              if (v === 'trochoidal') {
                if (op?.engagementAngleDeg === undefined) patches.engagementAngleDeg = 30;
                if (op?.loopRadiusFactor === undefined) patches.loopRadiusFactor = 0.6;
              }
              if (v === 'halfpipe' && op?.halfpipeProfile === undefined) {
                patches.halfpipeProfile = { kind: 'circular_arc', radius_mm: 5 };
              }
              if (op) project.updateOperation(op.id, patches);
            }}
          >
            <option value="cascade">cascade (concentric)</option>
            <option value="zigzag">zigzag (raster fill)</option>
            <option value="spiral">spiral</option>
            <option value="trochoidal">Trochoidal (load-limiting)</option>
            <option value="halfpipe">Halfpipe (slot, profiled floor)</option>
          </select>
        </label>
        {#if op.pocketStrategy === 'halfpipe'}
          <details class="subsection" open>
            <summary>Halfpipe</summary>
            <p class="hint" title="Halfpipe walks the slot's medial axis at varying Z so the cut floor matches the chosen profile. Tool kind: ball-nose for circular_arc, V-bit for v_bottom.">
              Slot floor profile.
            </p>
            <label class="row" title="Pipe profile: circular_arc gives a ball-bottom slot; v_bottom matches V-Carve.">
              <span>Profile</span>
              <select
                value={op.halfpipeProfile?.kind ?? 'circular_arc'}
                onchange={(e) => {
                  const v = (e.currentTarget as HTMLSelectElement).value;
                  if (v === 'circular_arc') {
                    patch('halfpipeProfile', { kind: 'circular_arc', radius_mm: op.halfpipeProfile?.kind === 'circular_arc' ? op.halfpipeProfile.radius_mm : 5 });
                  } else if (v === 'v_bottom') {
                    patch('halfpipeProfile', { kind: 'v_bottom', included_angle_deg: op.halfpipeProfile?.kind === 'v_bottom' ? op.halfpipeProfile.included_angle_deg : 60 });
                  }
                }}
              >
                <option value="circular_arc">circular arc (ball-bottom)</option>
                <option value="v_bottom">V-bottom</option>
              </select>
            </label>
            {#if op.halfpipeProfile?.kind === 'circular_arc'}
              <label class="row" title="Pipe radius in mm. Match this to the ball-nose cutter's radius for a true half-pipe.">
                <span>Radius</span>
                <div class="num-cell">
                  <input
                    type="number"
                    step="0.1"
                    min="0.1"
                    value={op.halfpipeProfile.radius_mm}
                    onchange={(e) => {
                      const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                      if (!isNaN(v) && v > 0) patch('halfpipeProfile', { kind: 'circular_arc', radius_mm: v });
                    }}
                  />
                  <span class="unit">mm</span>
                </div>
              </label>
            {/if}
            {#if op.halfpipeProfile?.kind === 'v_bottom'}
              <label class="row" title="V-bit included angle in degrees. Same semantics as the V-Carve tip angle.">
                <span>Included angle</span>
                <div class="num-cell">
                  <input
                    type="number"
                    step="1"
                    min="1"
                    max="179"
                    value={op.halfpipeProfile.included_angle_deg}
                    onchange={(e) => {
                      const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                      if (!isNaN(v) && v > 0) patch('halfpipeProfile', { kind: 'v_bottom', included_angle_deg: v });
                    }}
                  />
                  <span class="unit">°</span>
                </div>
              </label>
            {/if}
          </details>
        {/if}
        {#if op.pocketStrategy === 'trochoidal'}
          <details class="subsection" open>
            <summary>{$_('op.section.trochoidal')}</summary>
            <label
              class="row"
              title="Engagement arc angle in degrees. Lower = lighter cut, more loops; higher = aggressive. Drives centerline pitch."
            >
              <span>Engagement angle</span>
              <div class="range-cell">
                <span class="range-min">5°</span>
                <input
                  type="range"
                  min="5"
                  max="90"
                  step="1"
                  value={op.engagementAngleDeg ?? 30}
                  onchange={(e) => {
                    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                    if (!isNaN(v)) patch('engagementAngleDeg', Math.max(5, Math.min(90, v)));
                  }}
                />
                <span class="range-max">90°</span>
                <span class="num">{op.engagementAngleDeg ?? 30}°</span>
              </div>
            </label>
            <label
              class="row"
              title="Loop radius as a fraction of tool radius. 0.6 is a balanced default; 0.3 = tiny loops (very light), 1.0 = loops as large as the cutter."
            >
              <span>Loop radius factor</span>
              <div class="range-cell">
                <span class="range-min">0.3×</span>
                <input
                  type="range"
                  min="0.3"
                  max="1.0"
                  step="0.05"
                  value={op.loopRadiusFactor ?? 0.6}
                  onchange={(e) => {
                    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                    if (!isNaN(v)) patch('loopRadiusFactor', Math.max(0.3, Math.min(1.0, v)));
                  }}
                />
                <span class="range-max">1.0×</span>
                <span class="num">{(op.loopRadiusFactor ?? 0.6).toFixed(2)}×</span>
              </div>
            </label>
            {#if op.cutDirection === 'climb' || op.cutDirection === undefined || op.cutDirection === 'conventional'}
              {#if (op.cutDirection ?? 'conventional') === 'conventional'}
                <p class="hint warn">Trochoidal usually pairs with climb.</p>
              {/if}
            {/if}
            {#if op.plunge && op.plunge.kind !== 'helix'}
              <p class="hint warn">Trochoidal will override plunge to Helix.</p>
            {/if}
            {#if (op.tabPlacements && op.tabPlacements.length > 0) || (op.tabMode && op.tabMode.kind !== 'off')}
              <p class="hint warn">Tabs ignored on trochoidal pockets.</p>
            {/if}
          </details>
        {:else}
          <label
            class="row"
            title="XY overlap between consecutive pocket cuts. 0.5 = 50% overlap (step is half the tool diameter, the standard default). Higher = tighter cascade rings, cleaner fill on small pockets but slower; lower = bigger steps, faster but may leave stripes."
          >
            <span>XY overlap</span>
            <div class="num-cell">
              <input
                type="number"
                step="0.05"
                min="0.05"
                max="0.95"
                value={op.xyOverlap ?? 0.5}
                onchange={(e) => {
                  const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                  if (!isNaN(v))
                    patch('xyOverlap', Math.max(0.05, Math.min(0.95, v)));
                }}
              />
              <span class="unit">0–1</span>
            </div>
          </label>
        {/if}
      </fieldset>
    {/if}

    {#if op.kind === 'drill'}
      <fieldset>
        <legend>Drill cycle</legend>
        <label class="row">
          <span>Cycle</span>
          <select
            value={op.drillCycle?.kind ?? 'simple'}
            onchange={(e) => {
              const v = (e.currentTarget as HTMLSelectElement).value as
                | 'simple'
                | 'peck'
                | 'chip_break';
              const cur = op.drillCycle ?? ({ kind: 'simple', dwell_sec: 0 } as DrillCycle);
              const dwell = cur.dwell_sec ?? 0;
              const step =
                cur.kind === 'peck' || cur.kind === 'chip_break'
                  ? cur.peck_step_mm
                  : 1.0;
              if (v === 'simple') {
                patch('drillCycle', { kind: 'simple', dwell_sec: dwell } as DrillCycle);
              } else if (v === 'peck') {
                patch('drillCycle', {
                  kind: 'peck',
                  peck_step_mm: step,
                  dwell_sec: dwell,
                } as DrillCycle);
              } else {
                patch('drillCycle', {
                  kind: 'chip_break',
                  peck_step_mm: step,
                  dwell_sec: dwell,
                } as DrillCycle);
              }
            }}
          >
            <option value="simple" title="G81 — single plunge to depth, retract.">
              simple (G81)
            </option>
            <option
              value="peck"
              title="G83 — peck with full retract to clearance plane between pecks."
            >
              peck (G83)
            </option>
            <option
              value="chip_break"
              title="G73 — peck with chip-break (small partial retract between pecks)."
            >
              chip-break (G73)
            </option>
          </select>
        </label>
        {#if op.drillCycle && (op.drillCycle.kind === 'peck' || op.drillCycle.kind === 'chip_break')}
          <details class="subsection" open>
            <summary>{$_('op.section.drill_cycle')}</summary>
            <label class="row">
              <span>Peck step</span>
              <div class="num-cell">
                <input
                  type="number"
                  step="0.1"
                  min="0.1"
                  value={op.drillCycle.peck_step_mm}
                  onchange={(e) => {
                    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                    if (!isNaN(v) && v > 0 && op.drillCycle) {
                      const cur = op.drillCycle;
                      if (cur.kind === 'peck' || cur.kind === 'chip_break') {
                        patch('drillCycle', {
                          ...cur,
                          peck_step_mm: v,
                        } as DrillCycle);
                      }
                    }
                  }}
                />
                <span class="unit">mm</span>
              </div>
            </label>
          </details>
        {/if}
        <label class="row">
          <span>Dwell</span>
          <div class="num-cell">
            <input
              type="number"
              step="0.1"
              min="0"
              value={op.drillCycle?.dwell_sec ?? 0}
              onchange={(e) => {
                const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                if (!isNaN(v) && v >= 0) {
                  const cur = op.drillCycle ?? ({ kind: 'simple' } as DrillCycle);
                  patch('drillCycle', { ...cur, dwell_sec: v } as DrillCycle);
                }
              }}
            />
            <span class="unit">s</span>
          </div>
        </label>
        <label
          class="row"
          title="Stufenfase (rt1.20): after drilling each hole, the cutter walks a constant-Z revolution at the rim to break the edge. Depth is computed from the cutter's V-bit tip angle. Set Finish tool below to swap to a dedicated chamfer cutter (drill, then T<n> M6, then chamfer). Empty / 0 = no countersink."
        >
          <span>Chamfer width</span>
          <div class="num-cell">
            <input
              type="number"
              step="0.1"
              min="0"
              placeholder="0"
              value={op.chamferAfterWidthMm ?? ''}
              onchange={(e) => {
                const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                patch('chamferAfterWidthMm', isNaN(v) || v <= 0 ? undefined : v);
              }}
            />
            <span class="unit">mm</span>
          </div>
        </label>
      </fieldset>
    {/if}

    {#if op.kind === 'profile' || op.kind === 'pocket' || op.kind === 'engrave' || op.kind === 'drag_knife'}
      <fieldset>
        <legend>Feeds (overrides)</legend>
        <label class="row" title="Override the tool's feed rate (mm/min) for this operation only. Leave empty to use the tool default.">
          <span>Feed rate</span>
          <div class="num-cell">
            <input
              type="number"
              step="50"
              min="0"
              placeholder="tool default"
              value={op.feedRateOverride ?? ''}
              onchange={(e) => {
                const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
                patch('feedRateOverride', isNaN(v) || v <= 0 ? undefined : v);
              }}
            />
            <span class="unit">mm/min</span>
          </div>
        </label>
        <label class="row" title="Override the tool's plunge rate (mm/min) for Z descents in this operation. Leave empty to use the tool default.">
          <span>Plunge rate</span>
          <div class="num-cell">
            <input
              type="number"
              step="10"
              min="0"
              placeholder="tool default"
              value={op.plungeRateOverride ?? ''}
              onchange={(e) => {
                const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
                patch('plungeRateOverride', isNaN(v) || v <= 0 ? undefined : v);
              }}
            />
            <span class="unit">mm/min</span>
          </div>
        </label>
        <label class="row" title="Slow the feed at sharp Line→Line corners by this fraction. 0 = no reduction (default). 0.5 = half feed at corners. Most useful for zigzag pocket fills with their many 180° turns.">
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
            <span class="unit">0–1</span>
          </div>
        </label>
      </fieldset>
    {/if}

    {#if op.kind === 'vcarve'}
      {@const opTool = project.tools.find((tt) => tt.id === op.toolId)}
      <fieldset>
        <legend>V-Carve</legend>
        {#if opTool && opTool.kind !== 'v_bit'}
          <p class="warn-chip" title="V-Carve assumes a V-bit cone — pick a V-bit in the tool library or the carve depth math won't match the actual cutter.">
            Tool kind mismatch — V-Carve needs a V-bit.
          </p>
        {/if}
        <details class="subsection" open>
          <summary>{$_('op.section.vcarve_advanced')}</summary>
          <label
            class="row"
            title="Optional cap on the inscribed-circle radius (mm). Leave empty for no cap. Useful when a wide region would otherwise drive the V deeper than the bit's usable shoulder."
          >
            <span>Max width</span>
            <div class="num-cell">
              <input
                type="number"
                step="0.1"
                min="0"
                placeholder="no cap"
                value={op.carveMaxWidthMm ?? ''}
                onchange={(e) => {
                  const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                  patch('carveMaxWidthMm', isNaN(v) || v <= 0 ? undefined : v);
                }}
              />
              <span class="unit">mm</span>
            </div>
          </label>
          <label
            class="row"
            title="When on, run a refinement pass that re-cuts only the points whose first pass fell short of the geometric target depth. Off by default."
          >
            <span>Refine pass</span>
            <input
              type="checkbox"
              checked={op.multiPassRefine ?? false}
              onchange={(e) => patch('multiPassRefine', (e.currentTarget as HTMLInputElement).checked)}
            />
          </label>
        </details>
      </fieldset>
    {/if}

    {#if op.kind === 'chamfer'}
      {@const opTool = project.tools.find((tt) => tt.id === op.toolId)}
      <fieldset>
        <legend>Chamfer</legend>
        {#if opTool && opTool.kind !== 'v_bit'}
          <p class="warn-chip" title="Chamfer assumes a V-bit cone; flat / ball tools won't produce a true bevel. Pick a V-bit in the tool library.">
            Tool kind mismatch — Chamfer needs a V-bit.
          </p>
        {/if}
        <label
          class="row"
          title="Horizontal width of the chamfer cut on the workpiece. The Z depth is computed automatically from the V-bit's apex angle: depth = -width / tan(tipAngle/2). Default 1 mm."
        >
          <span>Width</span>
          <div class="num-cell">
            <input
              type="number"
              step="0.1"
              min="0"
              placeholder="1"
              value={op.chamferWidthMm ?? ''}
              onchange={(e) => {
                const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                patch('chamferWidthMm', isNaN(v) || v <= 0 ? undefined : v);
              }}
            />
            <span class="unit">mm</span>
          </div>
        </label>
        <label
          class="row"
          title="Cut the chamfer twice — once at the rough feed (cleanup) and once at the tool's finish-set feed (rt1.27) for surface quality."
        >
          <span>Finish pass</span>
          <input
            type="checkbox"
            checked={op.chamferFinishPass ?? false}
            onchange={(e) => patch('chamferFinishPass', (e.currentTarget as HTMLInputElement).checked)}
          />
        </label>
      </fieldset>
    {/if}

    {#if op.kind === 'thread'}
      <fieldset>
        <legend>Thread</legend>
        <p class="hint" title="Source must be a closed circle (drilled hole or stud diameter). The cutter walks a helix at one pitch of Z descent per revolution between Start depth and Depth.">
          Thread requires a closed circle as the source.
        </p>
        <label
          class="row"
          title="Z descent per full revolution. Picks the thread series: M6×1.0 → 1.0 mm, M3×0.5 → 0.5 mm. Positive value."
        >
          <span>Pitch</span>
          <div class="num-cell">
            <input
              type="number"
              step="0.05"
              min="0"
              placeholder="1"
              value={op.threadPitchMm ?? ''}
              onchange={(e) => {
                const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                patch('threadPitchMm', isNaN(v) || v <= 0 ? undefined : v);
              }}
            />
            <span class="unit">mm</span>
          </div>
        </label>
        <label
          class="row"
          title="Internal = tap-style (cutter inside the bore). External = die-style (cutter around a stud)."
        >
          <span>Direction</span>
          <select
            value={(op.threadInternal ?? true) ? 'internal' : 'external'}
            onchange={(e) => {
              const v = (e.currentTarget as HTMLSelectElement).value;
              patch('threadInternal', v === 'internal');
            }}
          >
            <option value="internal">Internal (bore)</option>
            <option value="external">External (stud)</option>
          </select>
        </label>
        <label
          class="row"
          title="Climb (CCW helix on a right-hand spindle) vs conventional (CW). Default off (conventional) — almost always best for surface quality on hobby machines."
        >
          <span>Climb</span>
          <input
            type="checkbox"
            checked={op.threadClimb ?? false}
            onchange={(e) => patch('threadClimb', (e.currentTarget as HTMLInputElement).checked)}
          />
        </label>
      </fieldset>
    {/if}

    {#if op.kind === 'helix'}
      <p class="empty">
        Helical entry isn't supported as a standalone operation yet. For
        helical plunge into a pocket, use a Pocket op and set
        <strong>Plunge → Helix</strong> in the Cut section.
      </p>
    {/if}
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
  h3 {
    margin: 0 0 0.4rem 0;
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
  }
  .empty {
    color: var(--text-faint);
    font-size: 0.78rem;
  }
  .empty.embedded-empty {
    text-align: center;
    font-size: 0.72rem;
    opacity: 0.7;
    margin: 0.4rem 0;
  }
  .placeholder {
    color: var(--text-faint);
    font-size: 0.78rem;
    font-style: italic;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 6.5rem) minmax(0, 1fr);
    gap: 0.5rem;
    align-items: center;
    margin: 0.2rem 0;
    font-size: 0.78rem;
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: 3px;
    margin: 0.4rem 0;
    padding: 0.3rem 0.5rem 0.4rem;
    background: var(--bg-elevated);
  }
  legend {
    font-size: 0.7rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0 0.3rem;
  }
  input,
  select {
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
  .hint {
    margin: 0.2rem 0 0;
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .hint.warn {
    color: var(--warn, #b86f00);
    background: var(--warn-bg, rgba(184, 111, 0, 0.08));
    border-left: 2px solid var(--warn, #b86f00);
    padding: 0.15rem 0.4rem;
    border-radius: 2px;
  }
  .num {
    font-variant-numeric: tabular-nums;
    font-size: 0.78rem;
    color: var(--text-muted);
    min-width: 3em;
    text-align: right;
  }
  .from-selection {
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
  .from-selection:disabled {
    cursor: not-allowed;
  }
  .from-selection.ghost {
    opacity: 0.5;
    border-style: dashed;
  }
  .warn-chip {
    margin: 0.2rem 0;
    padding: 0.2rem 0.4rem;
    border-radius: 3px;
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
    border: 1px solid var(--warn);
    font-size: 0.72rem;
  }
  .step-cell {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    min-width: 0;
  }
  .step-cell input {
    flex: 1 1 auto;
    min-width: 0;
  }
  .num-cell {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    min-width: 0;
  }
  .num-cell input {
    flex: 1 1 auto;
    min-width: 0;
  }
  .num-cell-pair input {
    flex: 1 1 0;
    min-width: 0;
  }
  .segmented {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 3px;
    overflow: hidden;
    background: var(--bg-elevated);
  }
  .segmented button {
    background: transparent;
    color: var(--text);
    border: 0;
    border-left: 1px solid var(--border);
    padding: 0.2rem 0.5rem;
    font-size: 0.7rem;
    text-transform: capitalize;
    cursor: pointer;
  }
  .segmented button:first-child {
    border-left: 0;
  }
  .segmented button.active {
    background: color-mix(in srgb, var(--accent) 30%, transparent);
    color: var(--text-strong);
  }
  .segmented button:hover:not(.active) {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .unit {
    font-size: 0.7rem;
    color: var(--text-muted);
    margin-left: 0.25rem;
    white-space: nowrap;
    flex: 0 0 auto;
  }
  .range-cell {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    min-width: 0;
  }
  .range-cell input[type='range'] {
    flex: 1 1 auto;
    min-width: 0;
    padding: 0;
  }
  .range-min,
  .range-max {
    font-size: 0.68rem;
    color: var(--text-faint);
    flex: 0 0 auto;
    white-space: nowrap;
  }
  .tool-cell {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    min-width: 0;
  }
  .tool-cell select {
    flex: 1 1 auto;
    min-width: 0;
  }
  .tool-edit {
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
  .tool-edit:hover {
    color: var(--accent-strong);
    border-color: var(--accent);
  }
  input.inherit::placeholder {
    font-style: italic;
    color: var(--text-faint);
  }
  input.invalid {
    border-color: var(--danger, #c44);
  }
  .reset-link {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    font-size: 0.7rem;
    text-decoration: underline;
    cursor: pointer;
    padding: 0;
    white-space: nowrap;
  }
  .step-error {
    margin: 0.1rem 0 0.2rem;
    padding: 0.15rem 0.4rem;
    background: color-mix(in srgb, var(--danger, #c44) 18%, transparent);
    color: var(--danger, #c44);
    border: 1px solid var(--danger, #c44);
    border-radius: 3px;
    font-size: 0.72rem;
    width: max-content;
  }
  .subsection {
    margin: 0.3rem 0 0.1rem;
    border-top: 1px solid var(--border);
    padding-top: 0.2rem;
  }
  .subsection > summary {
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
  .subsection > summary::-webkit-details-marker {
    display: none;
  }
  .subsection > summary::before {
    content: '▸';
    font-size: 0.6rem;
    transition: transform 0.12s ease;
    color: var(--text-faint);
  }
  .subsection[open] > summary::before {
    transform: rotate(90deg);
  }
</style>
