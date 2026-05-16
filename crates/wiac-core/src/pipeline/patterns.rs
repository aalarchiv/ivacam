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
                out.push(PatternInstance::translate(f64::from(i) * dx, f64::from(i) * dy));
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
                    out.push(PatternInstance::translate(f64::from(i) * dx, f64::from(j) * dy));
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
