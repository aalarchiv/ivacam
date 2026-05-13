<script lang="ts">
  /// V-Carve op-properties fieldset. Shown only when op.kind === 'vcarve'.
  /// Styling is inherited from OpPropertiesPanel's :global(.props ...)
  /// rules, so no <style> block is needed here.
  import { project, type OpEntry } from '../../state/project.svelte';
  import { _ } from 'svelte-i18n';

  interface Props {
    op: OpEntry;
    patch: <K extends keyof OpEntry>(field: K, value: OpEntry[K]) => void;
  }
  let { op, patch }: Props = $props();

  let opTool = $derived(project.tools.find((tt) => tt.id === op.toolId));
  let toolMismatch = $derived(opTool != null && opTool.kind !== 'v_bit');
</script>

<fieldset>
  <legend>V-Carve</legend>
  {#if toolMismatch}
    <p
      class="warn-chip"
      title="V-Carve assumes a V-bit cone — pick a V-bit in the tool library or the carve depth math won't match the actual cutter."
    >
      Tool kind mismatch — V-Carve needs a V-bit.
    </p>
  {/if}
  <details class="subsection" open>
    <summary>{$_('op.section.vcarve_advanced')}</summary>
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
      title="Planned for a future release: re-cut only the points whose first pass fell short of the geometric target depth. The control is disabled until the refinement pass ships."
    >
      <span>Refine pass</span>
      <input type="checkbox" checked={false} disabled />
      <span class="hint" style="margin-left:0.5rem">not yet implemented</span>
    </label>
  </details>
</fieldset>
