import { describe, it, expect } from 'vitest';
import {
  tierForWidth,
  isNarrowTier,
  PHONE_TABLET_BREAKPOINT_PX,
  TABLET_DESKTOP_BREAKPOINT_PX,
} from './layout';

describe('tierForWidth', () => {
  it('classifies sub-640px widths as phone', () => {
    expect(tierForWidth(0)).toBe('phone');
    expect(tierForWidth(360)).toBe('phone');
    expect(tierForWidth(PHONE_TABLET_BREAKPOINT_PX - 1)).toBe('phone');
  });

  it('classifies 640–1023px widths as tablet', () => {
    expect(tierForWidth(PHONE_TABLET_BREAKPOINT_PX)).toBe('tablet');
    expect(tierForWidth(768)).toBe('tablet');
    expect(tierForWidth(TABLET_DESKTOP_BREAKPOINT_PX - 1)).toBe('tablet');
  });

  it('classifies >=1024px widths as desktop', () => {
    expect(tierForWidth(TABLET_DESKTOP_BREAKPOINT_PX)).toBe('desktop');
    expect(tierForWidth(1280)).toBe('desktop');
    expect(tierForWidth(3840)).toBe('desktop');
  });

  it('uses lower-inclusive boundaries that match (min-width) queries', () => {
    // The exact breakpoint px belongs to the WIDER tier, so a
    // `(min-width: Npx)` match and this function never disagree.
    expect(tierForWidth(PHONE_TABLET_BREAKPOINT_PX)).not.toBe('phone');
    expect(tierForWidth(TABLET_DESKTOP_BREAKPOINT_PX)).not.toBe('tablet');
  });
});

describe('isNarrowTier', () => {
  it('is true for phone and tablet, false for desktop', () => {
    expect(isNarrowTier('phone')).toBe(true);
    expect(isNarrowTier('tablet')).toBe(true);
    expect(isNarrowTier('desktop')).toBe(false);
  });
});
