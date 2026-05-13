/// History stack tests. Pure logic — exercises the command-pattern
/// machinery, coalescing, transactions, and bounded depth. Mocks the
/// project state with a plain object so we don't need the Svelte rune
/// runtime in vitest's node environment.

import { describe, expect, it, vi } from 'vitest';
import { History, type Command } from './history';

interface CounterState {
  value: number;
  log: string[];
}

function setCmd(target: number, key?: string, label = `set ${target}`): Command {
  let prev = 0;
  return {
    label,
    apply: (s) => {
      const cs = s as CounterState;
      prev = cs.value;
      cs.value = target;
      cs.log.push(`apply:${target}`);
    },
    revert: (s) => {
      const cs = s as CounterState;
      cs.value = prev;
      cs.log.push(`revert:${target}`);
    },
    coalesce_key: key,
  };
}

function freshState(): CounterState {
  return { value: 0, log: [] };
}

describe('History.exec / undo / redo', () => {
  it('exec_then_undo_reverts', () => {
    const h = new History();
    const s = freshState();
    h.exec(setCmd(5), s);
    expect(s.value).toBe(5);
    expect(h.undo(s)).toBe(true);
    expect(s.value).toBe(0);
  });

  it('redo_after_undo_restores', () => {
    const h = new History();
    const s = freshState();
    h.exec(setCmd(7), s);
    h.undo(s);
    expect(s.value).toBe(0);
    expect(h.redo(s)).toBe(true);
    expect(s.value).toBe(7);
  });

  it('new_exec_clears_redo', () => {
    const h = new History();
    const s = freshState();
    h.exec(setCmd(1), s);
    h.exec(setCmd(2), s);
    h.undo(s);
    expect(s.value).toBe(1);
    h.exec(setCmd(99), s);
    expect(h.redo(s)).toBe(false);
    expect(s.value).toBe(99);
  });

  it('undo on empty returns false', () => {
    const h = new History();
    const s = freshState();
    expect(h.undo(s)).toBe(false);
    expect(h.redo(s)).toBe(false);
  });

  it('undoLabel / redoLabel report the top of each stack', () => {
    const h = new History();
    const s = freshState();
    expect(h.undoLabel()).toBeNull();
    expect(h.redoLabel()).toBeNull();
    h.exec(setCmd(1, undefined, 'first'), s);
    h.exec(setCmd(2, undefined, 'second'), s);
    expect(h.undoLabel()).toBe('second');
    h.undo(s);
    expect(h.undoLabel()).toBe('first');
    expect(h.redoLabel()).toBe('second');
  });
});

describe('History coalescing', () => {
  it('coalescing_merges_same_key_within_500ms', () => {
    const h = new History();
    const s = freshState();
    // First command sets value 1 (prev=0). Subsequent same-key commands
    // are coalesced — apply runs but no new entry on the undo stack, so
    // the single revert returns to 0.
    h.exec(setCmd(1, 'slider:depth'), s);
    for (let i = 2; i <= 100; i++) h.exec(setCmd(i, 'slider:depth'), s);
    expect(s.value).toBe(100);
    expect(h.undoSize).toBe(1);
    h.undo(s);
    expect(s.value).toBe(0);
  });

  it('coalescing_breaks_on_different_key', () => {
    const h = new History();
    const s = freshState();
    h.exec(setCmd(1, 'a'), s);
    h.exec(setCmd(2, 'b'), s);
    expect(h.undoSize).toBe(2);
    h.undo(s);
    expect(s.value).toBe(1);
    h.undo(s);
    expect(s.value).toBe(0);
  });

  it('coalescing_breaks_on_no_key', () => {
    const h = new History();
    const s = freshState();
    h.exec(setCmd(1, 'a'), s);
    h.exec(setCmd(2), s);
    expect(h.undoSize).toBe(2);
  });

  it('coalescing_breaks_after_500ms', () => {
    // Stub performance.now() rather than relying on fake timers — the
    // History uses performance.now() directly, which Vitest's fake-timers
    // mode doesn't intercept by default.
    const realNow = performance.now.bind(performance);
    let t = 1000;
    const spy = vi.spyOn(performance, 'now').mockImplementation(() => t);
    try {
      const h = new History();
      const s = freshState();
      h.exec(setCmd(1, 'slider:depth'), s);
      t += 600;
      h.exec(setCmd(2, 'slider:depth'), s);
      expect(h.undoSize).toBe(2);
    } finally {
      spy.mockRestore();
      void realNow;
    }
  });
});

describe('History transactions', () => {
  it('transaction_commits_as_single_step', () => {
    const h = new History();
    const s = freshState();
    h.beginTransaction('multi');
    h.exec(setCmd(1), s);
    h.exec(setCmd(2), s);
    h.exec(setCmd(3), s);
    h.commitTransaction();
    expect(s.value).toBe(3);
    expect(h.undoSize).toBe(1);
    expect(h.undoLabel()).toBe('multi');
    h.undo(s);
    expect(s.value).toBe(0);
  });

  it('transaction_cancel_discards', () => {
    const h = new History();
    const s = freshState();
    h.exec(setCmd(7), s);
    h.beginTransaction('aborted');
    h.exec(setCmd(8), s);
    h.exec(setCmd(9), s);
    expect(s.value).toBe(9);
    h.cancelTransaction(s);
    expect(s.value).toBe(7);
    expect(h.undoSize).toBe(1);
    expect(h.undoLabel()).toBe('set 7');
  });

  it('empty transaction does not push', () => {
    const h = new History();
    const s = freshState();
    h.beginTransaction('empty');
    h.commitTransaction();
    expect(h.undoSize).toBe(0);
  });

  it('nested transaction throws', () => {
    const h = new History();
    h.beginTransaction('outer');
    expect(() => h.beginTransaction('inner')).toThrow();
    h.commitTransaction();
  });

  it('redo of a committed transaction reapplies in order', () => {
    const h = new History();
    const s = freshState();
    h.beginTransaction('multi');
    h.exec(setCmd(1), s);
    h.exec(setCmd(2), s);
    h.commitTransaction();
    h.undo(s);
    expect(s.value).toBe(0);
    h.redo(s);
    expect(s.value).toBe(2);
  });
});

describe('History bounded depth', () => {
  it('bounded_max_depth', () => {
    const h = new History();
    const s = freshState();
    for (let i = 0; i < History.MAX_DEPTH + 50; i++) h.exec(setCmd(i), s);
    expect(h.undoSize).toBe(History.MAX_DEPTH);
  });
});

describe('History clear / version', () => {
  it('clear empties both stacks', () => {
    const h = new History();
    const s = freshState();
    h.exec(setCmd(1), s);
    h.exec(setCmd(2), s);
    h.undo(s);
    h.clear();
    expect(h.undoSize).toBe(0);
    expect(h.redoSize).toBe(0);
  });

  it('subscribe fires on every state change', () => {
    const h = new History();
    const s = freshState();
    let bumps = 0;
    h.subscribe(() => {
      bumps++;
    });
    h.exec(setCmd(1), s);
    h.exec(setCmd(2), s);
    h.undo(s);
    h.redo(s);
    h.clear();
    expect(bumps).toBeGreaterThanOrEqual(5);
  });
});
