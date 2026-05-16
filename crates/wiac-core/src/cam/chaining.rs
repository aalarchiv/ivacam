//! Segment chaining — port of viaConstructor's `calc.py:segments2objects` and
//! the supporting helpers (`get_next_line`, `lines_to_path`, `clean_segments`,
//! `find_outer_objects`, `find_tool_offsets`).
//!
//! Walks a flat segment list, glues endpoints into chains, classifies each
//! chain as open/closed, and discovers parent/child containment.

// # CAM/sim pedantic-lint exemptions
// Chain endpoint matching uses `p_a`/`p_b` segment-endpoint names.
#![allow(
    clippy::similar_names,
)]


use std::collections::HashMap;

use crate::cam::{is_inside_polygon, segment_to_points, segments_to_points, VcObject};
use crate::geometry::{Point2, Segment};

/// Distance below which two endpoints are treated as the same vertex.
const FUZZY: f64 = 1e-3;
/// Spatial-hash cell size — endpoints inside the same cell (or one of
/// the 8 neighbours) are candidates for `find_neighbor`. Must be ≥
/// FUZZY so a same-vertex pair never lands in non-neighbour cells.
const CELL_SIZE: f64 = FUZZY * 4.0;

/// Group `segments` into [`VcObject`]s (chains) by walking neighbor endpoints.
/// Closed chains (last endpoint matches first) get `closed = true`.
#[must_use] pub fn segments_to_objects(segments: &[Segment]) -> Vec<VcObject> {
    let mut taken = vec![false; segments.len()];
    let mut out = Vec::new();
    // Spatial hash over endpoints — each segment contributes both its
    // start and end so `find_neighbor` can probe nearby cells in O(1)
    // amortized instead of the legacy O(n) full scan. A 5000-segment
    // DXF goes from 25 M probes to ~10 k.
    let mut grid = build_endpoint_index(segments);

    while let Some(seed) = next_unused(&taken) {
        taken[seed] = true;
        consume_endpoints(&mut grid, seed, &segments[seed]);
        let mut chain = vec![segments[seed].clone()];
        loop {
            let tail = chain.last().unwrap().end;
            let Some(next_idx) = find_neighbor(segments, &taken, &grid, tail) else {
                break;
            };
            taken[next_idx] = true;
            consume_endpoints(&mut grid, next_idx, &segments[next_idx]);
            let mut s = segments[next_idx].clone();
            if !points_equal(s.start, tail) && points_equal(s.end, tail) {
                std::mem::swap(&mut s.start, &mut s.end);
                s.bulge = -s.bulge;
            }
            chain.push(s);
        }
        loop {
            let head = chain.first().unwrap().start;
            let Some(prev_idx) = find_neighbor(segments, &taken, &grid, head) else {
                break;
            };
            taken[prev_idx] = true;
            consume_endpoints(&mut grid, prev_idx, &segments[prev_idx]);
            let mut s = segments[prev_idx].clone();
            if !points_equal(s.end, head) && points_equal(s.start, head) {
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
            < FUZZY;
        out.push(VcObject::new(chain, closed));
    }

    out
}

/// (`cell_x`, `cell_y`) -> list of segment indices whose start OR end falls
/// in that cell. Segments appear in both their start cell and their end
/// cell so an endpoint probe can find them from either side.
type EndpointGrid = HashMap<(i64, i64), Vec<usize>>;

fn cell_of(p: Point2) -> (i64, i64) {
    // Project a CAM-scale coordinate (mm, bounded by stock dimensions
    // ≪ i64 range) into a grid cell. `.floor() as i64` is the standard
    // pattern and the value cannot truncate within the supported scale.
    #[allow(clippy::cast_possible_truncation)]
    (
        (p.x / CELL_SIZE).floor() as i64,
        (p.y / CELL_SIZE).floor() as i64,
    )
}

fn build_endpoint_index(segments: &[Segment]) -> EndpointGrid {
    let mut grid: EndpointGrid = HashMap::with_capacity(segments.len() * 2);
    for (i, s) in segments.iter().enumerate() {
        grid.entry(cell_of(s.start)).or_default().push(i);
        let end_cell = cell_of(s.end);
        if end_cell != cell_of(s.start) {
            grid.entry(end_cell).or_default().push(i);
        }
    }
    grid
}

fn consume_endpoints(grid: &mut EndpointGrid, idx: usize, seg: &Segment) {
    for cell in [cell_of(seg.start), cell_of(seg.end)] {
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

fn sample_point(obj: &VcObject) -> Point2 {
    if let Some(s) = obj.segments.first() {
        let pts = segment_to_points(s, 1);
        // Midpoint of the first segment is a good unambiguous probe.
        let a = pts.first().copied().unwrap_or(Point2::new(0.0, 0.0));
        let b = pts.last().copied().unwrap_or(a);
        Point2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
    } else {
        Point2::new(0.0, 0.0)
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
) -> Option<usize> {
    // Probe the 3×3 cell neighbourhood around `point`. Because CELL_SIZE
    // ≥ FUZZY, any segment whose endpoint is within FUZZY of `point`
    // must land in one of these 9 cells — so we never miss a candidate.
    let (cx, cy) = cell_of(point);
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
                if candidate_distance < FUZZY && best.map_or(true, |(_, d)| candidate_distance < d)
                {
                    best = Some((i, candidate_distance));
                }
            }
        }
    }
    best.map(|(i, _)| i)
}

fn points_equal(a: Point2, b: Point2) -> bool {
    a.distance(b) < FUZZY
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
}
