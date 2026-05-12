//! Segment chaining — port of viaConstructor's `calc.py:segments2objects` and
//! the supporting helpers (`get_next_line`, `lines_to_path`, `clean_segments`,
//! `find_outer_objects`, `find_tool_offsets`).
//!
//! Walks a flat segment list, glues endpoints into chains, classifies each
//! chain as open/closed, and discovers parent/child containment.

use crate::cam::{is_inside_polygon, segment_to_points, segments_to_points, VcObject};
use crate::geometry::{Point2, Segment};

/// Distance below which two endpoints are treated as the same vertex.
const FUZZY: f64 = 1e-3;

/// Group `segments` into [`VcObject`]s (chains) by walking neighbor endpoints.
/// Closed chains (last endpoint matches first) get `closed = true`.
pub fn segments_to_objects(segments: &[Segment]) -> Vec<VcObject> {
    let mut taken = vec![false; segments.len()];
    let mut out = Vec::new();

    while let Some(seed) = next_unused(&taken) {
        taken[seed] = true;
        let mut chain = vec![segments[seed].clone()];
        // Extend forward.
        loop {
            let tail = chain.last().unwrap().end;
            let Some(next_idx) = find_neighbor(segments, &taken, tail, true) else {
                break;
            };
            taken[next_idx] = true;
            // Possibly need to flip the segment so its `start` matches the tail.
            let mut s = segments[next_idx].clone();
            if !points_equal(s.start, tail) && points_equal(s.end, tail) {
                std::mem::swap(&mut s.start, &mut s.end);
                s.bulge = -s.bulge;
            }
            chain.push(s);
        }
        // Extend backward.
        loop {
            let head = chain.first().unwrap().start;
            let Some(prev_idx) = find_neighbor(segments, &taken, head, false) else {
                break;
            };
            taken[prev_idx] = true;
            let mut s = segments[prev_idx].clone();
            if !points_equal(s.end, head) && points_equal(s.start, head) {
                std::mem::swap(&mut s.start, &mut s.end);
                s.bulge = -s.bulge;
            }
            chain.insert(0, s);
        }
        let closed = chain.first().unwrap().start.distance(chain.last().unwrap().end) < FUZZY;
        out.push(VcObject::new(chain, closed));
    }

    out
}

/// Find containment relationships between objects: outer/inner indices.
/// Mirrors `calc.py:find_outer_objects` + `find_tool_offsets`.
///
/// Returns the maximum nesting depth across all objects (calc.py's
/// `max_outer`).
pub fn classify_containment(objects: &mut [VcObject]) -> usize {
    let n = objects.len();
    // Pre-flatten polygons for inside tests.
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

    for i in 0..n {
        let probe = sample_point(&objects[i]);
        for j in 0..n {
            if i == j || !objects[j].closed || polys[j].len() < 3 {
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
    point: Point2,
    _forward: bool,
) -> Option<usize> {
    // Pick the unused segment whose nearer endpoint is closest to
    // `point` within FUZZY. Was previously a "best" tracker that
    // broke on first match without using the recorded distance —
    // effectively a first-fit lookup that could pick a worse
    // candidate when two segments both lay within FUZZY of the seam.
    // True best-fit makes the chaining stable under input order.
    let mut best: Option<(usize, f64)> = None;
    for (i, seg) in segments.iter().enumerate() {
        if taken[i] {
            continue;
        }
        let candidate_distance = seg.start.distance(point).min(seg.end.distance(point));
        if candidate_distance < FUZZY
            && best.map_or(true, |(_, d)| candidate_distance < d)
        {
            best = Some((i, candidate_distance));
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
        let i_inner = objs.iter().position(|o| !o.outer_objects.is_empty()).unwrap();
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
