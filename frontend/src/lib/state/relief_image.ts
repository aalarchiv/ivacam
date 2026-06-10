/// Grayscale-image → relief brightness grid. The pure
/// `grayscaleDownsample` (box-average + luminance, Y-flipped to world
/// orientation) is unit-tested; `decodeImageFile` is the thin browser
/// wrapper that decodes a File via canvas and feeds it through.

export interface BrightnessGrid {
  cols: number;
  rows: number;
  /// Row-major normalized brightness in [0, 1]. Row 0 is the WORLD-bottom
  /// (min Y) row — the source image is flipped so the relief isn't upside
  /// down (image row 0 is the top; world iy=0 is the bottom).
  brightness: number[];
}

/// Minimal RGBA image view — `data` is tightly packed RGBA bytes,
/// row-major top-down (the `ImageData` layout).
export interface RgbaImage {
  data: Uint8ClampedArray | number[];
  width: number;
  height: number;
}

/// Downsample an RGBA image to a normalized grayscale [0, 1] grid, capping
/// the longer side to `maxDim` (aspect preserved). Each target cell box-
/// averages the source pixels it covers; luminance is the Rec.601 weights.
/// The source is flipped vertically so grid row 0 maps to the world-bottom.
export function grayscaleDownsample(img: RgbaImage, maxDim = 256): BrightnessGrid {
  const sw = img.width;
  const sh = img.height;
  if (sw <= 0 || sh <= 0 || maxDim <= 0) {
    return { cols: 0, rows: 0, brightness: [] };
  }
  const scale = Math.min(1, maxDim / Math.max(sw, sh));
  const cols = Math.max(1, Math.round(sw * scale));
  const rows = Math.max(1, Math.round(sh * scale));
  const brightness = new Array<number>(cols * rows);
  for (let ty = 0; ty < rows; ty++) {
    // Flip Y: target row ty (world, bottom-up) reads source rows from the
    // bottom of the image (image is top-down).
    const sty = rows - 1 - ty;
    const sy0 = Math.floor((sty * sh) / rows);
    const sy1 = Math.max(sy0 + 1, Math.floor(((sty + 1) * sh) / rows));
    for (let tx = 0; tx < cols; tx++) {
      const sx0 = Math.floor((tx * sw) / cols);
      const sx1 = Math.max(sx0 + 1, Math.floor(((tx + 1) * sw) / cols));
      let sum = 0;
      let n = 0;
      for (let sy = sy0; sy < sy1; sy++) {
        for (let sx = sx0; sx < sx1; sx++) {
          const i = (sy * sw + sx) * 4;
          const r = img.data[i];
          const g = img.data[i + 1];
          const b = img.data[i + 2];
          sum += 0.299 * r + 0.587 * g + 0.114 * b;
          n++;
        }
      }
      brightness[ty * cols + tx] = n > 0 ? sum / n / 255 : 0;
    }
  }
  return { cols, rows, brightness };
}

/// Browser-only: decode an image File to a brightness grid via a canvas.
/// Throws if 2D canvas isn't available or the image fails to decode.
export async function decodeImageFile(file: File, maxDim = 256): Promise<BrightnessGrid> {
  const bitmap = await createImageBitmap(file);
  try {
    const canvas = document.createElement('canvas');
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('2D canvas context unavailable');
    ctx.drawImage(bitmap, 0, 0);
    const img = ctx.getImageData(0, 0, bitmap.width, bitmap.height);
    return grayscaleDownsample({ data: img.data, width: img.width, height: img.height }, maxDim);
  } finally {
    bitmap.close();
  }
}
