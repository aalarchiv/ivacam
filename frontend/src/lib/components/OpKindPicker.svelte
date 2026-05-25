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
    vcarve: 'V-Carve',
    pause: 'Pause',
  };
  export const KIND_ICON: Record<OpKind, string> = {
    profile: '▢',
    pocket: '▣',
    drill: '◉',
    thread: '◎',
    chamfer: '◇',
    engrave: '✎',
    drag_knife: '✁',
    vcarve: '⌃',
    pause: '⏸',
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
    'pause',
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
    vcarve:
      'Variable-depth medial-axis carving with a V-bit. Tip dips deepest where the region is widest.',
    chamfer:
      'Chamfering pass with a V-bit. Set the chamfer width and the depth is derived from the bit angle.',
    thread:
      'Single-point thread milling inside a circular bore (internal) or around a stud (external). Requires a closed-circle selection.',
    pause:
      'Optional-stop: emits M0 with an operator message at this slot. The machine halts so the operator can change tools manually, inspect the cut, or flip the stock. Press Cycle Start to resume.',
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

  /// h0tx: each op kind's required machine capability. The picker
  /// hides kinds whose required capability isn't in the machine's
  /// effective set (empty `machine.capabilities` ⇒ `[mode]` —
  /// back-compat for projects that predate the field).
  const OP_REQUIRES: Record<PickerKind, ('mill' | 'laser' | 'drag')[]> = {
    profile: ['mill', 'laser'],
    pocket: ['mill'],
    pocket_outside: ['mill'],
    drill: ['mill'],
    thread: ['mill'],
    chamfer: ['mill'],
    engrave: ['mill', 'laser'],
    drag_knife: ['drag'],
    vcarve: ['mill'],
    // Pause carries no tool / motion — every machine can pause.
    pause: ['mill', 'laser', 'drag'],
  };
  const machineCapabilities = $derived<('mill' | 'laser' | 'drag')[]>(
    project.machine.capabilities && project.machine.capabilities.length > 0
      ? project.machine.capabilities
      : [project.machine.mode],
  );
  function isPickerKindSupported(kind: PickerKind): boolean {
    const req = OP_REQUIRES[kind];
    return req.some((c) => machineCapabilities.includes(c));
  }
  const visibleKinds = $derived(ALL_PICKER_KINDS.filter(isPickerKindSupported));

  /// Arrow-key nav across the 2-column picker grid. The picker is opened
  /// in several contexts (OperationsList "+", canvas right-click ctx menu,
  /// LayerList "+ Add"); without keyboard arrow nav, users dependent on
  /// the keyboard had to Tab through the whole document to walk items.
  function onPickerKey(e: KeyboardEvent) {
    const root = e.currentTarget as HTMLElement;
    const items = Array.from(
      root.querySelectorAll<HTMLElement>('button[role="menuitem"]:not(:disabled)'),
    );
    if (items.length === 0) return;
    const idx = items.indexOf(document.activeElement as HTMLElement);
    const cols = 2;
    let next = idx;
    if (e.key === 'ArrowDown') next = idx < 0 ? 0 : Math.min(items.length - 1, idx + cols);
    else if (e.key === 'ArrowUp') next = idx <= 0 ? 0 : Math.max(0, idx - cols);
    else if (e.key === 'ArrowRight') next = idx < 0 ? 0 : (idx + 1) % items.length;
    else if (e.key === 'ArrowLeft') next = idx <= 0 ? items.length - 1 : idx - 1;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = items.length - 1;
    else return;
    e.preventDefault();
    items[next]?.focus();
  }
  function autoFocusFirst(node: HTMLElement) {
    queueMicrotask(() => {
      const first = node.querySelector<HTMLElement>('button[role="menuitem"]:not(:disabled)');
      first?.focus();
    });
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="picker" role="menu" tabindex="-1" onkeydown={onPickerKey} use:autoFocusFirst>
  {#each visibleKinds as k (k)}
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
