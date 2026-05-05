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

/// Generate a zigzag (raster) pocket fill within `boundary`. The fill is
/// a series of horizontal sweep lines at Y stride `tool_diameter * 0.9`,
/// each segment trimmed to the polygon's interior. Adjacent strokes are
/// joined at their endpoints to form a single open polyline (returns a
/// chain of segments).
pub fn pocket_zigzag(boundary: &[Point2], tool_diameter: f64) -> Vec<Segment> {
    if boundary.len() < 3 || tool_diameter <= 0.0 {
        return Vec::new();
    }
    let stride = (tool_diameter * 0.9).max(0.1);
    let (min_y, max_y) = boundary
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.y), hi.max(p.y))
        });
    let (min_x, max_x) = boundary
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.x), hi.max(p.x))
        });

    let mut out = Vec::new();
    let mut prev_end: Option<Point2> = None;
    let mut flip = false;
    let mut y = min_y + tool_diameter * 0.5;
    while y <= max_y - tool_diameter * 0.5 + 1e-9 {
        let crossings = horizontal_crossings(boundary, y, min_x, max_x);
        // Group into entry/exit pairs (even-odd rule).
        let mut iter = crossings.chunks_exact(2);
        let mut strokes: Vec<(Point2, Point2)> = Vec::new();
        for pair in iter.by_ref() {
            let (a, b) = (pair[0], pair[1]);
            // Inset both ends by half a tool diameter so we don't carve
            // outside the polygon interior on the corners.
            let inset = tool_diameter * 0.5;
            let new_a = a.min(b - inset.min((b - a).abs())) + inset.min((b - a).abs() * 0.5);
            let new_b = a.max(b - inset.min((b - a).abs())) - inset.min((b - a).abs() * 0.5);
            if new_b <= new_a + 1e-6 {
                continue;
            }
            strokes.push((Point2::new(new_a, y), Point2::new(new_b, y)));
        }
        if flip {
            strokes.reverse();
            for s in &mut strokes {
                std::mem::swap(&mut s.0, &mut s.1);
            }
        }
        flip = !flip;
        for (a, b) in strokes {
            if let Some(prev) = prev_end {
                if prev.distance(a) > 1e-6 {
                    out.push(Segment::line(prev, a, "0", 7));
                }
            }
            out.push(Segment::line(a, b, "0", 7));
            prev_end = Some(b);
        }
        y += stride;
    }
    out
}

fn horizontal_crossings(poly: &[Point2], y: f64, min_x: f64, max_x: f64) -> Vec<f64> {
    let mut xs = Vec::new();
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        // Pure horizontal edge: skip; handled by neighbors.
        if (a.y - b.y).abs() < 1e-12 {
            continue;
        }
        let (lo, hi) = if a.y < b.y { (a, b) } else { (b, a) };
        // Half-open interval: [lo.y, hi.y) so we don't double-count
        // corner crossings.
        if y < lo.y - 1e-12 || y >= hi.y - 1e-12 {
            continue;
        }
        let t = (y - lo.y) / (hi.y - lo.y);
        let x = lo.x + t * (hi.x - lo.x);
        if x >= min_x - 1e-3 && x <= max_x + 1e-3 {
            xs.push(x);
        }
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs
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
/// pocket rings. When `zigzag` is true the inward cascade is replaced with
/// a single back-and-forth raster fill (one open polyline per offset).
///
/// Special case: if `obj` is a single CIRCLE/POINT segment with radius
/// smaller than the tool radius, we can't carve a pocket — emit a drill
/// at center instead (a zero-length cut that the gcode emitter will turn
/// into a plunge).
pub fn pocket_for_object(
    obj: &VcObject,
    tool_radius: f64,
    nocontour: bool,
    interpolate: usize,
    zigzag: bool,
) -> Vec<PolylineOffset> {
    let mut out = Vec::new();

    if let Some(drill) = small_circle_drill(obj, tool_radius) {
        out.push(drill);
        return out;
    }

    let boundary = parallel_offset_object(obj, tool_radius.abs());
    if boundary.is_empty() {
        return out;
    }
    for offset in &boundary {
        if !nocontour {
            out.push(offset.clone());
        }
        let pts = segments_to_points(&offset.segments, interpolate);

        if zigzag {
            let strokes = pocket_zigzag(&pts, tool_radius.abs() * 2.0);
            if !strokes.is_empty() {
                out.push(PolylineOffset {
                    segments: strokes,
                    closed: false,
                    level: 1,
                    is_pocket: 1,
                    layer: offset.layer.clone(),
                    color: offset.color,
                    source_object_idx: offset.source_object_idx,
                    tabs: Vec::new(),
                });
            }
            continue;
        }

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

/// If `obj` is a single closed CIRCLE smaller than the tool, return a
/// drill-only offset whose single segment is a zero-length POINT at the
/// circle's center. The gcode emitter handles this as plunge + retract.
fn small_circle_drill(obj: &VcObject, tool_radius: f64) -> Option<PolylineOffset> {
    use crate::geometry::SegmentKind;
    if !obj.closed || obj.segments.is_empty() {
        return None;
    }
    let kinds_circle_only = obj
        .segments
        .iter()
        .all(|s| s.kind == SegmentKind::Circle);
    if !kinds_circle_only {
        return None;
    }
    let center = obj.segments[0].center?;
    let radius = obj.segments[0].start.distance(center);
    if radius >= tool_radius * 0.95 {
        return None;
    }
    Some(PolylineOffset {
        segments: vec![Segment::point(center, &obj.layer, obj.color)],
        closed: false,
        level: 0,
        is_pocket: 0,
        layer: obj.layer.clone(),
        color: obj.color,
        source_object_idx: 0,
        tabs: Vec::new(),
    })
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
    fn small_circle_becomes_a_drill_point() {
        use crate::geometry::SegmentKind;
        // 1mm-radius circle (encoded as two semicircles like the importer
        // does) with a 3mm tool — pocket should collapse to a single drill.
        let r = 1.0;
        let center = Point2::new(5.0, 5.0);
        let p_right = Point2::new(center.x + r, center.y);
        let p_left = Point2::new(center.x - r, center.y);
        let half1 = Segment {
            kind: SegmentKind::Circle,
            start: p_right,
            end: p_left,
            bulge: 1.0,
            center: Some(center),
            layer: "0".into(),
            color: 7,
        };
        let half2 = Segment {
            kind: SegmentKind::Circle,
            start: p_left,
            end: p_right,
            bulge: 1.0,
            center: Some(center),
            layer: "0".into(),
            color: 7,
        };
        let obj = VcObject::new(vec![half1, half2], true);
        let offsets = pocket_for_object(&obj, 1.5, false, 6, false);
        assert_eq!(offsets.len(), 1);
        assert_eq!(offsets[0].segments.len(), 1);
        assert!(matches!(
            offsets[0].segments[0].kind,
            SegmentKind::Point
        ));
        assert!(offsets[0].segments[0].start.distance(center) < 1e-9);
    }

    #[test]
    fn zigzag_pocket_fills_a_square() {
        let boundary = vec![p(0.0, 0.0), p(20.0, 0.0), p(20.0, 20.0), p(0.0, 20.0)];
        let segs = pocket_zigzag(&boundary, 2.0);
        assert!(
            segs.len() > 5,
            "20x20 square at tool diameter 2 should produce many strokes; got {}",
            segs.len()
        );
        // Adjacent stroke endpoints should connect (no big jumps).
        for w in segs.windows(2) {
            let gap = w[0].end.distance(w[1].start);
            assert!(gap < 6.0, "stroke gap too large: {gap}");
        }
        // All endpoints should be inside the boundary's relaxed inset.
        for s in &segs {
            for pt in [s.start, s.end] {
                assert!(pt.x >= -0.01 && pt.x <= 20.01);
                assert!(pt.y >= -0.01 && pt.y <= 20.01);
            }
        }
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
