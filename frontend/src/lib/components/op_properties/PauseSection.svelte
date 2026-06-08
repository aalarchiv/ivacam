<script lang="ts">
  /// rt1.34 Pause op-properties: name + operator message. No tool, no
  /// source, no geometry — the op is a program-flow stop.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import type { OpField, OpFieldValue, PauseOp } from '../../state/project.svelte';

  interface Props {
    op: PauseOp;
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
  title="Rendered as a G-code comment immediately before the M0 stop. Shown on most controllers' operator console / pendant — write what the operator should do (e.g. 'Swap to 1/8 endmill', 'Flip the workpiece')."
>
  <span>Message</span>
  <input
    type="text"
    value={op.message ?? ''}
    placeholder="Tool change, inspect, flip stock, …"
    oninput={(e) => patch('message', (e.currentTarget as HTMLInputElement).value)}
  />
</label>
<p class="hint hint-pause">
  The pipeline emits <code>M5</code>, this message as a comment, <code>M0</code>, and
  <code>M3</code> at this slot. Spindle stops; pressing Cycle Start resumes.
</p>
