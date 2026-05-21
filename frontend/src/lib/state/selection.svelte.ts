/// UI-selection slice of ProjectState (audit 6cpl). Owns every
/// "what's the user currently looking at" field plus the simple
/// mutation methods. The cross-slice walks (series-select, which
/// reads `imported.object_meta` + `visibleLayers`) stay on the
/// parent `ProjectState` as orchestrators that call the slice's
/// `selectObjects` here.
///
/// 80gv: object selection (`selectedObjects` + `selectionAnchorObjectId`)
/// is now in the undo/redo stack via a marksDirty=false Command —
/// `ProjectState.selectObjects` etc. push through History. Other
/// view-state fields (selectedOpId, hoverSegment, …) are still NOT
/// in history — they're transient per-action highlights.

export type SelectionMode = 'replace' | 'add' | 'toggle';

/// Pure helper: given the current selection set + anchor and an
/// incoming `(ids, mode)` request, return the resulting state.
/// Used by both the live `SelectionState.selectObjects` and the
/// History command factory so the two paths share logic.
export function computeSelectionUpdate(
  prevSelected: ReadonlySet<number>,
  prevAnchor: number | null,
  ids: Iterable<number>,
  mode: SelectionMode,
): { selected: Set<number>; anchor: number | null } {
  const incoming = [...ids].filter((id) => id > 0);
  if (mode === 'replace') {
    return {
      selected: new Set(incoming),
      anchor: incoming.length === 1 ? incoming[0] : null,
    };
  }
  const next = new Set(prevSelected);
  let anchor = prevAnchor;
  if (mode === 'add') {
    for (const id of incoming) next.add(id);
    if (incoming.length === 1) anchor = incoming[0];
  } else {
    for (const id of incoming) {
      if (next.has(id)) next.delete(id);
      else next.add(id);
    }
    if (incoming.length === 1 && next.has(incoming[0])) anchor = incoming[0];
  }
  return { selected: next, anchor };
}

/// Cheap structural equality for two Set<number>'s. Used to drop
/// no-op selection commands before they hit the history stack.
export function selectionsEqual(a: ReadonlySet<number>, b: ReadonlySet<number>): boolean {
  if (a === b) return true;
  if (a.size !== b.size) return false;
  for (const id of a) if (!b.has(id)) return false;
  return true;
}

/// Canvas-pick modes. The user enters one explicitly from a UI
/// affordance ("Pick on canvas" buttons in OpPropertiesPanel) and the
/// 2D canvas / status-bar swap to the matching interaction while it's
/// non-null. Sticky: stays active across multiple picks until the user
/// presses Escape or clicks the panel button again (n79).
export type PickMode = { kind: 'approach-point'; opId: number };

export class SelectionState {
  /// Per-segment hover indicator (single segment, not the chain).
  hoverSegment = $state<number | null>(null);

  /// Object-level selection. Each id is a 1-based chain id from
  /// `imported.objects` (0 = unchained segment). Drives the
  /// operations-list "Set source from selection" path.
  selectedObjects = $state<Set<number>>(new Set());

  /// Anchor for Shift+click series-select — the last object the
  /// user clicked on directly (plain click or Ctrl+click that
  /// added it). Cleared when the selection is fully cleared or a
  /// bulk replace lands more than one id at once.
  selectionAnchorObjectId = $state<number | null>(null);

  /// Legacy entity-level (per-segment) selection. Kept for project-
  /// file back-compat; no longer drives the UI directly.
  selectedEntities = $state<Set<number>>(new Set());

  /// id of the currently-selected fixture. Drives the right-hand
  /// panel's fixture edit form.
  selectedFixtureId = $state<number | null>(null);

  /// id of the currently-selected op. Drives `OpPropertiesPanel`.
  selectedOpId = $state<number | null>(null);

  /// id of the currently-selected text layer. Drives the sidebar
  /// Text panel's expanded edit form. Mutually exclusive with
  /// `selectedOpId` at the UX level.
  selectedTextLayerId = $state<number | null>(null);

  /// Drives the Tool library dialog. When non-null, App.svelte
  /// opens the dialog and the dialog scrolls / highlights the row
  /// whose id matches. Cleared by the dialog on close.
  toolsDialogFocusId = $state<number | null>(null);

  /// Active canvas-pick interaction (n79). When set, the 2D canvas
  /// behaves as a picker for the named target on the named op, and
  /// the status bar prompts "ESC to finalize". Cleared on Escape, on
  /// clicking the panel button a second time, or when the op /
  /// project changes from under it.
  pickMode = $state<PickMode | null>(null);

  /// Single-object toggle. With `additive=false` it just sets the
  /// selection to `{id}`; with `additive=true` it XORs `id` into
  /// the existing set.
  toggleObject(id: number, additive = false): void {
    if (id <= 0) return;
    const next = additive ? new Set(this.selectedObjects) : new Set<number>();
    if (additive && next.has(id)) next.delete(id);
    else next.add(id);
    this.selectedObjects = next;
  }

  /// Bulk selection update with FreeCAD-style modifier semantics:
  ///
  ///   * `'replace'` — drop the current selection and use `ids`
  ///   * `'add'`     — union into the current selection (Shift+…)
  ///   * `'toggle'`  — flip each id (Ctrl+… / Cmd+…)
  ///
  /// Updates `selectionAnchorObjectId` per the audit-eqxd rules:
  /// a single-id replace or add lands the anchor on that id; a
  /// bulk replace (box-select) clears it; a toggle that adds the
  /// id sets it, a toggle that removes the id leaves it alone.
  selectObjects(ids: Iterable<number>, mode: SelectionMode): void {
    const { selected, anchor } = computeSelectionUpdate(
      this.selectedObjects,
      this.selectionAnchorObjectId,
      ids,
      mode,
    );
    this.selectedObjects = selected;
    this.selectionAnchorObjectId = anchor;
  }

  /// Clear the object selection AND drop the anchor so the next
  /// Shift+click can't draw a series-line from a removed object.
  /// Caller is responsible for clearing entity / fixture / op
  /// selections separately if that's what they want.
  clearSelection(): void {
    this.selectedObjects = new Set();
    this.selectionAnchorObjectId = null;
  }

  /// Set the active fixture id. `null` means "no fixture selected".
  selectFixture(id: number | null): void {
    this.selectedFixtureId = id;
  }
}
