import { describe, it, expect } from 'vitest';
import { deepEqual, reduceCloseAttempt } from './dialog-draft';

describe('deepEqual', () => {
  it('is invariant to key order (1xgj)', () => {
    expect(deepEqual({ a: 1, b: [2, 3] }, { b: [2, 3], a: 1 })).toBe(true);
  });
  it('distinguishes structure', () => {
    expect(deepEqual({ a: 1 }, { a: 1, b: undefined })).toBe(false);
    expect(deepEqual([1, 2], [2, 1])).toBe(false);
    expect(deepEqual(null, {})).toBe(false);
    expect(deepEqual({ a: { b: 2 } }, { a: { b: 3 } })).toBe(false);
    expect(deepEqual({ a: { b: 2 } }, { a: { b: 2 } })).toBe(true);
  });
});

describe('reduceCloseAttempt', () => {
  it('clean draft closes immediately', () => {
    expect(reduceCloseAttempt(false, false)).toEqual({ close: true, confirmingDiscard: false });
  });
  it('first dirty attempt arms the confirm bar instead of closing', () => {
    expect(reduceCloseAttempt(true, false)).toEqual({ close: false, confirmingDiscard: true });
  });
  it('second attempt (armed) confirms the discard', () => {
    expect(reduceCloseAttempt(true, true)).toEqual({ close: true, confirmingDiscard: false });
  });
});
