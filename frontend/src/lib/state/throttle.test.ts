import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { Throttle } from './throttle';

describe('Throttle', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('runs the first call immediately (leading edge)', () => {
    const t = new Throttle(32);
    const calls: number[] = [];
    t.run('k', () => calls.push(1));
    expect(calls).toEqual([1]); // synchronous
  });

  it('collapses a burst to leading + one trailing with the latest value', () => {
    const t = new Throttle(32);
    const seen: number[] = [];
    t.run('k', () => seen.push(1)); // leading, fires now
    t.run('k', () => seen.push(2)); // within window → pending
    t.run('k', () => seen.push(3)); // overwrites pending
    expect(seen).toEqual([1]);
    vi.advanceTimersByTime(32);
    // Trailing edge fires only the latest (3), not 2.
    expect(seen).toEqual([1, 3]);
  });

  it('does not re-fire when nothing happened during the window', () => {
    const t = new Throttle(32);
    const seen: number[] = [];
    t.run('k', () => seen.push(1));
    vi.advanceTimersByTime(100);
    expect(seen).toEqual([1]); // no trailing call queued
    // A later call after the window is a fresh leading edge.
    t.run('k', () => seen.push(2));
    expect(seen).toEqual([1, 2]);
  });

  it('caps a sustained burst to ~once per interval', () => {
    const t = new Throttle(32);
    let n = 0;
    // 10 events spaced 10ms apart over 100ms.
    for (let i = 0; i < 10; i++) {
      t.run('k', () => (n += 1));
      vi.advanceTimersByTime(10);
    }
    vi.advanceTimersByTime(40);
    // Leading + a trailing every 32ms over ~100ms ⇒ far fewer than 10.
    expect(n).toBeGreaterThan(0);
    expect(n).toBeLessThan(10);
  });

  it('keeps distinct keys independent', () => {
    const t = new Throttle(32);
    const seen: string[] = [];
    t.run('x', () => seen.push('x1'));
    t.run('y', () => seen.push('y1'));
    t.run('x', () => seen.push('x2'));
    t.run('y', () => seen.push('y2'));
    expect(seen).toEqual(['x1', 'y1']); // both leading
    vi.advanceTimersByTime(32);
    expect(seen.slice(2).sort()).toEqual(['x2', 'y2']); // both trailing
  });

  it('flush applies pending trailing calls immediately', () => {
    const t = new Throttle(1000);
    const seen: number[] = [];
    t.run('k', () => seen.push(1));
    t.run('k', () => seen.push(2)); // pending
    t.flush();
    expect(seen).toEqual([1, 2]);
    // After flush the window is closed; next call is a fresh leading edge.
    t.run('k', () => seen.push(3));
    expect(seen).toEqual([1, 2, 3]);
  });

  it('cancel drops pending trailing calls', () => {
    const t = new Throttle(1000);
    const seen: number[] = [];
    t.run('k', () => seen.push(1));
    t.run('k', () => seen.push(2)); // pending
    t.cancel();
    vi.advanceTimersByTime(2000);
    expect(seen).toEqual([1]); // 2 was dropped
  });
});
