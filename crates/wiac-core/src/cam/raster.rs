//! Laser raster engraving — per-pixel power mapping (rt1.12, phase 1).
//!
//! Pure brightness-grid → laser-power (`S`) mapping. It operates on the
//! same normalized-brightness `[0, 1]` row-major grid that
//! [`crate::project::ReliefSource`] already carries (decoded
//! frontend-side), so raster engraving reuses that representation rather
//! than decoding images in Rust — the relief path maps brightness → Z,
//! this maps brightness → laser power.
//!
//! Convention: **dark pixels burn hotter**. A brightness of `0.0` (black)
//! is the most material removed (highest power); `1.0` (white) is none.
//!
//! This module is deliberately self-contained: it knows nothing about
//! ops, the wire format, or the gcode emitter. Later phases reference a
//! `ReliefSource` from a `RasterEngrave` op and walk the grid this module
//! produces into power-modulated scanlines.

/// How a pixel's brightness maps to a commanded laser power (`S` word).
///
/// `level` thresholds compare against brightness in `[0, 1]`. `power` is
/// the `S` value emitted for an "on" (burning) pixel; binary curves emit
/// `0` for "off".
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PowerCurve {
    /// Continuous greyscale. Power lerps from `max` at black
    /// (brightness 0) down to `min` at white (brightness 1), so darker
    /// pixels burn hotter. `min`/`max` are `S` values; `min > max` is
    /// allowed (it simply inverts the ramp).
    Linear { min: u32, max: u32 },
    /// Hard cutoff. Pixels darker than `level` burn at `power`; lighter
    /// pixels stay off.
    Threshold { level: f32, power: u32 },
    /// Floyd–Steinberg error-diffusion dither to on/off. Good tonal
    /// reproduction; errors propagate, so it can show alignment
    /// artifacts under bidirectional scanning (prefer [`PowerCurve::Bayer`]
    /// there). `level` is the binarization point; `power` is the on `S`.
    FloydSteinberg { level: f32, power: u32 },
    /// Ordered (Bayer) dither to on/off. No error propagation — fully
    /// local and predictable, so it's free of the boustrophedon
    /// alignment artifacts that hurt error-diffusion on lasers.
    /// `matrix_size` must be a power of two (2 / 4 / 8); other values
    /// fall back to 4.
    Bayer { matrix_size: u8, power: u32 },
}

impl PowerCurve {
    /// Map a normalized-brightness grid to a row-major grid of per-pixel
    /// laser-power (`S`) values, same length and order as the input.
    ///
    /// `brightness` is row-major, `brightness.len() == cols * rows`, each
    /// value in `[0, 1]` (values outside are clamped). A mismatched
    /// length yields an empty grid (the caller built it wrong).
    #[must_use]
    pub fn power_grid(&self, brightness: &[f32], cols: usize, rows: usize) -> Vec<u32> {
        if cols == 0 || rows == 0 || brightness.len() != cols * rows {
            return Vec::new();
        }
        match *self {
            PowerCurve::Linear { min, max } => brightness
                .iter()
                .map(|&b| lerp_power(min, max, b.clamp(0.0, 1.0)))
                .collect(),
            PowerCurve::Threshold { level, power } => brightness
                .iter()
                .map(|&b| if b.clamp(0.0, 1.0) < level { power } else { 0 })
                .collect(),
            PowerCurve::FloydSteinberg { level, power } => {
                floyd_steinberg(brightness, cols, rows, level, power)
            }
            PowerCurve::Bayer { matrix_size, power } => {
                bayer_dither(brightness, cols, rows, matrix_size, power)
            }
        }
    }
}

/// Power at brightness `b ∈ [0, 1]`: `max` at black (0), `min` at white
/// (1). Linear in between, rounded to the nearest integer `S`.
fn lerp_power(min: u32, max: u32, b: f32) -> u32 {
    let lo = min as f32;
    let hi = max as f32;
    // dark (b→0) ⇒ hi (max power); white (b→1) ⇒ lo (min power).
    (hi + (lo - hi) * b).round().max(0.0) as u32
}

/// Floyd–Steinberg error diffusion, left-to-right then top-to-bottom.
/// Quantizes each (error-adjusted) pixel to black/white at `level` and
/// pushes the residual to the not-yet-visited neighbours with the
/// classic `7/3/5/1` (÷16) weights. Black pixels emit `power`, white `0`.
fn floyd_steinberg(
    brightness: &[f32],
    cols: usize,
    rows: usize,
    level: f32,
    power: u32,
) -> Vec<u32> {
    // Mutable working buffer so we can accumulate diffused error.
    let mut buf: Vec<f32> = brightness.iter().map(|&b| b.clamp(0.0, 1.0)).collect();
    let mut out = vec![0u32; cols * rows];
    for y in 0..rows {
        for x in 0..cols {
            let i = y * cols + x;
            let old = buf[i];
            // Quantize: below `level` ⇒ black (burn), else white (off).
            let black = old < level;
            let quant = if black { 0.0 } else { 1.0 };
            out[i] = if black { power } else { 0 };
            let err = old - quant;
            if x + 1 < cols {
                buf[i + 1] += err * (7.0 / 16.0);
            }
            if y + 1 < rows {
                if x > 0 {
                    buf[i + cols - 1] += err * (3.0 / 16.0);
                }
                buf[i + cols] += err * (5.0 / 16.0);
                if x + 1 < cols {
                    buf[i + cols + 1] += err * (1.0 / 16.0);
                }
            }
        }
    }
    out
}

/// Ordered Bayer dither. A pixel burns when its brightness is below the
/// tile-local Bayer threshold, so darker regions cross more thresholds
/// and burn more pixels — predictable, error-free, tileable.
fn bayer_dither(
    brightness: &[f32],
    cols: usize,
    rows: usize,
    matrix_size: u8,
    power: u32,
) -> Vec<u32> {
    let n = match matrix_size {
        2 | 4 | 8 => matrix_size as usize,
        _ => 4,
    };
    let matrix = bayer_thresholds(n);
    let mut out = vec![0u32; cols * rows];
    for y in 0..rows {
        for x in 0..cols {
            let i = y * cols + x;
            let t = matrix[(y % n) * n + (x % n)];
            out[i] = if brightness[i].clamp(0.0, 1.0) < t {
                power
            } else {
                0
            };
        }
    }
    out
}

/// Normalized Bayer threshold map of side `n` (a power of two), row-major,
/// each value in `(0, 1)`. Built by the standard recursive doubling
/// `M(2n) = [[4M+0, 4M+2], [4M+3, 4M+1]]` then mapped to
/// `(index + 0.5) / n²` so thresholds sit at cell centres.
fn bayer_thresholds(n: usize) -> Vec<f32> {
    let idx = bayer_indices(n);
    let denom = (n * n) as f32;
    idx.into_iter().map(|v| (v as f32 + 0.5) / denom).collect()
}

/// Recursive integer Bayer index matrix (values `0..n²`), row-major.
fn bayer_indices(n: usize) -> Vec<u32> {
    if n <= 1 {
        return vec![0];
    }
    let h = n / 2;
    let half = bayer_indices(h);
    let mut m = vec![0u32; n * n];
    for y in 0..h {
        for x in 0..h {
            let v = half[y * h + x];
            m[y * n + x] = 4 * v;
            m[y * n + (x + h)] = 4 * v + 2;
            m[(y + h) * n + x] = 4 * v + 3;
            m[(y + h) * n + (x + h)] = 4 * v + 1;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_dark_burns_hotter_white_is_min() {
        let c = PowerCurve::Linear { min: 0, max: 1000 };
        // black, mid, white.
        let g = c.power_grid(&[0.0, 0.5, 1.0], 3, 1);
        assert_eq!(g[0], 1000, "black ⇒ max power");
        assert_eq!(g[2], 0, "white ⇒ min power");
        assert_eq!(g[1], 500, "mid grey ⇒ midpoint");
    }

    #[test]
    fn linear_clamps_out_of_range_brightness() {
        let c = PowerCurve::Linear { min: 100, max: 900 };
        let g = c.power_grid(&[-0.5, 1.5], 2, 1);
        assert_eq!(g[0], 900, "below 0 clamps to black ⇒ max");
        assert_eq!(g[1], 100, "above 1 clamps to white ⇒ min");
    }

    #[test]
    fn threshold_is_binary_on_dark() {
        let c = PowerCurve::Threshold {
            level: 0.5,
            power: 800,
        };
        let g = c.power_grid(&[0.2, 0.5, 0.8], 3, 1);
        assert_eq!(g, vec![800, 0, 0], "below level burns, at/above is off");
    }

    #[test]
    fn floyd_steinberg_on_uniform_mid_grey_is_about_half_on() {
        // A uniform 50% field at level 0.5 should dither to ~half the
        // pixels burning. Error diffusion makes it close to exact.
        let (cols, rows) = (16, 16);
        let field = vec![0.5f32; cols * rows];
        let c = PowerCurve::FloydSteinberg {
            level: 0.5,
            power: 1,
        };
        let g = c.power_grid(&field, cols, rows);
        let on = g.iter().filter(|&&p| p > 0).count();
        let total = cols * rows;
        assert!(
            (on as f64 - total as f64 / 2.0).abs() <= total as f64 * 0.1,
            "expected ~50% on, got {on}/{total}"
        );
    }

    #[test]
    fn floyd_steinberg_extremes_are_solid() {
        let c = PowerCurve::FloydSteinberg {
            level: 0.5,
            power: 500,
        };
        let black = c.power_grid(&[0.0; 9], 3, 3);
        assert!(black.iter().all(|&p| p == 500), "all-black ⇒ all on");
        let white = c.power_grid(&[1.0; 9], 3, 3);
        assert!(white.iter().all(|&p| p == 0), "all-white ⇒ all off");
    }

    #[test]
    fn bayer_indices_match_classic_4x4() {
        // The canonical 4×4 Bayer index matrix (row-major).
        let expected = vec![0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
        assert_eq!(bayer_indices(4), expected);
    }

    #[test]
    fn bayer_dither_more_on_pixels_as_image_darkens() {
        let (cols, rows) = (8, 8);
        let on_count = |bright: f32| {
            let field = vec![bright; cols * rows];
            let c = PowerCurve::Bayer {
                matrix_size: 4,
                power: 1,
            };
            c.power_grid(&field, cols, rows)
                .iter()
                .filter(|&&p| p > 0)
                .count()
        };
        let dark = on_count(0.2);
        let mid = on_count(0.5);
        let light = on_count(0.8);
        assert!(
            dark > mid && mid > light,
            "darker ⇒ more burn: {dark} > {mid} > {light}"
        );
    }

    #[test]
    fn bayer_bad_matrix_size_falls_back_to_4() {
        // size 3 isn't a power of two ⇒ behaves like size 4.
        let field: Vec<f32> = (0..16).map(|i| i as f32 / 16.0).collect();
        let bad = PowerCurve::Bayer {
            matrix_size: 3,
            power: 1,
        }
        .power_grid(&field, 4, 4);
        let four = PowerCurve::Bayer {
            matrix_size: 4,
            power: 1,
        }
        .power_grid(&field, 4, 4);
        assert_eq!(bad, four);
    }

    #[test]
    fn length_mismatch_yields_empty() {
        let c = PowerCurve::Threshold {
            level: 0.5,
            power: 1,
        };
        assert!(c.power_grid(&[0.0, 1.0, 0.0], 2, 2).is_empty());
        assert!(c.power_grid(&[], 0, 0).is_empty());
    }
}
