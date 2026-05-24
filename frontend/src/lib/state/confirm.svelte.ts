/// Shared "confirm before proceeding" prompt store.
///
/// Renders an inline two-button dialog driven by a single $state slot.
/// Replaces ad-hoc `window.confirm` calls (Tauri C10 anti-pattern —
/// WebKitGTK's native confirm blocks the renderer) and the App.svelte
/// inline close-prompt overlay that had drifted from Modal-parity.
///
/// Usage:
///   const ok = await confirmStore.ask({
///     title: 'Quit wiaConstructor?',
///     body: 'You have unsaved changes. They will be lost if you quit now.',
///     primaryLabel: 'Discard & quit',
///     cancelLabel: 'Keep editing',
///     danger: true,
///   });
///   if (ok) await reallyQuit();
///
/// A pending prompt suppresses overlapping requests by resolving the
/// previous one as `false`; UI ensures only one prompt is on screen.

interface PendingConfirm {
  title: string;
  body: string;
  primaryLabel: string;
  cancelLabel: string;
  /// When true, primary button styled with the destructive `var(--danger)`
  /// palette. Use for "Discard & quit" / "Discard unsaved changes" flows.
  danger: boolean;
  /// Resolved from the rendered prompt's button handlers. Always called
  /// exactly once (either via answer() or via a later ask() replacing
  /// this one).
  resolve: (ok: boolean) => void;
}

class ConfirmStore {
  pending = $state<PendingConfirm | null>(null);

  ask(args: Omit<PendingConfirm, 'resolve'>): Promise<boolean> {
    // Replace any in-flight prompt with the new one and resolve the old
    // one as "cancelled" so its caller's promise doesn't dangle forever.
    if (this.pending) this.pending.resolve(false);
    return new Promise<boolean>((resolve) => {
      this.pending = { ...args, resolve };
    });
  }

  answer(ok: boolean): void {
    if (!this.pending) return;
    const p = this.pending;
    this.pending = null;
    p.resolve(ok);
  }
}

export const confirmStore = new ConfirmStore();
