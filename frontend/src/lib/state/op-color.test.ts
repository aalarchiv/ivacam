import { describe, expect, it } from 'vitest';
import { opHue, opSourceHsl, opSourceCss } from './op-color';

describe('op-color', () => {
  it('opHue is deterministic and in [0,1)', () => {
    for (let id = 0; id < 50; id++) {
      const h = opHue(id);
      expect(h).toBeGreaterThanOrEqual(0);
      expect(h).toBeLessThan(1);
      expect(opHue(id)).toBe(h); // stable
    }
  });

  it('adjacent op ids land far apart on the wheel (golden-ratio spread)', () => {
    // The golden-ratio conjugate guarantees ~0.382 minimum separation
    // between consecutive ids — they never collide into a near-same hue.
    for (let id = 0; id < 50; id++) {
      const d = Math.abs(opHue(id) - opHue(id + 1));
      const wrapped = Math.min(d, 1 - d);
      expect(wrapped).toBeGreaterThan(0.2);
    }
  });

  it('emphasis bumps saturation + lightness', () => {
    const [, s0, l0] = opSourceHsl(3, false);
    const [, s1, l1] = opSourceHsl(3, true);
    expect(s1).toBeGreaterThan(s0);
    expect(l1).toBeGreaterThan(l0);
  });

  it('opSourceCss renders a valid hsl() string with the op hue', () => {
    const css = opSourceCss(3, false);
    expect(css).toMatch(/^hsl\(\d+ \d+% \d+%\)$/);
    expect(css).toContain(`${Math.round(opHue(3) * 360)} `);
  });
});
