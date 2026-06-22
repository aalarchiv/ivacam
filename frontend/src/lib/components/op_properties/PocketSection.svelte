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
  import { t } from '../../i18n';

  interface Props {
    op: PocketOp;
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();

  let opTool = $derived(project.data.tools.find((tt) => tt.id === op.toolId));
</script>

{#if op.frameShape == null}
  <fieldset>
    <legend>{t('ops.pocket.outside.legend')}</legend>
    <p class="hint">
      {t('ops.pocket.outside.hint')}
    </p>
    <button
      type="button"
      class="reset-link"
      title={t('ops.pocket.outside.help')}
      onclick={() => {
        const diameter = opTool ? opTool.diameter : 3;
        patch('frameShape', 'rectangle');
        patch('framePaddingMm', 3 * diameter);
        patch('sourceCombine', 'difference');
      }}>{t('ops.pocket.outside.convert')}</button
    >
  </fieldset>
{/if}
{#if op.frameShape != null}
  <fieldset>
    <legend>{t('ops.pocket.frame.legend')}</legend>
    <details class="subsection" open>
      <summary>{t('ops.pocket.frame.summary')}</summary>
      <label class="row" title={t('ops.pocket.frame_shape.help')}>
        <span>{t('ops.pocket.frame_shape.label')}</span>
        <select
          value={op.frameShape}
          onchange={(e) =>
            patch('frameShape', (e.currentTarget as HTMLSelectElement).value as FrameShape)}
        >
          <option value="rectangle">{t('ops.pocket.frame.shape.rectangle')}</option>
          <option value="rounded_rectangle">{t('ops.pocket.frame.shape.rounded_rectangle')}</option>
        </select>
      </label>
      <label class="row" title={t('ops.pocket.frame_padding.help')}>
        <span>{t('ops.pocket.frame_padding.label')}</span>
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
        <label class="row" title={t('ops.pocket.frame_corner_radius.help')}>
          <span>{t('ops.pocket.frame_corner_radius.label')}</span>
          <div class="num-cell">
            <input
              type="number"
              step="0.5"
              min="0"
              placeholder={t('ops.pocket.frame_corner_radius.placeholder')}
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
  <legend>{t('ops.pocket.legend')}</legend>
  <label class="row">
    <span>{t('ops.pocket.strategy.label')}</span>
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
          patches.halfpipeProfile = { kind: 'circular_arc', radiusMm: 5 };
        }
        if (op) project.updateOperation(op.id, patches);
      }}
    >
      <option value="cascade">{t('ops.pocket.strategy.cascade')}</option>
      <option value="zigzag">{t('ops.pocket.strategy.zigzag')}</option>
      <option value="spiral">{t('ops.pocket.strategy.spiral')}</option>
      <option value="trochoidal">{t('ops.pocket.strategy.trochoidal')}</option>
      <option value="halfpipe">{t('ops.pocket.strategy.halfpipe')}</option>
    </select>
  </label>
  {#if op.pocketStrategy === 'halfpipe'}
    <details class="subsection" open>
      <summary>{t('ops.pocket.halfpipe.summary')}</summary>
      <p class="hint" title={t('ops.pocket.halfpipe_profile.help')}>
        {t('ops.pocket.halfpipe_profile.hint')}
      </p>
      <label class="row" title={t('ops.pocket.profile.help')}>
        <span>{t('ops.pocket.profile.label')}</span>
        <select
          value={op.halfpipeProfile?.kind ?? 'circular_arc'}
          onchange={(e) => {
            const v = (e.currentTarget as HTMLSelectElement).value;
            if (v === 'circular_arc') {
              patch('halfpipeProfile', {
                kind: 'circular_arc',
                radiusMm:
                  op.halfpipeProfile?.kind === 'circular_arc' ? op.halfpipeProfile.radiusMm : 5,
              });
            } else if (v === 'v_bottom') {
              patch('halfpipeProfile', {
                kind: 'v_bottom',
                includedAngleDeg:
                  op.halfpipeProfile?.kind === 'v_bottom'
                    ? op.halfpipeProfile.includedAngleDeg
                    : 60,
              });
            }
          }}
        >
          <option value="circular_arc">{t('ops.pocket.halfpipe.profile.circular_arc')}</option>
          <option value="v_bottom">{t('ops.pocket.halfpipe.profile.v_bottom')}</option>
        </select>
      </label>
      {#if op.halfpipeProfile?.kind === 'circular_arc'}
        <label class="row" title={t('ops.pocket.radius.help')}>
          <span>{t('ops.pocket.radius.label')}</span>
          <div class="num-cell">
            <input
              type="number"
              step="0.1"
              min="0.1"
              value={op.halfpipeProfile.radiusMm}
              onchange={(e) => {
                const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                if (!isNaN(v) && v > 0)
                  patch('halfpipeProfile', { kind: 'circular_arc', radiusMm: v });
              }}
            />
            <span class="unit">mm</span>
          </div>
        </label>
      {/if}
      {#if op.halfpipeProfile?.kind === 'v_bottom'}
        <label class="row" title={t('ops.pocket.included_angle.help')}>
          <span>{t('ops.pocket.included_angle.label')}</span>
          <div class="num-cell">
            <input
              type="number"
              step="1"
              min="1"
              max="179"
              value={op.halfpipeProfile.includedAngleDeg}
              onchange={(e) => {
                const v = parseFloat((e.currentTarget as HTMLInputElement).value);
                // Match the HTML min/max guards (1..179) — the prior
                // `v > 0` accepted 200° which produced a degenerate
                // V-bit profile with no warning.
                if (!isNaN(v) && v >= 1 && v <= 179)
                  patch('halfpipeProfile', { kind: 'v_bottom', includedAngleDeg: v });
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
      <summary>{t('ops.pocket.zigzag.summary')}</summary>
      <label class="row" title={t('ops.pocket.zigzag_angle.help')}>
        <span>{t('ops.pocket.zigzag_angle.label')}</span>
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
            title={t('ops.pocket.zigzag_angle_quick.help', { angle: a })}
          >
            {a}°
          </button>
        {/each}
      </div>
    </details>
  {/if}
  {#if op.pocketStrategy === 'trochoidal'}
    <details class="subsection" open>
      <summary>{t('ops.pocket.trochoidal.summary')}</summary>
      <label class="row" title={t('ops.pocket.engagement_angle.help')}>
        <span>{t('ops.pocket.engagement_angle.label')}</span>
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
      <label class="row" title={t('ops.pocket.loop_radius_factor.help')}>
        <span>{t('ops.pocket.loop_radius_factor.label')}</span>
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
          <p class="hint warn">{t('ops.pocket.trochoidal_climb.hint')}</p>
        {/if}
      {/if}
      {#if op.plunge && op.plunge.kind !== 'helix'}
        <p class="hint warn">{t('ops.pocket.trochoidal_plunge.hint')}</p>
      {/if}
      {#if (op.tabPlacements && op.tabPlacements.length > 0) || (op.tabMode && op.tabMode.kind !== 'off')}
        <p class="hint warn">{t('ops.pocket.trochoidal_tabs.hint')}</p>
      {/if}
    </details>
  {:else}
    {@const toolDefault = opTool?.defaultXyOverlap}
    {@const inheritedOverlap = toolDefault ?? 0.5}
    {@const opOverlap = op.xyOverlap}
    <label
      class="row"
      title={opOverlap === undefined
        ? t('ops.pocket.xy_overlap.help_empty', {
            source:
              toolDefault !== undefined
                ? t('ops.pocket.xy_overlap.source_tool', {
                    value: toolDefault,
                    tool: opTool?.name ?? '',
                  })
                : t('ops.pocket.xy_overlap.source_global'),
          })
        : t('ops.pocket.xy_overlap.help_set', {
            source:
              toolDefault !== undefined
                ? t('ops.pocket.xy_overlap.inherit_tool', {
                    value: toolDefault,
                    tool: opTool?.name ?? '',
                  })
                : t('ops.pocket.xy_overlap.inherit_global'),
          })}
    >
      <span>{t('ops.pocket.xy_overlap.label')}</span>
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
        <span class="unit" title={t('ops.pocket.xy_overlap.unit_help')}>fraction</span>
      </div>
    </label>
  {/if}
</fieldset>

<style>
  /* Italic styling for the XY overlap input when it's empty and
     inheriting from the tool's defaultXyOverlap. Reads as "this is a
     computed default, not a user-typed value". */
  input.inherit-italic::placeholder {
    font-style: italic;
    opacity: 0.75;
  }
  /* Quick-pick angle buttons for the zigzag direction. Match
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
