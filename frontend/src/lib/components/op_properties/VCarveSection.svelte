<script lang="ts">
  /// V-Carve op-properties fieldset. Shown only when op.kind === 'vcarve'.
  /// Styling is mostly inherited from OpPropertiesPanel's :global(.props ...)
  /// rules; the local style block below only covers the rt1.7 inlay
  /// button. (svelte-check treats a literal "style" tag in a JSDoc
  /// comment as the start of a real style block, so the word is spelled
  /// out rather than tagged here.)
  import {
    project,
    type OpField,
    type OpFieldValue,
    type VCarveOp,
  } from '../../state/project.svelte';
  // `project` import kept for the inlay-plug Duplicate-as-plug button
  // below (line 99). The toolMismatch chip moved to OpPropertiesPanel.

  interface Props {
    op: VCarveOp;
    /// Kind-aware patch — see ChamferSection for rationale.
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();
</script>

<fieldset>
  <legend>V-Carve</legend>
  <!-- Tool-kind warning is now emitted by OpPropertiesPanel against
       the central op_tool_constraint helper — see k94n. -->

  <!--
    Carve mode is the most consequential V-Carve setting (it changes the
    toolpath structure entirely), so it sits at the top of the section
    rather than buried inside Advanced. Stored as the
    `fullMedialAxis: boolean | undefined` field — undefined means
    default-perimeter for forward compat with old projects.
  -->
  <div
    class="row"
    title="Perimeter (default, Estlcam-style): the cutter traces the boundary offset inward by Max width / 2 at constant depth, leaving the centre plateau untouched. Medial axis (Aspire-style): depth varies along an interior spine through the widest parts of the region; useful for depth-gradient relief, but typically NOT what you want for sign-making."
  >
    <span>Mode</span>
    <div class="segmented">
      <button
        type="button"
        class:active={!(op.fullMedialAxis ?? false)}
        onclick={() => patch('fullMedialAxis', undefined)}
      >
        Perimeter
      </button>
      <button
        type="button"
        class:active={op.fullMedialAxis ?? false}
        onclick={() => patch('fullMedialAxis', true)}
      >
        Medial axis
      </button>
    </div>
  </div>
  <details class="subsection" open>
    <summary>Advanced</summary>
    <label
      class="row"
      title="Optional cap on the inscribed-circle radius (mm). Leave empty for no cap. Useful when a wide region would otherwise drive the V deeper than the bit's usable shoulder."
    >
      <span>Max width</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.1"
          min="0"
          placeholder="no cap"
          value={op.carveMaxWidthMm ?? ''}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            patch('carveMaxWidthMm', isNaN(v) || v <= 0 ? undefined : v);
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label
      class="row"
      title="Inlay plug clearance. Shrinks the source region inward by this amount before the V-Carve pass, so the plug ends up that much smaller per side than the pocket. Pocket side leaves this empty / 0; plug side typically uses 0.05–0.2 mm. Set both halves to the SAME value for a proper wedge fit."
    >
      <span>Source inset</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.05"
          min="0"
          placeholder="0"
          value={op.sourceInsetMm ?? ''}
          onchange={(e) => {
            const v = parseFloat((e.currentTarget as HTMLInputElement).value);
            patch('sourceInsetMm', isNaN(v) || v <= 0 ? undefined : v);
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <label
      class="row"
      title="Duplicates this V-Carve op as an inlay plug. The new op gets a 0.1 mm source inset by default — the plug is 0.1 mm smaller per side than the pocket. Cut the pocket on your base material; cut the plug on contrasting stock (flipped); glue together. Adjust 'Source inset' on either side to match — both halves need the SAME gap value for a proper wedge fit."
    >
      <span>Inlay plug</span>
      <button
        type="button"
        class="inlay-btn"
        onclick={() => {
          const dup = project.duplicateOperation(op.id);
          if (!dup) return;
          project.updateOperation(dup.id, {
            name: `${op.name} (plug)`,
            sourceInsetMm: 0.1,
          });
        }}
      >
        Duplicate as inlay plug
      </button>
    </label>
    <label
      class="row"
      title="Planned for a future release: re-cut only the points whose first pass fell short of the geometric target depth. The control is disabled until the refinement pass ships."
    >
      <span>Refine pass</span>
      <input type="checkbox" checked={false} disabled />
      <span class="hint hint-trailing">not yet implemented</span>
    </label>
  </details>
</fieldset>

<style>
  /* Trailing hint sibling — sits inline after a checkbox / input. Was an
     inline `style="margin-left:.5rem"`; promoted to a class. */
  :global(.hint.hint-trailing) {
    margin-left: 0.5rem;
  }
  /* rt1.7: 'Duplicate as inlay plug' button. Sized to match the
     existing per-section action buttons (re-pick, repick) so the V-Carve
     fieldset doesn't look like it bolted on something special. */
  .inlay-btn {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    color: var(--text-strong);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--border));
    border-radius: 3px;
    padding: 0.15rem 0.55rem;
    font-size: 0.74rem;
    cursor: pointer;
  }
  .inlay-btn:hover {
    background: color-mix(in srgb, var(--accent) 30%, transparent);
  }
</style>
