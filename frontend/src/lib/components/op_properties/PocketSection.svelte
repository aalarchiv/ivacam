<script lang="ts">
  /// Pocket op-properties: frame (Pocket Outside) controls + strategy
  /// picker + per-strategy subsections (halfpipe profile, trochoidal
  /// engagement, xy_overlap). Shown only when op.kind === 'pocket'.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import {
    project,
    type FrameShape,
    type OpEntry,
    type OpField,
    type OpFieldValue,
    type PocketOp,
    type PocketStrategy,
  } from '../../state/project.svelte';

  interface Props {
    op: PocketOp;
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();

  let opTool = $derived(project.tools.find((tt) => tt.id === op.toolId));
</script>

{#if op.frameShape == null}
  <fieldset>
    <legend>Pocket Outside</legend>
    <p class="hint">
      Convert this Pocket into a Pocket Outside operation: the pipeline auto-derives a frame around
      the selection at generate time and carves the area BETWEEN the frame and the selection.
    </p>
    <button
      type="button"
      class="reset-link"
      title="Convert this op to a Pocket Outside by attaching a synthetic frame around its source selection."
      onclick={() => {
        const diameter = opTool ? opTool.diameter : 3;
        patch('frameShape', 'rectangle');
        patch('framePaddingMm', 3 * diameter);
        patch('sourceCombine', 'difference');
      }}>Convert to Pocket Outside →</button
    >
  </fieldset>
{/if}
{#if op.frameShape != null}
  <fieldset>
    <legend>Frame</legend>
    <details class="subsection" open>
      <summary>Frame</summary>
      <label
        class="row"
        title="Shape of the synthetic frame the pipeline derives from your selection at generate time."
      >
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
      <label
        class="row"
        title="Padding (mm) added on every side of the selection bbox to size the frame. Default is 3 × tool diameter; once you type a value it stays manual."
      >
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
        <label
          class="row"
          title="Corner radius (mm) for the rounded rectangle. Empty = same as padding."
        >
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
      <p
        class="hint"
        title="Halfpipe walks the slot's medial axis at varying Z so the cut floor matches the chosen profile. Tool kind: ball-nose for circular_arc, V-bit for v_bottom."
      >
        Slot floor profile.
      </p>
      <label
        class="row"
        title="Pipe profile: circular_arc gives a ball-bottom slot; v_bottom matches V-Carve."
      >
        <span>Profile</span>
        <select
          value={op.halfpipeProfile?.kind ?? 'circular_arc'}
          onchange={(e) => {
            const v = (e.currentTarget as HTMLSelectElement).value;
            if (v === 'circular_arc') {
              patch('halfpipeProfile', {
                kind: 'circular_arc',
                radius_mm:
                  op.halfpipeProfile?.kind === 'circular_arc' ? op.halfpipeProfile.radius_mm : 5,
              });
            } else if (v === 'v_bottom') {
              patch('halfpipeProfile', {
                kind: 'v_bottom',
                included_angle_deg:
                  op.halfpipeProfile?.kind === 'v_bottom'
                    ? op.halfpipeProfile.included_angle_deg
                    : 60,
              });
            }
          }}
        >
          <option value="circular_arc">circular arc (ball-bottom)</option>
          <option value="v_bottom">V-bottom</option>
        </select>
      </label>
      {#if op.halfpipeProfile?.kind === 'circular_arc'}
        <label
          class="row"
          title="Pipe radius in mm. Match this to the ball-nose cutter's radius for a true half-pipe."
        >
          <span>Radius</span>
          <div class="num-cell">
            <input
              type="number"
              step="0.1"
              min="0.1"
              value={op.halfpipeProfile.radius_mm}
              onchange={(e) => {
                const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                if (!isNaN(v) && v > 0)
                  patch('halfpipeProfile', { kind: 'circular_arc', radius_mm: v });
              }}
            />
            <span class="unit">mm</span>
          </div>
        </label>
      {/if}
      {#if op.halfpipeProfile?.kind === 'v_bottom'}
        <label
          class="row"
          title="V-bit included angle in degrees. Same semantics as the V-Carve tip angle."
        >
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
                // Match the HTML min/max guards (1..179) — the prior
                // `v > 0` accepted 200° which produced a degenerate
                // V-bit profile with no warning.
                if (!isNaN(v) && v >= 1 && v <= 179)
                  patch('halfpipeProfile', { kind: 'v_bottom', included_angle_deg: v });
              }}
            />
            <span class="unit">°</span>
          </div>
        </label>
      {/if}
    </details>
  {/if}
  {#if op.pocketStrategy === 'zigzag'}
    <details class="subsection" open>
      <summary>Zigzag direction</summary>
      <label
        class="row"
        title="rt1.9: rotates the raster sweep. 0° = horizontal sweeps (default); 90° = vertical; 45° = diagonal. Long-thin pockets cut faster + cleaner when the sweep runs ALONG the long axis."
      >
        <span>Angle</span>
        <div class="range-cell">
          <span class="range-min">0°</span>
          <input
            type="range"
            min="0"
            max="180"
            step="5"
            value={op.pocketZigzagAngleDeg ?? 0}
            onchange={(e) => {
              const v = parseFloat((e.currentTarget as HTMLInputElement).value);
              if (!isNaN(v)) {
                patch('pocketZigzagAngleDeg', v === 0 ? undefined : Math.max(0, Math.min(180, v)));
              }
            }}
          />
          <span class="range-max">180°</span>
          <span class="num">{op.pocketZigzagAngleDeg ?? 0}°</span>
        </div>
      </label>
      <div class="quick-row">
        {#each [0, 45, 90, 135] as a (a)}
          <button
            type="button"
            class="quick-btn"
            class:active={(op.pocketZigzagAngleDeg ?? 0) === a}
            onclick={() => patch('pocketZigzagAngleDeg', a === 0 ? undefined : a)}
            title={`Set raster angle to ${a}°`}
          >
            {a}°
          </button>
        {/each}
      </div>
    </details>
  {/if}
  {#if op.pocketStrategy === 'trochoidal'}
    <details class="subsection" open>
      <summary>Trochoidal</summary>
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
    {@const toolDefault = opTool?.defaultXyOverlap}
    {@const inheritedOverlap = toolDefault ?? 0.5}
    {@const opOverlap = op.xyOverlap}
    <label
      class="row"
      title={opOverlap === undefined
        ? `XY overlap between consecutive pocket cuts. Empty = inherit from the tool (${toolDefault !== undefined ? `${toolDefault} from "${opTool?.name}"` : '0.5 global default'}). 0.5 = 50 % overlap (step is half the tool diameter). Higher = tighter cascade rings, cleaner fill but slower; lower = bigger steps, faster but may leave stripes.`
        : `XY overlap between consecutive pocket cuts. 0.5 = 50 % overlap (step is half the tool diameter). Higher = tighter cascade rings, cleaner fill but slower; lower = bigger steps, faster but may leave stripes. Clear the field to inherit ${toolDefault !== undefined ? `${toolDefault} from tool "${opTool?.name}"` : 'the 0.5 global default'}.`}
    >
      <span>XY overlap</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.05"
          min="0.05"
          max="0.95"
          value={opOverlap ?? ''}
          placeholder={String(inheritedOverlap)}
          class:inherit-italic={opOverlap === undefined}
          onchange={(e) => {
            const raw = (e.currentTarget as HTMLInputElement).value;
            if (raw === '') {
              patch('xyOverlap', undefined);
              return;
            }
            const v = parseFloat(raw);
            if (!isNaN(v)) patch('xyOverlap', Math.max(0.05, Math.min(0.95, v)));
          }}
        />
        <span class="unit" title="Unitless fraction between 0 and 1.">fraction</span>
      </div>
    </label>
  {/if}
</fieldset>

<style>
  /* dr5: italic styling for the XY overlap input when it's empty and
     inheriting from the tool's defaultXyOverlap. Reads as "this is a
     computed default, not a user-typed value". */
  input.inherit-italic::placeholder {
    font-style: italic;
    opacity: 0.75;
  }
  /* rt1.9: quick-pick angle buttons for the zigzag direction. Match
     the existing chip styles so they sit naturally below the slider. */
  .quick-row {
    display: inline-flex;
    gap: 0.25rem;
    margin-top: 0.25rem;
  }
  .quick-btn {
    background: var(--bg-elevated);
    color: var(--text-muted);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.1rem 0.5rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .quick-btn:hover {
    color: var(--text);
  }
  .quick-btn.active {
    background: color-mix(in srgb, var(--accent) 25%, var(--bg-elevated));
    color: var(--text-strong);
    border-color: color-mix(in srgb, var(--accent) 60%, var(--border));
  }
</style>
