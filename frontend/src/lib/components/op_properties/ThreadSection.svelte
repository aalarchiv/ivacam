<script lang="ts">
  /// Thread op-properties fieldset. Shown only when op.kind === 'thread';
  /// the parent narrows so the section receives a strongly-typed
  /// `ThreadOp` rather than the full OpEntry union.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import type { OpField, OpFieldValue, ThreadOp } from '../../state/project.svelte';
  import { t } from '../../i18n';

  interface Props {
    op: ThreadOp;
    /// Kind-aware patch — see ChamferSection for rationale.
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<fieldset>
  <legend>{t('ops.thread.legend')}</legend>
  <p
    class="hint"
    title="Source must be a closed circle (drilled hole or stud diameter). The cutter walks a helix at one pitch of Z descent per revolution between Start depth and Depth."
  >
    Thread requires a closed circle as the source.
  </p>
  <label
    class="row"
    title="Z descent per full revolution. Picks the thread series: M6×1.0 → 1.0 mm, M3×0.5 → 0.5 mm. Positive value."
  >
    <span>Pitch</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.05"
        min="0"
        placeholder="1"
        value={op.threadPitchMm ?? ''}
        onchange={(e) => {
          const v = parseFloat((e.currentTarget as HTMLInputElement).value);
          patch('threadPitchMm', isNaN(v) || v <= 0 ? undefined : v);
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label
    class="row"
    title="Internal = tap-style (cutter inside the bore). External = die-style (cutter around a stud)."
  >
    <span>Direction</span>
    <select
      value={(op.threadInternal ?? true) ? 'internal' : 'external'}
      onchange={(e) => {
        const v = (e.currentTarget as HTMLSelectElement).value;
        patch('threadInternal', v === 'internal');
      }}
    >
      <option value="internal">{t('ops.thread.side.internal')}</option>
      <option value="external">{t('ops.thread.side.external')}</option>
    </select>
  </label>
  <label
    class="row"
    title="Climb (CCW helix on a right-hand spindle) vs conventional (CW). Default off (conventional) — almost always best for surface quality on hobby machines."
  >
    <span>Climb</span>
    <input
      type="checkbox"
      checked={op.threadClimb ?? false}
      onchange={(e) => patch('threadClimb', (e.currentTarget as HTMLInputElement).checked)}
    />
  </label>
</fieldset>
