/// Cross-component signal for "focus this op's warning" (4kzy). The
/// per-op status badge in OperationsList sets it; GenerateBar's warnings
/// panel reacts by opening, expanding, and scrolling to the matching
/// row(s). Kept as a tiny rune-backed singleton so the two components
/// stay decoupled (no prop drilling through App.svelte).
class WarningFocus {
  /// op_id whose warnings should be revealed, or null when idle.
  opId = $state<number | null>(null);
  /// Bumped on every focus() call so repeated clicks on the SAME op
  /// re-fire the consuming $effect (which would otherwise not re-run
  /// when `opId` is unchanged).
  seq = $state(0);

  focus(opId: number) {
    this.opId = opId;
    this.seq += 1;
  }

  clear() {
    this.opId = null;
  }
}

export const warningFocus = new WarningFocus();
