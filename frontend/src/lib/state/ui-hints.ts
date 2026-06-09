/// Session-scoped UI hint gating. These flags are plain module state —
/// NOT persisted — so they reset on every app run (page reload / app
/// launch). No rune needed: the flags are consumed in event handlers at
/// the moment a menu would open, not read reactively in templates.

/// The empty right-click context menu (no objects / no text selected)
/// only offers a "you need to select something first" hint. It's the
/// same idea in the 2D and 3D panes and gets annoying when it pops on
/// every right-click, so show it at most once per session, shared across
/// both panes.
///
/// Returns `true` the FIRST time it's called in a session — the caller
/// should open the hint menu — and `false` thereafter, so the caller
/// skips the empty menu entirely. Call this ONLY on the empty-selection
/// path; a right-click with a real selection (the useful op-picker menu)
/// must not consume the hint.
let selectHintShown = false;

export function consumeSelectHint(): boolean {
  if (selectHintShown) return false;
  selectHintShown = true;
  return true;
}
