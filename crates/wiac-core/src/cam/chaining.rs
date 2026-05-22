//! Segment chaining — port of viaConstructor's `calc.py:segments2objects` and
//! the supporting helpers (`get_next_line`, `lines_to_path`, `clean_segments`,
//! `find_outer_objects`, `find_tool_offsets`).
//!
//! Walks a flat segment list, glues endpoints into chains, classifies each
//! chain as open/closed, and discovers parent/child containment.

// # CAM/sim pedantic-lint exemptions
// Chain endpoint matching uses `p_a`/`p_b` segment-endpoint names.
#![allow(clippy::similar_names)]

use std::collections::HashMap;

use crate::cam::{is_inside_polygon, segment_to_points, segments_to_points, VcObject};
use crate::geometry::{Point2, Segment};

/// Minimum endpoint-distance below which two segment endpoints are
/// treated as the same vertex. sj4t: a flat 1e-3 mm tolerance is too
/// loose for sub-mm imports (a 0.5 mm cabochon would have its edges
/// chained into the neighbouring contour). [`fuzzy_for_segments`]
/// returns a bbox-scaled tolerance so callers adapt to the working
/// scale; this constant remains as the lower bound and as a fallback
/// for callers that don't yet take a tolerance.
const FUZZY_MIN: f64 = 1e-3;
/// Scale factor applied to the diagonal of the segments' bbox to derive
/// an adaptive endpoint-merge tolerance (sj4t). A 200 mm-diagonal sheet
/// gets ~2e-2 mm, a 5 mm cabochon gets the floor at 1e-3 mm.
const FUZZY_BBOX_FRACTION: f64 = 1e-4;

/// Adaptive endpoint-merge tolerance for `segments`: max of
/// [`FUZZY_MIN`] and `FUZZY_BBOX_FRACTION * bbox_diagonal`. This is the
/// chaining + closure tolerance — too loose chains across thin
/// neighbours; too tight refuses to close hand-traced contours.
fn fuzzy_for_segments(segments: &[Segment]) -> f64 {
    if segments.is_empty() {
        return FUZZY_MIN;
    }
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for s in segments {
        for p in [s.start, s.end] {
            if p.x < min_x {
                min_x = p.x;
            }
            if p.y < min_y {
                min_y = p.y;
            }
            if p.x > max_x {
                max_x = p.x;
            }
            if p.y > max_y {
                max_y = p.y;
            }
        }
    }
    if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
        return FUZZY_MIN;
    }
    let diag = (max_x - min_x).hypot(max_y - min_y);
    (diag * FUZZY_BBOX_FRACTION).max(FUZZY_MIN)
}

/// Group `segments` into [`VcObject`]s (chains) by walking neighbor endpoints.
/// Closed chains (last endpoint matches first) get `closed = true`.
#[must_use]
pub fn segments_to_objects(segments: &[Segment]) -> Vec<VcObject> {
    let fuzzy = fuzzy_for_segments(segments);
    let cell_size = fuzzy * 4.0;
    let mut taken = vec![false; segments.len()];
    let mut out = Vec::new();
    // Spatial hash over endpoints — each segment contributes both its
    // start and end so `find_neighbor` can probe nearby cells in O(1)
    // amortized instead of the legacy O(n) full scan. A 5000-segment
    // DXF goes from 25 M probes to ~10 k.
    let mut grid = build_endpoint_index(segments, cell_size);

    while let Some(seed) = next_unused(&taken) {
        taken[seed] = true;
        consume_endpoints(&mut grid, seed, &segments[seed], cell_size);
        let mut chain = vec![segments[seed].clone()];
        loop {
            let tail = chain.last().unwrap().end;
            let Some(next_idx) = find_neighbor(segments, &taken, &grid, tail, fuzzy, cell_size)
            else {
                break;
            };
            taken[next_idx] = true;
            consume_endpoints(&mut grid, next_idx, &segments[next_idx], cell_size);
            let mut s = segments[next_idx].clone();
            if !points_equal(s.start, tail, fuzzy) && points_equal(s.end, tail, fuzzy) {
                std::mem::swap(&mut s.start, &mut s.end);
                s.bulge = -s.bulge;
            }
            chain.push(s);
        }
        loop {
            let head = chain.first().unwrap().start;
            let Some(prev_idx) = find_neighbor(segments, &taken, &grid, head, fuzzy, cell_size)
            else {
                break;
            };
            taken[prev_idx] = true;
            consume_endpoints(&mut grid, prev_idx, &segments[prev_idx], cell_size);
            let mut s = segments[prev_idx].clone();
            if !points_equal(s.end, head, fuzzy) && points_equal(s.start, head, fuzzy) {
                std::mem::swap(&mut s.start, &mut s.end);
                s.bulge = -s.bulge;
            }
            chain.insert(0, s);
        }
        let closed = chain
            .first()
            .unwrap()
            .start
            .distance(chain.last().unwrap().end)
            < fuzzy;
        out.push(VcObject::new(chain, closed));
    }

    out
}

/// (`cell_x`, `cell_y`) -> list of segment indices whose start OR end falls
/// in that cell. Segments appear in both their start cell and their end
/// cell so an endpoint probe can find them from either side.
type EndpointGrid = HashMap<(i64, i64), Vec<usize>>;

fn cell_of(p: Point2, cell_size: f64) -> (i64, i64) {
    // Project a CAM-scale coordinate (mm, bounded by stock dimensions
    // ≪ i64 range) into a grid cell. `.floor() as i64` is the standard
    // pattern and the value cannot truncate within the supported scale.
    #[allow(clippy::cast_possible_truncation)]
    (
        (p.x / cell_size).floor() as i64,
        (p.y / cell_size).floor() as i64,
    )
}

fn build_endpoint_index(segments: &[Segment], cell_size: f64) -> EndpointGrid {
    let mut grid: EndpointGrid = HashMap::with_capacity(segments.len() * 2);
    for (i, s) in segments.iter().enumerate() {
        grid.entry(cell_of(s.start, cell_size)).or_default().push(i);
        let end_cell = cell_of(s.end, cell_size);
        if end_cell != cell_of(s.start, cell_size) {
            grid.entry(end_cell).or_default().push(i);
        }
    }
    grid
}

fn consume_endpoints(grid: &mut EndpointGrid, idx: usize, seg: &Segment, cell_size: f64) {
    for cell in [cell_of(seg.start, cell_size), cell_of(seg.end, cell_size)] {
        if let Some(list) = grid.get_mut(&cell) {
            list.retain(|&v| v != idx);
        }
    }
}

/// Find containment relationships between objects: outer/inner indices.
/// Mirrors `calc.py:find_outer_objects` + `find_tool_offsets`.
///
/// Returns the maximum nesting depth across all objects (calc.py's
/// `max_outer`).
pub fn classify_containment(objects: &mut [VcObject]) -> usize {
    let n = objects.len();
    // Pre-flatten polygons for inside tests + precompute per-poly
    // bboxes. The bbox-first reject skips the full polygon-inside
    // call (~48-pt arc tessellation, point-in-polygon walk) whenever
    // the probe lies outside the candidate polygon's extent — typical
    // 200-object DXF goes from 40 000 inside-tests to a few hundred.
    let polys: Vec<Vec<Point2>> = objects
        .iter()
        .map(|o| {
            if o.closed {
                segments_to_points(&o.segments, 6)
            } else {
                Vec::new()
            }
        })
        .collect();
    let bboxes: Vec<Option<(f64, f64, f64, f64)>> = polys
        .iter()
        .map(|pts| {
            if pts.len() < 3 {
                None
            } else {
                Some(pts.iter().fold(
                    (
                        f64::INFINITY,
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                        f64::NEG_INFINITY,
                    ),
                    |(min_x, min_y, max_x, max_y), p| {
                        (
                            min_x.min(p.x),
                            min_y.min(p.y),
                            max_x.max(p.x),
                            max_y.max(p.y),
                        )
                    },
                ))
            }
        })
        .collect();

    for i in 0..n {
        let probe = sample_point(&objects[i]);
        for j in 0..n {
            if i == j || !objects[j].closed {
                continue;
            }
            let Some((min_x, min_y, max_x, max_y)) = bboxes[j] else {
                continue;
            };
            if probe.x < min_x || probe.x > max_x || probe.y < min_y || probe.y > max_y {
                continue;
            }
            if is_inside_polygon(&polys[j], probe) {
                objects[i].outer_objects.push(j);
                objects[j].inner_objects.push(i);
            }
        }
    }
    objects
        .iter()
        .map(|o| o.outer_objects.len())
        .max()
        .unwrap_or(0)
}

/// Probe point used by `classify_containment` — must be representative
/// of the object's interior in a way that lets the even-odd ray-cast
/// against another polygon return a deterministic answer for tangent or
/// overlapping geometry. The point intentionally sits CLOSE to the
/// boundary (rather than at the centroid) so two concentric closed
/// objects don't both falsely classify as contained in each other (the
/// centroid of an outer square containing an inner square lies inside
/// the inner — wrong answer for "is outer contained in inner").
///
/// is68: the prior implementation returned the chord midpoint of the
/// first segment. For a LINE segment that's a point ON the boundary,
/// which the even-odd rule handles consistently when probing against
/// OTHER polygons (the probe is on the OUTER object's edge, not on the
/// candidate container's edge). The problem was arcs: `segment_to_points`
/// for an arc with `interpolate=1` returns only [start, end] — the
/// "midpoint" is then the CHORD midpoint, which for a half-circle arc
/// lands at the CIRCLE'S CENTER. Two tangent circles whose touch point
/// is at the diameter endpoint then had centers coincident with each
/// other's boundary touch point, and even-odd classification flipped
/// non-deterministically.
///
/// Fix: for arc segments, return the TRUE arc midpoint (centre + tangent
/// direction at half-sweep) instead of the chord midpoint. Line segments
/// keep their legacy chord-midpoint behaviour.
fn sample_point(obj: &VcObject) -> Point2 {
    use crate::geometry::SegmentKind;
    let Some(s) = obj.segments.first() else {
        return Point2::new(0.0, 0.0);
    };
    match s.kind {
        SegmentKind::Arc | SegmentKind::Circle => {
            // True arc midpoint on the curve, NOT the chord midpoint.
            // The chord midpoint of a 180° arc collapses to the centre,
            // which lands inside another tangent-circle's interior at
            // the touch-point edge case (see is68).
            if let Some(c) = s.center {
                let r = s.start.distance(c);
                let a0 = (s.start.y - c.y).atan2(s.start.x - c.x);
                let a1 = (s.end.y - c.y).atan2(s.end.x - c.x);
                let mut sweep = a1 - a0;
                if s.bulge > 0.0 && sweep < 0.0 {
                    sweep += std::f64::consts::TAU;
                }
                if s.bulge < 0.0 && sweep > 0.0 {
                    sweep -= std::f64::consts::TAU;
                }
                let mid_a = a0 + sweep * 0.5;
                Point2::new(c.x + r * mid_a.cos(), c.y + r * mid_a.sin())
            } else {
                // No centre stashed → chord midpoint fallback. This is
                // worse than the arc midpoint but matches the legacy
                // behaviour for entities that lost their centre during
                // a transform.
                Point2::new(
                    (s.start.x + s.end.x) * 0.5,
                    (s.start.y + s.end.y) * 0.5,
                )
            }
        }
        _ => {
            // LINE / POINT: chord midpoint. The point sits ON the
            // boundary of `obj`, but containment is tested against OTHER
            // polygons — that's well-defined under the even-odd rule.
            let pts = segment_to_points(s, 1);
            let a = pts.first().copied().unwrap_or(Point2::new(0.0, 0.0));
            let b = pts.last().copied().unwrap_or(a);
            Point2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
        }
    }
}

fn next_unused(taken: &[bool]) -> Option<usize> {
    taken.iter().position(|t| !t)
}

fn find_neighbor(
    segments: &[Segment],
    taken: &[bool],
    grid: &EndpointGrid,
    point: Point2,
    fuzzy: f64,
    cell_size: f64,
) -> Option<usize> {
    // Probe the 3×3 cell neighbourhood around `point`. Because cell_size
    // ≥ fuzzy, any segment whose endpoint is within fuzzy of `point`
    // must land in one of these 9 cells — so we never miss a candidate.
    let (cx, cy) = cell_of(point, cell_size);
    let mut best: Option<(usize, f64)> = None;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let Some(list) = grid.get(&(cx + dx, cy + dy)) else {
                continue;
            };
            for &i in list {
                if taken[i] {
                    continue;
                }
                let seg = &segments[i];
                let candidate_distance = seg.start.distance(point).min(seg.end.distance(point));
                if candidate_distance < fuzzy && best.map_or(true, |(_, d)| candidate_distance < d)
                {
                    best = Some((i, candidate_distance));
                }
            }
        }
    }
    best.map(|(i, _)| i)
}

fn points_equal(a: Point2, b: Point2, fuzzy: f64) -> bool {
    a.distance(b) < fuzzy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Segment;

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    #[test]
    fn closed_square_chains_correctly() {
        let segs = vec![
            Segment::line(p(0.0, 0.0), p(10.0, 0.0), "0", 7),
            Segment::line(p(10.0, 0.0), p(10.0, 10.0), "0", 7),
            Segment::line(p(10.0, 10.0), p(0.0, 10.0), "0", 7),
            Segment::line(p(0.0, 10.0), p(0.0, 0.0), "0", 7),
        ];
        let objs = segments_to_objects(&segs);
        assert_eq!(objs.len(), 1);
        assert!(objs[0].closed);
        assert_eq!(objs[0].segments.len(), 4);
    }

    #[test]
    fn nested_squares_classify() {
        let outer = vec![
            Segment::line(p(0.0, 0.0), p(20.0, 0.0), "0", 7),
            Segment::line(p(20.0, 0.0), p(20.0, 20.0), "0", 7),
            Segment::line(p(20.0, 20.0), p(0.0, 20.0), "0", 7),
            Segment::line(p(0.0, 20.0), p(0.0, 0.0), "0", 7),
        ];
        let inner = vec![
            Segment::line(p(5.0, 5.0), p(15.0, 5.0), "0", 7),
            Segment::line(p(15.0, 5.0), p(15.0, 15.0), "0", 7),
            Segment::line(p(15.0, 15.0), p(5.0, 15.0), "0", 7),
            Segment::line(p(5.0, 15.0), p(5.0, 5.0), "0", 7),
        ];
        let mut all_segs = outer;
        all_segs.extend(inner);
        let mut objs = segments_to_objects(&all_segs);
        let depth = classify_containment(&mut objs);
        assert_eq!(objs.len(), 2);
        assert_eq!(depth, 1, "inner object should have outer_objects = [outer]");
        // Find which is which by closed area.
        let i_inner = objs
            .iter()
            .position(|o| !o.outer_objects.is_empty())
            .unwrap();
        assert_eq!(objs[i_inner].outer_objects.len(), 1);
        let i_outer = (i_inner + 1) % 2;
        assert_eq!(objs[i_outer].inner_objects.len(), 1);
    }

    #[test]
    fn open_chain_stays_open() {
        let segs = vec![
            Segment::line(p(0.0, 0.0), p(10.0, 0.0), "0", 7),
            Segment::line(p(10.0, 0.0), p(10.0, 10.0), "0", 7),
        ];
        let objs = segments_to_objects(&segs);
        assert_eq!(objs.len(), 1);
        assert!(!objs[0].closed);
    }

    /// sj4t: a large-bbox project (200 mm sheet) gets a looser endpoint-
    /// merge tolerance than a sub-mm project (a 0.5 mm cabochon). The
    /// adaptive tolerance is `max(1e-3 mm, 1e-4 * bbox_diag)`.
    #[test]
    fn fuzzy_scales_with_bbox_diag() {
        // Tiny: 1 mm diagonal → fuzzy floor 1e-3 mm.
        let tiny = vec![Segment::line(p(0.0, 0.0), p(0.707, 0.707), "0", 7)];
        let tol = fuzzy_for_segments(&tiny);
        assert!(
            (tol - FUZZY_MIN).abs() < 1e-12,
            "tiny bbox should hit the FUZZY_MIN floor, got {tol}",
        );
        // Large: 200 mm diagonal → 2e-2 mm.
        let big = vec![Segment::line(p(0.0, 0.0), p(141.42, 141.42), "0", 7)];
        let tol = fuzzy_for_segments(&big);
        assert!(
            tol > FUZZY_MIN * 10.0 && tol < 1.0,
            "large bbox tolerance should be bbox-scaled, got {tol}",
        );
    }

    /// sj4t regression: two sub-mm contours sitting 0.5 mm apart must NOT
    /// chain together. Previously the flat 1e-3 mm tolerance was so
    /// loose (relative to the geometry's working scale) that any
    /// rounding in the upstream importer could pull the two contours
    /// into a single chain. The adaptive tolerance now scales with
    /// the bbox diagonal so the chain detector "sees" only true
    /// endpoint joins.
    #[test]
    fn sub_mm_contours_dont_falsely_chain() {
        // Two short open chains 0.5 mm apart on the X axis.
        let segs = vec![
            Segment::line(p(0.0, 0.0), p(0.2, 0.0), "0", 7),
            Segment::line(p(0.7, 0.0), p(0.9, 0.0), "0", 7),
        ];
        let objs = segments_to_objects(&segs);
        assert_eq!(
            objs.len(),
            2,
            "two contours 0.5 mm apart should stay separate, got {} objects",
            objs.len(),
        );
    }

    /// is68 regression: a half-arc whose chord midpoint sits at the
    /// circle's CENTER would land at a coincident point with another
    /// circle's centre when the two circles are concentric — or, in the
    /// reported reproduction, at the touch-point of two tangent circles
    /// where the chord midpoint = (0, 0) sits ON the other circle's
    /// boundary, flipping classification under FP rounding. The fix
    /// is to use the TRUE arc midpoint (centre + tangent at half sweep)
    /// instead of the chord midpoint for arcs.
    ///
    /// We construct two circles whose first arc segment is a half-circle
    /// (bulge=1) and verify the arc midpoint lands at the TOP of the
    /// circle (not at its centre), so containment tests are deterministic.
    #[test]
    fn arc_midpoint_sample_point_is_on_curve_not_at_centre() {
        use crate::geometry::SegmentKind;
        // Half-arc of a unit circle centred at (5, 5): start=(6, 5),
        // end=(4, 5), bulge=1. The chord midpoint = (5, 5) = the circle
        // centre. The TRUE arc midpoint = (5, 6) — the top of the
        // circle.
        let centre = Point2::new(5.0, 5.0);
        let half = Segment {
            kind: SegmentKind::Circle,
            start: Point2::new(6.0, 5.0),
            end: Point2::new(4.0, 5.0),
            bulge: 1.0,
            center: Some(centre),
            layer: "0".into(),
            color: 7,
        };
        let mut chain = vec![half.clone()];
        // Close the chain with the other half-arc so the object is
        // closed (classify_containment skips opens).
        chain.push(Segment {
            kind: SegmentKind::Circle,
            start: Point2::new(4.0, 5.0),
            end: Point2::new(6.0, 5.0),
            bulge: 1.0,
            center: Some(centre),
            layer: "0".into(),
            color: 7,
        });
        let obj = VcObject::new(chain, true);
        let probe = sample_point(&obj);
        // Probe must NOT coincide with the centre (the chord midpoint).
        assert!(
            (probe.x - centre.x).abs() + (probe.y - centre.y).abs() > 0.5,
            "probe collapsed to chord midpoint = centre ({centre:?}); is68 fix not active. probe = {probe:?}"
        );
        // Probe is on the arc (distance 1 from centre = radius).
        let d = probe.distance(centre);
        assert!(
            (d - 1.0).abs() < 1e-9,
            "probe should be on the arc (distance 1 from centre), got distance {d}"
        );
    }
}
