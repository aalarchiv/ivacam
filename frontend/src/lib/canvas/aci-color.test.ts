import { describe, expect, it } from 'vitest';
import { resolveAci, hexToCss, ACI_FIXED } from './aci-color';
import { unpackFixtureColor, DEFAULT_FIXTURE_COLOR } from './fixture-color';

describe('resolveAci', () => {
  it('maps the fixed ACI palette to RGB hex', () => {
    expect(resolveAci(1)).toEqual({ kind: 'fixed', hex: 0xff0000 });
    expect(resolveAci(5)).toEqual({ kind: 'fixed', hex: 0x0000ff });
  });

  it('includes ACI 9 (the 3D copy used to omit it — 7iej.12)', () => {
    expect(resolveAci(9)).toEqual({ kind: 'fixed', hex: 0x808080 });
    expect(ACI_FIXED[9]).toBe(0x808080);
  });

  it('maps 7/256 to strong, 8 to muted, unmapped to faint tokens', () => {
    expect(resolveAci(7)).toEqual({ kind: 'token', token: '--text-strong', fallback: 0xe6e6e6 });
    expect(resolveAci(256)).toEqual({ kind: 'token', token: '--text-strong', fallback: 0xe6e6e6 });
    expect(resolveAci(8)).toEqual({ kind: 'token', token: '--text-muted', fallback: 0x888888 });
    expect(resolveAci(42)).toEqual({ kind: 'token', token: '--text-faint', fallback: 0xbbbbbb });
  });
});

describe('hexToCss', () => {
  it('formats 24-bit RGB as #rrggbb with zero-padding', () => {
    expect(hexToCss(0xff0000)).toBe('#ff0000');
    expect(hexToCss(0x000088)).toBe('#000088');
    expect(hexToCss(0x808080)).toBe('#808080');
  });
});

describe('unpackFixtureColor', () => {
  it('splits packed 0xRRGGBBAA into channels + rgb hex', () => {
    expect(unpackFixtureColor(0x11223344)).toEqual({
      r: 0x11,
      g: 0x22,
      b: 0x33,
      a: 0x44,
      hex: 0x112233,
    });
  });

  it('falls back to the default amber when null/undefined', () => {
    expect(unpackFixtureColor(null)).toEqual(unpackFixtureColor(DEFAULT_FIXTURE_COLOR));
    expect(unpackFixtureColor(undefined).hex).toBe(0xffa050);
  });
});
