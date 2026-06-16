//! Multi-object source combination — turns a list of selected closed
//! `VcObjects` into the actual region(s) the operation will machine.
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
//!   resulting `PolyTreeD`.
//! * `None` — no combination; one region per selected closed object with
//!   no holes (the legacy behavior, surfaced for callers who really
//!   want it).

// # CAM/sim pedantic-lint exemptions
// Region-corner naming (`p_bl`/`p_br`/`p_tl`/`p_tr`, `min_x`/`max_x`) is the
// canonical clipper2-rust subject/clip vocabulary.
#![allow(clippy::similar_names)]

use std::collections::{HashMap, HashSet};

use clipper2_rust::{
    boolean_op_tree_d, intersect_d, xor_d, ClipType, FillRule, PathD, PathsD,
    Point as ClipperPoint, PolyTreeD,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::{segments_to_points, VcObject};
use crate::geometry::{Point2, Segment};
use crate::project::SourceCombine;
use crate::project::ToolOffset;

/// Shape of the synthetic frame built around a Pocket-Outside selection.
/// Rectangle is a plain padded bbox; `RoundedRectangle` uses the same bbox
/// with a quarter-arc bulge at each corner. Oval and `TightOutline` are
/// deferred follow-ups (not yet implemented).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FrameShape {
    #[default]
    Rectangle,
    RoundedRectangle,
}

/// 90° quarter-arc bulge for a CCW rectangle corner: tan(π/8) =
/// √2 − 1. Used by `RoundedRectangle` to round the four corners.
const QUARTER_ARC_BULGE: f64 = std::f64::consts::SQRT_2 - 1.0;

/// Build a synthetic frame `VcObject` around `selection`, padded by
/// `padding_mm` on every side. The result is a closed `VcObject` with
/// `tool_offset = Inside` (the cutter sits inside this outer boundary)
/// on layer "Frame", color 6 (cyan). For `RoundedRectangle`, `corner_radius_mm`
/// defaults to `padding_mm` when None.
#[must_use]
pub fn build_frame(
    selection: &[&VcObject],
    shape: FrameShape,
    padding_mm: f64,
    corner_radius_mm: Option<f64>,
) -> VcObject {
    // Endpoint bbox of every segment in the selection. Bulged arcs whose
    // midpoint extends past the chord are a v1 limitation noted in the
    // bd issue — endpoints-only is good enough for typical text/plaque
    // selections.
    let mut bbox = crate::geometry::BBox::EMPTY;
    for obj in selection {
        for s in &obj.segments {
            bbox.extend_point(s.start);
            bbox.extend_point(s.end);
        }
    }
    if !bbox.is_finite() {
        bbox = crate::geometry::BBox {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 0.0,
            max_y: 0.0,
        };
    }
    let pad = padding_mm.max(0.0);
    let min_x = bbox.min_x - pad;
    let min_y = bbox.min_y - pad;
    let max_x = bbox.max_x + pad;
    let max_y = bbox.max_y + pad;

    let layer = "Frame";
    let color = 6;
    let segments = match shape {
        FrameShape::Rectangle => {
            let p_bl = Point2::new(min_x, min_y);
            let p_br = Point2::new(max_x, min_y);
            let p_tr = Point2::new(max_x, max_y);
            let p_tl = Point2::new(min_x, max_y);
            vec![
                Segment::line(p_bl, p_br, layer, color),
                Segment::line(p_br, p_tr, layer, color),
                Segment::line(p_tr, p_tl, layer, color),
                Segment::line(p_tl, p_bl, layer, color),
            ]
        }
        FrameShape::RoundedRectangle => {
            let r = corner_radius_mm.unwrap_or(pad).max(0.0);
            // Clamp radius to half the smaller side so the arcs don't
            // overlap on a tiny frame.
            let max_r = ((max_x - min_x).min(max_y - min_y) * 0.5).max(0.0);
            let r = r.min(max_r);
            // CCW winding: bottom edge → right edge → top edge → left edge,
            // with a quarter-arc bulge at each turn.
            let s_bl_h = Point2::new(min_x + r, min_y);
            let s_br_h = Point2::new(max_x - r, min_y);
            let s_br_v = Point2::new(max_x, min_y + r);
            let s_tr_v = Point2::new(max_x, max_y - r);
            let s_tr_h = Point2::new(max_x - r, max_y);
            let s_tl_h = Point2::new(min_x + r, max_y);
            let s_tl_v = Point2::new(min_x, max_y - r);
            let s_bl_v = Point2::new(min_x, min_y + r);
            vec![
                Segment::line(s_bl_h, s_br_h, layer, color),
                Segment::arc(s_br_h, s_br_v, QUARTER_ARC_BULGE, None, layer, color),
                Segment::line(s_br_v, s_tr_v, layer, color),
                Segment::arc(s_tr_v, s_tr_h, QUARTER_ARC_BULGE, None, layer, color),
                Segment::line(s_tr_h, s_tl_h, layer, color),
                Segment::arc(s_tl_h, s_tl_v, QUARTER_ARC_BULGE, None, layer, color),
                Segment::line(s_tl_v, s_bl_v, layer, color),
                Segment::arc(s_bl_v, s_bl_h, QUARTER_ARC_BULGE, None, layer, color),
            ]
        }
    };
    let mut frame = VcObject::new(segments, true);
    frame.layer = layer.into();
    frame.color = color;
    frame.tool_offset = ToolOffset::Inside;
    frame
}

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
    pub layer: std::sync::Arc<str>,
    pub color: i32,
}

/// Tessellation density for the boundary polygons fed to clipper2. Same
/// constant the per-op driver uses for islands.
const TESS_INTERPOLATE: usize = 6;

/// Per-call lazy cache of tessellated boundary points, keyed by object
/// index in the caller's slice. `combine_*` modes all walk the
/// same object set — `combine_auto` reads each object as a boundary AND
/// as a potential hole of its outer; `combine_difference` /
/// `_intersection` / `_union` / `_xor` each call `paths_for` repeatedly
/// across subjects + clips with overlap. Caching here means a single
/// `segments_to_points` per unique object per `combine_source_regions`
/// invocation regardless of how many times it's referenced.
///
/// Scoped to the call — there's no cross-call invalidation concern.
/// `VcObject` can stay `Clone + Copy`-shaped (the `OnceCell` alternative
/// would have required wider API changes).
#[derive(Default)]
struct TessCache {
    by_idx: std::collections::HashMap<usize, std::rc::Rc<Vec<Point2>>>,
}

impl TessCache {
    fn get(&mut self, idx: usize, obj: &VcObject) -> std::rc::Rc<Vec<Point2>> {
        self.by_idx
            .entry(idx)
            .or_insert_with(|| {
                std::rc::Rc::new(segments_to_points(&obj.segments, TESS_INTERPOLATE))
            })
            .clone()
    }
}

/// Clipper2 internal precision (decimal digits). `4` ≈ 1e-4 mm grid.
const CLIPPER_PRECISION: i32 = 4;

/// Combine the user's source selection into machined regions.
///
/// `selected` is a slice of indices into `objects`; only closed objects
/// are considered (open selections are silently ignored — they're not
/// pocketable boundaries). When the selection is empty, returns an empty
/// vec.
#[must_use]
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

    let mut cache = TessCache::default();
    match mode {
        SourceCombine::Auto => combine_auto(objects, &selected_closed, &mut cache),
        SourceCombine::None => combine_none(objects, &selected_closed, &mut cache),
        SourceCombine::Union => combine_union(objects, &selected_closed, &mut cache),
        SourceCombine::Intersection => combine_intersection(objects, &selected_closed, &mut cache),
        SourceCombine::Xor => combine_xor(objects, &selected_closed, &mut cache),
        SourceCombine::Difference => combine_difference(objects, &selected_closed, &mut cache),
    }
}

fn combine_auto(
    objects: &[VcObject],
    selected: &[usize],
    cache: &mut TessCache,
) -> Vec<CombinedRegion> {
    let selected_set: HashSet<usize> = selected.iter().copied().collect();
    let mut out = Vec::new();
    // When ranking containment, the NESTING DEPTH of `idx` inside
    // the selected set decides whether it's a region outer (even depth)
    // or a region hole (odd depth). `outer_objects` is the flat list of
    // every selected ancestor, so the depth is its count.
    //
    // Memoize per idx: the raw closure was O(ancestors) and was invoked
    // once per selected object PLUS once per inner object per selected
    // object, so a deeply nested selection recomputed the same depths
    // O(N) times. Precompute every idx the loop will query (each selected
    // object and its inner objects) once.
    let depth_raw = |idx: usize| -> usize {
        objects[idx]
            .outer_objects
            .iter()
            .filter(|o| selected_set.contains(o))
            .count()
    };
    let mut depth_memo: HashMap<usize, usize> = HashMap::new();
    for &idx in selected {
        depth_memo.entry(idx).or_insert_with(|| depth_raw(idx));
        for &i in &objects[idx].inner_objects {
            depth_memo.entry(i).or_insert_with(|| depth_raw(i));
        }
    }
    let selected_depth = |idx: usize| -> usize {
        depth_memo
            .get(&idx)
            .copied()
            .unwrap_or_else(|| depth_raw(idx))
    };
    for &idx in selected {
        let obj = &objects[idx];
        // EVEN-depth objects (no selected ancestor, OR two selected
        // ancestors with one nested in the other, etc.) are region
        // outers. ODD-depth objects are HOLES of the next-outer region
        // and are skipped here.
        let depth = selected_depth(idx);
        if depth % 2 == 1 {
            continue;
        }
        let boundary = (*cache.get(idx, obj)).clone();
        // Holes are the inner objects whose depth ranks ONE deeper than
        // `idx` (i.e. depth + 1). Inner objects at depth + 2 are
        // grandchildren — they're outers of their own region and get
        // their own iteration of this loop, NOT a hole of `idx`.
        let holes: Vec<Vec<Point2>> = obj
            .inner_objects
            .iter()
            .copied()
            .filter(|i| selected_set.contains(i))
            .filter(|i| selected_depth(*i) == depth + 1)
            .filter_map(|i| objects.get(i).filter(|o| o.closed).map(|o| (i, o)))
            .map(|(i, inner)| (*cache.get(i, inner)).clone())
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

fn combine_none(
    objects: &[VcObject],
    selected: &[usize],
    cache: &mut TessCache,
) -> Vec<CombinedRegion> {
    selected
        .iter()
        .map(|&idx| {
            let obj = &objects[idx];
            CombinedRegion {
                boundary: (*cache.get(idx, obj)).clone(),
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
fn combine_difference(
    objects: &[VcObject],
    selected: &[usize],
    cache: &mut TessCache,
) -> Vec<CombinedRegion> {
    let Some((first, rest)) = selected.split_first() else {
        return Vec::new();
    };
    let subjects = paths_for(&[*first], objects, cache);
    let clips = paths_for(rest, objects, cache);
    let mut tree = PolyTreeD::new();
    boolean_op_tree_d(
        ClipType::Difference,
        FillRule::NonZero,
        &subjects,
        &clips,
        &mut tree,
        CLIPPER_PRECISION,
    );
    // Difference is order-sensitive ("first minus the rest"), so
    // attributing the result to the FIRST selected object's layer / color
    // is the right semantic choice — even if the first is degenerate the
    // user's intent is "carve from THAT shape's layer".
    let template = &objects[*first];
    polytree_to_regions(&tree, *first, &template.layer, template.color)
}

/// Pick the first selected object whose tessellated boundary has
/// at least 3 points. Boolean-op modes (Intersection / Xor) re-attribute
/// the result to this object's layer/color so the gcode emitter doesn't
/// inherit a degenerate first selection's metadata (e.g. an
/// accidentally-included open polyline with layer "Tabs" but no area).
/// Falls back to `selected[0]` when every input is degenerate — at that
/// point the boolean op will return empty anyway, but we keep the
/// signature stable.
fn first_non_degenerate(objects: &[VcObject], selected: &[usize], cache: &mut TessCache) -> usize {
    for &idx in selected {
        if let Some(obj) = objects.get(idx) {
            if obj.closed && cache.get(idx, obj).len() >= 3 {
                return idx;
            }
        }
    }
    selected[0]
}

/// Union of N subjects in one shot. Uses `boolean_op_tree_d` with empty
/// clips because we need the polytree variant for hole recovery —
/// clipper's bare `union_subjects_d` only returns flat `PathsD`. The
/// "subjects minus nothing" framing is what clipper folds into a
/// self-union.
fn combine_union(
    objects: &[VcObject],
    selected: &[usize],
    cache: &mut TessCache,
) -> Vec<CombinedRegion> {
    let subjects = paths_for(selected, objects, cache);
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
    // Union is order-insensitive — inherit from first non-degenerate.
    let tmpl_idx = first_non_degenerate(objects, selected, cache);
    let template = &objects[tmpl_idx];
    polytree_to_regions(&tree, tmpl_idx, &template.layer, template.color)
}

/// N-way intersection by folding 2-way intersections. Clipper's
/// Intersection is binary (subjects ∩ clips), so to intersect multiple
/// polygons we keep a running result and intersect each next one against
/// it.
fn combine_intersection(
    objects: &[VcObject],
    selected: &[usize],
    cache: &mut TessCache,
) -> Vec<CombinedRegion> {
    let mut running = paths_for(&[selected[0]], objects, cache);
    for &idx in &selected[1..] {
        let next = paths_for(&[idx], objects, cache);
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
    // Inherit layer/color from the first NON-degenerate selected
    // object — otherwise a degenerate-first (open polyline, < 3 pts)
    // selection silently propagates the wrong metadata into the boolean
    // result. Intersection / Xor are order-INSENSITIVE so picking a
    // non-degenerate is a safe semantic improvement.
    let tmpl_idx = first_non_degenerate(objects, selected, cache);
    let template = &objects[tmpl_idx];
    polytree_to_regions(&tree, tmpl_idx, &template.layer, template.color)
}

/// N-way symmetric difference, folded similarly.
fn combine_xor(
    objects: &[VcObject],
    selected: &[usize],
    cache: &mut TessCache,
) -> Vec<CombinedRegion> {
    let mut running = paths_for(&[selected[0]], objects, cache);
    for &idx in &selected[1..] {
        let next = paths_for(&[idx], objects, cache);
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
    // Same rationale as combine_intersection — Xor is symmetric.
    let tmpl_idx = first_non_degenerate(objects, selected, cache);
    let template = &objects[tmpl_idx];
    polytree_to_regions(&tree, tmpl_idx, &template.layer, template.color)
}

fn paths_for(indices: &[usize], objects: &[VcObject], cache: &mut TessCache) -> PathsD {
    let mut paths = PathsD::new();
    for &idx in indices {
        let obj = match objects.get(idx) {
            Some(o) if o.closed => o,
            _ => continue,
        };
        let pts = cache.get(idx, obj);
        if pts.len() < 3 {
            continue;
        }
        let mut path = PathD::new();
        for p in pts.iter() {
            path.push(ClipperPoint::new(p.x, p.y));
        }
        paths.push(path);
    }
    paths
}

/// Walk the `PolyTreeD` and emit one `CombinedRegion` per outer ring at
/// every odd nesting level. `PolyTree` alternates outer/hole/outer/... so
/// the root's children are outers, their children are holes, their
/// grandchildren are outers again (an island inside a hole inside an
/// outer — i.e. a re-entrant boss the cutter must leave standing).
///
/// Previously this only walked top-level outers and their direct
/// hole-children. Grandchildren (re-entrant outers nested inside a hole)
/// were silently dropped — auto-combine with a frame-plus-window-plus-boss
/// DXF produced a single annulus that machined straight through the boss.
/// We now recurse so each outer at any depth becomes its own region with
/// its direct children as holes.
fn polytree_to_regions(
    tree: &PolyTreeD,
    source_idx: usize,
    layer: &str,
    color: i32,
) -> Vec<CombinedRegion> {
    let mut out = Vec::new();
    let Some(root) = tree.nodes.first() else {
        return out;
    };
    // Stack-based recursion across odd-level outers.
    let mut stack: Vec<usize> = root.children().to_vec();
    while let Some(outer_idx) = stack.pop() {
        let outer_node = &tree.nodes[outer_idx];
        let boundary = pathd_to_points(outer_node.polygon());
        if boundary.len() < 3 {
            // Skip degenerate outer; still descend into grandchildren
            // (they're attached to a hole, and the hole's children are
            // outers — we want them visible even if this rung is empty).
            for &hi in outer_node.children() {
                for &gi in tree.nodes[hi].children() {
                    stack.push(gi);
                }
            }
            continue;
        }
        let mut holes: Vec<Vec<Point2>> = Vec::new();
        for &hi in outer_node.children() {
            let hole_node = &tree.nodes[hi];
            let hole_pts = pathd_to_points(hole_node.polygon());
            if hole_pts.len() >= 3 {
                holes.push(hole_pts);
            }
            // Each hole's children are outers one rung deeper — queue
            // them so a depth-2 nested outer becomes its own region.
            for &gi in hole_node.children() {
                stack.push(gi);
            }
        }
        out.push(CombinedRegion {
            boundary,
            holes,
            source_idx,
            layer: std::sync::Arc::from(layer),
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
        let objs = build_objects(vec![
            closed_box(50.0, 0.0, 0.0),
            closed_box(20.0, 15.0, 15.0),
        ]);
        let selected: Vec<usize> = (0..objs.len()).collect();
        let regions = combine_source_regions(&objs, &selected, SourceCombine::Auto);
        assert_eq!(regions.len(), 1, "expected one annulus region");
        assert_eq!(regions[0].holes.len(), 1, "inner box should be a hole");
    }

    #[test]
    fn none_emits_one_region_per_selected_object() {
        let objs = build_objects(vec![
            closed_box(50.0, 0.0, 0.0),
            closed_box(20.0, 15.0, 15.0),
        ]);
        let selected: Vec<usize> = (0..objs.len()).collect();
        let regions = combine_source_regions(&objs, &selected, SourceCombine::None);
        assert_eq!(regions.len(), 2);
        assert!(regions.iter().all(|r| r.holes.is_empty()));
    }

    #[test]
    fn union_of_overlapping_squares_yields_one_region() {
        // Two 30x30 squares overlapping by 10x10 in the middle.
        let objs = build_objects(vec![
            closed_box(30.0, 0.0, 0.0),
            closed_box(30.0, 20.0, 0.0),
        ]);
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
        let objs = build_objects(vec![
            closed_box(50.0, 0.0, 0.0),
            closed_box(20.0, 15.0, 15.0),
        ]);
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
    fn build_frame_rectangle() {
        // Single 10x10 box at origin, padding=10 → 30x30 frame whose
        // bbox spans (-10,-10) to (20,20), centered on the source bbox
        // center (5, 5).
        let objs = build_objects(vec![closed_box(10.0, 0.0, 0.0)]);
        let selection: Vec<&VcObject> = objs.iter().collect();
        let frame = build_frame(&selection, FrameShape::Rectangle, 10.0, None);
        assert!(frame.closed);
        assert_eq!(frame.segments.len(), 4);
        assert_eq!(frame.layer.as_ref(), "Frame");
        assert_eq!(frame.color, 6);
        assert!(matches!(frame.tool_offset, ToolOffset::Inside));
        let mut bbox = crate::geometry::BBox::EMPTY;
        for s in &frame.segments {
            bbox.extend_point(s.start);
            bbox.extend_point(s.end);
        }
        assert!((bbox.min_x - -10.0).abs() < 1e-9);
        assert!((bbox.min_y - -10.0).abs() < 1e-9);
        assert!((bbox.max_x - 20.0).abs() < 1e-9);
        assert!((bbox.max_y - 20.0).abs() < 1e-9);
        assert!((bbox.width() - 30.0).abs() < 1e-9);
        assert!((bbox.height() - 30.0).abs() < 1e-9);
        let cx = (bbox.min_x + bbox.max_x) * 0.5;
        let cy = (bbox.min_y + bbox.max_y) * 0.5;
        assert!((cx - 5.0).abs() < 1e-9);
        assert!((cy - 5.0).abs() < 1e-9);
    }

    #[test]
    fn build_frame_rounded_rectangle() {
        // 10x10 box at origin + padding 10 → 30x30 outer envelope with
        // 4 lines + 4 quarter-arc corners = 8 segments total.
        let objs = build_objects(vec![closed_box(10.0, 0.0, 0.0)]);
        let selection: Vec<&VcObject> = objs.iter().collect();
        let frame = build_frame(&selection, FrameShape::RoundedRectangle, 10.0, None);
        assert!(frame.closed);
        assert_eq!(frame.segments.len(), 8);
        let lines = frame
            .segments
            .iter()
            .filter(|s| matches!(s.kind, crate::geometry::SegmentKind::Line))
            .count();
        let arcs = frame
            .segments
            .iter()
            .filter(|s| matches!(s.kind, crate::geometry::SegmentKind::Arc))
            .count();
        assert_eq!(lines, 4);
        assert_eq!(arcs, 4);
        for s in &frame.segments {
            if matches!(s.kind, crate::geometry::SegmentKind::Arc) {
                assert!(
                    (s.bulge - QUARTER_ARC_BULGE).abs() < 1e-9,
                    "arc bulge expected ≈ tan(π/8), got {}",
                    s.bulge,
                );
            }
        }
    }

    #[test]
    fn intersection_of_overlapping_squares_yields_overlap_region() {
        let objs = build_objects(vec![
            closed_box(30.0, 0.0, 0.0),
            closed_box(30.0, 20.0, 0.0),
        ]);
        let selected: Vec<usize> = (0..objs.len()).collect();
        let regions = combine_source_regions(&objs, &selected, SourceCombine::Intersection);
        assert_eq!(regions.len(), 1);
        let area = polygon_area(&regions[0].boundary);
        // 10×30 strip in the middle.
        assert!((area - 300.0).abs() < 5.0, "expected ~300, got {area}");
    }

    /// When the first selected object is degenerate (e.g. an
    /// open polyline that got mis-included), boolean ops still produce
    /// a sensible region from the remaining inputs, but the result
    /// must inherit the layer/color of the first NON-degenerate
    /// selected object. Otherwise the cutter inherits a stale layer
    /// from an object that contributed nothing.
    #[test]
    fn boolean_inherits_layer_from_first_non_degenerate() {
        // Two valid closed squares, both on layer "Body". Intentionally
        // tag them differently so we can verify which one's metadata
        // surfaced.
        let outer = closed_box(30.0, 0.0, 0.0);
        let inner = closed_box(30.0, 20.0, 0.0);
        let mut objs = build_objects(vec![outer, inner]);
        // Re-tag both objects so we can spot which template was used.
        objs[0].layer = std::sync::Arc::from("ShouldNotAppear");
        objs[0].color = 99;
        objs[1].layer = std::sync::Arc::from("ExpectedLayer");
        objs[1].color = 42;
        // Force objs[0] to be degenerate by clearing its segments;
        // segments_to_points() then returns < 3 pts, which paths_for
        // skips. The boolean result inherits from objs[1].
        objs[0].segments.clear();
        let selected: Vec<usize> = (0..objs.len()).collect();
        let regions = combine_source_regions(&objs, &selected, SourceCombine::Intersection);
        // Intersection of (degenerate) with (square) collapses to empty
        // — but at construction time the template attribution path is
        // exercised. We test union which actually returns a region.
        let _ = regions; // intersection may be empty; that's fine
        let regions = combine_source_regions(&objs, &selected, SourceCombine::Union);
        assert!(
            !regions.is_empty(),
            "union of valid square should produce a region"
        );
        assert_eq!(
            regions[0].layer.as_ref(),
            "ExpectedLayer",
            "union should inherit layer from first non-degenerate input",
        );
        assert_eq!(regions[0].color, 42);
    }

    /// Regression: depth-2 nested polygons (outer with a hole that
    /// contains another outer, e.g. a plate with a window with a label
    /// boss in the middle) must emit TWO `CombinedRegion`s under
    /// `SourceCombine::Auto` — one for the outer-with-window-as-hole, and
    /// one for the boss-with-no-holes (the boss is an even-depth nested
    /// outer that gets its own machinable region). Pre-fix `combine_auto`
    /// flattened the boss into the outer's hole list, which meant the
    /// gcode pocketed straight through the boss.
    #[test]
    fn combine_auto_handles_depth_two_nested_polygons() {
        // outer: 100x100 box (depth 0)
        // inner1 (depth 1): 60x60 box centered on outer
        // inner2 (depth 2 — re-entrant boss): 20x20 box centered inside inner1
        let objs = build_objects(vec![
            closed_box(100.0, 0.0, 0.0),
            closed_box(60.0, 20.0, 20.0),
            closed_box(20.0, 40.0, 40.0),
        ]);
        let selected: Vec<usize> = (0..objs.len()).collect();
        let regions = combine_source_regions(&objs, &selected, SourceCombine::Auto);
        // We expect TWO regions:
        //   - region 1: outer (100x100) with the 60x60 box as a hole
        //   - region 2: boss (20x20) on its own (no holes)
        assert_eq!(
            regions.len(),
            2,
            "expected 2 regions (outer-with-hole + boss); got {}",
            regions.len()
        );
        // Identify the boss region by its small boundary area.
        let boss = regions
            .iter()
            .find(|r| polygon_area(&r.boundary) < 1000.0)
            .expect("expected a boss region with small area");
        assert!(boss.holes.is_empty(), "boss has no inner holes");
        // Identify the outer-with-hole region.
        let outer = regions
            .iter()
            .find(|r| polygon_area(&r.boundary) > 5000.0)
            .expect("expected an outer region with large area");
        assert_eq!(
            outer.holes.len(),
            1,
            "outer has the 60x60 as a hole, not the boss"
        );
    }
}
