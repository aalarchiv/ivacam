/// Pure logic for the canonical dialog draft/commit/discard helper.
/// The rune-bearing DialogDraft class lives in
/// dialog-draft.svelte.ts; the comparison + close-protocol rules live
/// here so vitest exercises them without the rune runtime.

/// Order-invariant structural equality over plain JSON-ish data
/// (objects / arrays / primitives — the shape dialog drafts have).
/// Stringify-compare flagged reordered-but-equal drafts dirty.
export function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== typeof b) return false;
  if (a === null || b === null) return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      if (!deepEqual(a[i], b[i])) return false;
    }
    return true;
  }
  if (typeof a !== 'object') return false;
  const ao = a as Record<string, unknown>;
  const bo = b as Record<string, unknown>;
  const ak = Object.keys(ao);
  const bk = Object.keys(bo);
  if (ak.length !== bk.length) return false;
  for (const k of ak) {
    if (!Object.prototype.hasOwnProperty.call(bo, k)) return false;
    if (!deepEqual(ao[k], bo[k])) return false;
  }
  return true;
}

/// Two-step discard protocol: given the current dirty + armed state,
/// decide whether the dialog should really close and what the next
/// armed state is. First close attempt on a dirty draft arms the inline
/// confirm bar (close=false); the second attempt confirms. window.confirm
/// is deliberately not used — it silently returns false in some webviews.
export function reduceCloseAttempt(
  isDirty: boolean,
  confirmingDiscard: boolean,
): { close: boolean; confirmingDiscard: boolean } {
  if (!isDirty || confirmingDiscard) return { close: true, confirmingDiscard: false };
  return { close: false, confirmingDiscard: true };
}
