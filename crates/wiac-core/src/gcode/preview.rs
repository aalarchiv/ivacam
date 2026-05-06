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
pub enum SegmentKind {
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
    pub kind: SegmentKind,
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
                        }
                    }
                }
                "X" | "x" => x = val * unit_scale,
                "Y" | "y" => y = val * unit_scale,
                "Z" | "z" => {
                    z = val * unit_scale;
                    had_z = true;
                }
                _ => {}
            }
        }
        let from = state;
        let to = Pose3 { x, y, z };
        let moved = from.x != to.x || from.y != to.y || from.z != to.z;
        if !moved {
            continue;
        }
        let kind = match active_code {
            0 => SegmentKind::Rapid,
            1 => {
                if had_z && from.x == to.x && from.y == to.y {
                    if to.z > from.z {
                        SegmentKind::Retract
                    } else {
                        SegmentKind::Plunge
                    }
                } else {
                    SegmentKind::Cut
                }
            }
            2 | 3 => SegmentKind::Arc,
            _ => SegmentKind::Cut,
        };
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
        assert!(matches!(segs[0].kind, SegmentKind::Rapid));
        assert!(matches!(segs[1].kind, SegmentKind::Cut));
        assert_eq!(segs[1].to.y, 10.0);
    }

    #[test]
    fn plunge_vs_retract() {
        let g = "G21\nG0 X0 Y0 Z5\nG1 Z-2 F100\nG1 Z5 F200\n";
        let segs = interpret(g);
        assert_eq!(segs.len(), 3);
        assert!(matches!(segs[0].kind, SegmentKind::Rapid));
        assert!(matches!(segs[1].kind, SegmentKind::Plunge));
        assert!(matches!(segs[2].kind, SegmentKind::Retract));
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
