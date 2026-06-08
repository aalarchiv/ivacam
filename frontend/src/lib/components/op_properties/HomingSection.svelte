<script lang="ts">
  /// 8n4k Homing op-properties: name + safe-Z toggle. The op emits G28
  /// (machine-specific home) and, optionally, a rapid Z lift afterwards.
  /// No tool, no source, no Z schedule.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import type { HomingOp, OpField, OpFieldValue } from '../../state/project.svelte';

  interface Props {
    op: HomingOp;
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<label class="row">
  <span>Name</span>
  <input
    type="text"
    value={op.name}
    oninput={(e) => patch('name', (e.currentTarget as HTMLInputElement).value)}
  />
</label>
<label
  class="row"
  title="When checked, the pipeline follows the G28 with a rapid G0 Z<safe> at the op's fastMoveZ so the next op starts from a known clearance. Most controllers leave the spindle wherever machine zero puts it, so the default is on."
>
  <span>Retract to safe Z after G28</span>
  <input
    type="checkbox"
    checked={op.retractToSafeZ ?? true}
    onchange={(e) => patch('retractToSafeZ', (e.currentTarget as HTMLInputElement).checked)}
  />
</label>
<p class="hint hint-pause">
  The pipeline emits <code>G28</code> (move to machine home){(op.retractToSafeZ ?? true)
    ? ' and a rapid Z lift to safe height'
    : ''}. No motion in X / Y other than the homing itself. The cutter does not engage.
</p>
