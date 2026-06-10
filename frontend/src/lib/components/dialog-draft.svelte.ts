/// Canonical draft / commit / discard state for modal dialogs.
///
/// Before this, every dialog reinvented the pattern: ToolLibraryDialog
/// hand-rolled deepEqual, MachineDialog fingerprinted via
/// JSON.stringify, AddTextDialog diffed per field, PostProcessorEditor
/// cloned without a dirty check. One template now:
///
///   const dd = new DialogDraft<ToolEntry[]>();
///   $effect(() => { if (open) dd.open($state.snapshot(project.data.tools)); });
///   // bind inputs to dd.draft…; on Save: commit(dd.draft); dd.markClean()
///   // on X / Esc / backdrop: if (dd.requestClose()) onClose();
///
/// Dirty checking is structural deep-equality against a pristine clone
/// captured at open() — invariant to key order and to Svelte 5 $state
/// proxy wrappers ($state.snapshot strips them before comparing). The
/// discard guard is the two-step inline pattern the dialogs already
/// converged on (see reduceCloseAttempt in dialog-draft.ts).

import { deepEqual, reduceCloseAttempt } from './dialog-draft';

export { deepEqual };

/// Deep clone that survives Svelte 5 $state proxies: snapshot first
/// (strips the reactive wrappers), then structuredClone for the copy.
function clone<T>(v: T): T {
  return structuredClone($state.snapshot(v)) as T;
}

export class DialogDraft<T> {
  /// The editable working copy the dialog binds its inputs to. Deeply
  /// reactive ($state), seeded by open().
  draft = $state<T | null>(null);

  /// Pristine clone captured at open() / markClean(). Plain (non-$state)
  /// — it's only read for the dirty comparison.
  private pristine: T | null = null;

  /// Two-step discard guard: armed by the first requestClose() on a
  /// dirty draft; the dialog renders its inline confirm bar while true.
  confirmingDiscard = $state(false);

  /// Seed the draft from the live value (a $state proxy is fine — it's
  /// stripped during cloning).
  open(initial: T): void {
    this.draft = clone(initial);
    this.pristine = clone(initial);
    this.confirmingDiscard = false;
  }

  /// Drop the draft (dialog closed). Subsequent isDirty is false.
  close(): void {
    this.draft = null;
    this.pristine = null;
    this.confirmingDiscard = false;
  }

  get isDirty(): boolean {
    if (this.draft === null || this.pristine === null) return false;
    return !deepEqual($state.snapshot(this.draft), this.pristine);
  }

  /// Re-baseline after a successful commit so the dialog can stay open
  /// without re-prompting (Save-and-continue flows).
  markClean(): void {
    if (this.draft !== null) this.pristine = clone(this.draft);
  }

  /// Close protocol: returns true when the caller should really close
  /// (clean draft, or the user confirmed the discard); returns false
  /// and arms the inline confirm bar on the first dirty attempt.
  requestClose(): boolean {
    const next = reduceCloseAttempt(this.isDirty, this.confirmingDiscard);
    this.confirmingDiscard = next.confirmingDiscard;
    return next.close;
  }

  /// "Keep editing" button on the inline confirm bar.
  cancelDiscard(): void {
    this.confirmingDiscard = false;
  }
}
