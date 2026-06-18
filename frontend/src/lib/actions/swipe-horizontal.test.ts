import { describe, it, expect } from 'vitest';
import { swipeDirection } from './swipe-horizontal';

describe('swipeDirection', () => {
  it('returns null for a tap (no travel)', () => {
    expect(swipeDirection(0, 0, 50)).toBeNull();
  });

  it('ignores mostly-vertical travel', () => {
    expect(swipeDirection(20, 60, 100)).toBeNull();
  });

  it('detects a deliberate horizontal drag past the trigger distance', () => {
    expect(swipeDirection(-40, 5, 600)).toBe('left');
    expect(swipeDirection(40, 5, 600)).toBe('right');
  });

  it('detects a short fast flick', () => {
    expect(swipeDirection(-20, 3, 120)).toBe('left');
    expect(swipeDirection(20, 3, 120)).toBe('right');
  });

  it('ignores a short slow drag (neither far nor fast)', () => {
    expect(swipeDirection(-20, 3, 800)).toBeNull();
  });
});
