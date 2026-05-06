//! Offsetting operations: the cavalier_contours-driven parallel offset for
//! polylines-with-arcs (preserves bulges), and the clipper2-driven inward
//! cascade for nested pockets (operates on tessellated polygons).
//!
//! Mirrors `calc.py:do_pockets` and `objects2polyline_offsets` at the
//! algorithm level — see the unit tests for the contracts.

use cavalier_contours::polyline::{PlineSource, PlineSourceMut, PlineVertex, Polyline};
#[cfg(feature = "pocket-cascade")]
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
///
/// Convenience wrapper for the common single-boundary case; calls
/// [`pocket_cascade_with_islands`] with no holes.
pub fn pocket_cascade(boundary: &[Point2], delta: f64) -> Vec<Vec<Point2>> {
    pocket_cascade_with_islands(boundary, &[], delta)
}

/// Inward-cascade pocket offsets that respect islands (closed contours
/// inside the boundary that should be left uncut). Each `island` is a
/// closed polyline already inflated by `tool_radius` outward — the
/// caller is responsible for that pre-inflation, matching the upstream
/// Python `do_pockets` islands branch.
///
/// Without the `pocket-cascade` feature (e.g. wasm32 builds without a C++
/// stdlib) the cascade is unavailable and this returns an empty list.
#[cfg(feature = "pocket-cascade")]
pub fn pocket_cascade_with_islands(
    boundary: &[Point2],
    islands: &[Vec<Point2>],
    delta: f64,
) -> Vec<Vec<Point2>> {
    if boundary.len() < 3 || delta <= 1e-9 {
        return Vec::new();
    }
    let mut current: Paths = build_paths(boundary, islands);
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
            break;
        }
    }
    rings
}

#[cfg(not(feature = "pocket-cascade"))]
pub fn pocket_cascade_with_islands(
    _boundary: &[Point2],
    _islands: &[Vec<Point2>],
    _delta: f64,
) -> Vec<Vec<Point2>> {
    Vec::new()
}

#[cfg(feature = "pocket-cascade")]
fn build_paths(boundary: &[Point2], islands: &[Vec<Point2>]) -> Paths {
    // Clipper2 treats CW-wound rings as holes when EndType::Polygon is in
    // play. Force the outer boundary CCW and the islands CW regardless of
    // how the caller hands them in.
    let mut all: Vec<Vec<(f64, f64)>> = Vec::with_capacity(islands.len() + 1);
    let outer = if signed_area(boundary) > 0.0 {
        boundary.to_vec()
    } else {
        let mut r = boundary.to_vec();
        r.reverse();
        r
    };
    all.push(outer.iter().map(|p| (p.x, p.y)).collect());
    for island in islands {
        if island.len() < 3 {
            continue;
        }
        let hole = if signed_area(island) < 0.0 {
            island.clone()
        } else {
            let mut r = island.clone();
            r.reverse();
            r
        };
        all.push(hole.iter().map(|p| (p.x, p.y)).collect());
    }
    all.into()
}

#[cfg(feature = "pocket-cascade")]
fn signed_area(pts: &[Point2]) -> f64 {
    if pts.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}

/// Combine a parallel-offset boundary pass with an inward cascade. Returns
/// the boundary ring first (if `nocontour=false`), then progressively-inward
/// pocket rings. When `zigzag` is true the inward cascade is replaced with
/// a single back-and-forth raster fill (one open polyline per offset).
///
/// `islands` are closed contours that should be left uncut inside the
/// pocket (e.g. raised features). Each island gets pre-inflated by the
/// tool radius before being subtracted from the cascade.
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
    islands: &[Vec<Point2>],
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

        let rings = pocket_cascade_with_islands(&pts, islands, tool_radius.abs());
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

#[allow(dead_code)]
fn _math_unused() {
    let _ = math::TWO_PI;
}

/// Apply overcut to every closed offset whose source object exists in
/// `objects`. The dip targets each offset's owning original boundary, not the
/// offset's own corners, so cascade rings still respect the parent shape.
pub fn apply_overcut_to_offsets(
    offsets: &mut [PolylineOffset],
    objects: &[VcObject],
    tool_radius: f64,
) {
    for off in offsets.iter_mut() {
        if !off.closed {
            continue;
        }
        if let Some(obj) = objects.get(off.source_object_idx) {
            apply_overcut(off, &obj.segments, tool_radius);
        }
    }
}

/// Apply overcut to a closed offset polyline whose reflex (concave) corners
/// need a small dip toward the original wall so the cutter (radius
/// `tool_radius`) clears the geometric corner.
///
/// Pre-conditions: `offset.closed`, polyline is wound CCW (interior on left),
/// `boundary_segments` is the original object boundary the offset was derived
/// from. Arcs are skipped (no overcut applied across them).
///
/// At each reflex corner of the offset polyline we cast a ray along the
/// outward bisector and stop at the first boundary endpoint that lies on the
/// ray. The dip length is `dist_to_boundary - tool_radius`; the inserted
/// vertex pattern is `corner, dip, corner` so the cutter swings out and back.
pub fn apply_overcut(
    offset: &mut PolylineOffset,
    boundary_segments: &[Segment],
    tool_radius: f64,
) {
    use std::f64::consts::FRAC_PI_4;
    if !offset.closed || offset.segments.len() < 3 {
        return;
    }
    let r_abs = tool_radius.abs();
    let n = offset.segments.len();
    let pts: Vec<(Point2, f64)> = offset
        .segments
        .iter()
        .map(|s| (s.start, s.bulge))
        .collect();

    let mut emitted: Vec<(f64, f64, f64)> = Vec::with_capacity(n * 2);

    for i in 0..n {
        let prev = pts[(i + n - 1) % n].0;
        let cur = pts[i].0;
        let next = pts[(i + 1) % n].0;
        let in_bulge = pts[(i + n - 1) % n].1;
        let out_bulge = pts[i].1;

        // Always emit the corner first.
        emitted.push((cur.x, cur.y, out_bulge));

        // Skip arc-bounded corners; the dip only makes sense between two
        // straight segments.
        if in_bulge.abs() > 1e-12 || out_bulge.abs() > 1e-12 {
            continue;
        }

        let tin = (cur.x - prev.x, cur.y - prev.y);
        let tout = (next.x - cur.x, next.y - cur.y);
        let len_in = (tin.0 * tin.0 + tin.1 * tin.1).sqrt();
        let len_out = (tout.0 * tout.0 + tout.1 * tout.1).sqrt();
        if len_in < 1e-9 || len_out < 1e-9 {
            continue;
        }
        let ti = (tin.0 / len_in, tin.1 / len_in);
        let to_ = (tout.0 / len_out, tout.1 / len_out);

        // Signed turn: positive = left (convex on CCW), negative = right
        // (reflex on CCW). Need a sharp right turn.
        let cross = ti.0 * to_.1 - ti.1 * to_.0;
        let dot = ti.0 * to_.0 + ti.1 * to_.1;
        let turn = cross.atan2(dot);
        if turn >= -FRAC_PI_4 {
            continue;
        }

        // Outward bisector at a reflex corner: opposite of (-tin + tout)
        // (which points into the interior at convex corners). At a reflex
        // corner the geometric "interior" direction sits on the OPPOSITE side
        // of the offset's local interior — i.e. toward the original wall —
        // so we negate.
        let bx = -ti.0 + to_.0;
        let by = -ti.1 + to_.1;
        let blen = (bx * bx + by * by).sqrt();
        if blen < 1e-9 {
            continue;
        }
        let out = (-bx / blen, -by / blen);

        // Probe boundary endpoints along the outward ray.
        let mut nearest: Option<f64> = None;
        for seg in boundary_segments {
            for p1 in [seg.start, seg.end] {
                let dx = p1.x - cur.x;
                let dy = p1.y - cur.y;
                let along = dx * out.0 + dy * out.1;
                if along <= 1e-6 {
                    continue;
                }
                let perp = (dx * out.1 - dy * out.0).abs();
                if perp > 0.25 {
                    continue;
                }
                if nearest.map_or(true, |c| along < c) {
                    nearest = Some(along);
                }
            }
        }
        let Some(dist) = nearest else {
            continue;
        };
        let dip = dist - r_abs;
        if dip <= 1e-6 {
            continue;
        }
        let dip_x = cur.x + out.0 * dip;
        let dip_y = cur.y + out.1 * dip;
        // Pattern at the corner: corner, dip, corner. The first `corner` is
        // the one we already pushed (with its outgoing bulge cleared so the
        // dip-to is a straight line); we need to fix that.
        if let Some(last_emit) = emitted.last_mut() {
            // We just pushed (cur, out_bulge). Reset its outgoing bulge to 0
            // so the segment to the dip is straight.
            last_emit.2 = 0.0;
        }
        emitted.push((dip_x, dip_y, 0.0));
        emitted.push((cur.x, cur.y, out_bulge));
    }

    if emitted.len() < 3 || emitted.len() == n {
        return;
    }

    let mut new_segs: Vec<Segment> = Vec::with_capacity(emitted.len());
    let m = emitted.len();
    for i in 0..m {
        let a = emitted[i];
        let b = emitted[(i + 1) % m];
        let kind = if a.2.abs() > 1e-12 {
            SegmentKind::Arc
        } else {
            SegmentKind::Line
        };
        new_segs.push(Segment {
            kind,
            start: Point2 { x: a.0, y: a.1 },
            end: Point2 { x: b.0, y: b.1 },
            bulge: a.2,
            center: None,
            layer: offset.layer.clone(),
            color: offset.color,
        });
    }
    offset.segments = new_segs;
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
        let offsets = pocket_for_object(&obj, 1.5, false, 6, false, &[]);
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

    #[cfg(feature = "pocket-cascade")]
    #[test]
    fn pocket_cascade_with_island_skips_around_it() {
        // 30x30 outer with a 10x10 island centered at (15, 15).
        let outer = vec![p(0.0, 0.0), p(30.0, 0.0), p(30.0, 30.0), p(0.0, 30.0)];
        let island = vec![p(10.0, 10.0), p(20.0, 10.0), p(20.0, 20.0), p(10.0, 20.0)];
        let rings = pocket_cascade_with_islands(&outer, &[island], 2.0);
        assert!(!rings.is_empty(), "should produce at least one ring");
        // No ring should cross the island's interior.
        for ring in &rings {
            for pt in ring {
                let inside = pt.x > 10.5 && pt.x < 19.5 && pt.y > 10.5 && pt.y < 19.5;
                assert!(!inside, "pocket ring crossed the island at {:?}", pt);
            }
        }
    }

    #[test]
    fn overcut_dips_into_inner_corner() {
        // L-shaped boundary CCW: a 20x20 square with a 10x10 notch removed
        // from the top-right. The reflex corner sits at (10, 10).
        // Boundary CCW: (0,0)→(20,0)→(20,10)→(10,10)→(10,20)→(0,20)→(0,0).
        let boundary = vec![
            Segment::line(p(0.0, 0.0), p(20.0, 0.0), "0", 7),
            Segment::line(p(20.0, 0.0), p(20.0, 10.0), "0", 7),
            Segment::line(p(20.0, 10.0), p(10.0, 10.0), "0", 7),
            Segment::line(p(10.0, 10.0), p(10.0, 20.0), "0", 7),
            Segment::line(p(10.0, 20.0), p(0.0, 20.0), "0", 7),
            Segment::line(p(0.0, 20.0), p(0.0, 0.0), "0", 7),
        ];
        // A radius-1 inward parallel offset of an L would put the reflex
        // corner at the offset (~(11,11)) on a CCW polyline. We construct
        // it by hand to keep the test independent of cavc's exact mitering.
        let r = 1.0_f64;
        let mut offset = PolylineOffset {
            segments: vec![
                Segment::line(p(r, r), p(20.0 - r, r), "0", 7),
                Segment::line(p(20.0 - r, r), p(20.0 - r, 10.0 - r), "0", 7),
                Segment::line(p(20.0 - r, 10.0 - r), p(10.0 + r, 10.0 - r), "0", 7),
                Segment::line(p(10.0 + r, 10.0 - r), p(10.0 + r, 20.0 - r), "0", 7),
                Segment::line(p(10.0 + r, 20.0 - r), p(r, 20.0 - r), "0", 7),
                Segment::line(p(r, 20.0 - r), p(r, r), "0", 7),
            ],
            closed: true,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
        };
        let before = offset.segments.len();
        // Wait — for an inside-of-shape offset like a pocket, the offset poly
        // is wound CCW and the L's reflex corner becomes a CONVEX corner on
        // the offset (mitered). For overcut we need the reflex case: that's
        // an OUTSIDE cut around an L-shaped island where the offset poly is
        // CW. Reverse the offset segments to get the right winding.
        offset.segments.reverse();
        for s in offset.segments.iter_mut() {
            std::mem::swap(&mut s.start, &mut s.end);
        }
        apply_overcut(&mut offset, &boundary, 1.0);
        // At the lone reflex corner we add 2 extra vertices (= 2 extra segments).
        assert!(
            offset.segments.len() > before,
            "overcut should add segments at sharp reflex corners (was {before}, now {})",
            offset.segments.len()
        );
        // All inserted vertices stay in the data-space bbox of the original.
        for s in &offset.segments {
            for pt in [s.start, s.end] {
                assert!(pt.x >= -0.01 && pt.x <= 20.01, "overcut vertex out of bbox: {pt:?}");
                assert!(pt.y >= -0.01 && pt.y <= 20.01, "overcut vertex out of bbox: {pt:?}");
            }
        }
    }

    #[cfg(feature = "pocket-cascade")]
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
