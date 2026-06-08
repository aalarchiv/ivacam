/// Shared "confirm before proceeding" prompt store.
///
/// Renders an inline dialog driven by a single $state slot. Replaces
/// ad-hoc `window.confirm` calls (Tauri C10 anti-pattern — WebKitGTK's
/// native confirm blocks the renderer) and the App.svelte inline
/// close-prompt overlay that had drifted from Modal-parity.
///
/// Two-way usage (boolean):
///   const ok = await confirmStore.ask({
///     title: 'Quit ivaCAM?',
///     body: 'You have unsaved changes. They will be lost if you quit now.',
///     primaryLabel: 'Discard & quit',
///     cancelLabel: 'Keep editing',
///     danger: true,
///   });
///   if (ok) await reallyQuit();
///
/// Three-way usage (Save / Don't save / Cancel) via `extraLabel`:
///   const choice = await confirmStore.askChoice({
///     title: 'Unsaved changes',
///     body: 'Save before you open another project?',
///     primaryLabel: 'Save & continue',
///     extraLabel: "Don't save",
///     cancelLabel: 'Cancel',
///     danger: false,
///     extraDanger: true,
///   });
///   // choice is 'primary' | 'extra' | 'cancel'
///
/// A pending prompt suppresses overlapping requests by resolving the
/// previous one as `cancel`; UI ensures only one prompt is on screen.

/// Which button the user picked. `extra` is the optional middle button
/// (absent in two-way prompts).
export type ConfirmChoice = 'primary' | 'extra' | 'cancel';

interface PendingConfirm {
  title: string;
  body: string;
  primaryLabel: string;
  cancelLabel: string;
  /// Optional middle button. When set, the prompt is three-way and
  /// `askChoice` can resolve to `'extra'`. Omit for a plain two-way
  /// confirm.
  extraLabel?: string;
  /// When true, the primary button is styled with the destructive
  /// `var(--danger)` palette. Use for "Discard & quit" flows where the
  /// primary action loses work.
  danger: boolean;
  /// When true, the middle (`extra`) button is styled destructive. Use
  /// for the "Don't save" choice in a Save/Don't-save/Cancel prompt
  /// where the primary action (Save) is the safe one.
  extraDanger?: boolean;
  /// Resolved from the rendered prompt's button handlers. Always called
  /// exactly once (either via answer() or via a later askChoice()/ask()
  /// replacing this one).
  resolve: (choice: ConfirmChoice) => void;
}

class ConfirmStore {
  pending = $state<PendingConfirm | null>(null);

  /// Three-way prompt. Resolves to the button the user chose. ESC /
  /// backdrop / a replacing prompt all resolve to `'cancel'`.
  askChoice(args: Omit<PendingConfirm, 'resolve'>): Promise<ConfirmChoice> {
    // Replace any in-flight prompt with the new one and resolve the old
    // one as "cancelled" so its caller's promise doesn't dangle forever.
    if (this.pending) this.pending.resolve('cancel');
    return new Promise<ConfirmChoice>((resolve) => {
      this.pending = { ...args, resolve };
    });
  }

  /// Two-way convenience: resolves `true` only when the user picked the
  /// primary button. Back-compat wrapper over `askChoice` for the many
  /// callers that just want a yes/no.
  ask(args: Omit<PendingConfirm, 'resolve' | 'extraLabel' | 'extraDanger'>): Promise<boolean> {
    return this.askChoice(args).then((choice) => choice === 'primary');
  }

  answer(choice: ConfirmChoice): void {
    if (!this.pending) return;
    const p = this.pending;
    this.pending = null;
    p.resolve(choice);
  }
}

export const confirmStore = new ConfirmStore();
