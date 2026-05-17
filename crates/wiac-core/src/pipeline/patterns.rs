//! Pattern repetition helpers (Linear / Grid / Polar). The per-op
//! pipeline expands a [`PatternConfig`](crate::project::PatternConfig)
//! into a list of [`PatternInstance`] transforms via [`pattern_offsets`],
//! then walks each instance's segments via [`apply_pattern_to_segments`]
//! / [`apply_pattern_to_point`]. The first instance always carries the
//! identity transform so a 1-instance pattern is equivalent to no
//! pattern at all.

use crate::geometry::{Point2, Segment};
use crate::project::PatternConfig;

/// A single materialized pattern instance: an arbitrary affine
/// translate + rotation, applied to whatever geometry the op carries.
/// Rotation is around `(cx, cy)`.
#[derive(Debug, Clone, Copy)]
pub(super) struct PatternInstance {
    pub(super) dx: f64,
    pub(super) dy: f64,
    pub(super) cx: f64,
    pub(super) cy: f64,
    /// Precomputed `cos(angle_rad)`. Cached on the instance so
    /// `apply_pattern_to_segments` doesn't redo trig per (instance × object)
    /// pair — for a Polar pattern with N instances and K selected objects,
    /// that previously meant 2·N·K trig calls.
    pub(super) cos_a: f64,
    pub(super) sin_a: f64,
    /// True when the rotation is identity. Lets the transform shortcut
    /// to translate-only, skipping the (cx, cy) recentering math
    /// entirely. Always true for Linear and Grid patterns.
    pub(super) pure_translate: bool,
}

impl PatternInstance {
    fn translate(dx: f64, dy: f64) -> Self {
        Self {
            dx,
            dy,
            cx: 0.0,
            cy: 0.0,
            cos_a: 1.0,
            sin_a: 0.0,
            pure_translate: true,
        }
    }

    fn polar(cx: f64, cy: f64, angle_rad: f64) -> Self {
        Self {
            dx: 0.0,
            dy: 0.0,
            cx,
            cy,
            cos_a: angle_rad.cos(),
            sin_a: angle_rad.sin(),
            // Identity rotation collapses to the translate path even
            // for Polar pattern at i=0 (the first instance is always
            // the source in place).
            pure_translate: angle_rad.abs() < 1e-12,
        }
    }
}

/// Materialize a pattern config into a list of instance transforms.
/// The first element of the returned list is always the identity
/// transform — the source geometry stays in place at instance 0 — so
/// a 1-instance pattern is equivalent to no pattern at all.
pub(super) fn pattern_offsets(pattern: PatternConfig) -> Vec<PatternInstance> {
    let mut out = Vec::new();
    match pattern {
        PatternConfig::Linear { count, dx, dy } => {
            // count is an inclusive total. count == 0 → no instances at
            // all (degenerate, but well-defined: the op emits nothing).
            for i in 0..count {
                out.push(PatternInstance::translate(
                    f64::from(i) * dx,
                    f64::from(i) * dy,
                ));
            }
        }
        PatternConfig::Grid {
            count_x,
            count_y,
            dx,
            dy,
        } => {
            for j in 0..count_y {
                for i in 0..count_x {
                    out.push(PatternInstance::translate(
                        f64::from(i) * dx,
                        f64::from(j) * dy,
                    ));
                }
            }
        }
        PatternConfig::Polar {
            count,
            center_x,
            center_y,
            angle_step_deg,
            start_angle_deg,
        } => {
            let step_rad = angle_step_deg.to_radians();
            let start_rad = start_angle_deg.to_radians();
            for i in 0..count {
                out.push(PatternInstance::polar(
                    center_x,
                    center_y,
                    start_rad + f64::from(i) * step_rad,
                ));
            }
        }
    }
    out
}

/// Apply a pattern instance transform to every endpoint and arc center
/// of `segments` in place: rotate around (cx, cy) by `angle_rad`, then
/// translate by (dx, dy). Bulge stays the same — it's a local angle
/// ratio, invariant under rotation and translation.
pub(super) fn apply_pattern_to_segments(segments: &mut [Segment], inst: PatternInstance) {
    if inst.pure_translate {
        if inst.dx == 0.0 && inst.dy == 0.0 {
            // Identity transform — first pattern instance is always the
            // source in place. Skip the per-segment work entirely.
            return;
        }
        for s in segments.iter_mut() {
            s.start.x += inst.dx;
            s.start.y += inst.dy;
            s.end.x += inst.dx;
            s.end.y += inst.dy;
            if let Some(c) = s.center.as_mut() {
                c.x += inst.dx;
                c.y += inst.dy;
            }
        }
        return;
    }
    for s in segments.iter_mut() {
        s.start = transform_point(s.start, inst);
        s.end = transform_point(s.end, inst);
        if let Some(c) = s.center {
            s.center = Some(transform_point(c, inst));
        }
    }
}

pub(super) fn apply_pattern_to_point(p: Point2, inst: PatternInstance) -> Point2 {
    if inst.pure_translate {
        return Point2::new(p.x + inst.dx, p.y + inst.dy);
    }
    transform_point(p, inst)
}

fn transform_point(p: Point2, inst: PatternInstance) -> Point2 {
    let dx = p.x - inst.cx;
    let dy = p.y - inst.cy;
    let rx = inst.cx + dx * inst.cos_a - dy * inst.sin_a;
    let ry = inst.cy + dx * inst.sin_a + dy * inst.cos_a;
    Point2::new(rx + inst.dx, ry + inst.dy)
}

#[cfg(test)]
mod tests {
    use crate::cam::setup::ToolOffset;
    use crate::pipeline::test_helpers::{
        cut_x_values, endmill, profile_op, profile_op_with_pattern, project_with,
    };
    use crate::pipeline::{run_pipeline, PipelineRequest};
    use crate::project::PatternConfig;

    /// Linear pattern: 3 instances translated dx=20. Cuts span all
    /// three X bands and the X range covers all three instances.
    #[test]
    fn linear_pattern_emits_translated_copies() {
        let project = project_with(
            vec![profile_op_with_pattern(PatternConfig::Linear {
                count: 3,
                dx: 20.0,
                dy: 0.0,
            })],
            vec![endmill(1, 3.0)],
        );
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let xs = cut_x_values(&resp.gcode);
        assert!(
            !xs.is_empty(),
            "pattern op produced no cuts:\n{}",
            resp.gcode
        );
        let max_x = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min_x = xs.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(
            max_x > 38.0,
            "expected X to reach the third instance (>~38), got max {} in:\n{}",
            max_x,
            resp.gcode,
        );
        assert!(
            min_x < 5.0,
            "expected X to also touch the first instance (<5), got min {} in:\n{}",
            min_x,
            resp.gcode,
        );
        let near_first = xs.iter().filter(|x| **x >= -2.0 && **x <= 22.0).count();
        let near_second = xs.iter().filter(|x| **x >= 18.0 && **x <= 42.0).count();
        let near_third = xs.iter().filter(|x| **x >= 38.0 && **x <= 62.0).count();
        assert!(
            near_first > 0 && near_second > 0 && near_third > 0,
            "expected cuts in all three instance bands ({}, {}, {}):\n{}",
            near_first,
            near_second,
            near_third,
            resp.gcode,
        );
    }

    #[test]
    fn grid_pattern_emits_count_xcount_y_instances() {
        let project = project_with(
            vec![profile_op_with_pattern(PatternConfig::Grid {
                count_x: 2,
                count_y: 2,
                dx: 30.0,
                dy: 30.0,
            })],
            vec![endmill(1, 3.0)],
        );
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            resp.stats.closed_object_count, 4,
            "expected 4 closed objects from a 2x2 grid, got {}\n{}",
            resp.stats.closed_object_count, resp.gcode
        );
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for line in resp.gcode.lines() {
            if !(line.starts_with("G1") || line.starts_with("G0")) {
                continue;
            }
            if let Some(idx) = line.find('X') {
                let rest = &line[idx + 1..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                    .unwrap_or(rest.len());
                if let Ok(v) = rest[..end].parse::<f64>() {
                    if v > max_x {
                        max_x = v;
                    }
                }
            }
            if let Some(idx) = line.find('Y') {
                let rest = &line[idx + 1..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                    .unwrap_or(rest.len());
                if let Ok(v) = rest[..end].parse::<f64>() {
                    if v > max_y {
                        max_y = v;
                    }
                }
            }
        }
        assert!(
            max_x > 45.0 && max_y > 45.0,
            "grid should extend into the second column AND the second row (X>{}, Y>{}):\n{}",
            max_x,
            max_y,
            resp.gcode,
        );
    }

    #[test]
    fn polar_pattern_rotates_around_center() {
        let project = project_with(
            vec![profile_op_with_pattern(PatternConfig::Polar {
                count: 4,
                center_x: 0.0,
                center_y: 0.0,
                angle_step_deg: 90.0,
                start_angle_deg: 0.0,
            })],
            vec![endmill(1, 3.0)],
        );
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            resp.stats.closed_object_count, 4,
            "expected 4 closed objects from a 4-instance polar pattern, got {}\n{}",
            resp.stats.closed_object_count, resp.gcode
        );
        let mut quad_pos_pos = false;
        let mut quad_neg_pos = false;
        let mut quad_neg_neg = false;
        let mut quad_pos_neg = false;
        let mut last_x: Option<f64> = None;
        let mut last_y: Option<f64> = None;
        for line in resp.gcode.lines() {
            if !(line.starts_with("G1") || line.starts_with("G0")) {
                continue;
            }
            let mut x = last_x;
            let mut y = last_y;
            for (label, slot) in [('X', &mut x), ('Y', &mut y)] {
                if let Some(idx) = line.find(label) {
                    let rest = &line[idx + 1..];
                    let end = rest
                        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                        .unwrap_or(rest.len());
                    if let Ok(v) = rest[..end].parse::<f64>() {
                        *slot = Some(v);
                    }
                }
            }
            last_x = x;
            last_y = y;
            if let (Some(xv), Some(yv)) = (x, y) {
                if xv > 5.0 && yv > 5.0 {
                    quad_pos_pos = true;
                }
                if xv < -5.0 && yv > 5.0 {
                    quad_neg_pos = true;
                }
                if xv < -5.0 && yv < -5.0 {
                    quad_neg_neg = true;
                }
                if xv > 5.0 && yv < -5.0 {
                    quad_pos_neg = true;
                }
            }
        }
        assert!(
            quad_pos_pos && quad_neg_pos && quad_neg_neg && quad_pos_neg,
            "expected polar cuts in all four quadrants (++, -+, --, +-): {} {} {} {}\n{}",
            quad_pos_pos,
            quad_neg_pos,
            quad_neg_neg,
            quad_pos_neg,
            resp.gcode,
        );
    }

    /// Locks in back-compat: a Profile op with `pattern: None` must
    /// produce the exact same gcode it produced before pattern support
    /// was added.
    #[test]
    fn pattern_none_keeps_existing_behavior() {
        let project_a = project_with(
            vec![profile_op(1, 1, ToolOffset::Outside)],
            vec![endmill(1, 3.0)],
        );
        let mut op_b = profile_op(1, 1, ToolOffset::Outside);
        op_b.pattern = None;
        let project_b = project_with(vec![op_b], vec![endmill(1, 3.0)]);
        let resp_a = run_pipeline(
            PipelineRequest {
                project: project_a,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let resp_b = run_pipeline(
            PipelineRequest {
                project: project_b,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            resp_a.gcode, resp_b.gcode,
            "pattern: None must be byte-identical to a no-pattern op",
        );
    }
}
