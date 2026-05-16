
<script lang="ts">
  /// Profile op-properties: Tool offset picker + lead-in / lead-out
  /// inputs. Shown only when op.kind === 'profile'.
  /// Styles inherited from OpPropertiesPanel's :global(.props ...) rules.
  import {
    type OpField,
    type OpFieldValue,
    type ProfileOffset,
    type ProfileOp,
  } from '../../state/project.svelte';

  interface Props {
    op: ProfileOp;
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<fieldset>
  <legend>Profile</legend>
  <label class="row">
    <span>Tool offset</span>
    <select
      value={op.offset}
      onchange={(e) =>
        patch('offset', (e.currentTarget as HTMLSelectElement).value as ProfileOffset)}
    >
      <option value="outside">outside</option>
      <option value="inside">inside</option>
      <option value="on">on path</option>
    </select>
  </label>
</fieldset>

<fieldset>
  <legend>Leads</legend>
  <label
    class="row"
    title="Lead-IN style. Off: rapid + plunge directly to the contour start. Straight: rapid to a point perpendicular to the start, then linear into the contour. Arc: tangent quarter-arc roll-on so the cutter eases into the cut without dwelling at the start."
  >
    <span>Lead in</span>
    <select
      value={op.leadInKind ?? 'off'}
      onchange={(e) =>
        patch(
          'leadInKind',
          (e.currentTarget as HTMLSelectElement).value as 'off' | 'straight' | 'arc',
        )}
    >
      <option value="off">off</option>
      <option value="straight">straight</option>
      <option value="arc">arc (roll-on)</option>
    </select>
  </label>
  {#if op.leadInKind && op.leadInKind !== 'off'}
    <label
      class="row"
      title={op.leadInKind === 'arc'
        ? 'Roll-on arc RADIUS (mm). The arc is a quarter-circle tangent to the contour at the entry point.'
        : 'Straight-line LENGTH (mm) of the perpendicular hop into the contour.'}
    >
      <span>{op.leadInKind === 'arc' ? 'Radius' : 'Length'}</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          min="0"
          value={op.leadIn ?? 5}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            patch('leadIn', isNaN(v) || v < 0 ? 0 : v);
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
  {/if}
  <label
    class="row"
    title="Lead-OUT style. Mirror of lead-in: how the cutter departs the contour at the END of the cut path. Arc gives a tangent roll-off; Straight a perpendicular exit; Off ends the cut at the contour end with a vertical retract."
  >
    <span>Lead out</span>
    <select
      value={op.leadOutKind ?? 'off'}
      onchange={(e) =>
        patch(
          'leadOutKind',
          (e.currentTarget as HTMLSelectElement).value as 'off' | 'straight' | 'arc',
        )}
    >
      <option value="off">off</option>
      <option value="straight">straight</option>
      <option value="arc">arc (roll-off)</option>
    </select>
  </label>
  {#if op.leadOutKind && op.leadOutKind !== 'off'}
    <label
      class="row"
      title={op.leadOutKind === 'arc'
        ? 'Roll-off arc RADIUS (mm). Quarter-circle tangent to the contour at the exit point.'
        : 'Straight-line LENGTH (mm) of the perpendicular exit from the contour.'}
    >
      <span>{op.leadOutKind === 'arc' ? 'Radius' : 'Length'}</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.5"
          min="0"
          value={op.leadOut ?? 5}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            patch('leadOut', isNaN(v) || v < 0 ? 0 : v);
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
  {/if}
</fieldset>
