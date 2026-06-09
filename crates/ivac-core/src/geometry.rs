//! Core geometry types: points, segments, layers, bounding boxes.
//!
//! Mirrors `schema/openapi.yaml` so the JSON contract is shared. All field
//! names use serde renames where they differ from the YAML keys.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

impl Point2 {
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[must_use]
    pub fn distance(self, other: Self) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        dx.hypot(dy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum SegmentKind {
    Line,
    Arc,
    Circle,
    Point,
}

/// A flat LINE/ARC primitive. ARC geometry is encoded as the bulge between
/// `start` and `end` (bulge = `tan(included_angle / 4)`).
///
/// `layer` is `Arc<str>` rather than `String` (jzpl Phase 2). A typical
/// DXF has thousands of segments across a handful of layer names — the
/// Arc lets every segment on the same layer share one allocation. Clone
/// becomes a refcount bump; serde + `JsonSchema` treat it as a normal
/// string on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Segment {
    #[serde(rename = "type")]
    pub kind: SegmentKind,
    pub start: Point2,
    pub end: Point2,
    #[serde(default)]
    pub bulge: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center: Option<Point2>,
    #[serde(default = "default_layer")]
    #[schemars(with = "String")]
    pub layer: std::sync::Arc<str>,
    #[serde(default = "default_color")]
    pub color: i32,
}

fn default_layer() -> std::sync::Arc<str> {
    std::sync::Arc::from("0")
}
fn default_color() -> i32 {
    7
}

impl Segment {
    pub fn line(
        start: Point2,
        end: Point2,
        layer: impl Into<std::sync::Arc<str>>,
        color: i32,
    ) -> Self {
        Self {
            kind: SegmentKind::Line,
            start,
            end,
            bulge: 0.0,
            center: None,
            layer: layer.into(),
            color,
        }
    }

    pub fn arc(
        start: Point2,
        end: Point2,
        bulge: f64,
        center: Option<Point2>,
        layer: impl Into<std::sync::Arc<str>>,
        color: i32,
    ) -> Self {
        Self {
            kind: SegmentKind::Arc,
            start,
            end,
            bulge,
            center,
            layer: layer.into(),
            color,
        }
    }

    pub fn point(at: Point2, layer: impl Into<std::sync::Arc<str>>, color: i32) -> Self {
        Self {
            kind: SegmentKind::Point,
            start: at,
            end: at,
            bulge: 0.0,
            center: None,
            layer: layer.into(),
            color,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Layer {
    pub name: String,
    pub color: i32,
    pub segment_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BBox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl BBox {
    pub const EMPTY: Self = Self {
        min_x: f64::INFINITY,
        min_y: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        max_y: f64::NEG_INFINITY,
    };

    pub fn extend_point(&mut self, p: Point2) {
        self.min_x = self.min_x.min(p.x);
        self.min_y = self.min_y.min(p.y);
        self.max_x = self.max_x.max(p.x);
        self.max_y = self.max_y.max(p.y);
    }

    #[must_use]
    pub fn from_segments(segments: &[Segment]) -> Self {
        let mut bbox = Self::EMPTY;
        for s in segments {
            bbox.extend_point(s.start);
            bbox.extend_point(s.end);
        }
        if !bbox.is_finite() {
            // No segments / all rejected. Mimic the Python default 0,0–10,10.
            bbox = Self {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 10.0,
                max_y: 10.0,
            };
        }
        bbox
    }

    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.min_x.is_finite()
            && self.min_y.is_finite()
            && self.max_x.is_finite()
            && self.max_y.is_finite()
    }

    #[must_use]
    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }
    #[must_use]
    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }
}

/// Point-in-polygon test via a half-open scanline crossing count. An edge
/// counts only over `[lo.y, hi.y)` (with a 1e-12 epsilon) and horizontal
/// edges are skipped, so a ray grazing a shared vertex isn't double-
/// counted — the boundary-robust variant the offset / pocket / V-carve
/// fill code relies on. `verts` is an open ring (the closing edge is
/// implied). Contrast [`is_inside_polygon`], the plain no-epsilon ray
/// cast used where exact-boundary behavior doesn't matter.
// x/y (the test point) and the scanline locals (n, i, j) are the
// conventional single-letter names for a ray-cast crossing count.
#[allow(clippy::many_single_char_names)]
#[must_use]
pub fn point_in_polygon(verts: &[Point2], x: f64, y: f64) -> bool {
    let n = verts.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        if (a.y - b.y).abs() < 1e-12 {
            continue;
        }
        let (lo, hi) = if a.y < b.y { (a, b) } else { (b, a) };
        if y < lo.y - 1e-12 || y >= hi.y - 1e-12 {
            continue;
        }
        let t = (y - lo.y) / (hi.y - lo.y);
        let xi = lo.x + t * (hi.x - lo.x);
        if xi > x {
            inside = !inside;
        }
    }
    inside
}

/// Point-in-polygon test via a classic even-odd ray cast (no epsilon). A
/// vertex on a horizontal scanline can be counted twice, so prefer
/// [`point_in_polygon`] where exact-boundary robustness matters; this
/// variant is used by chaining / V-carve region nesting / fixture checks
/// where the probe point is well inside or outside a face.
#[must_use]
pub fn is_inside_polygon(points: &[Point2], p: Point2) -> bool {
    if points.len() < 3 {
        return false;
    }
    let mut inside = false;
    let n = points.len();
    let mut j = n - 1;
    for i in 0..n {
        let pi = points[i];
        let pj = points[j];
        let crosses_y = (pi.y > p.y) != (pj.y > p.y);
        if crosses_y {
            let x_at = pi.x + (p.y - pi.y) * (pj.x - pi.x) / (pj.y - pi.y);
            if p.x < x_at {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

#[cfg(test)]
// `assert_eq!(bbox.min_x, -3.0)` — values are copied verbatim from earlier
// `.extend_point` calls, so exact float equality is the correct test.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn segment_serialize_matches_json_contract() {
        let s = Segment::line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0), "0", 7);
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["type"], "LINE");
        assert_eq!(json["start"]["x"], 0.0);
        assert_eq!(json["end"]["x"], 10.0);
        assert_eq!(json["bulge"], 0.0);
        assert_eq!(json["layer"], "0");
        assert_eq!(json["color"], 7);
        assert!(json.get("center").is_none()); // Skipped when None.
    }

    #[test]
    fn arc_serializes_center() {
        let s = Segment::arc(
            Point2::new(10.0, 0.0),
            Point2::new(0.0, 10.0),
            1.0,
            Some(Point2::new(0.0, 0.0)),
            "0",
            7,
        );
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["type"], "ARC");
        assert_eq!(json["center"]["x"], 0.0);
    }

    #[test]
    fn bbox_extends() {
        let mut bbox = BBox::EMPTY;
        bbox.extend_point(Point2::new(1.0, 2.0));
        bbox.extend_point(Point2::new(-3.0, 4.0));
        assert_eq!(bbox.min_x, -3.0);
        assert_eq!(bbox.max_x, 1.0);
        assert_eq!(bbox.max_y, 4.0);
    }
}

/// Register this module's wire types in the OpenAPI components map.
/// Co-located with the type definitions (kb1y) so adding a wire type is
/// a same-file edit; `crate::schema::components_schemas` composes these.
pub(crate) fn register_schemas(map: &mut crate::schema::SchemaMap) {
    crate::schema::insert::<Point2>(map, "Point2");
    crate::schema::insert::<BBox>(map, "BBox");
    crate::schema::insert::<Layer>(map, "Layer");
    crate::schema::insert::<Segment>(map, "Segment");
}
