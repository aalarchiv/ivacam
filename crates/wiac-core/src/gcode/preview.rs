//! Gcode interpreter that produces 3D toolpath polylines for the preview
//! renderer. Port of preview_plugins/gcode.py.
//!
//! Reads emitted gcode line-by-line, tracks XYZ + active modal G-code, and
//! emits typed [`ToolpathSegment`]s the frontend feeds straight to Three.js.
//!
//! Each segment carries its source `gcode_line` (1-based) and the active
//! `op_id` for bidirectional gcode-↔-toolpath linking. `op_id` is set by
//! reading `; OP <n>` comment markers the per-op emitter writes; segments
//! before the first marker get `op_id = 0`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Pose3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MoveKind {
    Rapid,
    Cut,
    Plunge,
    Retract,
    Arc,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolpathSegment {
    pub from: Pose3,
    pub to: Pose3,
    pub kind: MoveKind,
    /// 1-based line number in the source gcode that produced this move.
    /// 0 means "synthetic / unknown".
    #[serde(default)]
    pub gcode_line: u32,
    /// Operation id from the per-op emitter. 0 = legacy / unstamped.
    #[serde(default)]
    pub op_id: u32,
}

/// Lookup table the frontend uses to wire the gcode text panel to the 3D
/// toolpath: line N in the gcode corresponds to `segments[lines_to_segment[N]]`,
/// and `segments_to_line[i]` is the 1-based gcode line that produced
/// segment `i`. Both vectors are dense — gcode lines that don't move the
/// tool map to `usize::MAX` so callers can detect the gap.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct GcodeIndex {
    pub lines_to_segment: Vec<u32>,
    pub segments_to_line: Vec<u32>,
}

const NO_SEGMENT: u32 = u32::MAX;

/// Parse `gcode` and return a stream of toolpath segments. Supports the
/// minimal subset wiaConstructor itself emits (G0/G1 + G2/G3 with I/J
/// arc-center, plus G20/G21 unit switching). Anything else is ignored
/// gracefully. `; OP <n>` comments switch the active op id for later
/// segments (used by the per-op emitter).
pub fn interpret(gcode: &str) -> Vec<ToolpathSegment> {
    let (segments, _) = interpret_with_index(gcode);
    segments
}

/// Same as [`interpret`] but also returns the line ↔ segment lookup.
/// Frontend uses this to wire the gcode text panel to the 3D playhead.
pub fn interpret_with_index(gcode: &str) -> (Vec<ToolpathSegment>, GcodeIndex) {
    let mut state = Pose3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    let mut active_code = 0u8;
    let mut active_op: u32 = 0;
    let mut out = Vec::new();
    let mut unit_scale = 1.0;
    let mut lines_to_segment: Vec<u32> = Vec::new();
    let mut segments_to_line: Vec<u32> = Vec::new();

    for (idx0, raw) in gcode.lines().enumerate() {
        // Push a placeholder for this line; we'll overwrite if it produces
        // a segment.
        lines_to_segment.push(NO_SEGMENT);
        let line_no = (idx0 + 1) as u32;

        // Inspect comments (raw, before stripping) for op markers.
        if let Some(op_id) = parse_op_marker(raw) {
            active_op = op_id;
            continue;
        }

        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        let mut x = state.x;
        let mut y = state.y;
        let mut z = state.z;
        let mut had_z = false;
        // I / J / R for G2 / G3. I/J = center offset from arc start in
        // X/Y; R = radius (alternative form). Without these the arc is
        // implicitly treated as a chord — which the wireframe + sim
        // would then carve as a straight line across the arc's
        // diameter (the bug this tessellation guards against).
        let mut i_off: Option<f64> = None;
        let mut j_off: Option<f64> = None;
        // R word: for G2/G3 it's a radius; for G81/G82/G83/G73 it's
        // the retract plane (Z) the canned cycle returns to. We keep
        // these in the same variable since they're mutually exclusive
        // per line.
        let mut r_val: Option<f64> = None;
        for tok in line.split_whitespace() {
            let (head, val_str) = tok.split_at(1);
            let val: f64 = val_str.parse().unwrap_or(0.0);
            match head {
                "G" | "g" => {
                    if let Ok(n) = val_str.parse::<u8>() {
                        if (0..=3).contains(&n) {
                            active_code = n;
                        } else if n == 20 {
                            unit_scale = 25.4;
                        } else if n == 21 {
                            unit_scale = 1.0;
                        } else if matches!(n, 73 | 81 | 82 | 83) {
                            // Drill canned cycle. Recorded so the
                            // expansion below knows to emit the
                            // rapid + plunge + retract triplet
                            // instead of a single diagonal "rapid"
                            // (the pre-fix bug: G81 X10 Y10 Z-3 R2
                            // after a G0 was treated as a G0 to
                            // (10, 10, -3), drawing a diagonal
                            // segment THROUGH the workpiece).
                            active_code = n;
                        }
                    }
                }
                "X" | "x" => x = val * unit_scale,
                "Y" | "y" => y = val * unit_scale,
                "Z" | "z" => {
                    z = val * unit_scale;
                    had_z = true;
                }
                "I" | "i" => i_off = Some(val * unit_scale),
                "J" | "j" => j_off = Some(val * unit_scale),
                "R" | "r" => r_val = Some(val * unit_scale),
                _ => {}
            }
        }
        // Drill canned cycle expansion. The post emits one G81/G82/G83/G73
        // line per hole with the target X/Y/Z and the retract R. Expand
        // it into three preview segments:
        //   1. Horizontal rapid from current pos to (X, Y, current_z)
        //   2. Vertical plunge to (X, Y, Z) at feed (Plunge kind)
        //   3. Vertical retract to (X, Y, R) at rapid (Retract kind)
        // After the cycle, the cutter is at (X, Y, R). The next iteration
        // can rapid back up to fast_z via the post's emitted `G0 Z<fast_z>`
        // before the following G81.
        if matches!(active_code, 73 | 81 | 82 | 83) {
            let r_z = r_val.unwrap_or(state.z);
            let mid_xy = Pose3 {
                x,
                y,
                z: state.z,
            };
            let bottom = Pose3 { x, y, z };
            let retracted = Pose3 { x, y, z: r_z };
            let from = state;
            let mut push = |from: Pose3, to: Pose3, kind: MoveKind| {
                if from.x == to.x && from.y == to.y && from.z == to.z {
                    return;
                }
                let seg_idx = out.len() as u32;
                out.push(ToolpathSegment {
                    from,
                    to,
                    kind,
                    gcode_line: line_no,
                    op_id: active_op,
                });
                let last = lines_to_segment.len() - 1;
                if lines_to_segment[last] == NO_SEGMENT {
                    lines_to_segment[last] = seg_idx;
                }
                segments_to_line.push(line_no);
            };
            push(from, mid_xy, MoveKind::Rapid);
            push(mid_xy, bottom, MoveKind::Plunge);
            push(bottom, retracted, MoveKind::Retract);
            state = retracted;
            // Reset active_code so a subsequent non-canned-cycle line
            // (e.g. a plain `G0 Z10` between holes) isn't misclassified.
            // The post explicitly re-emits the G code on every line, so
            // we don't need to keep G81 modal in the interpreter.
            active_code = 0;
            continue;
        }
        let from = state;
        let to = Pose3 { x, y, z };
        let moved = from.x != to.x || from.y != to.y || from.z != to.z;
        if !moved {
            continue;
        }
        let kind = match active_code {
            0 => MoveKind::Rapid,
            1 => {
                if had_z && from.x == to.x && from.y == to.y {
                    if to.z > from.z {
                        MoveKind::Retract
                    } else {
                        MoveKind::Plunge
                    }
                } else {
                    MoveKind::Cut
                }
            }
            2 | 3 => MoveKind::Arc,
            _ => MoveKind::Cut,
        };
        if matches!(kind, MoveKind::Arc) && (i_off.is_some() || j_off.is_some()) {
            // Tessellate G2/G3 into chord segments along the actual
            // arc. Otherwise the previewer emits a single chord from
            // start to end — a half-circle becomes a horizontal line
            // across the diameter, which both the wireframe and the
            // heightfield simulator render and carve along (visible
            // bug: profile-Outside on a circle "looks like a cut on
            // the source line").
            let cx = from.x + i_off.unwrap_or(0.0);
            let cy = from.y + j_off.unwrap_or(0.0);
            let r = ((from.x - cx).powi(2) + (from.y - cy).powi(2)).sqrt();
            let theta_start = (from.y - cy).atan2(from.x - cx);
            let theta_end = (to.y - cy).atan2(to.x - cx);
            let mut sweep = theta_end - theta_start;
            // G2 = CW, G3 = CCW. Bring sweep into the right half-plane
            // for the requested direction; +0/-0 sweep with X/Y
            // co-incident becomes a full revolution (G2/G3 X<same>
            // Y<same> I... is a full circle in many dialects).
            const TAU: f64 = std::f64::consts::TAU;
            let coincident = (from.x - to.x).abs() < 1e-9 && (from.y - to.y).abs() < 1e-9;
            if active_code == 3 {
                // CCW
                if coincident {
                    sweep = TAU;
                } else if sweep <= 1e-9 {
                    sweep += TAU;
                }
            } else {
                // CW (G2)
                if coincident {
                    sweep = -TAU;
                } else if sweep >= -1e-9 {
                    sweep -= TAU;
                }
            }
            // ~10° per chord — chord error r·(1-cos(5°)) ≈ 0.004·r,
            // i.e. <0.04 mm error on a 10 mm arc. With a 4-chord
            // minimum a quarter-circle gets at least 4 chords.
            let n = (sweep.abs() / (10f64.to_radians())).ceil().max(4.0) as usize;
            let dtheta = sweep / (n as f64);
            let dz = to.z - from.z;
            let mut prev = from;
            let first_seg_idx = out.len() as u32;
            for k in 1..=n {
                let theta = theta_start + dtheta * (k as f64);
                let nx = if k == n { to.x } else { cx + r * theta.cos() };
                let ny = if k == n { to.y } else { cy + r * theta.sin() };
                let nz = if k == n {
                    to.z
                } else {
                    from.z + dz * (k as f64) / (n as f64)
                };
                let chord_to = Pose3 { x: nx, y: ny, z: nz };
                out.push(ToolpathSegment {
                    from: prev,
                    to: chord_to,
                    kind: MoveKind::Arc,
                    gcode_line: line_no,
                    op_id: active_op,
                });
                segments_to_line.push(line_no);
                prev = chord_to;
            }
            // lines_to_segment points at the first chord of this arc
            // (jumpToLine seeks to the start of the arc).
            let last = lines_to_segment.len() - 1;
            lines_to_segment[last] = first_seg_idx;
            state = to;
            continue;
        }
        let seg_idx = out.len() as u32;
        out.push(ToolpathSegment {
            from,
            to,
            kind,
            gcode_line: line_no,
            op_id: active_op,
        });
        // Last entry placeholder is for *this* line — overwrite it.
        let last = lines_to_segment.len() - 1;
        lines_to_segment[last] = seg_idx;
        segments_to_line.push(line_no);
        state = to;
    }
    (
        out,
        GcodeIndex {
            lines_to_segment,
            segments_to_line,
        },
    )
}

/// Extract the op id from a `; OP <n>` or `(OP <n>)` marker. Returns
/// `None` for non-marker lines.
fn parse_op_marker(raw: &str) -> Option<u32> {
    let s = raw.trim();
    let body = s
        .strip_prefix(';')
        .or_else(|| s.strip_prefix('('))
        .map(|b| b.trim_end_matches(')').trim())?;
    let rest = body
        .strip_prefix("OP")
        .or_else(|| body.strip_prefix("op"))?
        .trim();
    rest.parse::<u32>().ok()
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
mod tests {
    use super::*;

    #[test]
    fn rapid_then_cut() {
        let g = "G21\nG90\nG0 X10 Y0\nG1 X10 Y10 F800\n";
        let segs = interpret(g);
        assert_eq!(segs.len(), 2);
        assert!(matches!(segs[0].kind, MoveKind::Rapid));
        assert!(matches!(segs[1].kind, MoveKind::Cut));
        assert_eq!(segs[1].to.y, 10.0);
    }

    #[test]
    fn plunge_vs_retract() {
        let g = "G21\nG0 X0 Y0 Z5\nG1 Z-2 F100\nG1 Z5 F200\n";
        let segs = interpret(g);
        assert_eq!(segs.len(), 3);
        assert!(matches!(segs[0].kind, MoveKind::Rapid));
        assert!(matches!(segs[1].kind, MoveKind::Plunge));
        assert!(matches!(segs[2].kind, MoveKind::Retract));
    }

    /// Regression: a G81 canned cycle after a G0 Z lift used to be
    /// interpreted as a diagonal G0 to (X, Y, Z), drawing a straight
    /// line through the workpiece. It now expands into a horizontal
    /// rapid, a vertical plunge, and a vertical retract — what the
    /// real machine executes.
    #[test]
    fn drill_canned_cycle_expands_into_rapid_plunge_retract() {
        // Two-hole drill program. Each cycle starts with a G0 lift to
        // fast_z (10 mm), then G81 X Y Z=−3 R=2.
        let g = "\
            G21\n\
            G90\n\
            G0 Z10\n\
            G81 X1 Y1 Z-3 R2\n\
            G0 Z10\n\
            G81 X5 Y5 Z-3 R2\n";
        let segs = interpret(g);
        // Each G0 Z10 = 1 segment. Each G81 = 3 segments.
        // Total = 1 + 3 + 1 + 3 = 8.
        assert_eq!(segs.len(), 8, "got {:#?}", segs);

        // First G81: horizontal rapid at z=10, plunge to z=-3, retract to z=2.
        let g81_a = &segs[1..4];
        assert!(matches!(g81_a[0].kind, MoveKind::Rapid));
        assert_eq!(g81_a[0].from, Pose3 { x: 0.0, y: 0.0, z: 10.0 });
        assert_eq!(g81_a[0].to, Pose3 { x: 1.0, y: 1.0, z: 10.0 });
        assert!(matches!(g81_a[1].kind, MoveKind::Plunge));
        assert_eq!(g81_a[1].from, Pose3 { x: 1.0, y: 1.0, z: 10.0 });
        assert_eq!(g81_a[1].to, Pose3 { x: 1.0, y: 1.0, z: -3.0 });
        assert!(matches!(g81_a[2].kind, MoveKind::Retract));
        assert_eq!(g81_a[2].to, Pose3 { x: 1.0, y: 1.0, z: 2.0 });

        // Second G0 lift: vertical rapid from (1, 1, 2) to (1, 1, 10).
        assert!(matches!(segs[4].kind, MoveKind::Rapid));
        assert_eq!(segs[4].from, Pose3 { x: 1.0, y: 1.0, z: 2.0 });
        assert_eq!(segs[4].to, Pose3 { x: 1.0, y: 1.0, z: 10.0 });

        // Second G81: horizontal rapid at z=10, NOT a diagonal into the
        // workpiece (the bug we're guarding against).
        let g81_b = &segs[5..8];
        assert!(matches!(g81_b[0].kind, MoveKind::Rapid));
        assert_eq!(g81_b[0].from, Pose3 { x: 1.0, y: 1.0, z: 10.0 });
        assert_eq!(g81_b[0].to, Pose3 { x: 5.0, y: 5.0, z: 10.0 });
        assert!(matches!(g81_b[1].kind, MoveKind::Plunge));
        assert_eq!(g81_b[1].from, Pose3 { x: 5.0, y: 5.0, z: 10.0 });
        assert_eq!(g81_b[1].to, Pose3 { x: 5.0, y: 5.0, z: -3.0 });
    }

    #[test]
    fn ignores_comments() {
        let g = "(setup)\n; just a note\nG0 X1 Y2\n";
        let segs = interpret(g);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].to.x, 1.0);
    }

    #[test]
    fn segments_carry_their_source_gcode_line() {
        // Lines 1..=4 in the source. The two G0 / G1 land segments at
        // lines 3 and 4.
        let g = "G21\nG90\nG0 X10 Y0\nG1 X10 Y10 F800\n";
        let segs = interpret(g);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].gcode_line, 3);
        assert_eq!(segs[1].gcode_line, 4);
    }

    #[test]
    fn op_markers_stamp_subsequent_segments() {
        let g = "; OP 1\nG0 X1 Y0\nG1 X2 Y0 F800\n; OP 2\nG1 X3 Y0\n";
        let segs = interpret(g);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].op_id, 1);
        assert_eq!(segs[1].op_id, 1);
        assert_eq!(segs[2].op_id, 2);
    }

    #[test]
    fn gcode_index_round_trips() {
        let g = "G21\n; OP 1\nG0 X1 Y0\nG1 X2 Y0\nG1 X3 Y0\n";
        let (segs, idx) = interpret_with_index(g);
        assert_eq!(segs.len(), 3);
        // Per the source: line 1 G21 (no segment), line 2 OP marker (none),
        // line 3 G0 → seg[0], line 4 G1 → seg[1], line 5 G1 → seg[2].
        assert_eq!(idx.lines_to_segment[2], 0); // line 3 → segment 0
        assert_eq!(idx.lines_to_segment[3], 1);
        assert_eq!(idx.lines_to_segment[4], 2);
        assert_eq!(idx.segments_to_line, vec![3, 4, 5]);
        // Lines without a segment are NO_SEGMENT.
        assert_eq!(idx.lines_to_segment[0], super::NO_SEGMENT);
        assert_eq!(idx.lines_to_segment[1], super::NO_SEGMENT);
    }
}
