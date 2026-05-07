//! Multi-object source combination — turns a list of selected closed
//! VcObjects into the actual region(s) the operation will machine.
//!
//! The user picks objects in the UI; what they *mean* is "the area
//! enclosed by these contours under some boolean rule". This module
//! materializes that region(s) so the per-op driver can pocket / profile
//! the right shape instead of iterating each contour independently.
//!
//! Modes:
//! * `Auto` — containment-aware: nested closed selected objects become
//!   islands of their outermost selected ancestor. Matches the behavior
//!   the per-op driver already implements when the field is unset.
//! * `Union / Difference / Intersection / Xor` — clipper2-driven boolean
//!   ops on the tessellated polygons; outers/holes are recovered from the
//!   resulting PolyTreeD.
//! * `None` — no combination; one region per selected closed object with
//!   no holes (the pre-j7y behavior, surfaced for callers who really
//!   want it).

use std::collections::HashSet;

use clipper2_rust::{
    boolean_op_tree_d, intersect_d, union_subjects_d, xor_d, ClipType, FillRule, PathD, PathsD,
    Point as ClipperPoint, PolyTreeD,
};

use crate::cam::{segments_to_points, VcObject};
use crate::geometry::Point2;
use crate::project::SourceCombine;

/// One machined region: an outer boundary plus zero or more holes
/// (islands the cutter must avoid).
#[derive(Debug, Clone)]
pub struct CombinedRegion {
    pub boundary: Vec<Point2>,
    pub holes: Vec<Vec<Point2>>,
    /// Index into `objects` for tooling/coloring/layer attribution. For
    /// boolean modes it's the first selected object that contributed to
    /// this region; for Auto/None it's the index of the boundary object.
    pub source_idx: usize,
    pub layer: String,
    pub color: i32,
}

/// Tessellation density for the boundary polygons fed to clipper2. Same
/// constant the per-op driver uses for islands.
const TESS_INTERPOLATE: usize = 6;

/// Clipper2 internal precision (decimal digits). `4` ≈ 1e-4 mm grid.
const CLIPPER_PRECISION: i32 = 4;

/// Combine the user's source selection into machined regions.
///
/// `selected` is a slice of indices into `objects`; only closed objects
/// are considered (open selections are silently ignored — they're not
/// pocketable boundaries). When the selection is empty, returns an empty
/// vec.
pub fn combine_source_regions(
    objects: &[VcObject],
    selected: &[usize],
    mode: SourceCombine,
) -> Vec<CombinedRegion> {
    let selected_closed: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|i| objects.get(*i).is_some_and(|o| o.closed))
        .collect();
    if selected_closed.is_empty() {
        return Vec::new();
    }

    match mode {
        SourceCombine::Auto => combine_auto(objects, &selected_closed),
        SourceCombine::None => combine_none(objects, &selected_closed),
        SourceCombine::Union => combine_union(objects, &selected_closed),
        SourceCombine::Intersection => combine_intersection(objects, &selected_closed),
        SourceCombine::Xor => combine_xor(objects, &selected_closed),
        SourceCombine::Difference => combine_difference(objects, &selected_closed),
    }
}

fn combine_auto(objects: &[VcObject], selected: &[usize]) -> Vec<CombinedRegion> {
    let selected_set: HashSet<usize> = selected.iter().copied().collect();
    let mut out = Vec::new();
    for &idx in selected {
        let obj = &objects[idx];
        // Skip if any other selected object contains this one — it'll be
        // an island of that one's region.
        if obj.outer_objects.iter().any(|o| selected_set.contains(o)) {
            continue;
        }
        let boundary = segments_to_points(&obj.segments, TESS_INTERPOLATE);
        let holes: Vec<Vec<Point2>> = obj
            .inner_objects
            .iter()
            .filter(|i| selected_set.contains(i))
            .filter_map(|i| objects.get(*i))
            .filter(|inner| inner.closed)
            .map(|inner| segments_to_points(&inner.segments, TESS_INTERPOLATE))
            .collect();
        out.push(CombinedRegion {
            boundary,
            holes,
            source_idx: idx,
            layer: obj.layer.clone(),
            color: obj.color,
        });
    }
    out
}

fn combine_none(objects: &[VcObject], selected: &[usize]) -> Vec<CombinedRegion> {
    selected
        .iter()
        .map(|&idx| {
            let obj = &objects[idx];
            CombinedRegion {
                boundary: segments_to_points(&obj.segments, TESS_INTERPOLATE),
                holes: Vec::new(),
                source_idx: idx,
                layer: obj.layer.clone(),
                color: obj.color,
            }
        })
        .collect()
}

/// Subjects = first selected; clips = union of the rest. Maps to the
/// natural CAM meaning of "carve the first thing minus everything else".
fn combine_difference(objects: &[VcObject], selected: &[usize]) -> Vec<CombinedRegion> {
    let (first, rest) = match selected.split_first() {
        Some(pair) => pair,
        None => return Vec::new(),
    };
    let subjects = paths_for(&[*first], objects);
    let clips = paths_for(rest, objects);
    let mut tree = PolyTreeD::new();
    boolean_op_tree_d(
        ClipType::Difference,
        FillRule::NonZero,
        &subjects,
        &clips,
        &mut tree,
        CLIPPER_PRECISION,
    );
    let template = &objects[*first];
    polytree_to_regions(&tree, *first, template.layer.clone(), template.color)
}

/// Union of N subjects in one shot — clipper's `union_subjects_d` does
/// exactly this and has the polytree variant we need for hole recovery.
fn combine_union(objects: &[VcObject], selected: &[usize]) -> Vec<CombinedRegion> {
    let subjects = paths_for(selected, objects);
    // For Union, "intersection between subjects only" doesn't apply, so
    // we use union_subjects via a no-op subjects-only Difference (subj
    // minus empty clips), which clipper folds into a self-union. Simpler
    // and gets us the polytree version: just fall through boolean_op_tree.
    let clips = PathsD::new();
    let mut tree = PolyTreeD::new();
    boolean_op_tree_d(
        ClipType::Union,
        FillRule::NonZero,
        &subjects,
        &clips,
        &mut tree,
        CLIPPER_PRECISION,
    );
    let template = &objects[selected[0]];
    let _ = union_subjects_d; // silence unused-import lint when only the tree variant is used
    polytree_to_regions(&tree, selected[0], template.layer.clone(), template.color)
}

/// N-way intersection by folding 2-way intersections. Clipper's
/// Intersection is binary (subjects ∩ clips), so to intersect multiple
/// polygons we keep a running result and intersect each next one against
/// it.
fn combine_intersection(objects: &[VcObject], selected: &[usize]) -> Vec<CombinedRegion> {
    let mut running = paths_for(&[selected[0]], objects);
    for &idx in &selected[1..] {
        let next = paths_for(&[idx], objects);
        running = intersect_d(&running, &next, FillRule::NonZero, CLIPPER_PRECISION);
        if running.is_empty() {
            // Empty intersection — no region survives. Bail early.
            return Vec::new();
        }
    }
    // Re-run as a tree so we can extract holes (intersect_d returns flat
    // PathsD without parent/child info).
    let clips = PathsD::new();
    let mut tree = PolyTreeD::new();
    boolean_op_tree_d(
        ClipType::Union,
        FillRule::NonZero,
        &running,
        &clips,
        &mut tree,
        CLIPPER_PRECISION,
    );
    let template = &objects[selected[0]];
    polytree_to_regions(&tree, selected[0], template.layer.clone(), template.color)
}

/// N-way symmetric difference, folded similarly.
fn combine_xor(objects: &[VcObject], selected: &[usize]) -> Vec<CombinedRegion> {
    let mut running = paths_for(&[selected[0]], objects);
    for &idx in &selected[1..] {
        let next = paths_for(&[idx], objects);
        running = xor_d(&running, &next, FillRule::NonZero, CLIPPER_PRECISION);
        if running.is_empty() {
            return Vec::new();
        }
    }
    let clips = PathsD::new();
    let mut tree = PolyTreeD::new();
    boolean_op_tree_d(
        ClipType::Union,
        FillRule::NonZero,
        &running,
        &clips,
        &mut tree,
        CLIPPER_PRECISION,
    );
    let template = &objects[selected[0]];
    polytree_to_regions(&tree, selected[0], template.layer.clone(), template.color)
}

fn paths_for(indices: &[usize], objects: &[VcObject]) -> PathsD {
    let mut paths = PathsD::new();
    for &idx in indices {
        let obj = match objects.get(idx) {
            Some(o) if o.closed => o,
            _ => continue,
        };
        let pts = segments_to_points(&obj.segments, TESS_INTERPOLATE);
        if pts.len() < 3 {
            continue;
        }
        let mut path = PathD::new();
        for p in &pts {
            path.push(ClipperPoint::new(p.x, p.y));
        }
        paths.push(path);
    }
    paths
}

/// Walk the PolyTreeD root and emit one CombinedRegion per top-level
/// outer path. Holes are the direct children of each top-level node
/// (PolyTree alternates outer/hole/outer/... per nesting level).
fn polytree_to_regions(
    tree: &PolyTreeD,
    source_idx: usize,
    layer: String,
    color: i32,
) -> Vec<CombinedRegion> {
    let mut out = Vec::new();
    let root = &tree.nodes[0];
    for &top_idx in root.children() {
        let top = &tree.nodes[top_idx];
        let boundary = pathd_to_points(top.polygon());
        if boundary.len() < 3 {
            continue;
        }
        let holes: Vec<Vec<Point2>> = top
            .children()
            .iter()
            .map(|&hi| pathd_to_points(tree.nodes[hi].polygon()))
            .filter(|pts| pts.len() >= 3)
            .collect();
        out.push(CombinedRegion {
            boundary,
            holes,
            source_idx,
            layer: layer.clone(),
            color,
        });
    }
    out
}

fn pathd_to_points(path: &PathD) -> Vec<Point2> {
    path.iter().map(|p| Point2::new(p.x, p.y)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::chaining::{classify_containment, segments_to_objects};
    use crate::geometry::Segment;

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    fn closed_box(side: f64, ox: f64, oy: f64) -> Vec<Segment> {
        vec![
            Segment::line(p(ox, oy), p(ox + side, oy), "0", 7),
            Segment::line(p(ox + side, oy), p(ox + side, oy + side), "0", 7),
            Segment::line(p(ox + side, oy + side), p(ox, oy + side), "0", 7),
            Segment::line(p(ox, oy + side), p(ox, oy), "0", 7),
        ]
    }

    fn build_objects(segments_lists: Vec<Vec<Segment>>) -> Vec<VcObject> {
        let mut all = Vec::new();
        for s in segments_lists {
            all.extend(s);
        }
        let mut objects = segments_to_objects(&all);
        classify_containment(&mut objects);
        objects
    }

    fn polygon_area(pts: &[Point2]) -> f64 {
        // Shoelace; absolute value because clipper may return either
        // winding depending on the op.
        let mut acc = 0.0;
        for win in pts.windows(2) {
            acc += win[0].x * win[1].y - win[1].x * win[0].y;
        }
        if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
            acc += last.x * first.y - first.x * last.y;
        }
        acc.abs() * 0.5
    }

    #[test]
    fn auto_emits_outer_with_inner_as_hole() {
        let objs = build_objects(vec![closed_box(50.0, 0.0, 0.0), closed_box(20.0, 15.0, 15.0)]);
        let selected: Vec<usize> = (0..objs.len()).collect();
        let regions = combine_source_regions(&objs, &selected, SourceCombine::Auto);
        assert_eq!(regions.len(), 1, "expected one annulus region");
        assert_eq!(regions[0].holes.len(), 1, "inner box should be a hole");
    }

    #[test]
    fn none_emits_one_region_per_selected_object() {
        let objs = build_objects(vec![closed_box(50.0, 0.0, 0.0), closed_box(20.0, 15.0, 15.0)]);
        let selected: Vec<usize> = (0..objs.len()).collect();
        let regions = combine_source_regions(&objs, &selected, SourceCombine::None);
        assert_eq!(regions.len(), 2);
        assert!(regions.iter().all(|r| r.holes.is_empty()));
    }

    #[test]
    fn union_of_overlapping_squares_yields_one_region() {
        // Two 30x30 squares overlapping by 10x10 in the middle.
        let objs = build_objects(vec![closed_box(30.0, 0.0, 0.0), closed_box(30.0, 20.0, 0.0)]);
        let selected: Vec<usize> = (0..objs.len()).collect();
        let regions = combine_source_regions(&objs, &selected, SourceCombine::Union);
        assert_eq!(regions.len(), 1);
        let area = polygon_area(&regions[0].boundary);
        // Two 900-area squares overlapping by 300 → union 1500.
        assert!(
            (area - 1500.0).abs() < 1.0,
            "expected union area ~1500, got {area}",
        );
    }

    #[test]
    fn difference_carves_inner_out_of_outer() {
        let objs = build_objects(vec![closed_box(50.0, 0.0, 0.0), closed_box(20.0, 15.0, 15.0)]);
        // Difference: first - rest. inner_box index is 1, outer is 0
        // (chaining order). Pick outer as first.
        let regions = combine_source_regions(&objs, &[0, 1], SourceCombine::Difference);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].holes.len(), 1, "inner becomes a hole");
        let outer_area = polygon_area(&regions[0].boundary);
        let hole_area: f64 = regions[0].holes.iter().map(|h| polygon_area(h)).sum();
        // Net area should be 50² - 20² = 2100.
        assert!(
            ((outer_area - hole_area) - 2100.0).abs() < 5.0,
            "expected net 2100, got {} - {} = {}",
            outer_area,
            hole_area,
            outer_area - hole_area,
        );
    }

    #[test]
    fn intersection_of_overlapping_squares_yields_overlap_region() {
        let objs = build_objects(vec![closed_box(30.0, 0.0, 0.0), closed_box(30.0, 20.0, 0.0)]);
        let selected: Vec<usize> = (0..objs.len()).collect();
        let regions = combine_source_regions(&objs, &selected, SourceCombine::Intersection);
        assert_eq!(regions.len(), 1);
        let area = polygon_area(&regions[0].boundary);
        // 10×30 strip in the middle.
        assert!((area - 300.0).abs() < 5.0, "expected ~300, got {area}");
    }
}
