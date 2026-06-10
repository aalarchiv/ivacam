/// Multi-level undo / redo built on the command pattern. Every project
/// mutation flows through a `Command` with `apply` and `revert`; commands
/// live on an undo stack, undo pops + reverts, redo pushes back.
///
/// Excluded from history (view-state, not project state):
///   • project.playhead         — toolpath scrub position
///   • project.sel.selectedOpId     — currently-selected operation
///   • project.sel.selectedEntities — legacy per-segment selection
///   • project.sel.selectedFixtureId
///   • project.data.visibleLayers    — layer visibility toggles
///   • project.sel.hoverSegment, project.tabMode, project.data.regionsVisible
///   • project.data.settings         — per-installation prefs (localStorage)
///   • project.error, project.data.dirty, project.loading, project.gen.generating
///   • project.gen.simDiagnostics, project.gen.generated, project.toolpath*
///
/// Selection-aware commands: object selection IS in the undo
/// stack so Ctrl+Z reverts to the previous selection set. Commands
/// with `marksDirty: false` push into history but do NOT mark the
/// project file dirty — view-only state.
///
/// History is per-session only. We don't persist it to .vc-project.json.
///
/// Plain-TS module (not `.svelte.ts`) so vitest can import it without
/// the Svelte rune compiler. Reactivity is achieved by bumping a
/// `version` counter every time the stacks change; UI code subscribes
/// via the project state's `$state` that mirrors this counter.

export interface Command {
  /// Human-readable label shown in the Edit menu.
  label: string;
  apply(state: unknown): void;
  revert(state: unknown): void;
  /// Optional coalesce key: commands with the same coalesce key emitted
  /// within COALESCE_MS of each other merge into one undo step. Used
  /// for slider drags and number-field typing so the user doesn't have
  /// to undo a hundred 0.1 mm steps.
  coalesce_key?: string;
  /// Default true — exec() flips `state.dirty = true` after apply so
  /// the project file shows unsaved changes. Selection commands
  /// pass `false` so the canvas selection enters the undo stack
  /// without falsely flagging the project as edited.
  marksDirty?: boolean;
}

interface Transaction {
  label: string;
  commands: Command[];
}

export class History {
  static readonly COALESCE_MS = 500;
  static readonly MAX_DEPTH = 200;

  private undoStack: Command[] = [];
  private redoStack: Command[] = [];
  private lastCommandTime = 0;
  private transaction: Transaction | null = null;
  /// Monotonically incrementing token bumped on every state change. Used
  /// by the Svelte UI layer to trigger reactivity (mirrored into a
  /// `$state` counter on the project). Cannot live in this file as a
  /// `$state` rune directly because vitest's node-only test config
  /// (frontend/vitest.config.ts) skips the Svelte plugin — every
  /// History test would fail with "$state is not defined". A planned
  /// test-config upgrade would let us drop the mirror.
  private _version = 0;
  private listener: (() => void) | null = null;

  /// Hook called after every mutation (push / undo / redo / commit).
  /// project.svelte.ts wires this to bump a $state counter.
  subscribe(fn: () => void): void {
    this.listener = fn;
  }

  get version(): number {
    return this._version;
  }
  get undoSize(): number {
    return this.undoStack.length;
  }
  get redoSize(): number {
    return this.redoStack.length;
  }

  exec(cmd: Command, state: unknown): void {
    if (this.transaction) {
      const wrapped = wrapWithDirty(cmd, state);
      wrapped.apply(state);
      this.transaction.commands.push(wrapped);
      return;
    }
    const last = this.undoStack[this.undoStack.length - 1];
    const now = nowMs();
    const coalesces =
      cmd.coalesce_key != null &&
      last != null &&
      last.coalesce_key === cmd.coalesce_key &&
      now - this.lastCommandTime < History.COALESCE_MS;
    if (coalesces) {
      // Apply the new command but keep the original `revert` so the
      // single undo step takes the user back to before this run started.
      // The original wrapper's prevDirty already captures the pre-edit
      // dirty state from when the first command ran.
      const marksDirty = cmd.marksDirty !== false;
      cmd.apply(state);
      // Selection (and other marksDirty=false) commands don't flip
      // the dirty bit even when coalescing.
      if (marksDirty) markDirty(state);
      // Repoint the retained entry's `apply` at THIS (latest) command so
      // a later redo replays to the final value, not the first. Without
      // this, redo of a coalesced slider/number drag (0→100) would restore
      // the first intermediate value (e.g. 1) because the stacked entry
      // still held command #1's apply. `revert` is left untouched so undo
      // still unwinds to before the run started.
      last.apply = (s) => {
        cmd.apply(s);
        if (marksDirty) markDirty(s);
      };
    } else {
      const wrapped = wrapWithDirty(cmd, state);
      wrapped.apply(state);
      this.undoStack.push(wrapped);
      if (this.undoStack.length > History.MAX_DEPTH) this.undoStack.shift();
    }
    this.redoStack = [];
    this.lastCommandTime = now;
    this.bump();
  }

  undo(state: unknown): boolean {
    if (this.transaction) return false;
    const cmd = this.undoStack.pop();
    if (!cmd) return false;
    cmd.revert(state);
    this.redoStack.push(cmd);
    this.bump();
    return true;
  }

  redo(state: unknown): boolean {
    if (this.transaction) return false;
    const cmd = this.redoStack.pop();
    if (!cmd) return false;
    cmd.apply(state);
    this.undoStack.push(cmd);
    this.bump();
    return true;
  }

  undoLabel(): string | null {
    const cmd = this.undoStack[this.undoStack.length - 1];
    return cmd ? cmd.label : null;
  }

  redoLabel(): string | null {
    const cmd = this.redoStack[this.redoStack.length - 1];
    return cmd ? cmd.label : null;
  }

  /// Begin a compound transaction. Subsequent `exec` calls are buffered
  /// rather than pushed individually; `commitTransaction` collapses them
  /// into a single undo entry. Nested transactions are not supported.
  beginTransaction(label: string): void {
    if (this.transaction) {
      throw new Error('history: nested transactions not supported');
    }
    this.transaction = { label, commands: [] };
  }

  commitTransaction(): void {
    const tx = this.transaction;
    this.transaction = null;
    if (!tx || tx.commands.length === 0) return;
    const compound: Command = {
      label: tx.label,
      apply: (state) => {
        for (const c of tx.commands) c.apply(state);
      },
      revert: (state) => {
        // Revert in reverse order so dependent inserts unwind correctly.
        for (let i = tx.commands.length - 1; i >= 0; i--) tx.commands[i].revert(state);
      },
    };
    this.undoStack.push(compound);
    if (this.undoStack.length > History.MAX_DEPTH) this.undoStack.shift();
    this.redoStack = [];
    this.lastCommandTime = nowMs();
    this.bump();
  }

  /// Cancel the current transaction. Reverts every buffered command in
  /// reverse order so the state matches pre-transaction; nothing pushed
  /// to the undo stack.
  cancelTransaction(state: unknown): void {
    const tx = this.transaction;
    this.transaction = null;
    if (!tx) return;
    for (let i = tx.commands.length - 1; i >= 0; i--) tx.commands[i].revert(state);
    this.bump();
  }

  /// Drop everything. Used on project load/restore so the user can't
  /// undo back across a file boundary into a different project.
  clear(): void {
    this.undoStack = [];
    this.redoStack = [];
    this.transaction = null;
    this.lastCommandTime = 0;
    this.bump();
  }

  inTransaction(): boolean {
    return this.transaction != null;
  }

  private bump(): void {
    this._version++;
    this.listener?.();
  }
}

function nowMs(): number {
  if (typeof performance !== 'undefined' && typeof performance.now === 'function') {
    return performance.now();
  }
  return Date.now();
}

/// Wrap a command so dirty-bookkeeping rides along with apply/revert:
///   * the closure captures the pre-apply `dirty` value
///   * apply() runs the wrapped body, then forces `dirty = true`
///   * revert() runs the wrapped body, then restores the pre-apply
///     dirty — so undoing back to a clean state actually clears the
///     dirty flag, instead of leaving the project marked dirty after
///     every Ctrl+Z (which the legacy `t.dirty = true` in every
///     command's revert did).
///
/// Tolerant of states without a `dirty` field — generic Command isn't
/// coupled to ProjectState here.
function wrapWithDirty(cmd: Command, state: unknown): Command {
  const target = state as { dirty?: boolean };
  const prevDirty = 'dirty' in target ? !!target.dirty : false;
  const marksDirty = cmd.marksDirty !== false;
  return {
    label: cmd.label,
    coalesce_key: cmd.coalesce_key,
    marksDirty,
    apply(s) {
      cmd.apply(s);
      if (marksDirty) markDirty(s);
    },
    revert(s) {
      cmd.revert(s);
      if (marksDirty) {
        const t = s as { dirty?: boolean };
        if ('dirty' in t) t.dirty = prevDirty;
      }
    },
  };
}

function markDirty(state: unknown): void {
  const t = state as { dirty?: boolean };
  if ('dirty' in t) t.dirty = true;
}
