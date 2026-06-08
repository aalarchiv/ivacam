<script lang="ts">
  /// 8n4k Probe op-properties: G38.2 probe move along the selected axis.
  /// No tool / source needed — the operator wires the probe to the machine.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import type { OpField, OpFieldValue, ProbeOp } from '../../state/project.svelte';

  interface Props {
    op: ProbeOp;
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
  title="Which axis to probe along. Z is the common case (zero the WCS Z against the stock top); X / Y are edge-finder cycles."
>
  <span>Axis</span>
  <select
    value={op.axis ?? 'z'}
    onchange={(e) => patch('axis', (e.currentTarget as HTMLSelectElement).value as 'x' | 'y' | 'z')}
  >
    <option value="x">X</option>
    <option value="y">Y</option>
    <option value="z">Z</option>
  </select>
</label>
<label
  class="row"
  title="Search distance, mm. Sign follows the controller — NEGATIVE Z probes DOWN into stock; positive X / Y probes outward. The controller halts at the trigger; this is the maximum search."
>
  <span>Distance (mm)</span>
  <input
    type="number"
    step="0.1"
    value={op.distanceMm ?? -10}
    oninput={(e) =>
      patch('distanceMm', Number.parseFloat((e.currentTarget as HTMLInputElement).value))}
  />
</label>
<label
  class="row"
  title="Probe feed rate (mm/min). 50–200 mm/min is typical for a touch-trigger probe — slow enough to trip repeatably."
>
  <span>Feed (mm/min)</span>
  <input
    type="number"
    step="10"
    min="1"
    value={op.feedMmMin ?? 100}
    oninput={(e) =>
      patch('feedMmMin', Number.parseInt((e.currentTarget as HTMLInputElement).value, 10))}
  />
</label>
<p class="hint hint-pause">
  The pipeline emits <code
    >G38.2 {(op.axis ?? 'z').toUpperCase()}{op.distanceMm ?? -10} F{op.feedMmMin ?? 100}</code
  >. The controller halts at the trigger.
</p>
