/// EntityCanvas2D selection reducer. Pure function:
///
///   (click input, current pointer-event modifiers) → SelectionAction[]
///
/// Owns the five things selection involves: decode modifiers, branch on
/// hit-vs-no-hit, arm box-select on empty space, fire
/// `project.selectObjects` / `project.clearSelection` /
/// `project.seriesSelectTo`, and tweak `project.sel.selectedOpId`.
/// Extracted here so the same shape can be unit-tested without mounting
/// the canvas, and so the component is just event-capture → reducer
/// → action dispatch.
///
/// The reducer is intentionally side-effect-free; it returns a list of
/// `SelectionAction`s that the caller dispatches against the live
/// project. Box-select state (a UI-only let-state on the canvas) is
/// represented as a `'arm-box-select'` action — the canvas owns the
/// armed-rectangle store and the pointer-capture, but the modifier
/// resolution and series→box fallback live here.
///
/// See `entity-selection.test.ts` for the modifier matrix.
export type SelectionMode = 'replace' | 'add' | 'toggle' | 'series';

export type BoxSelectMode = 'replace' | 'add' | 'toggle';

export type SelectionAction =
  | { kind: 'clear-selection' }
  | { kind: 'clear-fixture-selection' }
  | { kind: 'select-objects'; ids: number[]; mode: 'replace' | 'add' | 'toggle' }
  | { kind: 'series-select-to'; id: number }
  | { kind: 'set-active-op'; opId: number }
  | { kind: 'arm-box-select'; mode: BoxSelectMode };

export interface SelectionClick {
  /// Object id at the cursor, or `null` for empty-space clicks.
  hitObjectId: number | null;
  /// Modifier flags from the original PointerEvent.
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
}

/// Decode modifier flags into a selection mode. Priority order:
/// Shift > Ctrl/Cmd > plain.
export function modeFromModifiers(c: {
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
}): SelectionMode {
  if (c.shiftKey) return 'series';
  if (c.ctrlKey || c.metaKey) return 'toggle';
  return 'replace';
}

/// Reduce a single canvas click + the op-membership map for the hit
/// object into the actions the live project should perform.
///
/// `objectToOps[objectId] → opIds[]` is consulted only when the click
/// hit an object and mode resolves to `replace` (single-click on a
/// segment switches the active op to the first op that consumes it —
/// surfaces the right edit form on the right-hand panel). Modifier
/// clicks are about building selections, not switching active op.
export function reduceCanvasClick(
  click: SelectionClick,
  objectToOps?: ReadonlyMap<number, readonly number[]>,
): SelectionAction[] {
  const mode = modeFromModifiers(click);

  if (click.hitObjectId == null) {
    // Empty-space click. Plain (replace) clears the selection; modifier
    // clicks preserve the selection so the user can't accidentally drop
    // it mid-modifier. Box-select arms either way; series falls back to
    // additive box so Shift+drag stays useful when the cursor is over
    // empty space.
    const out: SelectionAction[] = [];
    if (mode === 'replace') {
      out.push({ kind: 'clear-selection' }, { kind: 'clear-fixture-selection' });
    }
    const boxMode: BoxSelectMode = mode === 'series' ? 'add' : mode;
    out.push({ kind: 'arm-box-select', mode: boxMode });
    return out;
  }

  // Hit an object. objectId 0 means "no chained object" — caller already
  // filters those out, but be defensive.
  if (click.hitObjectId === 0) return [];

  const out: SelectionAction[] =
    mode === 'series'
      ? [{ kind: 'series-select-to', id: click.hitObjectId }]
      : [{ kind: 'select-objects', ids: [click.hitObjectId], mode }];

  if (mode === 'replace' && objectToOps) {
    const ops = objectToOps.get(click.hitObjectId);
    if (ops && ops.length > 0) {
      out.push({ kind: 'set-active-op', opId: ops[0] });
    }
  }

  return out;
}
