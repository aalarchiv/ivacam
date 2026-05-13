<script lang="ts">
  /// Pattern (repeat-this-op) fieldset. Universal — applies to every
  /// op kind. Styles inherited from OpPropertiesPanel's :global(.props ...)
  /// rules.
  import type { OpEntry } from '../../state/project.svelte';

  interface Props {
    op: OpEntry;
    patch: <K extends keyof OpEntry>(field: K, value: OpEntry[K]) => void;
  }
  let { op, patch }: Props = $props();
</script>

<fieldset>
  <legend>Pattern (repeat this op)</legend>
  <p class="hint">
    Run this operation once per pattern instance with the source geometry translated or rotated. The
    original geometry stays at the (0, 0) / 0° instance — single-count patterns are equivalent to no
    pattern.
  </p>
  <label
    class="row"
    title="Pattern shape — Linear array, rectangular Grid, or Polar (rotational) array."
  >
    <span>Pattern</span>
    <select
      value={op.pattern?.kind ?? 'none'}
      onchange={(e) => {
        const v = (e.currentTarget as HTMLSelectElement).value;
        if (v === 'none') {
          patch('pattern', undefined);
        } else if (v === 'linear') {
          patch('pattern', { kind: 'linear', count: 2, dx: 10, dy: 0 });
        } else if (v === 'grid') {
          patch('pattern', { kind: 'grid', count_x: 2, count_y: 2, dx: 10, dy: 10 });
        } else if (v === 'polar') {
          patch('pattern', {
            kind: 'polar',
            count: 4,
            center_x: 0,
            center_y: 0,
            angle_step_deg: 90,
          });
        }
      }}
    >
      <option value="none">None</option>
      <option value="linear">Linear array</option>
      <option value="grid">Rectangular grid</option>
      <option value="polar">Polar array</option>
    </select>
  </label>
  {#if op.pattern?.kind === 'linear'}
    {@const lin = op.pattern}
    <label
      class="row"
      title="Total number of instances along the array, including the original at offset (0, 0)."
    >
      <span>Count</span>
      <input
        type="number"
        min="1"
        step="1"
        value={lin.count}
        onchange={(e) => {
          const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
          if (Number.isFinite(v) && v >= 1) patch('pattern', { ...lin, count: v });
        }}
      />
    </label>
    <label class="row" title="X offset between consecutive instances (mm).">
      <span>Δx</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={lin.dx}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...lin, dx: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label class="row" title="Y offset between consecutive instances (mm).">
      <span>Δy</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={lin.dy}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...lin, dy: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
  {:else if op.pattern?.kind === 'grid'}
    {@const grid = op.pattern}
    <label class="row" title="Instances along the X axis.">
      <span>Count X</span>
      <input
        type="number"
        min="1"
        step="1"
        value={grid.count_x}
        onchange={(e) => {
          const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
          if (Number.isFinite(v) && v >= 1) patch('pattern', { ...grid, count_x: v });
        }}
      />
    </label>
    <label class="row" title="Instances along the Y axis.">
      <span>Count Y</span>
      <input
        type="number"
        min="1"
        step="1"
        value={grid.count_y}
        onchange={(e) => {
          const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
          if (Number.isFinite(v) && v >= 1) patch('pattern', { ...grid, count_y: v });
        }}
      />
    </label>
    <label class="row" title="X spacing between grid columns (mm).">
      <span>Δx</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={grid.dx}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...grid, dx: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label class="row" title="Y spacing between grid rows (mm).">
      <span>Δy</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={grid.dy}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...grid, dy: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
  {:else if op.pattern?.kind === 'polar'}
    {@const pol = op.pattern}
    <label class="row" title="Total instances around the center, including the original at 0°.">
      <span>Count</span>
      <input
        type="number"
        min="1"
        step="1"
        value={pol.count}
        onchange={(e) => {
          const v = parseInt((e.currentTarget as HTMLInputElement).value, 10);
          if (Number.isFinite(v) && v >= 1) patch('pattern', { ...pol, count: v });
        }}
      />
    </label>
    <label class="row" title="X coordinate of the rotation center (mm).">
      <span>Center X</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={pol.center_x}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...pol, center_x: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label class="row" title="Y coordinate of the rotation center (mm).">
      <span>Center Y</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          value={pol.center_y}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...pol, center_y: v });
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label
      class="row"
      title="Angle between consecutive instances (degrees). 360 / count for a full revolution."
    >
      <span>Step</span>
      <div class="num-cell">
        <input
          type="number"
          step="1"
          value={pol.angle_step_deg}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(v)) patch('pattern', { ...pol, angle_step_deg: v });
          }}
        />
        <span class="unit">°</span>
      </div>
    </label>
  {/if}
</fieldset>
