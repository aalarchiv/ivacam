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
    #[must_use] pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[must_use] pub fn distance(self, other: Self) -> f64 {
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
    pub layer: String,
    #[serde(default = "default_color")]
    pub color: i32,
}

fn default_layer() -> String {
    "0".into()
}
fn default_color() -> i32 {
    7
}

impl Segment {
    pub fn line(start: Point2, end: Point2, layer: impl Into<String>, color: i32) -> Self {
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
        layer: impl Into<String>,
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

    pub fn point(at: Point2, layer: impl Into<String>, color: i32) -> Self {
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

    #[must_use] pub fn from_segments(segments: &[Segment]) -> Self {
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

    #[must_use] pub fn is_finite(&self) -> bool {
        self.min_x.is_finite()
            && self.min_y.is_finite()
            && self.max_x.is_finite()
            && self.max_y.is_finite()
    }

    #[must_use] pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }
    #[must_use] pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }
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
