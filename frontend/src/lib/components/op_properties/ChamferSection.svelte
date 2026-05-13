<script lang="ts">
  /// Chamfer op-properties fieldset. Shown only when op.kind === 'chamfer'.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import { project, type OpEntry } from '../../state/project.svelte';

  interface Props {
    op: OpEntry;
    patch: <K extends keyof OpEntry>(field: K, value: OpEntry[K]) => void;
  }
  let { op, patch }: Props = $props();

  let opTool = $derived(project.tools.find((tt) => tt.id === op.toolId));
  let toolMismatch = $derived(opTool != null && opTool.kind !== 'v_bit');
</script>

<fieldset>
  <legend>Chamfer</legend>
  {#if toolMismatch}
    <p
      class="warn-chip"
      title="Chamfer assumes a V-bit cone; flat / ball tools won't produce a true bevel. Pick a V-bit in the tool library."
    >
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
