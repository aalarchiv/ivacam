<script lang="ts">
  /// 8n4k CycleMarker op-properties: comment-only marker. Pendants and
  /// gcode viewers that index by program line can jump to the next marker.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import type { CycleMarkerOp, OpField, OpFieldValue } from '../../state/project.svelte';

  interface Props {
    op: CycleMarkerOp;
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
  title="Wrapped with `--- … ---` and emitted as a G-code comment at this slot. No controller motion."
>
  <span>Label</span>
  <input
    type="text"
    value={op.label ?? ''}
    placeholder="Halfway, Flip stock, …"
    oninput={(e) => patch('label', (e.currentTarget as HTMLInputElement).value)}
  />
</label>
<p class="hint hint-pause">
  The pipeline emits one comment line: <code>; --- {op.label ?? ''} ---</code>. No motion, no modal
  change.
</p>
