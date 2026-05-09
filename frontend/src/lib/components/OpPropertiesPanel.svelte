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
  } from '../state/project.svelte';

  /// One-line description per cut direction for the option titles.
  const CUT_DIR_HELP: Record<CutDirection, string> = {
    conventional: 'Cutter rotation OPPOSES feed at contact. Safer on machines with backlash; chip starts thin.',
    climb: 'Cutter rotation matches feed at contact. Better surface finish; needs a stiff machine.',
  };
  const PLUNGE_HELP: Record<'direct' | 'ramp' | 'helix', string> = {
    direct:
      'Straight Z plunge into material. Safe for center-cutting end mills on shallow steps; risky on harder materials.',
    ramp:
      'Ramped descent: cutter walks forward along the path while Z descends, taking a chip in both directions. Required for non-center-cutting bits.',
    helix:
      'Helical entry: cutter spirals down on a small circle inside the closed pocket boundary, then walks to the path start. Standard for non-center-cutting endmills and harder materials. Falls back to ramp on open paths or when the helix circle does not fit.',
  };
  const TAB_TYPE_HELP: Record<'rectangle' | 'ramp', string> = {
    rectangle:
      'Straight Z lift over each tab: cutter pops up to tab height, runs across, drops back down. Simple and fast.',
    ramp:
      'Sloped entry/exit: cutter ramps UP at the configured angle, holds tab height for the flat top, then ramps DOWN. Smoother machine motion and cleaner tab transitions.',
  };

  /// Tooltip blurb per combine mode — kept short so it fits in a native
  /// option's `title` attribute (most browsers cut after ~2 lines).
  const COMBINE_HELP: Record<SourceCombine, string> = {
    auto: 'Containment-aware: nested closed objects become holes (outer + inner = annulus). Default.',
    union: 'Boolean union of all selected closed contours.',
    difference: 'First selected minus the union of the rest.',
    intersection: 'Boolean intersection of all selected closed contours.',
    xor: 'Symmetric difference (xor) of all selected closed contours.',
    none: 'No combination — one boundary per selected object, no holes.',
  };

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
</script>

<aside class="props" class:embedded>
  {#if !embedded}
    <h3>Properties</h3>
  {/if}

  {#if !op}
    {#if !embedded}
      <p class="empty">Select an operation in the list to edit it.</p>
    {/if}
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
      <select
        value={op.toolId}
        onchange={(e) => patch('toolId', parseInt((e.currentTarget as HTMLSelectElement).value, 10))}
      >
        {#each project.tools as t (t.id)}
          <option value={t.id}>#{t.id} {t.name} ({t.diameter}mm)</option>
        {/each}
      </select>
    </label>

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
        <label class="row" title={COMBINE_HELP[op.sourceCombine ?? 'auto']}>
          <span>Combine</span>
          <select
            value={op.sourceCombine ?? 'auto'}
            onchange={(e) =>
              patch('sourceCombine', (e.currentTarget as HTMLSelectElement).value as SourceCombine)}
          >
            <option value="auto" title={COMBINE_HELP.auto}>auto (containment)</option>
            <option value="union" title={COMBINE_HELP.union}>union</option>
            <option value="difference" title={COMBINE_HELP.difference}>difference</option>
            <option value="intersection" title={COMBINE_HELP.intersection}>intersection</option>
            <option value="xor" title={COMBINE_HELP.xor}>xor</option>
            <option value="none" title={COMBINE_HELP.none}>none (per object)</option>
          </select>
        </label>
      {/if}
      <button
        class="from-selection"
        type="button"
        disabled={project.selectedObjects.size === 0}
        onclick={() => {
          patch('sourceLayers', null);
          patch('sourceObjects', [...project.selectedObjects]);
        }}
        title="Use the chains currently highlighted in the 2D pane"
      >Set from current selection ({project.selectedObjects.size})</button>
    </fieldset>

    <fieldset>
      <legend>Cut</legend>
      <label class="row">
        <span>Final depth</span>
        <input
          type="number" step="0.1" value={op.depth}
          onchange={(e) => patch('depth', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
        />
      </label>
      <label class="row">
        <span>Start depth</span>
        <input
          type="number" step="0.1" value={op.startDepth}
          onchange={(e) => patch('startDepth', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
        />
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
      </label>
      <label
        class="row"
        title="Cut past the nominal depth by this many mm. Useful for through-cuts on edge-clamped sheet so the cutter clears the bottom. 0 = no extension."
      >
        <span>Through depth</span>
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
      </label>
      <label
        class="row"
        title="Explicit comma-separated list of Z depths (negative numbers, e.g. -0.5, -1.5, -3). When non-empty, overrides Step / Finish step / Through depth. Empty = use the step-down loop."
      >
        <span>Depth list</span>
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
      </label>
      {#if op.kind === 'profile' || op.kind === 'pocket'}
        <label class="row" title={CUT_DIR_HELP[op.cutDirection ?? 'conventional']}>
          <span>Direction</span>
          <select
            value={op.cutDirection ?? 'conventional'}
            onchange={(e) =>
              patch('cutDirection', (e.currentTarget as HTMLSelectElement).value as CutDirection)}
          >
            <option value="conventional" title={CUT_DIR_HELP.conventional}>conventional</option>
            <option value="climb" title={CUT_DIR_HELP.climb}>climb</option>
          </select>
        </label>
        <label class="row" title={CUT_DIR_HELP[op.finishCutDirection ?? 'conventional']}>
          <span>Finish dir</span>
          <select
            value={op.finishCutDirection ?? 'conventional'}
            onchange={(e) =>
              patch('finishCutDirection', (e.currentTarget as HTMLSelectElement).value as CutDirection)}
          >
            <option value="conventional" title={CUT_DIR_HELP.conventional}>conventional</option>
            <option value="climb" title={CUT_DIR_HELP.climb}>climb</option>
          </select>
        </label>
        <label class="row" title={PLUNGE_HELP[op.plunge?.kind ?? 'direct']}>
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
                // Pick a sane default helix radius from the selected
                // tool's diameter (1.5 × tool radius). Falls back to
                // 3mm if the tool can't be resolved.
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
            <option value="direct" title={PLUNGE_HELP.direct}>direct</option>
            <option value="ramp" title={PLUNGE_HELP.ramp}>ramp</option>
            <option value="helix" title={PLUNGE_HELP.helix}>helix</option>
          </select>
        </label>
        {#if op.plunge && op.plunge.kind === 'ramp'}
          <label class="row" title="Ramp angle in degrees. 1°–5° is gentle, 10°+ is aggressive. The ramp's horizontal length is step / tan(angle).">
            <span>Ramp angle</span>
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
          </label>
        {:else if op.plunge && op.plunge.kind === 'helix'}
          <label class="row" title="Helix descent angle in degrees. 1°–5° is gentle, 10°+ is aggressive. Each revolution drops Z by 2π·radius·tan(angle).">
            <span>Helix angle</span>
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
          </label>
          <label class="row" title="Helix radius in mm. Should be ≥ tool radius; sane default is 1.5 × tool radius. Larger = more clearance, more material removed by the spiral.">
            <span>Helix radius</span>
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
          </label>
        {/if}
      {/if}
    </fieldset>

    {#if op.kind === 'profile' || op.kind === 'pocket'}
      <fieldset>
        <legend>Tabs</legend>
        <label class="row" title={TAB_TYPE_HELP[op.tabType ?? 'rectangle']}>
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
            <option value="rectangle" title={TAB_TYPE_HELP.rectangle}>rectangle</option>
            <option value="ramp" title={TAB_TYPE_HELP.ramp}>ramp</option>
          </select>
        </label>
        {#if op.tabType === 'ramp'}
          <label
            class="row"
            title="Ramp angle in degrees. 30° (default) gives a 1:√3 slope. Smaller = gentler, longer ramps; larger = steeper, more like a Rectangle tab. Horizontal ramp length = tabs.height / tan(angle)."
          >
            <span>Ramp angle</span>
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
          </label>
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
            <span>{op.leadInKind === 'arc' ? 'Radius' : 'Length'} (mm)</span>
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
            <span>{op.leadOutKind === 'arc' ? 'Radius' : 'Length'} (mm)</span>
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
          </label>
        {/if}
      </fieldset>
    {:else if op.kind === 'pocket'}
      <fieldset>
        <legend>Pocket</legend>
        <label class="row">
          <span>Strategy</span>
          <select
            value={op.pocketStrategy ?? 'cascade'}
            onchange={(e) => patch('pocketStrategy', (e.currentTarget as HTMLSelectElement).value as PocketStrategy)}
          >
            <option value="cascade">cascade (concentric)</option>
            <option value="zigzag">zigzag (raster fill)</option>
            <option value="spiral">spiral</option>
          </select>
        </label>
        <label
          class="row"
          title="XY overlap between consecutive pocket cuts. 0.5 = 50% overlap (step is half the tool diameter, the standard default). Higher = tighter cascade rings, cleaner fill on small pockets but slower; lower = bigger steps, faster but may leave stripes."
        >
          <span>XY overlap</span>
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
        </label>
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
          <label class="row">
            <span>Peck step (mm)</span>
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
          </label>
        {/if}
        <label class="row">
          <span>Dwell (s)</span>
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
        </label>
      </fieldset>
    {/if}

    {#if op.kind === 'profile' || op.kind === 'pocket' || op.kind === 'engrave' || op.kind === 'drag_knife'}
      <fieldset>
        <legend>Feeds (overrides)</legend>
        <label class="row" title="Override the tool's feed rate (mm/min) for this op only. Leave empty to use the tool default.">
          <span>Feed rate</span>
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
        </label>
        <label class="row" title="Override the tool's plunge rate (mm/min) for Z descents in this op. Leave empty to use the tool default.">
          <span>Plunge rate</span>
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
        </label>
        <label class="row" title="Slow the feed at sharp Line→Line corners by this fraction. 0 = no reduction (default). 0.5 = half feed at corners. Most useful for zigzag pocket fills with their many 180° turns.">
          <span>Corner slow</span>
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
        </label>
      </fieldset>
    {/if}

    {#if op.kind === 'thread' || op.kind === 'chamfer' || op.kind === 'helix'}
      <p class="empty">
        This operation kind is parsed but the gcode emitter for it ships
        with the next backend slice; the run will return
        <code>UnimplementedKind</code> for now.
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
  code {
    background: var(--bg-input);
    padding: 0 0.2rem;
    border-radius: 2px;
  }
  .hint {
    margin: 0.2rem 0 0;
    font-size: 0.72rem;
    color: var(--text-muted);
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
    opacity: 0.45;
    cursor: not-allowed;
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
</style>
