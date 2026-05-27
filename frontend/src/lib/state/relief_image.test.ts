import { describe, expect, it } from 'vitest';
import { grayscaleDownsample, type RgbaImage } from './relief_image';

/// Build an RGBA image from a per-pixel grayscale value (0..255),
/// row-major top-down.
function grayImage(
  width: number,
  height: number,
  value: (x: number, y: number) => number,
): RgbaImage {
  const data = new Uint8ClampedArray(width * height * 4);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const v = value(x, y);
      const i = (y * width + x) * 4;
      data[i] = v;
      data[i + 1] = v;
      data[i + 2] = v;
      data[i + 3] = 255;
    }
  }
  return { data, width, height };
}

describe('grayscaleDownsample', () => {
  it('normalizes brightness to [0,1] and preserves dims under the cap', () => {
    // 2x2: black, white / mid, mid.
    const img = grayImage(2, 2, (x) => (x === 0 ? 0 : 255));
    const g = grayscaleDownsample(img, 256);
    expect(g.cols).toBe(2);
    expect(g.rows).toBe(2);
    // Y-flip: grid row 0 is the world-bottom (image's bottom row). Both
    // rows are identical here, so just check the brightness mapping.
    expect(g.brightness[0]).toBeCloseTo(0, 5); // x=0 → black
    expect(g.brightness[1]).toBeCloseTo(1, 5); // x=1 → white
  });

  it('flips Y so grid row 0 is the world-bottom', () => {
    // Top row (image y=0) white, bottom row (image y=1) black.
    const img = grayImage(1, 2, (_x, y) => (y === 0 ? 255 : 0));
    const g = grayscaleDownsample(img, 256);
    expect(g.cols).toBe(1);
    expect(g.rows).toBe(2);
    // Grid row 0 = world bottom = image bottom row = black.
    expect(g.brightness[0]).toBeCloseTo(0, 5);
    // Grid row 1 = world top = image top row = white.
    expect(g.brightness[1]).toBeCloseTo(1, 5);
  });

  it('caps the longer side to maxDim, preserving aspect', () => {
    const img = grayImage(100, 50, () => 128);
    const g = grayscaleDownsample(img, 10);
    expect(g.cols).toBe(10);
    expect(g.rows).toBe(5);
    expect(g.brightness).toHaveLength(50);
    // Uniform grey ≈ 128/255.
    expect(g.brightness[0]).toBeCloseTo(128 / 255, 3);
  });

  it('box-averages a 2x2 → 1x1 downsample', () => {
    // Values 0, 255, 255, 255 → mean 191.25 → /255.
    const img = grayImage(2, 2, (x, y) => (x === 0 && y === 0 ? 0 : 255));
    const g = grayscaleDownsample(img, 1);
    expect(g.cols).toBe(1);
    expect(g.rows).toBe(1);
    expect(g.brightness[0]).toBeCloseTo(191.25 / 255, 4);
  });

  it('handles a degenerate empty image', () => {
    const g = grayscaleDownsample({ data: [], width: 0, height: 0 }, 256);
    expect(g).toEqual({ cols: 0, rows: 0, brightness: [] });
  });
});
