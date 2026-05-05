//! Offsetting operations: the cavalier_contours-driven parallel offset for
//! polylines-with-arcs (preserves bulges), and the clipper2-driven inward
//! cascade for nested pockets (operates on tessellated polygons).
//!
//! Mirrors `calc.py:do_pockets` and `objects2polyline_offsets` at the
//! algorithm level — see the unit tests for the contracts.

use cavalier_contours::polyline::{PlineSource, PlineSourceMut, PlineVertex, Polyline};
use clipper2::{EndType, JoinType, Paths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::cam::{segments_to_points, VcObject};
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::math;

/// One concentric offset of a closed object — used for both the boundary
/// pass and any inward pocket cascade rings.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PolylineOffset {
    pub segments: Vec<Segment>,
    pub closed: bool,
    /// 0 = outer boundary, 1+ = pocket cascade inward.
    pub level: u32,
    /// 0 = boundary, 1 = zigzag fill stroke, 2 = pocket ring.
    pub is_pocket: u8,
    pub layer: String,
    pub color: i32,
    pub source_object_idx: usize,
    /// Tab positions (data-space XY) the cutter should lift over while
    /// cutting this offset. Frontend places these via mtm.10; the gcode
    /// emitter splits the cut at each crossing and lifts Z to tabs.height.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<TabPoint>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct TabPoint {
    pub x: f64,
    pub y: f64,
}

/// Project a list of imported-segment-keyed tab points onto a generated
/// offset's tab list. We snap each tab to the closest point on the
/// offset's polyline; tabs that land further than `max_distance` from the
/// nearest segment are dropped (they belong to a different object).
pub fn attach_tabs_to_offsets(
    offsets: &mut [PolylineOffset],
    tabs_by_object: &HashMap<usize, Vec<TabPoint>>,
    max_distance: f64,
) {
    for offset in offsets.iter_mut() {
        let Some(tabs) = tabs_by_object.get(&offset.source_object_idx) else {
            continue;
        };
        for tab in tabs {
            // Snap to closest point on any segment of this offset.
            if let Some(snap) = snap_to_offset(offset, *tab, max_distance) {
                offset.tabs.push(snap);
            }
        }
    }
}

fn snap_to_offset(
    offset: &PolylineOffset,
    tab: TabPoint,
    max_distance: f64,
) -> Option<TabPoint> {
    let mut best: Option<(TabPoint, f64)> = None;
    for seg in &offset.segments {
        let p = closest_point_on_segment(seg, tab);
        let d = (p.x - tab.x).hypot(p.y - tab.y);
        if d > max_distance {
            continue;
        }
        if best.map_or(true, |(_, bd)| d < bd) {
            best = Some((p, d));
        }
    }
    best.map(|(p, _)| p)
}

fn closest_point_on_segment(seg: &Segment, tab: TabPoint) -> TabPoint {
    let dx = seg.end.x - seg.start.x;
    let dy = seg.end.y - seg.start.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-12 {
        return TabPoint {
            x: seg.start.x,
            y: seg.start.y,
        };
    }
    let t = (((tab.x - seg.start.x) * dx + (tab.y - seg.start.y) * dy) / len_sq).clamp(0.0, 1.0);
    TabPoint {
        x: seg.start.x + t * dx,
        y: seg.start.y + t * dy,
    }
}

/// Generate parallel offsets of `obj` at `delta`. Negative delta = inward
/// (right of CCW polylines), positive = outward. Preserves bulge so arc
/// geometry round-trips through the offset.
pub fn parallel_offset_object(obj: &VcObject, delta: f64) -> Vec<PolylineOffset> {
    if obj.segments.is_empty() {
        return Vec::new();
    }
    let pline = vc_to_pline(obj);
    let offsets = pline.parallel_offset(delta);
    offsets
        .into_iter()
        .map(|o| PolylineOffset {
            segments: pline_to_segments(&o, &obj.layer, obj.color),
            closed: o.is_closed(),
            level: 0,
            is_pocket: 0,
            layer: obj.layer.clone(),
            color: obj.color,
            source_object_idx: 0,
            tabs: Vec::new(),
        })
        .collect()
}

/// Inward-cascade pocket offsets. `boundary` is the (already-tool-radius-offset)
/// inner boundary; `delta` is the per-ring step (positive number — caller
/// doesn't need to negate). Returns rings from outermost to innermost.
pub fn pocket_cascade(boundary: &[Point2], delta: f64) -> Vec<Vec<Point2>> {
    if boundary.len() < 3 || delta <= 1e-9 {
        return Vec::new();
    }
    let mut current: Paths = boundary
        .iter()
        .map(|p| (p.x, p.y))
        .collect::<Vec<_>>()
        .into();
    let mut rings = Vec::new();
    loop {
        let next = current.inflate(-delta, JoinType::Round, EndType::Polygon, 2.0);
        let raw: Vec<Vec<(f64, f64)>> = next.clone().into();
        if raw.is_empty() || raw.iter().all(|r| r.len() < 3) {
            break;
        }
        for ring in &raw {
            if ring.len() >= 3 {
                rings.push(ring.iter().map(|(x, y)| Point2::new(*x, *y)).collect());
            }
        }
        current = next;
        if rings.len() > 1024 {
            break; // pathological — bail out.
        }
    }
    rings
}

/// Combine a parallel-offset boundary pass with an inward cascade. Returns
/// the boundary ring first (if `nocontour=false`), then progressively-inward
/// pocket rings.
pub fn pocket_for_object(
    obj: &VcObject,
    tool_radius: f64,
    nocontour: bool,
    interpolate: usize,
) -> Vec<PolylineOffset> {
    let mut out = Vec::new();
    // cavc convention: positive delta = LEFT of tangent. CCW polygons have
    // their interior on the left, so positive shrinks them. Pocketing is
    // always inward, so we hand cavc a positive delta.
    let boundary = parallel_offset_object(obj, tool_radius.abs());
    if boundary.is_empty() {
        return out;
    }
    for offset in &boundary {
        if !nocontour {
            out.push(offset.clone());
        }
        // Build the polygon from the offset's segments.
        let pts = segments_to_points(&offset.segments, interpolate);
        let rings = pocket_cascade(&pts, tool_radius.abs());
        for (i, ring) in rings.iter().enumerate() {
            if ring.len() < 2 {
                continue;
            }
            let mut segs = Vec::with_capacity(ring.len());
            for win in ring.windows(2) {
                segs.push(Segment::line(win[0], win[1], &offset.layer, offset.color));
            }
            // Close the ring.
            if let (Some(first), Some(last)) = (ring.first(), ring.last()) {
                if first.distance(*last) > 1e-6 {
                    segs.push(Segment::line(*last, *first, &offset.layer, offset.color));
                }
            }
            out.push(PolylineOffset {
                segments: segs,
                closed: true,
                level: (i + 1) as u32,
                is_pocket: 2,
                layer: offset.layer.clone(),
                color: offset.color,
                source_object_idx: offset.source_object_idx,
                tabs: Vec::new(),
            });
        }
    }
    out
}

// ─── conversions ────────────────────────────────────────────────────────────

fn vc_to_pline(obj: &VcObject) -> Polyline<f64> {
    let mut pl = if obj.closed {
        Polyline::new_closed()
    } else {
        Polyline::new()
    };
    for seg in &obj.segments {
        let bulge = if seg.kind == SegmentKind::Line {
            0.0
        } else {
            seg.bulge
        };
        pl.add_vertex(PlineVertex::new(seg.start.x, seg.start.y, bulge));
    }
    if !obj.closed {
        if let Some(last) = obj.segments.last() {
            pl.add_vertex(PlineVertex::new(last.end.x, last.end.y, 0.0));
        }
    }
    pl
}

fn pline_to_segments(pl: &Polyline<f64>, layer: &str, color: i32) -> Vec<Segment> {
    let n = pl.vertex_count();
    if n == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n);
    let last = if pl.is_closed() { n } else { n - 1 };
    for i in 0..last {
        let v0 = pl.at(i);
        let v1 = pl.at((i + 1) % n);
        let start = Point2::new(v0.x, v0.y);
        let end = Point2::new(v1.x, v1.y);
        if v0.bulge.abs() > 1e-12 {
            out.push(Segment::arc(start, end, v0.bulge, None, layer, color));
        } else {
            out.push(Segment::line(start, end, layer, color));
        }
    }
    out
}

// expose a tiny helper used by chaining tests
pub(crate) fn segments_signed_area(segments: &[Segment]) -> f64 {
    // Shoelace area of the closed polygon traced by the segment endpoints.
    if segments.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for s in segments {
        sum += (s.start.x * s.end.y) - (s.end.x * s.start.y);
        // Bulge contribution to area (Cavalier Contours treats this exactly,
        // but for chaining containment a small approximation is fine).
        if s.bulge.abs() > 1e-12 {
            let chord = s.start.distance(s.end);
            let sagitta = (s.bulge * chord) * 0.5;
            sum += sagitta * chord; // arc bulge area approx
        }
    }
    sum * 0.5
}

#[allow(dead_code)]
fn _math_unused() {
    let _ = math::TWO_PI;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point2;

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    fn closed_square(side: f64) -> VcObject {
        VcObject::new(
            vec![
                Segment::line(p(0.0, 0.0), p(side, 0.0), "0", 7),
                Segment::line(p(side, 0.0), p(side, side), "0", 7),
                Segment::line(p(side, side), p(0.0, side), "0", 7),
                Segment::line(p(0.0, side), p(0.0, 0.0), "0", 7),
            ],
            true,
        )
    }

    #[test]
    fn inward_offset_shrinks_a_square() {
        // Cavalier Contours convention: positive delta = LEFT of tangent.
        // Our square is wound CCW (interior on the left), so +2 is inward.
        let obj = closed_square(20.0);
        let offsets = parallel_offset_object(&obj, 2.0);
        assert!(!offsets.is_empty());
        let (mut minx, mut maxx, mut miny, mut maxy) =
            (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for s in &offsets[0].segments {
            minx = minx.min(s.start.x).min(s.end.x);
            maxx = maxx.max(s.start.x).max(s.end.x);
            miny = miny.min(s.start.y).min(s.end.y);
            maxy = maxy.max(s.start.y).max(s.end.y);
        }
        let w = maxx - minx;
        let h = maxy - miny;
        assert!((w - 16.0).abs() < 1e-3, "got width {w}");
        assert!((h - 16.0).abs() < 1e-3, "got height {h}");
    }

    #[test]
    fn pocket_cascade_produces_inward_rings() {
        let boundary = vec![p(0.0, 0.0), p(20.0, 0.0), p(20.0, 20.0), p(0.0, 20.0)];
        let rings = pocket_cascade(&boundary, 2.0);
        assert!(rings.len() >= 4, "expect at least 4 rings, got {}", rings.len());
        // Each ring is contained in the previous (smaller bbox).
        let mut prev_area = f64::INFINITY;
        for ring in &rings {
            let mut area = 0.0;
            for w in ring.windows(2) {
                area += (w[0].x * w[1].y) - (w[1].x * w[0].y);
            }
            area = area.abs() * 0.5;
            assert!(area < prev_area, "rings should shrink");
            prev_area = area;
        }
    }
}
