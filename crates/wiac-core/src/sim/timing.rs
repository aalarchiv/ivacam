//! Acceleration- and jerk-aware program-time estimation.
//!
//! The naive `path_length / feedrate` underpredicts real run-time by
//! 1.5–3× on hobby machines because every short segment forces an
//! accel/decel cycle that never reaches the commanded feed. This module
//! integrates a trapezoidal motion profile per segment with a
//! look-ahead pass that lowers the junction speed at corners, mirroring
//! what `LinuxCNC` / GRBL do at runtime.
//!
//! Algorithm (v1, trapezoidal — S-curve refinement is Phase 2):
//!   1. Resolve length, unit direction, max feed for each segment.
//!   2. Look-ahead: junction speed `v_j = sqrt(a · min(len_i, len_{i+1}) ·
//!      (1 + cos θ) / 2)`, clamped to `min(feed_i`, feed_{i+1}). cos = +1
//!      (collinear) saturates at feed; cos = -1 (180° reversal) → 0.
//!   3. Trapezoidal profile per segment with the resolved entry/exit
//!      speeds (collapses to a triangle when `s_acc + s_dec > s`).
//!   4. Aggregate plus tool-change time and spindle pause.
//!
//! Per-axis accel for diagonal moves: `a_eff = min(a_x/|dx|, a_y/|dy|,
//! a_z/|dz|)` over the unit-direction components > epsilon. Tie-break is
//! "smallest wins". Look-ahead is unbounded — full toolpath in memory.

// # CAM/sim pedantic-lint exemptions
// Test helpers use parallel `axes_x`/`axes_y`/`axes_z` names that enumerate
// the three axes of an `AxisLimits` triple.
#![allow(clippy::similar_names)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::setup::{AxisLimits, MachineConfig};
use crate::gcode::preview::{MoveKind, Pose3, ToolpathSegment};

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimeEstimate {
    pub total_s: f64,
    pub cut_s: f64,
    pub rapid_s: f64,
    pub plunge_s: f64,
    pub retract_s: f64,
    pub arc_s: f64,
    pub toolchange_s: f64,
    pub spindle_warmup_s: f64,
}

const DEFAULT_ACCEL_MM_S2: f64 = 250.0;
const DEFAULT_RAPID_MM_MIN: f64 = 5000.0;
const DIR_EPS: f64 = 1e-6;

/// Public hook for the pipeline: reads the emitted gcode to recover
/// modal F values for each segment, then estimates total run-time.
/// `tool_changes` is the count of tool-changes M6 events (produces
/// `n * machine.toolchange_s`); `spindle_warmup_s` is summed across all
/// `tool.pause` per used tool.
#[must_use]
pub fn estimate_from_gcode(
    gcode: &str,
    segments: &[ToolpathSegment],
    machine: &MachineConfig,
    tool_changes: u32,
    spindle_warmup_s: f64,
) -> TimeEstimate {
    let feeds = feeds_per_segment(gcode, segments);
    estimate(segments, &feeds, machine, tool_changes, spindle_warmup_s)
}

/// Per-op plunge / feed limits sourced from the project's tool library
/// (v7f5). The timing estimator caps each segment's feedrate to the
/// declared `plunge_rate` for [`MoveKind::Plunge`] segments and to
/// `feed_rate` for [`MoveKind::Cut`] / [`MoveKind::Arc`] segments — both
/// from below modal F (so a post that emits a single `F<feed>` line at
/// the start of an op still uses the slower plunge rate for the plunge
/// segment, instead of crediting the plunge with the cutting feed).
///
/// `op_id == 0` matches segments emitted before any `; OP <n>` marker;
/// pre-marker geometry is rare in practice but the lookup falls back to
/// the modal-F value when no entry is found.
#[derive(Debug, Clone, Copy)]
pub struct OpRates {
    pub op_id: u32,
    /// Plunge feedrate (rate_v) for this op's tool, mm/min. 0 = use modal F.
    pub plunge_rate_mm_min: u32,
    /// Cutting feedrate (rate_h) for this op's tool, mm/min. 0 = use modal F.
    pub feed_rate_mm_min: u32,
}

/// Like [`estimate_from_gcode`] but also clamps per-segment feeds to
/// the tool's declared plunge/cut rates (v7f5). `op_rates` is a small
/// lookup of `op_id → (plunge_rate, feed_rate)`; segments whose `op_id`
/// isn't present fall through to the modal-F behavior.
#[must_use]
pub fn estimate_from_gcode_with_rates(
    gcode: &str,
    segments: &[ToolpathSegment],
    machine: &MachineConfig,
    tool_changes: u32,
    spindle_warmup_s: f64,
    op_rates: &[OpRates],
) -> TimeEstimate {
    let feeds = feeds_per_segment(gcode, segments);
    let clamped = clamp_feeds_by_kind(segments, &feeds, op_rates);
    estimate(segments, &clamped, machine, tool_changes, spindle_warmup_s)
}

/// Clamp per-segment modal feeds against the tool's declared rates so a
/// post that wrote a single F at the start of the op (typical for short
/// hand-written gcode) doesn't credit the plunge with the cutting feed.
fn clamp_feeds_by_kind(
    segments: &[ToolpathSegment],
    feeds_mm_min: &[f64],
    op_rates: &[OpRates],
) -> Vec<f64> {
    if op_rates.is_empty() {
        return feeds_mm_min.to_vec();
    }
    segments
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            let modal = feeds_mm_min.get(i).copied().unwrap_or(0.0);
            let Some(rates) = op_rates.iter().find(|r| r.op_id == seg.op_id) else {
                return modal;
            };
            let cap_mm_min = match seg.kind {
                MoveKind::Plunge => rates.plunge_rate_mm_min,
                MoveKind::Cut | MoveKind::Arc => rates.feed_rate_mm_min,
                MoveKind::Rapid | MoveKind::Retract => 0,
            };
            if cap_mm_min == 0 {
                return modal;
            }
            let cap = f64::from(cap_mm_min);
            // The cap is the tool's authoritative rate for this kind.
            // When modal F was set HIGHER (post wrote F<cut_feed> and the
            // plunge inherited it), the tool's rate wins. When modal F
            // was set LOWER (operator override / canned-cycle plunge with
            // its own F), the modal value wins. Floor of 0 on modal
            // makes "no F set yet" use the cap.
            if modal <= 0.0 {
                cap
            } else {
                modal.min(cap)
            }
        })
        .collect()
}

/// Core entry point: takes pre-resolved per-segment feedrates (mm/min)
/// and produces a `TimeEstimate`. `tool_changes` and `spindle_warmup_s`
/// are added on top of motion time.
#[must_use]
pub fn estimate(
    segments: &[ToolpathSegment],
    feeds_mm_min: &[f64],
    machine: &MachineConfig,
    tool_changes: u32,
    spindle_warmup_s: f64,
) -> TimeEstimate {
    if !machine.use_kinematic_time_estimate {
        return estimate_naive(
            segments,
            feeds_mm_min,
            machine,
            tool_changes,
            spindle_warmup_s,
        );
    }
    estimate_trapezoidal(
        segments,
        feeds_mm_min,
        machine,
        tool_changes,
        spindle_warmup_s,
    )
}

fn estimate_trapezoidal(
    segments: &[ToolpathSegment],
    feeds_mm_min: &[f64],
    machine: &MachineConfig,
    tool_changes: u32,
    spindle_warmup_s: f64,
) -> TimeEstimate {
    let accel = machine
        .accel
        .unwrap_or(AxisLimits::uniform(DEFAULT_ACCEL_MM_S2));
    let rapid_mm_min = machine.rapid_speed.unwrap_or(DEFAULT_RAPID_MM_MIN);

    let n = segments.len();
    let mut lengths = vec![0.0_f64; n];
    let mut dirs = vec![[0.0_f64; 3]; n];
    let mut feeds = vec![0.0_f64; n];
    let mut accels = vec![0.0_f64; n];

    for (i, seg) in segments.iter().enumerate() {
        let (len, dir) = length_and_dir(seg.from, seg.to);
        lengths[i] = len;
        dirs[i] = dir;
        let feed_mm_min = match seg.kind {
            MoveKind::Rapid => rapid_mm_min,
            _ => feeds_mm_min.get(i).copied().unwrap_or(0.0).max(1.0),
        };
        feeds[i] = feed_mm_min / 60.0;
        accels[i] = effective_accel(dir, accel);
    }

    let mut v_in = vec![0.0_f64; n];
    let mut v_out = vec![0.0_f64; n];
    for i in 0..n {
        if i + 1 < n {
            let (len_i, len_j) = (lengths[i], lengths[i + 1]);
            let cos_t = dot(dirs[i], dirs[i + 1]);
            let cos_clamped = cos_t.clamp(-1.0, 1.0);
            let a_min = accels[i].min(accels[i + 1]).max(0.0);
            let l_min = len_i.min(len_j);
            let v_j = (a_min * l_min * (1.0 + cos_clamped) * 0.5).max(0.0).sqrt();
            let v_j = v_j.min(feeds[i]).min(feeds[i + 1]);
            v_out[i] = v_j;
            v_in[i + 1] = v_j;
        }
    }
    if n > 0 {
        v_in[0] = 0.0;
        v_out[n - 1] = 0.0;
    }

    // Backward pass: clamp v_in to what can be reached from v_out under
    // the segment's accel limit. This ensures every segment can decel
    // to its programmed v_out without violating constraints.
    for i in (0..n).rev() {
        let a = accels[i].max(1e-6);
        let v_out_i = v_out[i];
        let v_in_max = (v_out_i * v_out_i + 2.0 * a * lengths[i]).max(0.0).sqrt();
        if v_in[i] > v_in_max {
            v_in[i] = v_in_max;
            if i > 0 {
                v_out[i - 1] = v_in_max;
            }
        }
    }
    // Forward pass: clamp v_out to what can be reached from v_in.
    for i in 0..n {
        let a = accels[i].max(1e-6);
        let v_in_i = v_in[i];
        let v_out_max = (v_in_i * v_in_i + 2.0 * a * lengths[i]).max(0.0).sqrt();
        if v_out[i] > v_out_max {
            v_out[i] = v_out_max;
            if i + 1 < n {
                v_in[i + 1] = v_out_max;
            }
        }
    }

    let mut cut_s = 0.0;
    let mut rapid_s = 0.0;
    let mut plunge_s = 0.0;
    let mut retract_s = 0.0;
    let mut arc_s = 0.0;
    for i in 0..n {
        let dt = trapezoidal_time(lengths[i], v_in[i], v_out[i], feeds[i], accels[i]);
        match segments[i].kind {
            MoveKind::Rapid => rapid_s += dt,
            MoveKind::Cut => cut_s += dt,
            MoveKind::Plunge => plunge_s += dt,
            MoveKind::Retract => retract_s += dt,
            MoveKind::Arc => arc_s += dt,
        }
    }

    let toolchange_s = f64::from(tool_changes) * machine.toolchange_s;
    let total_s = cut_s + rapid_s + plunge_s + retract_s + arc_s + toolchange_s + spindle_warmup_s;
    TimeEstimate {
        total_s,
        cut_s,
        rapid_s,
        plunge_s,
        retract_s,
        arc_s,
        toolchange_s,
        spindle_warmup_s,
    }
}

fn estimate_naive(
    segments: &[ToolpathSegment],
    feeds_mm_min: &[f64],
    machine: &MachineConfig,
    tool_changes: u32,
    spindle_warmup_s: f64,
) -> TimeEstimate {
    let rapid_mm_min = machine.rapid_speed.unwrap_or(DEFAULT_RAPID_MM_MIN);
    let mut cut_s = 0.0;
    let mut rapid_s = 0.0;
    let mut plunge_s = 0.0;
    let mut retract_s = 0.0;
    let mut arc_s = 0.0;
    for (i, seg) in segments.iter().enumerate() {
        let (len, _) = length_and_dir(seg.from, seg.to);
        let feed_mm_min = match seg.kind {
            MoveKind::Rapid => rapid_mm_min,
            _ => feeds_mm_min.get(i).copied().unwrap_or(0.0).max(1.0),
        };
        let v = feed_mm_min / 60.0;
        let dt = if v > 0.0 { len / v } else { 0.0 };
        match seg.kind {
            MoveKind::Rapid => rapid_s += dt,
            MoveKind::Cut => cut_s += dt,
            MoveKind::Plunge => plunge_s += dt,
            MoveKind::Retract => retract_s += dt,
            MoveKind::Arc => arc_s += dt,
        }
    }
    let toolchange_s = f64::from(tool_changes) * machine.toolchange_s;
    let total_s = cut_s + rapid_s + plunge_s + retract_s + arc_s + toolchange_s + spindle_warmup_s;
    TimeEstimate {
        total_s,
        cut_s,
        rapid_s,
        plunge_s,
        retract_s,
        arc_s,
        toolchange_s,
        spindle_warmup_s,
    }
}

fn length_and_dir(from: Pose3, to: Pose3) -> (f64, [f64; 3]) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let dz = to.z - from.z;
    let len = (dx * dx + dy * dy + dz * dz).sqrt();
    if len < 1e-12 {
        return (0.0, [0.0, 0.0, 0.0]);
    }
    (len, [dx / len, dy / len, dz / len])
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Per-axis accel reduction for a diagonal move. The bound for axis k is
/// `a_k / |dir_k|`; the move's effective accel is the smallest such
/// bound across active axes (those with |`dir_k`| > `DIR_EPS`).
fn effective_accel(dir: [f64; 3], a: AxisLimits) -> f64 {
    let limits = [a.x, a.y, a.z];
    let mut best = f64::INFINITY;
    for k in 0..3 {
        let d = dir[k].abs();
        if d > DIR_EPS {
            let bound = limits[k] / d;
            if bound < best {
                best = bound;
            }
        }
    }
    if best.is_finite() {
        best
    } else {
        DEFAULT_ACCEL_MM_S2
    }
}

/// Time for a single segment under a trapezoidal profile.
/// `s` length, `v0` entry, `v1` exit, `vf` cruise cap, `a` accel.
fn trapezoidal_time(s: f64, v0: f64, v1: f64, vf: f64, a: f64) -> f64 {
    if s <= 1e-12 {
        return 0.0;
    }
    let a = a.max(1e-6);
    let vf = vf.max(v0.max(v1));
    let s_acc = ((vf * vf) - (v0 * v0)) / (2.0 * a);
    let s_dec = ((vf * vf) - (v1 * v1)) / (2.0 * a);
    if s_acc + s_dec <= s + 1e-12 {
        let t_acc = (vf - v0) / a;
        let t_dec = (vf - v1) / a;
        let s_cruise = (s - s_acc - s_dec).max(0.0);
        let t_cruise = if vf > 0.0 { s_cruise / vf } else { 0.0 };
        return t_acc + t_cruise + t_dec;
    }
    // Triangular profile: solve for the peak we actually reach.
    let vp_sq = a * s + 0.5 * (v0 * v0 + v1 * v1);
    let vp = vp_sq.max(v0.max(v1)).sqrt();
    (vp - v0) / a + (vp - v1) / a
}

/// Walk gcode in lockstep with `interpret_with_index`'s segment output to
/// recover the F value modal at each segment. Segments produced by the
/// arc tessellator share the F of the originating G2/G3 line.
fn feeds_per_segment(gcode: &str, segments: &[ToolpathSegment]) -> Vec<f64> {
    // gcode lines are 1..n contiguous, so a dense Vec<f64> indexed by
    // line_no is one allocation and O(1) lookup — no hashing cost.
    // Index 0 stays at 0.0 since gcode lines are 1-based.
    let line_count = gcode.lines().count();
    let mut feed_by_line: Vec<f64> = vec![0.0; line_count + 1];
    let mut current: f64 = 0.0;
    for (idx0, raw) in gcode.lines().enumerate() {
        let line = strip_comment(raw);
        for tok in line.split_whitespace() {
            if let Some(rest) = tok.strip_prefix(['F', 'f']) {
                if let Ok(v) = rest.parse::<f64>() {
                    if v > 0.0 {
                        current = v;
                    }
                }
            }
        }
        feed_by_line[idx0 + 1] = current;
    }
    segments
        .iter()
        .map(|s| {
            let i = s.gcode_line as usize;
            if i < feed_by_line.len() {
                feed_by_line[i]
            } else {
                0.0
            }
        })
        .collect()
}

fn strip_comment(line: &str) -> String {
    let mut out = String::new();
    let mut in_paren = false;
    for ch in line.chars() {
        if ch == '(' {
            in_paren = true;
            continue;
        }
        if ch == ')' {
            in_paren = false;
            continue;
        }
        if ch == ';' {
            break;
        }
        if !in_paren {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
// `assert_eq!(feed, 800.0)` etc. — the value 800 came from a gcode literal
// "F800" parsed verbatim, so exact equality is the right check.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn cut_seg(from: (f64, f64, f64), to: (f64, f64, f64)) -> ToolpathSegment {
        ToolpathSegment {
            from: Pose3 {
                x: from.0,
                y: from.1,
                z: from.2,
            },
            to: Pose3 {
                x: to.0,
                y: to.1,
                z: to.2,
            },
            kind: MoveKind::Cut,
            gcode_line: 0,
            op_id: 0,
        }
    }

    fn machine() -> MachineConfig {
        MachineConfig {
            accel: Some(AxisLimits::uniform(250.0)),
            ..MachineConfig::default()
        }
    }

    #[test]
    fn single_segment_trapezoid() {
        // 100 mm at 1000 mm/min, accel 250 mm/s². Reference ≈ 6.07 s.
        let segs = vec![cut_seg((0.0, 0.0, 0.0), (100.0, 0.0, 0.0))];
        let feeds = vec![1000.0];
        let est = estimate(&segs, &feeds, &machine(), 0, 0.0);
        let expected = 6.07;
        assert!(
            (est.total_s - expected).abs() / expected < 0.01,
            "got {} expected ~{}",
            est.total_s,
            expected
        );
        assert!((est.cut_s - est.total_s).abs() < 1e-9);
    }

    #[test]
    fn two_collinear_segments_match_single() {
        let single = vec![cut_seg((0.0, 0.0, 0.0), (100.0, 0.0, 0.0))];
        let split = vec![
            cut_seg((0.0, 0.0, 0.0), (50.0, 0.0, 0.0)),
            cut_seg((50.0, 0.0, 0.0), (100.0, 0.0, 0.0)),
        ];
        let m = machine();
        let a = estimate(&single, &[1000.0], &m, 0, 0.0);
        let b = estimate(&split, &[1000.0, 1000.0], &m, 0, 0.0);
        assert!(
            (a.total_s - b.total_s).abs() < 1e-3,
            "collinear split should match: {} vs {}",
            a.total_s,
            b.total_s,
        );
    }

    #[test]
    fn ninety_degree_corner_no_slowdown_when_feed_below_clamp() {
        // 50 mm + 50 mm at 1000 mm/min around a 90° corner.
        // v_j = sqrt(250 * 50 * 0.5) ≈ 79 mm/s ≈ 4750 mm/min;
        // clamped to feed (1000 mm/min ≈ 16.67 mm/s) ⇒ no slowdown.
        let split = vec![
            cut_seg((0.0, 0.0, 0.0), (50.0, 0.0, 0.0)),
            cut_seg((50.0, 0.0, 0.0), (50.0, 50.0, 0.0)),
        ];
        let m = machine();
        let a = estimate(&split, &[1000.0, 1000.0], &m, 0, 0.0);
        // Compare to two collinear 50 mm segments: should be the same to within rounding.
        let collinear = vec![
            cut_seg((0.0, 0.0, 0.0), (50.0, 0.0, 0.0)),
            cut_seg((50.0, 0.0, 0.0), (100.0, 0.0, 0.0)),
        ];
        let b = estimate(&collinear, &[1000.0, 1000.0], &m, 0, 0.0);
        assert!(
            (a.total_s - b.total_s).abs() < 1e-3,
            "corner under feed-clamp should match collinear: {} vs {}",
            a.total_s,
            b.total_s,
        );
    }

    #[test]
    fn ninety_degree_corner_slows_vs_collinear_at_high_feed() {
        // At 5000 mm/min (≈83.3 mm/s), v_j ≈ sqrt(250·50·0.5) ≈ 79 mm/s
        // < feed, so the junction is the binding constraint. The 90°
        // corner takes longer than the collinear-equivalent path (where
        // junction = feed and the cutter cruises through).
        let m = machine();
        let split_corner = vec![
            cut_seg((0.0, 0.0, 0.0), (50.0, 0.0, 0.0)),
            cut_seg((50.0, 0.0, 0.0), (50.0, 50.0, 0.0)),
        ];
        let split_collinear = vec![
            cut_seg((0.0, 0.0, 0.0), (50.0, 0.0, 0.0)),
            cut_seg((50.0, 0.0, 0.0), (100.0, 0.0, 0.0)),
        ];
        let est_corner = estimate(&split_corner, &[5000.0, 5000.0], &m, 0, 0.0);
        let est_straight = estimate(&split_collinear, &[5000.0, 5000.0], &m, 0, 0.0);
        assert!(
            est_corner.total_s > est_straight.total_s,
            "corner should slow down vs collinear: corner {} vs straight {}",
            est_corner.total_s,
            est_straight.total_s,
        );
    }

    #[test]
    fn triangular_profile_short_segment() {
        // 1 mm at 5000 mm/min, accel 250 mm/s² — segment too short to
        // reach commanded feed, so triangular. Peak ≈ sqrt(250*1) ≈
        // 15.8 mm/s; time ≈ 2*15.8/250 ≈ 0.126 s.
        let segs = vec![cut_seg((0.0, 0.0, 0.0), (1.0, 0.0, 0.0))];
        let est = estimate(&segs, &[5000.0], &machine(), 0, 0.0);
        let expected = 0.1265;
        assert!(
            (est.total_s - expected).abs() / expected < 0.02,
            "triangular: got {} expected ~{}",
            est.total_s,
            expected
        );
    }

    #[test]
    fn machine_config_round_trips_kinematic_fields() {
        let m = MachineConfig {
            accel: Some(AxisLimits {
                x: 300.0,
                y: 280.0,
                z: 120.0,
            }),
            jerk: Some(AxisLimits {
                x: 5000.0,
                y: 5000.0,
                z: 1500.0,
            }),
            toolchange_s: 7.5,
            rapid_speed: Some(8000.0),
            ..MachineConfig::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: MachineConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.accel, m.accel);
        assert_eq!(back.jerk, m.jerk);
        assert_eq!(back.toolchange_s, m.toolchange_s);
        assert_eq!(back.rapid_speed, m.rapid_speed);
        assert!(back.use_kinematic_time_estimate);
    }

    #[test]
    fn feeds_recovered_from_gcode_track_modal_f() {
        let gcode = "G21\nG0 X1 Y0\nG1 X10 Y0 F800\nG1 X20 Y0\nG1 X30 Y0 F1200\n";
        let (segs, _) = crate::gcode::preview::interpret_with_index(gcode);
        let feeds = feeds_per_segment(gcode, &segs);
        assert_eq!(feeds.len(), 4);
        assert_eq!(feeds[0], 0.0); // G0 — F not yet set on that line
        assert_eq!(feeds[1], 800.0);
        assert_eq!(feeds[2], 800.0);
        assert_eq!(feeds[3], 1200.0);
    }

    #[test]
    fn naive_fallback_when_kinematic_disabled() {
        let segs = vec![cut_seg((0.0, 0.0, 0.0), (100.0, 0.0, 0.0))];
        let mut m = machine();
        m.use_kinematic_time_estimate = false;
        let est = estimate(&segs, &[1000.0], &m, 0, 0.0);
        // 100 mm / (1000/60 mm/s) = 6.0 s exact.
        assert!((est.total_s - 6.0).abs() < 1e-6);
    }

    #[test]
    fn diagonal_z_dominated_move_uses_smallest_axis_bound() {
        // Direction (0, 0, 1) → effective accel = a_z (Z is the smallest
        // axis here). Length 10 mm at feed 600 mm/min ≈ 10 mm/s, accel
        // 100 mm/s². s_acc = 100/200 = 0.5; cruise 9 mm; t = 2*0.1 +
        // 9/10 = 1.1 s.
        let segs = vec![ToolpathSegment {
            from: Pose3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            to: Pose3 {
                x: 0.0,
                y: 0.0,
                z: -10.0,
            },
            kind: MoveKind::Plunge,
            gcode_line: 0,
            op_id: 0,
        }];
        let m = MachineConfig {
            accel: Some(AxisLimits {
                x: 500.0,
                y: 500.0,
                z: 100.0,
            }),
            ..MachineConfig::default()
        };
        let est = estimate(&segs, &[600.0], &m, 0, 0.0);
        assert!(
            (est.total_s - 1.1).abs() / 1.1 < 0.02,
            "got {} expected ~1.1",
            est.total_s
        );
    }

    #[test]
    fn plunge_segment_uses_plunge_rate_when_modal_f_is_cutting_feed() {
        // v7f5: A post that emits a single F<feed> at the start of
        // the op leaves the plunge G1 inheriting the cutting feed.
        // The estimator must clamp the plunge segment to the tool's
        // plunge_rate so the time prediction is realistic.
        //
        // Hand-computed reference: a 5 mm plunge at plunge_rate=200
        // mm/min (3.33 mm/s) + a 50 mm cut at feed=1200 mm/min (20
        // mm/s). Naive estimate = (5/3.33 + 50/20) = 4.00 s.
        // (Ignoring accel for simplicity — both segs are long enough
        // to cruise; the trapezoidal estimator gets within ~10%.)
        let segs = vec![
            ToolpathSegment {
                from: Pose3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                to: Pose3 {
                    x: 0.0,
                    y: 0.0,
                    z: -5.0,
                },
                kind: MoveKind::Plunge,
                gcode_line: 0,
                op_id: 7,
            },
            ToolpathSegment {
                from: Pose3 {
                    x: 0.0,
                    y: 0.0,
                    z: -5.0,
                },
                to: Pose3 {
                    x: 50.0,
                    y: 0.0,
                    z: -5.0,
                },
                kind: MoveKind::Cut,
                gcode_line: 0,
                op_id: 7,
            },
        ];
        // The post wrote ONE F1200 line — modal F = 1200 on both.
        let modal_feeds = vec![1200.0, 1200.0];
        let op_rates = vec![OpRates {
            op_id: 7,
            plunge_rate_mm_min: 200,
            feed_rate_mm_min: 1200,
        }];
        let clamped = clamp_feeds_by_kind(&segs, &modal_feeds, &op_rates);
        // The plunge feed should be capped at 200; the cut stays at 1200.
        assert!(
            (clamped[0] - 200.0).abs() < 1e-9,
            "plunge feed should be clamped to plunge_rate (200), got {}",
            clamped[0]
        );
        assert!(
            (clamped[1] - 1200.0).abs() < 1e-9,
            "cut feed should remain at modal F (1200), got {}",
            clamped[1]
        );
        // Without the clamp the naive run-time would be (5 / 20 + 50 /
        // 20) = 2.75 s (≈ 6× too fast on the plunge). With the clamp
        // we're around 4 s + accel/decel; assert the run-time is in the
        // 3.5 .. 6 s window (well above 2.75).
        let est_clamped =
            estimate(&segs, &clamped, &machine(), 0, 0.0);
        let est_unclamped = estimate(&segs, &modal_feeds, &machine(), 0, 0.0);
        assert!(
            est_clamped.total_s > est_unclamped.total_s * 1.3,
            "clamped total {} should be > 1.3× unclamped {}",
            est_clamped.total_s,
            est_unclamped.total_s,
        );
        assert!(
            est_clamped.total_s > 3.0 && est_clamped.total_s < 6.0,
            "clamped estimate {} outside 3..6 s window",
            est_clamped.total_s,
        );
    }

    #[test]
    fn empty_op_rates_preserves_modal_feeds() {
        // Backstop: when no op-rate entry matches, the modal F values
        // pass through unchanged (legacy single-arg behavior).
        let segs = vec![cut_seg((0.0, 0.0, 0.0), (10.0, 0.0, 0.0))];
        let feeds = vec![800.0];
        let clamped = clamp_feeds_by_kind(&segs, &feeds, &[]);
        assert_eq!(clamped, feeds);
        let other = vec![OpRates {
            op_id: 999,
            plunge_rate_mm_min: 100,
            feed_rate_mm_min: 500,
        }];
        let clamped = clamp_feeds_by_kind(&segs, &feeds, &other);
        assert_eq!(clamped, feeds, "seg op_id=0 doesn't match 999");
    }

    #[test]
    fn toolchange_and_warmup_added() {
        let segs: Vec<ToolpathSegment> = vec![];
        let mut m = machine();
        m.toolchange_s = 5.0;
        let est = estimate(&segs, &[], &m, 2, 3.0);
        assert!((est.toolchange_s - 10.0).abs() < 1e-9);
        assert!((est.spindle_warmup_s - 3.0).abs() < 1e-9);
        assert!((est.total_s - 13.0).abs() < 1e-9);
    }
}
