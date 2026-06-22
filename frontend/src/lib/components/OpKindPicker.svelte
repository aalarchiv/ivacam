<script lang="ts" module>
  import type { OpKind } from '../state/project.svelte';
  import { t } from '../i18n';

  /// Synthetic kind that wraps a regular Pocket op with frame_shape +
  /// difference combine pre-filled. Exported so callers can switch on it.
  export type PickerKind = OpKind | 'pocket_outside';

  /// Localized display label for an op kind. The enum *key* is stable
  /// (project-file compatibility); only the label is translated. Reactive
  /// when called in markup / `$derived` — switching language re-renders.
  export function kindLabel(kind: OpKind): string {
    return t(`ops.kind.${kind}`);
  }
  export const KIND_ICON: Record<OpKind, string> = {
    profile: '▢',
    pocket: '▣',
    drill: '◉',
    thread: '◎',
    chamfer: '◇',
    engrave: '✎',
    drag_knife: '✁',
    t_slot: '⊤',
    dovetail: '⋀',
    vcarve: '⌃',
    pause: '⏸',
    homing: '⌂',
    probe: '⇣',
    cycle_marker: '◈',
    gcode_include: '⎙',
    relief_mill: '⛰',
    raster_engrave: '▦',
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
    't_slot',
    'dovetail',
    'vcarve',
    'relief_mill',
    'raster_engrave',
    'pause',
    'homing',
    'probe',
    'cycle_marker',
    'gcode_include',
  ];

  export const PICKER_ICON: Record<PickerKind, string> = {
    ...KIND_ICON,
    pocket_outside: '⊞',
  };

  /// Localized display label for a picker kind (op kinds + the synthetic
  /// `pocket_outside`). See `kindLabel` — reactive in markup.
  export function pickerLabel(kind: PickerKind): string {
    return t(`ops.kind.${kind}`);
  }

  /// One-line localized description per picker kind. Surfaced as the
  /// native `title` tooltip on every picker entry and on each row's kind
  /// icon, plus the matching aria-label for screen readers. Keep the
  /// catalog strings short — they have to fit the OS tooltip pane.
  export function pickerHelp(kind: PickerKind): string {
    return t(`ops.help.${kind}`);
  }
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
    requireSelectionForPocketOutside && project.sel.selectedObjects.size === 0,
  );

  /// Each op kind's required machine capability. The picker
  /// hides kinds whose required capability isn't in the machine's
  /// effective set (empty `machine.capabilities` ⇒ `[mode]` — the
  /// default when capabilities is absent).
  const OP_REQUIRES: Record<PickerKind, ('mill' | 'laser' | 'drag' | 'plasma')[]> = {
    // Plasma cuts outlines (and holes are inner profiles), so profile is
    // plasma-capable; area-clearing / Z-aware ops stay mill/laser.
    profile: ['mill', 'laser', 'plasma'],
    pocket: ['mill'],
    pocket_outside: ['mill'],
    drill: ['mill'],
    thread: ['mill'],
    chamfer: ['mill'],
    engrave: ['mill', 'laser'],
    drag_knife: ['drag'],
    t_slot: ['mill'],
    dovetail: ['mill'],
    vcarve: ['mill'],
    relief_mill: ['mill'],
    // Laser raster engraving is laser-only (matches the backend
    // laser gate + the op×mode warning).
    raster_engrave: ['laser'],
    // Pause carries no tool / motion — every machine can pause.
    pause: ['mill', 'laser', 'drag', 'plasma'],
    // Program-only building blocks (Homing / Probe / CycleMarker)
    // emit raw G-code and don't depend on a cutter mode. Show them on
    // every machine.
    homing: ['mill', 'laser', 'drag', 'plasma'],
    probe: ['mill', 'laser', 'drag', 'plasma'],
    cycle_marker: ['mill', 'laser', 'drag', 'plasma'],
    gcode_include: ['mill', 'laser', 'drag', 'plasma'],
  };
  const machineCapabilities = $derived<('mill' | 'laser' | 'drag' | 'plasma')[]>(
    project.data.machine.capabilities && project.data.machine.capabilities.length > 0
      ? project.data.machine.capabilities
      : [project.data.machine.mode],
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
    let next: number;
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

<div class="picker" role="menu" tabindex="-1" onkeydown={onPickerKey} use:autoFocusFirst>
  {#each visibleKinds as k (k)}
    {@const disabled = k === 'pocket_outside' && pocketOutsideDisabled}
    <button
      class="kind"
      role="menuitem"
      onclick={() => onPick(k)}
      {disabled}
      title={disabled ? t('ops.picker.select_first') : pickerHelp(k)}
      aria-label={t('ops.picker.add_aria', { label: pickerLabel(k) })}
    >
      <span class="ico" aria-hidden="true">{PICKER_ICON[k]}</span>
      <span>{pickerLabel(k)}</span>
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
