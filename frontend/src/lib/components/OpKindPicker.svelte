<script lang="ts" module>
  import type { OpKind } from '../state/project.svelte';

  /// Synthetic kind that wraps a regular Pocket op with frame_shape +
  /// difference combine pre-filled. Exported so callers can switch on it.
  export type PickerKind = OpKind | 'pocket_outside';

  export const KIND_LABEL: Record<OpKind, string> = {
    profile: 'Profile',
    pocket: 'Pocket',
    drill: 'Drill',
    thread: 'Thread',
    chamfer: 'Chamfer',
    engrave: 'Engrave',
    drag_knife: 'Drag-knife',
    helix: 'Helix',
    vcarve: 'V-Carve',
  };
  export const KIND_ICON: Record<OpKind, string> = {
    profile: '▢',
    pocket: '▣',
    drill: '◉',
    thread: '◎',
    chamfer: '◇',
    engrave: '✎',
    drag_knife: '✁',
    helix: '◎',
    vcarve: '⌃',
  };
  // Helix is omitted intentionally: it's an OperationKind in the
  // schema but the dedicated standalone helix-op emitter isn't shipped
  // yet (the backend returns UnimplementedKind). Users get helical
  // entry by adding a Pocket and setting `Plunge → Helix` in the Cut
  // section, which IS supported. Re-add 'helix' here when the
  // standalone emitter lands.
  export const ALL_PICKER_KINDS: PickerKind[] = [
    'profile',
    'pocket',
    'pocket_outside',
    'drill',
    'thread',
    'chamfer',
    'engrave',
    'drag_knife',
    'vcarve',
  ];

  export const PICKER_LABEL: Record<PickerKind, string> = {
    ...KIND_LABEL,
    pocket_outside: 'Pocket Outside',
  };
  export const PICKER_ICON: Record<PickerKind, string> = {
    ...KIND_ICON,
    pocket_outside: '⊞',
  };

  /// One-line description per op kind. Surfaced as the native `title`
  /// tooltip on every picker entry and on each row's kind icon, plus the
  /// matching aria-label for screen readers. Keep these short — they
  /// have to fit the OS tooltip pane.
  export const PICKER_HELP: Record<PickerKind, string> = {
    profile:
      'Cuts along the contour of selected geometry. Tool stays inside, outside, or on the path.',
    pocket:
      'Removes material inside a closed boundary. Choose Cascade/Zigzag/Spiral/Trochoidal strategy.',
    pocket_outside:
      'Pocket the area BETWEEN a frame and the selection. Useful for raised text/graphics where the surrounding area is removed. Requires at least one selected object.',
    drill:
      'Drills holes at point geometry or small closed circles. Choose simple / peck / chip-break cycle.',
    engrave: 'Tool-on engraving along the source path. No offset.',
    drag_knife: 'Drag-knife cuts with trail-compensation arcs at corners.',
    helix: 'Helical descent into a closed contour.',
    vcarve:
      'Variable-depth medial-axis carving with a V-bit. Tip dips deepest where the region is widest.',
    chamfer:
      'Chamfering pass with a V-bit. Set the chamfer width and the depth is derived from the bit angle.',
    thread:
      'Single-point thread milling inside a circular bore (internal) or around a stud (external). Requires a closed-circle selection.',
  };
</script>

<script lang="ts">
  /// Grid of operation kinds (the same set the OperationsList "+ Add"
  /// menu offers). Used both as the inline picker under OperationsList
  /// and from the EntityCanvas2D right-click menu.
  import { project } from '../state/project.svelte';

  interface Props {
    onPick: (kind: PickerKind) => void;
    /// When true, the Pocket Outside entry is disabled if the user has
    /// no canvas selection (the wrapper needs source objects).
    requireSelectionForPocketOutside?: boolean;
  }
  let { onPick, requireSelectionForPocketOutside = true }: Props = $props();

  const pocketOutsideDisabled = $derived(
    requireSelectionForPocketOutside && project.selectedObjects.size === 0,
  );
</script>

<div class="picker" role="menu">
  {#each ALL_PICKER_KINDS as k (k)}
    {@const disabled = k === 'pocket_outside' && pocketOutsideDisabled}
    <button
      class="kind"
      role="menuitem"
      onclick={() => onPick(k)}
      {disabled}
      title={disabled ? 'Select one or more objects in the canvas first.' : PICKER_HELP[k] || ''}
      aria-label={`Add ${PICKER_LABEL[k]} operation`}
    >
      <span class="ico" aria-hidden="true">{PICKER_ICON[k]}</span>
      <span>{PICKER_LABEL[k]}</span>
    </button>
  {/each}
</div>

<style>
  .picker {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.2rem;
    padding: 0.3rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 4px;
  }
  .kind {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    background: transparent;
    color: var(--text);
    border: 1px solid transparent;
    border-radius: 3px;
    padding: 0.2rem 0.4rem;
    font-size: 0.78rem;
    cursor: pointer;
    text-align: left;
  }
  .kind:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    border-color: var(--accent);
  }
  .kind:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .ico {
    font-size: 0.95rem;
    color: var(--accent-strong);
    width: 1rem;
    text-align: center;
  }
</style>
