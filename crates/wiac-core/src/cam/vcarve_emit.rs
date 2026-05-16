//! V-Carve progressive-deepening Z emission.
//!
//! A V-bit has zero tip area, so a one-shot deep plunge into a wide
//! groove is mechanically impossible. Instead, the cutter walks the
//! medial-axis polyline at progressively greater depths (depth-per-pass
//! steps). On each forward sweep we cut one level deeper at every
//! visited point but never below the polyline's actual target Z; when
//! the polyline rises (the groove narrows), we reverse the sweep back
//! to where the previous level was last cut, then resume forward.
//!
//! This module returns a list of Z-stamped polylines (`Vec<Vec<(x, y,
//! z)>>`) ready to be turned into G-code by the standard polyline
//! emitter; it does NOT call into the post-processor itself, which
//! keeps the module decoupled from the gcode crate.

// # CAM/sim pedantic-lint exemptions
// V-carve emitter casts medial-axis sample indices (bounded by polyline
// length) to f64 for arc-fit input.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
)]


/// One waypoint along the emitted toolpath: absolute XYZ. Multiple
/// returned polylines are connected by G0 lifts to safe Z by the
/// caller.
pub type ZPolyline = Vec<(f64, f64, f64)>;

/// Build the full V-Carve sweep for a single per-point-Z polyline.
///
/// `axis` is `(x, y, z, r)` where `z <= 0` is the geometric target
/// depth at that point and `r` is the inscribed-circle radius (kept
/// only for diagnostics). `depth_per_pass` is the per-level step
/// magnitude (always positive — the cutter goes negative). The result
/// is a single polyline whose Z monotonically respects the ratchet:
/// every segment starts at the cut-Z reached by the previous segment
/// and never violates the polyline's actual `z`.
pub fn ratchet_emit(axis: &[(f64, f64, f64, f64)], depth_per_pass: f64) -> ZPolyline {
    if axis.len() < 2 {
        return Vec::new();
    }
    let dpp = depth_per_pass.abs().max(1e-6);

    // Densify the polyline at each Z-level crossing so the cutter
    // doesn't skip a level between two points whose original Z values
    // straddle it.
    let z_min = axis.iter().map(|&(_, _, z, _)| z).fold(0.0_f64, f64::min);
    let n_levels = ((-z_min) / dpp).ceil() as usize;
    let mut levels: Vec<f64> = (1..=n_levels).map(|i| -(i as f64) * dpp).collect();
    levels.push(z_min);
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
    levels.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    let mut dense: Vec<(f64, f64, f64)> = Vec::with_capacity(axis.len() * 2);
    for win in axis.windows(2) {
        let (ax, ay, az, _) = win[0];
        let (bx, by, bz, _) = win[1];
        if dense.is_empty() {
            dense.push((ax, ay, az));
        }
        // Insert a waypoint at every level in (min(az,bz), max(az,bz)).
        let (lo, hi) = if az < bz { (az, bz) } else { (bz, az) };
        for &lvl in &levels {
            if lvl > lo + 1e-9 && lvl < hi - 1e-9 {
                let t = (lvl - az) / (bz - az);
                if t > 1e-9 && t < 1.0 - 1e-9 {
                    dense.push((ax + t * (bx - ax), ay + t * (by - ay), lvl));
                }
            }
        }
        dense.push((bx, by, bz));
    }
    // Coalesce consecutive duplicates introduced by level-crossing
    // insertion at the segment boundaries.
    let mut compact: Vec<(f64, f64, f64)> = Vec::with_capacity(dense.len());
    for p in dense {
        if let Some(last) = compact.last() {
            if (last.0 - p.0).abs() < 1e-9
                && (last.1 - p.1).abs() < 1e-9
                && (last.2 - p.2).abs() < 1e-9
            {
                continue;
            }
        }
        compact.push(p);
    }
    if compact.len() < 2 {
        return Vec::new();
    }

    // Ratchet sweep. cut_z[i] tracks the Z already cut at point i —
    // initialized to 0 (top of stock). Each forward pass advances cut_z
    // toward dense[i].2 by at most one DPP. When the polyline rises
    // above the current cutting front, we step back to the previous
    // level's leading edge, then resume forward at the next deeper
    // level.
    let n = compact.len();
    let target_z: Vec<f64> = compact.iter().map(|&(_, _, z)| z).collect();
    let mut cut_z: Vec<f64> = vec![0.0; n];

    let mut path: Vec<(f64, f64, f64)> = Vec::new();
    path.push((compact[0].0, compact[0].1, 0.0));

    let mut current_level = -dpp;
    loop {
        let mut progressed = false;
        // Forward sweep at current_level: cut every point to
        // max(target_z[i], current_level), but only when that's deeper
        // than cut_z[i].
        for i in 0..n {
            let mut next_z = current_level.max(target_z[i]);
            if next_z > cut_z[i] {
                next_z = cut_z[i];
            }
            if next_z < cut_z[i] - 1e-9 {
                cut_z[i] = next_z;
                path.push((compact[i].0, compact[i].1, next_z));
                progressed = true;
            } else if i > 0 {
                // No new material at this point on this level; emit a
                // travel move at the current cut depth so the polyline
                // stays continuous.
                path.push((compact[i].0, compact[i].1, cut_z[i]));
            }
        }
        if !progressed {
            break;
        }
        // Reverse sweep back over the segment we just cut, at the same
        // (already-reached) depth, so the bit ends up at the start
        // ready for the next deeper level. This is the "ratchet"
        // backstroke. We don't lower Z further on this reverse pass —
        // it's a position move, not a cut.
        for i in (0..n).rev() {
            path.push((compact[i].0, compact[i].1, cut_z[i]));
        }
        current_level -= dpp;
        if current_level < target_z.iter().fold(0.0_f64, |a, &b| a.min(b)) - dpp {
            break;
        }
    }

    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_axis_returns_empty() {
        assert!(ratchet_emit(&[], 1.0).is_empty());
        assert!(ratchet_emit(&[(0.0, 0.0, -1.0, 0.5)], 1.0).is_empty());
    }

    #[test]
    fn single_pass_at_or_above_dpp() {
        // Polyline whose deepest point is shallower than DPP — should
        // cut to the target on the first level and stop.
        let axis = vec![(0.0, 0.0, -0.5, 0.25), (5.0, 0.0, -0.5, 0.25)];
        let path = ratchet_emit(&axis, 1.0);
        assert!(!path.is_empty());
        let z_min = path.iter().map(|t| t.2).fold(0.0_f64, f64::min);
        assert!((z_min + 0.5).abs() < 1e-6, "z_min = {z_min}");
    }

    #[test]
    fn deep_polyline_progresses_in_levels() {
        // Polyline reaching -3 mm with DPP 1 — expect at least 3
        // distinct Z-levels visited.
        let axis = vec![
            (0.0, 0.0, -3.0, 1.5),
            (5.0, 0.0, -3.0, 1.5),
            (10.0, 0.0, -3.0, 1.5),
        ];
        let path = ratchet_emit(&axis, 1.0);
        let mut levels: Vec<f64> = path.iter().map(|t| t.2).collect();
        levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
        levels.dedup_by(|a, b| (*a - *b).abs() < 0.05);
        assert!(
            levels.len() >= 3,
            "expected ≥3 distinct Z levels, got {levels:?}"
        );
    }
}
