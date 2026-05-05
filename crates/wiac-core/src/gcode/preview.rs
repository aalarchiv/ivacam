//! Gcode interpreter that produces 3D toolpath polylines for the preview
//! renderer. Port of preview_plugins/gcode.py.
//!
//! Reads emitted gcode line-by-line, tracks XYZ + active modal G-code, and
//! emits typed [`ToolpathSegment`]s the frontend feeds straight to Three.js.

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
}

/// Parse `gcode` and return a stream of toolpath segments. Supports the
/// minimal subset wiaConstructor itself emits (G0/G1 + G2/G3 with I/J
/// arc-center, plus G20/G21 unit switching). Anything else is ignored
/// gracefully.
pub fn interpret(gcode: &str) -> Vec<ToolpathSegment> {
    let mut state = Pose3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    let mut active_code = 0u8;
    let mut out = Vec::new();
    let mut unit_scale = 1.0;
    for raw in gcode.lines() {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        let mut x = state.x;
        let mut y = state.y;
        let mut z = state.z;
        let mut i = 0.0;
        let mut j = 0.0;
        let mut had_x = false;
        let mut had_y = false;
        let mut had_z = false;
        let mut had_i = false;
        let mut had_j = false;
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
                "X" | "x" => {
                    x = val * unit_scale;
                    had_x = true;
                }
                "Y" | "y" => {
                    y = val * unit_scale;
                    had_y = true;
                }
                "Z" | "z" => {
                    z = val * unit_scale;
                    had_z = true;
                }
                "I" | "i" => {
                    i = val * unit_scale;
                    had_i = true;
                }
                "J" | "j" => {
                    j = val * unit_scale;
                    had_j = true;
                }
                _ => {}
            }
        }
        let _ = (had_x, had_y, had_i, had_j);
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
        out.push(ToolpathSegment { from, to, kind });
        state = to;
    }
    out
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
}
