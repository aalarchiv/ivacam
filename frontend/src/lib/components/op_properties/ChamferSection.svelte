<script lang="ts">
  /// Chamfer op-properties fieldset. Shown only when op.kind === 'chamfer'.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import type { ChamferOp, OpField, OpFieldValue } from '../../state/project.svelte';
  import { t } from '../../i18n';

  interface Props {
    op: ChamferOp;
    /// Kind-aware patch (OpField + OpFieldValue) so calls like
    /// `patch('chamferWidthMm', 1.5)` type-check across every
    /// section without each section redeclaring a per-variant
    /// signature.
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<fieldset>
  <legend>{t('ops.chamfer.legend')}</legend>
  <!-- Tool-kind warning is now emitted by OpPropertiesPanel against
       the central op_tool_constraint helper — see k94n. -->

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
    title="Cut the chamfer twice — once at the rough feed (cleanup) and once at the tool's finish-set feed for surface quality."
  >
    <span>Finish pass</span>
    <input
      type="checkbox"
      checked={op.chamferFinishPass ?? false}
      onchange={(e) => patch('chamferFinishPass', (e.currentTarget as HTMLInputElement).checked)}
    />
  </label>
</fieldset>
