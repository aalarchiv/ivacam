/// rt1.12: mirrors the Rust oracle in
/// `crates/wiac-core/src/cam/raster.rs` (the `tests` module). Each case
/// here is the TS twin of a Rust `#[test]` so the live preview stays
/// faithful to what the backend emitter actually burns.

import { describe, it, expect } from 'vitest';
import type { PowerCurve } from './op_types';
import {
  powerGrid,
  bayerIndices,
  maxPower,
  brightnessHistogram,
  powerGridToRgba,
  estimateBurnSeconds,
} from './raster_preview';

describe('powerGrid — linear', () => {
  it('dark burns hotter, white is min, mid is midpoint', () => {
    const c: PowerCurve = { kind: 'linear', min: 0, max: 1000 };
    const g = powerGrid(c, [0.0, 0.5, 1.0], 3, 1);
    expect(g[0]).toBe(1000); // black ⇒ max power
    expect(g[2]).toBe(0); // white ⇒ min power
    expect(g[1]).toBe(500); // mid grey ⇒ midpoint
  });

  it('clamps out-of-range brightness', () => {
    const c: PowerCurve = { kind: 'linear', min: 100, max: 900 };
    const g = powerGrid(c, [-0.5, 1.5], 2, 1);
    expect(g[0]).toBe(900); // below 0 clamps to black ⇒ max
    expect(g[1]).toBe(100); // above 1 clamps to white ⇒ min
  });
});

describe('powerGrid — threshold', () => {
  it('is binary on dark', () => {
    const c: PowerCurve = { kind: 'threshold', level: 0.5, power: 800 };
    const g = powerGrid(c, [0.2, 0.5, 0.8], 3, 1);
    expect(g).toEqual([800, 0, 0]); // below level burns, at/above is off
  });
});

describe('powerGrid — floyd_steinberg', () => {
  it('uniform mid-grey is about half on', () => {
    const cols = 16;
    const rows = 16;
    const field = new Array<number>(cols * rows).fill(0.5);
    const c: PowerCurve = { kind: 'floyd_steinberg', level: 0.5, power: 1 };
    const g = powerGrid(c, field, cols, rows);
    const on = g.filter((p) => p > 0).length;
    const total = cols * rows;
    expect(Math.abs(on - total / 2)).toBeLessThanOrEqual(total * 0.1);
  });

  it('extremes are solid', () => {
    const c: PowerCurve = { kind: 'floyd_steinberg', level: 0.5, power: 500 };
    const black = powerGrid(c, new Array(9).fill(0.0), 3, 3);
    expect(black.every((p) => p === 500)).toBe(true);
    const white = powerGrid(c, new Array(9).fill(1.0), 3, 3);
    expect(white.every((p) => p === 0)).toBe(true);
  });
});

describe('bayer', () => {
  it('indices match the classic 4×4 matrix', () => {
    const expected = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
    expect(bayerIndices(4)).toEqual(expected);
  });

  it('more on-pixels as the image darkens', () => {
    const cols = 8;
    const rows = 8;
    const onCount = (bright: number) => {
      const field = new Array<number>(cols * rows).fill(bright);
      const c: PowerCurve = { kind: 'bayer', matrixSize: 4, power: 1 };
      return powerGrid(c, field, cols, rows).filter((p) => p > 0).length;
    };
    const dark = onCount(0.2);
    const mid = onCount(0.5);
    const light = onCount(0.8);
    expect(dark).toBeGreaterThan(mid);
    expect(mid).toBeGreaterThan(light);
  });

  it('bad matrix size falls back to 4', () => {
    const field = Array.from({ length: 16 }, (_, i) => i / 16);
    const bad = powerGrid({ kind: 'bayer', matrixSize: 3, power: 1 }, field, 4, 4);
    const four = powerGrid({ kind: 'bayer', matrixSize: 4, power: 1 }, field, 4, 4);
    expect(bad).toEqual(four);
  });
});

describe('powerGrid — guards', () => {
  it('length mismatch yields empty', () => {
    const c: PowerCurve = { kind: 'threshold', level: 0.5, power: 1 };
    expect(powerGrid(c, [0.0, 1.0, 0.0], 2, 2)).toEqual([]);
    expect(powerGrid(c, [], 0, 0)).toEqual([]);
  });
});

describe('maxPower', () => {
  it('uses the hotter endpoint for linear (inverted allowed)', () => {
    expect(maxPower({ kind: 'linear', min: 0, max: 1000 })).toBe(1000);
    expect(maxPower({ kind: 'linear', min: 900, max: 100 })).toBe(900);
  });
  it('uses power for binary curves', () => {
    expect(maxPower({ kind: 'threshold', level: 0.5, power: 700 })).toBe(700);
    expect(maxPower({ kind: 'bayer', matrixSize: 4, power: 300 })).toBe(300);
  });
});

describe('powerGridToRgba', () => {
  it('peak power renders black, zero renders white, and flips Y', () => {
    // 2 rows × 1 col: world row 0 (bottom) = peak, row 1 (top) = 0.
    const peak = 1000;
    const rgba = powerGridToRgba([peak, 0], 1, 2, peak);
    // ImageData row 0 = top = world row 1 (power 0) ⇒ white.
    expect([rgba[0], rgba[1], rgba[2], rgba[3]]).toEqual([255, 255, 255, 255]);
    // ImageData row 1 = bottom = world row 0 (peak) ⇒ black.
    expect([rgba[4], rgba[5], rgba[6], rgba[7]]).toEqual([0, 0, 0, 255]);
  });
});

describe('estimateBurnSeconds', () => {
  it('returns 0 for degenerate input', () => {
    const base = {
      widthMm: 100,
      heightMm: 50,
      resolutionMm: 0.1,
      feedMmMin: 1000,
      link: 'lift_between' as const,
      overscanFactor: 0,
      scanDirection: 'along_x' as const,
    };
    expect(estimateBurnSeconds({ ...base, feedMmMin: 0 })).toBe(0);
    expect(estimateBurnSeconds({ ...base, resolutionMm: 0 })).toBe(0);
    expect(estimateBurnSeconds({ ...base, widthMm: 0 })).toBe(0);
  });

  it('lift_between costs more than bidirectional (return traverse)', () => {
    const base = {
      widthMm: 100,
      heightMm: 50,
      resolutionMm: 0.5,
      feedMmMin: 1000,
      overscanFactor: 0,
      scanDirection: 'along_x' as const,
    };
    const lift = estimateBurnSeconds({ ...base, link: 'lift_between' });
    const bidi = estimateBurnSeconds({ ...base, link: 'bidirectional' });
    expect(lift).toBeGreaterThan(bidi);
  });

  it('computes a concrete bidirectional time', () => {
    // 100×50 mm, 0.5 mm pitch ⇒ 100 rows along X, each 100 mm. Engrave
    // = 100*100 = 10000 mm; step-overs = 100*0.5 = 50 mm; no return.
    // At 1000 mm/min = 16.667 mm/s ⇒ 10050/16.667 ≈ 603 s.
    const s = estimateBurnSeconds({
      widthMm: 100,
      heightMm: 50,
      resolutionMm: 0.5,
      feedMmMin: 1000,
      link: 'bidirectional',
      overscanFactor: 0,
      scanDirection: 'along_x',
    });
    expect(s).toBeCloseTo((10000 + 50) / (1000 / 60), 1);
  });

  it('overscan increases the estimate', () => {
    const base = {
      widthMm: 100,
      heightMm: 50,
      resolutionMm: 0.5,
      feedMmMin: 1000,
      link: 'bidirectional' as const,
      scanDirection: 'along_x' as const,
    };
    expect(estimateBurnSeconds({ ...base, overscanFactor: 0.2 })).toBeGreaterThan(
      estimateBurnSeconds({ ...base, overscanFactor: 0 }),
    );
  });
});

describe('brightnessHistogram', () => {
  it('buckets values across [0,1] and catches exactly 1.0 in the last bin', () => {
    const hist = brightnessHistogram([0.0, 0.0, 0.5, 1.0], 4);
    expect(hist[0]).toBe(2); // two zeros
    expect(hist[2]).toBe(1); // 0.5 → bin 2
    expect(hist[3]).toBe(1); // 1.0 → last bin
    expect(hist.reduce((a, b) => a + b, 0)).toBe(4);
  });
});
