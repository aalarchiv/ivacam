//! CAM math layer — port of viaConstructor's `calc.py`.
//!
//! Three logical groups:
//! * `geometry` helpers (lines, angles, distances, polygon-inside) — pure math
//! * `chaining` (segments → closed/open `VcObjects`) — port of `segments2objects`
//! * `offsets` (`cavalier_contours` + clipper2-rust driven contour offsetting and pockets)

// # CAM/sim pedantic-lint exemptions
// Core CAM helpers (`segments_to_objects`, polygon area, point-in-polygon)
// use textbook short names (`a`, `b`, `n`, `area`) and walk over bounded
// vertex-index ranges (≪ 2^52).
#![allow(clippy::cast_precision_loss, clippy::similar_names)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geometry::{Point2, Segment, SegmentKind};
use crate::math;

pub mod chaining;
pub mod chamfer;
pub mod geometry_cache;
pub mod halfpipe;
pub mod inscribed;
pub mod offsets;
pub mod raster;
pub mod setup;
pub mod source_combine;
pub mod surface;
pub mod surface_mill;
pub mod tabs;
pub mod thread;
pub mod trochoidal;
pub mod vcarve;
pub mod vcarve_emit;

/// `VcObject` analogue: a chain of segments grouped after `segments2objects`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcObject {
    pub segments: Vec<Segment>,
    pub closed: bool,
    /// "outside" / "inside" / "none" — see `setup::ToolOffset`.
    pub tool_offset: crate::project::ToolOffset,
    /// Per-object override of the tool-radius offset (None ⇒ tool diameter / 2).
    pub overwrite_offset: Option<f64>,
    /// IDs of objects fully containing this one.
    pub outer_objects: Vec<usize>,
    /// IDs of objects fully contained by this one.
    pub inner_objects: Vec<usize>,
    #[schemars(with = "String")]
    pub layer: std::sync::Arc<str>,
    pub color: i32,
    /// Optional starting point for cut-order seeding.
    pub start: Option<Point2>,
    /// Per-object setup overrides (mill depth, leads, tabs, …).
    pub setup: setup::Setup,
}

impl VcObject {
    #[must_use]
    pub fn new(segments: Vec<Segment>, closed: bool) -> Self {
        let layer: std::sync::Arc<str> = segments
            .first()
            .map_or_else(|| std::sync::Arc::from("0"), |s| s.layer.clone());
        let color = segments.first().map_or(7, |s| s.color);
        Self {
            segments,
            closed,
            tool_offset: crate::project::ToolOffset::None,
            overwrite_offset: None,
            outer_objects: Vec::new(),
            inner_objects: Vec::new(),
            layer,
            color,
            start: None,
            setup: setup::Setup::default(),
        }
    }
}

// ─── Pure geometry helpers (port of calc.py:60–340) ────────────────────────

/// Distance between two 2D points.
#[must_use]
pub fn calc_distance(a: Point2, b: Point2) -> f64 {
    a.distance(b)
}

/// Angle of the line a→b in radians, in (-π, π].
#[must_use]
pub fn angle_of_line(a: Point2, b: Point2) -> f64 {
    (b.y - a.y).atan2(b.x - a.x)
}

/// Square distance from point `p` to the *infinite* line through a→b.
/// Negative on the right of a→b, positive on the left (matches calc.py
/// sign-by-cross convention).
#[must_use]
pub fn distance_to_line_signed(a: Point2, b: Point2, p: Point2) -> f64 {
    let len = a.distance(b);
    if len < 1e-12 {
        return a.distance(p);
    }
    let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
    cross / len
}

/// Returns the (x, y) of the line-segment intersection of (s1→e1) and (s2→e2)
/// if they cross within both segments' parameter ranges, else None.
#[must_use]
pub fn lines_intersect(s1: Point2, e1: Point2, s2: Point2, e2: Point2) -> Option<Point2> {
    let dx1 = e1.x - s1.x;
    let dy1 = e1.y - s1.y;
    let dx2 = e2.x - s2.x;
    let dy2 = e2.y - s2.y;
    let denom = dx1 * dy2 - dy1 * dx2;
    if denom.abs() < 1e-12 {
        return None;
    }
    let t = ((s2.x - s1.x) * dy2 - (s2.y - s1.y) * dx2) / denom;
    let u = ((s2.x - s1.x) * dy1 - (s2.y - s1.y) * dx1) / denom;
    if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
        Some(Point2::new(s1.x + t * dx1, s1.y + t * dy1))
    } else {
        None
    }
}

/// Bounding box of a list of points.
#[must_use]
pub fn points_bbox(points: &[Point2]) -> Option<crate::BBox> {
    if points.is_empty() {
        return None;
    }
    let mut bbox = crate::BBox::EMPTY;
    for p in points {
        bbox.extend_point(*p);
    }
    Some(bbox)
}

/// Center of mass of a polygon (treats `points` as polygon vertices in order).
#[must_use]
pub fn polygon_centroid(points: &[Point2]) -> Option<Point2> {
    if points.is_empty() {
        return None;
    }
    let (mut sx, mut sy) = (0.0, 0.0);
    for p in points {
        sx += p.x;
        sy += p.y;
    }
    Some(Point2::new(
        sx / points.len() as f64,
        sy / points.len() as f64,
    ))
}

/// Re-exported from [`crate::geometry::is_inside_polygon`] (moved there
/// to keep the polygon predicates in one place); kept here so existing
/// `crate::cam::is_inside_polygon` call sites resolve unchanged.
pub use crate::geometry::is_inside_polygon;

/// Convert a [`Segment`] (LINE or ARC-with-bulge) into a flat polyline of
/// `points` for clipper-side polygon ops. `interpolate` controls per-arc
/// subdivision (≥1 step, default 6 to match `calc.py:vertex2points` default).
#[must_use]
pub fn segment_to_points(seg: &Segment, interpolate: usize) -> Vec<Point2> {
    if seg.bulge.abs() < 1e-12 || seg.kind == SegmentKind::Line {
        return vec![seg.start, seg.end];
    }
    // Tessellate proportional to sweep, with at least `interpolate` steps.
    let max_step = (std::f64::consts::TAU / 8.0).max(0.05);
    let coarse = math::tessellate_arc(seg.start, seg.end, seg.bulge, max_step);
    if interpolate <= 1 {
        return coarse;
    }
    // Uniformly sub-sample the parametric arc for `interpolate * sweep_steps` points.
    let (center, a0, a1, radius) = math::bulge_to_arc(seg.start, seg.end, seg.bulge);
    let mut sweep = a1 - a0;
    if seg.bulge > 0.0 && sweep < 0.0 {
        sweep += std::f64::consts::TAU;
    }
    if seg.bulge < 0.0 && sweep > 0.0 {
        sweep -= std::f64::consts::TAU;
    }
    let n = interpolate * 8; // ≥8 per quadrant for fidelity
    let mut out = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = a0 + sweep * (i as f64) / (n as f64);
        out.push(Point2::new(
            center.x + radius * t.cos(),
            center.y + radius * t.sin(),
        ));
    }
    if let Some(p) = out.first_mut() {
        *p = seg.start;
    }
    if let Some(p) = out.last_mut() {
        *p = seg.end;
    }
    out
}

// ─── Object transforms ─────────────────────────────────────────────────────
//
// Pure helpers that mutate a `VcObject` (or any `&mut [Segment]`). They
// keep the segment list shape — bulges, kinds, layers — intact, only
// touching coordinates.

/// Rotate every point of `segments` around `pivot` by `angle_rad`.
pub fn rotate_segments(segments: &mut [Segment], pivot: Point2, angle_rad: f64) {
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();
    for s in segments.iter_mut() {
        s.start = rotate_point(s.start, pivot, cos_a, sin_a);
        s.end = rotate_point(s.end, pivot, cos_a, sin_a);
        if let Some(c) = s.center {
            s.center = Some(rotate_point(c, pivot, cos_a, sin_a));
        }
    }
}

/// Translate every point by `(dx, dy)`.
pub fn translate_segments(segments: &mut [Segment], dx: f64, dy: f64) {
    for s in segments.iter_mut() {
        s.start.x += dx;
        s.start.y += dy;
        s.end.x += dx;
        s.end.y += dy;
        if let Some(c) = s.center {
            s.center = Some(Point2::new(c.x + dx, c.y + dy));
        }
    }
}

/// Uniformly scale around `pivot`.
pub fn scale_segments(segments: &mut [Segment], pivot: Point2, factor: f64) {
    for s in segments.iter_mut() {
        s.start = scale_point(s.start, pivot, factor);
        s.end = scale_point(s.end, pivot, factor);
        if let Some(c) = s.center {
            s.center = Some(scale_point(c, pivot, factor));
        }
        // Bulge survives uniform scale unchanged (it's an angle ratio).
    }
}

/// Mirror across the X-axis line `y = pivot.y`. Negates bulge so arcs
/// stay valid in the mirrored frame.
pub fn mirror_segments_x(segments: &mut [Segment], pivot: Point2) {
    for s in segments.iter_mut() {
        s.start.y = 2.0 * pivot.y - s.start.y;
        s.end.y = 2.0 * pivot.y - s.end.y;
        if let Some(c) = s.center {
            s.center = Some(Point2::new(c.x, 2.0 * pivot.y - c.y));
        }
        s.bulge = -s.bulge;
    }
}

/// Mirror across the Y-axis line `x = pivot.x`. Negates bulge.
pub fn mirror_segments_y(segments: &mut [Segment], pivot: Point2) {
    for s in segments.iter_mut() {
        s.start.x = 2.0 * pivot.x - s.start.x;
        s.end.x = 2.0 * pivot.x - s.end.x;
        if let Some(c) = s.center {
            s.center = Some(Point2::new(2.0 * pivot.x - c.x, c.y));
        }
        s.bulge = -s.bulge;
    }
}

fn rotate_point(p: Point2, pivot: Point2, cos_a: f64, sin_a: f64) -> Point2 {
    let dx = p.x - pivot.x;
    let dy = p.y - pivot.y;
    Point2::new(
        pivot.x + dx * cos_a - dy * sin_a,
        pivot.y + dx * sin_a + dy * cos_a,
    )
}

fn scale_point(p: Point2, pivot: Point2, factor: f64) -> Point2 {
    Point2::new(
        pivot.x + (p.x - pivot.x) * factor,
        pivot.y + (p.y - pivot.y) * factor,
    )
}

/// Combined bbox over many objects (calc.py:objects2minmax).
#[must_use]
pub fn objects_bbox(objects: &[VcObject]) -> Option<crate::BBox> {
    let mut bbox = crate::BBox::EMPTY;
    let mut any = false;
    for obj in objects {
        for s in &obj.segments {
            bbox.extend_point(s.start);
            bbox.extend_point(s.end);
            any = true;
        }
    }
    if any {
        Some(bbox)
    } else {
        None
    }
}

/// Flatten a sequence of segments to a polyline. Connecting endpoints are
/// shared (no duplicate consecutive points).
#[must_use]
pub fn segments_to_points(segments: &[Segment], interpolate: usize) -> Vec<Point2> {
    let mut out: Vec<Point2> = Vec::new();
    for s in segments {
        let pts = segment_to_points(s, interpolate);
        if out.is_empty() {
            out.extend_from_slice(&pts);
        } else if let Some(last) = out.last() {
            if last.distance(pts[0]) < 1e-6 {
                out.extend_from_slice(&pts[1..]);
            } else {
                // Gap — insert anyway.
                out.extend_from_slice(&pts);
            }
        }
    }
    out
}

/// Register this module's wire types in the OpenAPI components map.
/// Co-located with the type definitions so adding a wire type is
/// a same-file edit; `crate::schema::components_schemas` composes these.
pub(crate) fn register_schemas(map: &mut crate::schema::SchemaMap) {
    crate::schema::insert::<VcObject>(map, "VcObject");
    crate::schema::insert::<offsets::PolylineOffset>(map, "PolylineOffset");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn line_intersection_basic() {
        let i = lines_intersect(
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 10.0),
            Point2::new(0.0, 10.0),
            Point2::new(10.0, 0.0),
        )
        .unwrap();
        assert!(approx(i.x, 5.0));
        assert!(approx(i.y, 5.0));
    }

    #[test]
    fn parallel_lines_no_intersection() {
        let i = lines_intersect(
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(0.0, 1.0),
            Point2::new(10.0, 1.0),
        );
        assert!(i.is_none());
    }

    #[test]
    fn polygon_inside_outside() {
        let sq = vec![
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(10.0, 10.0),
            Point2::new(0.0, 10.0),
        ];
        assert!(is_inside_polygon(&sq, Point2::new(5.0, 5.0)));
        assert!(!is_inside_polygon(&sq, Point2::new(15.0, 5.0)));
        assert!(!is_inside_polygon(&sq, Point2::new(-1.0, 5.0)));
    }

    #[test]
    fn translate_and_rotate_round_trip() {
        let mut segs = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0), "0", 7),
            Segment::arc(
                Point2::new(10.0, 0.0),
                Point2::new(0.0, 10.0),
                1.0,
                Some(Point2::new(0.0, 0.0)),
                "0",
                7,
            ),
        ];
        translate_segments(&mut segs, 5.0, 5.0);
        translate_segments(&mut segs, -5.0, -5.0);
        assert!(segs[0].start.distance(Point2::new(0.0, 0.0)) < 1e-9);
        assert!(segs[0].end.distance(Point2::new(10.0, 0.0)) < 1e-9);

        rotate_segments(&mut segs, Point2::new(0.0, 0.0), std::f64::consts::PI);
        rotate_segments(&mut segs, Point2::new(0.0, 0.0), -std::f64::consts::PI);
        assert!(segs[0].end.distance(Point2::new(10.0, 0.0)) < 1e-9);
    }

    #[test]
    fn mirror_negates_bulge() {
        let mut segs = vec![Segment::arc(
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            0.5,
            None,
            "0",
            7,
        )];
        let original_bulge = segs[0].bulge;
        mirror_segments_x(&mut segs, Point2::new(0.0, 0.0));
        assert!((segs[0].bulge + original_bulge).abs() < 1e-9);
    }

    #[test]
    fn arc_segment_tessellation_is_smooth() {
        let s = Segment::arc(
            Point2::new(1.0, 0.0),
            Point2::new(-1.0, 0.0),
            1.0, // bulge=1 → 180° CCW
            None,
            "0",
            7,
        );
        let pts = segment_to_points(&s, 6);
        assert!(pts.len() > 8);
        // All points should be ~unit-distance from origin.
        for p in &pts {
            assert!((p.x * p.x + p.y * p.y - 1.0).abs() < 1e-3);
        }
    }
}
