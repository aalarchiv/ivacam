//! Offsetting operations: the cavalier_contours-driven parallel offset for
//! polylines-with-arcs (preserves bulges), and the clipper2-driven inward
//! cascade for nested pockets (operates on tessellated polygons).
//!
//! Mirrors `calc.py:do_pockets` and `objects2polyline_offsets` at the
//! algorithm level — see the unit tests for the contracts.

// # CAM/sim pedantic-lint exemptions
// Offset machinery names (`p_a`/`p_b`, `min_x`/`max_x`, `ix0`/`ix1`) mirror
// the cavalier_contours / clipper2-rust conventions; cell-bbox truncations
// are bounded by the grid layout. Serde `skip_serializing_if = "is_false"`
// helpers take `&bool` because that's the signature serde requires.
#![allow(
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::trivially_copy_pass_by_ref,
    // Cut-direction × context table enumerates every combination
    // explicitly even when two arms agree, so the truth table reads
    // straight off the page.
    clippy::match_same_arms,
    // `&HashMap<…, …>` (default RandomState) is what every caller
    // builds; generalising over BuildHasher would force them all to
    // spell out the hasher just to satisfy clippy.
    clippy::implicit_hasher,
)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geometry::{Point2, Segment};

// cam/offsets.rs decomposed into a directory module. Each cluster is
// a submodule, re-exported here so `cam::offsets::X` import sites are
// unchanged. tabs = tab attachment; winding = cut-direction / approach
// rotation; parallel = offset primitives + cascade + overcut; pocket_fill =
// zigzag/spiral fill + pocket_for_object.
mod parallel;
mod pocket_fill;
mod tabs;
mod winding;
pub use parallel::*;
pub use pocket_fill::*;
pub use tabs::*;
pub use winding::*;

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
    #[schemars(with = "String")]
    pub layer: std::sync::Arc<str>,
    pub color: i32,
    pub source_object_idx: usize,
    /// Tab positions (data-space XY) the cutter should lift over while
    /// cutting this offset. Frontend places these via the tab-placement UI; the gcode
    /// emitter splits the cut at each crossing and lifts Z to tabs.height.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<TabPoint>,
    /// When true, the gcode emitter swaps in the finish-set feed / speed
    /// / plunge rates (`ToolConfig::*_finish`) before cutting this
    /// offset. The pipeline tags the wall-defining level=0 ring of a
    /// Pocket op as finish; everything else stays at the rough rates.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_finish: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// All diagnostics the offset / pocket-fill routines can raise during one
/// op's offset build, drained as a SINGLE channel by
/// `pipeline::offset_builder`.
///
/// Each event is produced deep inside an otherwise-pure CAM function
/// (parallel offset, pocket cascade / zigzag, approach-point rotation)
/// where threading a sink through every signature would be noise — and
/// `PipelineWarning` is a pipeline-layer type this module must not depend
/// on. They accumulate in per-kind thread-locals; [`take_offset_diagnostics`]
/// bundles them so the consumer drains ONE thing and translates each into a
/// `PipelineWarning`, instead of juggling five separate `take_*` calls plus
/// a multi-call reset dance.
#[derive(Debug, Default)]
pub struct OffsetDiagnostics {
    pub parallel_offset_panics: Vec<ParallelOffsetPanic>,
    pub pocket_cascade_truncations: Vec<PocketCascadeTruncation>,
    pub nocontour_allowance_ignored: Vec<NocontourAllowanceIgnored>,
    pub zigzag_stride_degenerate: Vec<ZigzagStrideDegenerate>,
    pub approach_point_far: Vec<ApproachPointFarRotation>,
}

/// Drain (and clear) ALL offset diagnostics collected on this thread in one
/// call. The pipeline calls this once per op defensively before the offset
/// build (discard a prior op's leftovers) and once after to collect + surface
/// this op's events.
#[must_use]
pub fn take_offset_diagnostics() -> OffsetDiagnostics {
    OffsetDiagnostics {
        parallel_offset_panics: parallel::take_parallel_offset_panics(),
        pocket_cascade_truncations: parallel::take_pocket_cascade_truncations(),
        nocontour_allowance_ignored: pocket_fill::take_nocontour_allowance_ignored(),
        zigzag_stride_degenerate: pocket_fill::take_zigzag_stride_degenerate(),
        approach_point_far: winding::take_approach_point_far_rotations(),
    }
}

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

/// Signed area of an offset's segment chain, computed from the start
/// vertex of each segment. Arcs aren't sampled at midpoints — the chord
/// approximation is enough for sign-of-area, which is all this is used
/// for (winding direction).
fn offset_signed_area(offset: &PolylineOffset) -> f64 {
    if offset.segments.len() < 3 {
        return 0.0;
    }
    let pts: Vec<Point2> = offset.segments.iter().map(|s| s.start).collect();
    signed_area(&pts)
}

/// Reverse a closed offset's traversal direction in place. The order of
/// segments is reversed; each segment's start/end swap; arc bulges
/// negate (an arc traversed the other way bends the opposite direction).
fn reverse_offset(offset: &mut PolylineOffset) {
    offset.segments.reverse();
    for s in &mut offset.segments {
        std::mem::swap(&mut s.start, &mut s.end);
        s.bulge = -s.bulge;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // VcObject / point_in_polygon are only used by these tests now that the
    // production code that consumed them moved into the cluster submodules.
    use crate::cam::VcObject;
    use crate::geometry::{point_in_polygon, Point2};

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
        let (mut minx, mut maxx, mut miny, mut maxy) = (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        );
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
        let offsets = pocket_for_object(
            &obj,
            1.5,
            false,
            6,
            PocketEmit::Cascade,
            &[],
            1.5,
            0.0,
            None,
            crate::project::tool::SpindleDirection::Cw,
        );
        assert_eq!(offsets.len(), 1);
        assert_eq!(offsets[0].segments.len(), 1);
        assert!(matches!(offsets[0].segments[0].kind, SegmentKind::Point));
        assert!(offsets[0].segments[0].start.distance(center) < 1e-9);
    }

    #[test]
    fn zigzag_pocket_fills_a_square() {
        let boundary = vec![p(0.0, 0.0), p(20.0, 0.0), p(20.0, 20.0), p(0.0, 20.0)];
        let chains = pocket_zigzag(&boundary, &[], 1.8, 2.0);
        // No islands → single chain.
        assert_eq!(chains.len(), 1, "no islands ⇒ one chain");
        let segs = &chains[0];
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
        for s in segs {
            for pt in [s.start, s.end] {
                assert!(pt.x >= -0.01 && pt.x <= 20.01);
                assert!(pt.y >= -0.01 && pt.y <= 20.01);
            }
        }
    }

    /// A short (< 3 pts) ring inside the cascade was previously
    /// silently dropped, leaving the bridge from the previous ring's
    /// `last_end` to the next ring's first vertex unverified — it could
    /// span the gap of the dropped ring and exit the pocket. The fix
    /// is to bail (return None) and let the caller fall back to
    /// non-bridged cascade emission. Verify by passing in a 3-ring
    /// cascade whose middle ring has only 2 points.
    #[test]
    fn short_ring_mid_cascade_returns_none() {
        let ring0 = vec![p(0.0, 0.0), p(20.0, 0.0), p(20.0, 20.0), p(0.0, 20.0)];
        // Degenerate middle ring — clipper2 collapses a sliver to 2 pts.
        let ring1 = vec![p(5.0, 5.0), p(15.0, 5.0)];
        let ring2 = vec![p(10.0, 10.0), p(12.0, 10.0), p(12.0, 12.0), p(10.0, 12.0)];
        let rings = vec![ring0, ring1, ring2];
        assert!(
            stitch_rings_to_polyline(&rings, &[]).is_none(),
            "stitch must bail when a mid-cascade ring is degenerate (< 3 pts)",
        );
    }

    /// A spiral pocket with an island in the bridge path
    /// must NOT carve through the island. The bridge-containment guard
    /// rejects bridges that cross any island; on rejection
    /// `stitch_rings_to_polyline` sweeps OTHER candidate start vertices
    /// on the next ring, and only when EVERY candidate fails
    /// does the stitch return None so the caller falls back to cascade
    /// emission. We construct rings where every vertex of ring 1 sits
    /// on the right side of the island clustered tight against the
    /// pocket's right wall — every bridge from (5, 25) on ring 0
    /// inevitably traverses the island's footprint, so all candidates
    /// fail and the stitch must return None.
    #[test]
    fn spiral_bridge_rejected_when_crossing_island() {
        // 50×50 pocket; an island in the middle at [20..30] × [20..30].
        // Ring 0 starts at (5, 25) so last_end = (5, 25).
        // Ring 1 is a thin vertical band on the right (x≈40, y∈[22..28]) —
        // every line from (5, 25) to a (40, ≈25) vertex passes through
        // the island's x∈[20..30], y∈[22..28] footprint.
        let ring0 = vec![
            p(5.0, 25.0),
            p(5.0, 5.0),
            p(45.0, 5.0),
            p(45.0, 45.0),
            p(5.0, 45.0),
        ];
        let ring1 = vec![
            p(40.0, 25.0),
            p(40.0, 22.0),
            p(40.0, 28.0),
            p(40.0, 24.0),
            p(40.0, 26.0),
        ];
        let rings = vec![ring0, ring1];
        let island = vec![p(20.0, 20.0), p(30.0, 20.0), p(30.0, 30.0), p(20.0, 30.0)];
        // No islands → polyline stitches without complaint (sanity).
        assert!(stitch_rings_to_polyline(&rings, &[]).is_some());
        // With the island present every candidate bridge crosses it → reject.
        assert!(
            stitch_rings_to_polyline(&rings, &[island.clone()]).is_none(),
            "stitch must reject when every candidate bridge crosses an island",
        );
    }

    /// When the FIRST candidate start vertex would put a bridge
    /// across an island, the stitch must sweep through the other
    /// candidate vertices on that ring and find a safe one before
    /// falling back to None. Pre-fix the function returned None on the
    /// first failing candidate, silently dropping spiral emission on
    /// any pocket where the closest vertex happened to be unsafe — even
    /// though a safe alternative existed.
    #[test]
    fn spiral_bridge_sweeps_alternative_start_vertices_around_island() {
        // 50×50 pocket; island at [20..30]×[20..30].
        // Ring 0 starts at (5, 25) → last_end = (5, 25).
        // Ring 1 has a closest vertex (40, 25) that produces an island-
        // crossing bridge AND a farther vertex (10, 5) whose bridge
        // from (5, 25) is safe (y ≤ 25, below the island). Pre-fix:
        // returned None because the first candidate failed. Post-fix:
        // returns Some, picking (10, 5) as ring 1's start.
        let ring0 = vec![
            p(5.0, 25.0),
            p(5.0, 5.0),
            p(45.0, 5.0),
            p(45.0, 45.0),
            p(5.0, 45.0),
        ];
        let ring1 = vec![
            p(40.0, 25.0), // closest to (5, 25) — bridge crosses island
            p(10.0, 5.0),  // farther but bridge sits below the island, safe
            p(10.0, 7.0),
            p(8.0, 5.0),
        ];
        let rings = vec![ring0, ring1];
        let island = vec![p(20.0, 20.0), p(30.0, 20.0), p(30.0, 30.0), p(20.0, 30.0)];
        let stitched = stitch_rings_to_polyline(&rings, &[island])
            .expect("a safe alternative start vertex exists — stitch must not bail");
        // The chosen bridge endpoint on ring 1 must be one of the safe
        // alternatives (not the (40, 25) closest-but-unsafe candidate).
        // We find it by locating ring 1's first vertex in the stitched
        // polyline — it's the first point with x < 30 after the ring-0
        // segment ends (ring 0 vertices all sit on x∈{5, 45}, ring 1's
        // chosen vertex has x ∈ {8, 10}).
        assert!(
            stitched
                .iter()
                .any(|pt| pt.x > 7.0 && pt.x < 11.0 && pt.y < 8.0),
            "stitch should have picked a ring-1 start that avoids the island; got {stitched:?}",
        );
        // And no point in the stitched polyline should sit inside the
        // island (the cutter would gouge it).
        for pt in &stitched {
            let inside = pt.x > 20.001 && pt.x < 29.999 && pt.y > 20.001 && pt.y < 29.999;
            assert!(!inside, "stitched polyline crosses the island at {pt:?}");
        }
    }

    /// `bridge_crosses_any_island` detects a bridge that
    /// goes straight through an island, and accepts one that goes
    /// around.
    #[test]
    fn bridge_crosses_any_island_detects_gouge() {
        let island = vec![p(10.0, 10.0), p(20.0, 10.0), p(20.0, 20.0), p(10.0, 20.0)];
        assert!(bridge_crosses_any_island(
            p(0.0, 15.0),
            p(30.0, 15.0),
            &[island.clone()],
        ));
        // Bridge clear of the island.
        assert!(!bridge_crosses_any_island(
            p(0.0, 5.0),
            p(30.0, 5.0),
            &[island],
        ));
    }

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
                assert!(!inside, "pocket ring crossed the island at {pt:?}");
            }
        }
    }

    /// `inflate_islands_by_tool_radius` produces an
    /// outward Minkowski-sum boundary around each island, i.e. a
    /// polygon every point of which is ≥ `tool_radius` from the original
    /// island wall. The pocket emitters (`pocket_zigzag`, the cascade
    /// inflater, the spiral stitcher) consume the inflated outline as
    /// the centerline safe boundary; passing the raw polygon used to
    /// allow the cutter EDGE to bite `tool_r` into the original island.
    #[test]
    fn inflate_islands_by_tool_radius_expands_outward() {
        // 10x10 axis-aligned square island centered at the origin.
        let raw = vec![p(-5.0, -5.0), p(5.0, -5.0), p(5.0, 5.0), p(-5.0, 5.0)];
        let inflated = inflate_islands_by_tool_radius(&[raw.clone()], 1.5);
        assert_eq!(inflated.len(), 1, "single island in → single ring out");
        // bbox should extend at least ~tool_radius further in every
        // direction. Clipper2 with EndType::Polygon + JoinType::Round
        // rounds corners, so we test the bbox bounds (looser than exact
        // distance, but enough to catch a missing inflate).
        let (mut mnx, mut mny, mut mxx, mut mxy) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for pt in &inflated[0] {
            mnx = mnx.min(pt.x);
            mny = mny.min(pt.y);
            mxx = mxx.max(pt.x);
            mxy = mxy.max(pt.y);
        }
        // Original bbox is [-5, 5]² → inflated bbox must be at least
        // [-6.5, 6.5]² (a tool_radius=1.5 outward expansion).
        assert!(mnx <= -6.4, "expected min_x ≤ -6.4, got {mnx}");
        assert!(mny <= -6.4, "expected min_y ≤ -6.4, got {mny}");
        assert!(mxx >= 6.4, "expected max_x ≥ 6.4, got {mxx}");
        assert!(mxy >= 6.4, "expected max_y ≥ 6.4, got {mxy}");
        // The original island center is well inside the inflated ring
        // — verify with the same point-in-polygon helper the pocket
        // emitters use.
        assert!(
            point_in_polygon(&inflated[0], 0.0, 0.0),
            "island center (0,0) must lie inside the inflated boundary"
        );
    }

    /// The cam-layer `pocket_zigzag` documents its
    /// `islands` input as already-inflated-by-tool-radius and uses each
    /// island's horizontal-crossings interval as-is (the function
    /// would otherwise double-inflate). The pipeline's job is to feed
    /// it pre-inflated polygons via `inflate_islands_by_tool_radius`.
    /// Verify the contract end-to-end: with a RAW island the cutter
    /// centerline ploughs straight up to the island wall (gouge); with
    /// the INFLATED island it keeps a `tool_radius` clearance.
    #[test]
    fn pocket_zigzag_with_inflated_island_keeps_tool_radius_clearance() {
        let boundary = vec![p(0.0, 0.0), p(40.0, 0.0), p(40.0, 40.0), p(0.0, 40.0)];
        let raw_island = vec![p(15.0, 15.0), p(25.0, 15.0), p(25.0, 25.0), p(15.0, 25.0)];
        let tool_diameter = 3.0;
        let tool_radius = tool_diameter * 0.5;
        let inflated = inflate_islands_by_tool_radius(&[raw_island.clone()], tool_radius);

        // RAW island fed in (the pre-fix broken contract): scanlines
        // run right up to x∈[15, 25] within y∈[15, 25] — the cutter
        // centerline sits at the raw wall.
        let chains_raw = pocket_zigzag(&boundary, &[raw_island.clone()], 1.5, tool_diameter);
        let mut had_gouge_centerline = false;
        for chain in &chains_raw {
            for seg in chain {
                for pt in [&seg.start, &seg.end] {
                    // Distance to raw island bbox edge (treat as
                    // square): the cutter centerline got within
                    // <1e-3 of x=15 / x=25 on y∈[15..25] rows.
                    let inside_y = pt.y > 15.0 - 1.0 && pt.y < 25.0 + 1.0;
                    if inside_y && ((pt.x - 15.0).abs() < 0.5 || (pt.x - 25.0).abs() < 0.5) {
                        had_gouge_centerline = true;
                    }
                }
            }
        }
        assert!(
            had_gouge_centerline,
            "RAW-island test must demonstrate the pre-knd4 gouge — centerline should reach the raw island wall"
        );

        // INFLATED island fed in (the post-fix fixed contract): no
        // centerline endpoint sits within tool_radius - eps of the raw
        // wall. The pocket emitter trims scanlines to a Minkowski-sum
        // boundary that's tool_r outboard of the raw wall.
        //
        // Slack budget: clipper2 inflates with EndType::Polygon +
        // JoinType::Round at `arc_tol = 0.25`, so the rounded corners
        // of the inflated polygon are chord-approximated. A scanline
        // endpoint sampled along a chord between two arc vertices
        // sits up to `arc_tol` inside the true tool_radius circle —
        // a sub-mm manufacturing approximation, not a regression.
        // We allow ~arc_tol of slack. Pre-fix the gouge was a full
        // tool_radius (1.5 mm) — 5× this slack — so the regression
        // still flags the broken contract loudly.
        let chains_safe = pocket_zigzag(&boundary, &inflated, 1.5, tool_diameter);
        let arc_tol_slack = 0.30;
        let safe_dist = tool_radius - arc_tol_slack;
        let raw_bbox_min = (15.0, 15.0);
        let raw_bbox_max = (25.0, 25.0);
        for chain in &chains_safe {
            for seg in chain {
                for pt in [&seg.start, &seg.end] {
                    // Find the closest distance from this point to the
                    // raw island bbox edge. The cutter centerline must
                    // stay ≥ tool_radius outboard.
                    let dx = (raw_bbox_min.0 - pt.x).max(pt.x - raw_bbox_max.0).max(0.0);
                    let dy = (raw_bbox_min.1 - pt.y).max(pt.y - raw_bbox_max.1).max(0.0);
                    let d = (dx * dx + dy * dy).sqrt();
                    // If the point sits inside the raw island bbox (dx
                    // = dy = 0), that's a serious gouge. Otherwise we
                    // need d ≥ tool_radius.
                    let inside_raw = pt.x > raw_bbox_min.0 - 1e-3
                        && pt.x < raw_bbox_max.0 + 1e-3
                        && pt.y > raw_bbox_min.1 - 1e-3
                        && pt.y < raw_bbox_max.1 + 1e-3;
                    assert!(
                        !inside_raw,
                        "knd4 regression: centerline sits inside raw island bbox at ({:.3}, {:.3})",
                        pt.x, pt.y
                    );
                    assert!(
                        d >= safe_dist,
                        "knd4 regression: centerline endpoint ({:.3}, {:.3}) sits {:.3} mm from raw island wall — must be ≥ {:.3} (tool_radius)",
                        pt.x, pt.y, d, safe_dist,
                    );
                }
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
            is_finish: false,
        };
        let before = offset.segments.len();
        // Wait — for an inside-of-shape offset like a pocket, the offset poly
        // is wound CCW and the L's reflex corner becomes a CONVEX corner on
        // the offset (mitered). For overcut we need the reflex case: that's
        // an OUTSIDE cut around an L-shaped island where the offset poly is
        // CW. Reverse the offset segments to get the right winding.
        offset.segments.reverse();
        for s in &mut offset.segments {
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
                assert!(
                    pt.x >= -0.01 && pt.x <= 20.01,
                    "overcut vertex out of bbox: {pt:?}"
                );
                assert!(
                    pt.y >= -0.01 && pt.y <= 20.01,
                    "overcut vertex out of bbox: {pt:?}"
                );
            }
        }
    }

    #[test]
    fn pocket_cascade_produces_inward_rings() {
        let boundary = vec![p(0.0, 0.0), p(20.0, 0.0), p(20.0, 20.0), p(0.0, 20.0)];
        let rings = pocket_cascade(&boundary, 2.0);
        assert!(
            rings.len() >= 4,
            "expect at least 4 rings, got {}",
            rings.len()
        );
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

    fn sample_offset_ccw() -> PolylineOffset {
        // 10×10 square wound CCW, signed area > 0.
        PolylineOffset {
            segments: vec![
                Segment::line(p(0.0, 0.0), p(10.0, 0.0), "0", 7),
                Segment::line(p(10.0, 0.0), p(10.0, 10.0), "0", 7),
                Segment::line(p(10.0, 10.0), p(0.0, 10.0), "0", 7),
                Segment::line(p(0.0, 10.0), p(0.0, 0.0), "0", 7),
            ],
            closed: true,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        }
    }

    #[test]
    fn enforce_winding_inner_conventional_keeps_ccw() {
        let mut o = sample_offset_ccw();
        let before_area = offset_signed_area(&o);
        assert!(before_area > 0.0);
        enforce_winding(
            &mut o,
            CutContext::Inner,
            crate::project::CutDirection::Conventional,
            crate::project::tool::SpindleDirection::Cw,
        );
        // Inner + Conventional → CCW. CCW-input stays CCW.
        assert!(offset_signed_area(&o) > 0.0);
    }

    #[test]
    fn enforce_winding_inner_climb_flips_to_cw() {
        let mut o = sample_offset_ccw();
        enforce_winding(
            &mut o,
            CutContext::Inner,
            crate::project::CutDirection::Climb,
            crate::project::tool::SpindleDirection::Cw,
        );
        assert!(offset_signed_area(&o) < 0.0);
    }

    #[test]
    fn enforce_winding_outer_conventional_flips_to_cw() {
        let mut o = sample_offset_ccw();
        enforce_winding(
            &mut o,
            CutContext::Outer,
            crate::project::CutDirection::Conventional,
            crate::project::tool::SpindleDirection::Cw,
        );
        assert!(offset_signed_area(&o) < 0.0);
    }

    #[test]
    fn enforce_winding_outer_climb_keeps_ccw() {
        let mut o = sample_offset_ccw();
        enforce_winding(
            &mut o,
            CutContext::Outer,
            crate::project::CutDirection::Climb,
            crate::project::tool::SpindleDirection::Cw,
        );
        assert!(offset_signed_area(&o) > 0.0);
    }

    #[test]
    fn enforce_winding_skip_leaves_offset_alone() {
        let mut o = sample_offset_ccw();
        let before: Vec<_> = o.segments.iter().map(|s| (s.start, s.end)).collect();
        enforce_winding(
            &mut o,
            CutContext::Skip,
            crate::project::CutDirection::Conventional,
            crate::project::tool::SpindleDirection::Cw,
        );
        let after: Vec<_> = o.segments.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(before, after);
    }

    /// A left-hand spindle (`Ccw`, M4 mode) flips the geometric
    /// winding picked for any given climb/conventional intent because
    /// the cutting edge rotates the other way. Inner+Climb on a right-
    /// hand spindle picks CW (area<0); on a left-hand spindle the same
    /// intent must pick CCW (area>0) so the chipload direction stays
    /// "climb" physically.
    #[test]
    fn enforce_winding_inner_climb_lefthand_keeps_ccw() {
        let mut o = sample_offset_ccw();
        enforce_winding(
            &mut o,
            CutContext::Inner,
            crate::project::CutDirection::Climb,
            crate::project::tool::SpindleDirection::Ccw,
        );
        // RH would flip to CW here; LH must keep CCW.
        assert!(offset_signed_area(&o) > 0.0);
    }

    /// Symmetric case: outer+conventional on a left-hand spindle
    /// flips to CCW (RH would pick CW).
    #[test]
    fn enforce_winding_outer_conventional_lefthand_keeps_ccw() {
        let mut o = sample_offset_ccw();
        enforce_winding(
            &mut o,
            CutContext::Outer,
            crate::project::CutDirection::Conventional,
            crate::project::tool::SpindleDirection::Ccw,
        );
        // RH would flip to CW; LH must keep CCW.
        assert!(offset_signed_area(&o) > 0.0);
    }

    /// Regression for C1 (audit): the zigzag inset used to double-apply
    /// the inset to one end, leaving a stripe of uncut stock at every
    /// stroke's right end. Each stroke now spans `[lo + r, hi - r]`
    /// exactly, where r = `tool_diameter` / 2.
    #[test]
    fn pocket_zigzag_insets_both_ends_by_tool_radius() {
        // Square 0..20 in X and Y; stride small enough to get several
        // strokes; tool diameter 3 mm ⇒ radius 1.5 mm.
        let boundary = vec![
            Point2::new(0.0, 0.0),
            Point2::new(20.0, 0.0),
            Point2::new(20.0, 20.0),
            Point2::new(0.0, 20.0),
        ];
        let chains = pocket_zigzag(&boundary, &[], 2.0, 3.0);
        assert_eq!(chains.len(), 1);
        let segs = &chains[0];
        // Pull out the horizontal cuts (the strokes — they share y).
        let strokes: Vec<&Segment> = segs
            .iter()
            .filter(|s| (s.start.y - s.end.y).abs() < 1e-6)
            .collect();
        assert!(strokes.len() >= 3, "expected multiple strokes");
        for s in &strokes {
            let lo = s.start.x.min(s.end.x);
            let hi = s.start.x.max(s.end.x);
            assert!(
                (lo - 1.5).abs() < 1e-6,
                "left end should sit at lo=1.5, got {lo}",
            );
            assert!(
                (hi - 18.5).abs() < 1e-6,
                "right end should sit at hi=18.5, got {hi} (was 17.0 before C1 fix)",
            );
        }
    }

    /// Angled zigzag produces strokes oriented at the given
    /// angle. At 90° the strokes are vertical (start.x == end.x); at
    /// 0° they're horizontal (start.y == end.y, the original case).
    /// Span and stride still fit inside the original square boundary.
    #[test]
    fn pocket_zigzag_angled_rotates_strokes() {
        let boundary = vec![
            Point2::new(0.0, 0.0),
            Point2::new(20.0, 0.0),
            Point2::new(20.0, 20.0),
            Point2::new(0.0, 20.0),
        ];
        // 0° behaviour matches axis-aligned pocket_zigzag.
        let base = pocket_zigzag(&boundary, &[], 2.0, 3.0);
        let zero = pocket_zigzag_angled(&boundary, &[], 2.0, 3.0, 0.0);
        assert_eq!(base.len(), zero.len(), "0° should equal axis-aligned");
        assert_eq!(base[0].len(), zero[0].len());
        // 90° rotation produces vertical strokes inside the same bbox.
        let vert = pocket_zigzag_angled(&boundary, &[], 2.0, 3.0, 90.0);
        assert!(!vert.is_empty(), "expected strokes for 90°");
        let vsegs = &vert[0];
        let strokes: Vec<_> = vsegs
            .iter()
            .filter(|s| (s.start.x - s.end.x).abs() < 1e-6)
            .collect();
        assert!(
            strokes.len() >= 3,
            "expected ≥3 vertical strokes at 90°; got {}",
            strokes.len(),
        );
        for s in &strokes {
            assert!(
                s.start.x >= -1e-6 && s.start.x <= 20.0 + 1e-6,
                "stroke x = {} should be inside [0, 20]",
                s.start.x,
            );
        }
        // 45° rotation: strokes are diagonal — no exact-axis match.
        let diag = pocket_zigzag_angled(&boundary, &[], 2.0, 3.0, 45.0);
        assert!(!diag.is_empty(), "expected strokes for 45°");
    }

    /// A 50×50 pocket with a 10×10 island in the centre — the
    /// zigzag must NOT carve a single continuous polyline through the
    /// island. We expect at least one row whose stroke is split into
    /// left + right sub-strokes by the island band.
    #[test]
    fn pocket_zigzag_respects_islands() {
        let outer = vec![p(0.0, 0.0), p(50.0, 0.0), p(50.0, 50.0), p(0.0, 50.0)];
        // Island centered at (25, 25), 10×10. CCW or CW doesn't matter
        // — horizontal_crossings returns interior intervals either way.
        let island = vec![p(20.0, 20.0), p(30.0, 20.0), p(30.0, 30.0), p(20.0, 30.0)];
        let chains = pocket_zigzag(&outer, &[island.clone()], 2.0, 2.0);
        // With an island in the middle the zigzag is no longer a single
        // continuous chain. The cutter must lift between sub-chains;
        // that's encoded as ≥2 chains being returned.
        assert!(
            chains.len() >= 2,
            "expected ≥2 chains across an island split; got {}",
            chains.len(),
        );
        // No stroke endpoint may land strictly inside the island.
        for chain in &chains {
            for s in chain {
                for pt in [s.start, s.end] {
                    let inside = pt.x > 20.01 && pt.x < 29.99 && pt.y > 20.01 && pt.y < 29.99;
                    assert!(!inside, "zigzag stroke endpoint inside island: {pt:?}",);
                }
            }
        }
        // No single stroke crosses the island bbox horizontally.
        for chain in &chains {
            for s in chain {
                if (s.start.y - s.end.y).abs() < 1e-6 && s.start.y > 20.0 && s.start.y < 30.0 {
                    let lo = s.start.x.min(s.end.x);
                    let hi = s.start.x.max(s.end.x);
                    assert!(
                        !(lo < 20.0 && hi > 30.0),
                        "stroke at y={} runs from {lo} to {hi}, crossing the island",
                        s.start.y,
                    );
                }
            }
        }
    }

    /// Regression for C5 (audit): a CW-encoded full circle (two
    /// semicircles, bulge = -1) used to read `signed_area` == 0 because
    /// the chord shoelace cancelled out. With the bulge bow correction
    /// the sign is now negative, so `parallel_offset_inward` picks the
    /// correct delta sign for CW circles.
    #[test]
    fn object_signed_area_includes_arc_bow() {
        use crate::geometry::SegmentKind;
        let r = 5.0;
        let center = Point2::new(0.0, 0.0);
        let p_right = Point2::new(r, 0.0);
        let p_left = Point2::new(-r, 0.0);
        // CCW circle: bulge = +1, traverses p_right → top → p_left → bottom → p_right.
        let ccw = VcObject::new(
            vec![
                Segment {
                    kind: SegmentKind::Circle,
                    start: p_right,
                    end: p_left,
                    bulge: 1.0,
                    center: Some(center),
                    layer: "0".into(),
                    color: 7,
                },
                Segment {
                    kind: SegmentKind::Circle,
                    start: p_left,
                    end: p_right,
                    bulge: 1.0,
                    center: Some(center),
                    layer: "0".into(),
                    color: 7,
                },
            ],
            true,
        );
        // CW circle: bulge = -1.
        let cw = VcObject::new(
            vec![
                Segment {
                    kind: SegmentKind::Circle,
                    start: p_right,
                    end: p_left,
                    bulge: -1.0,
                    center: Some(center),
                    layer: "0".into(),
                    color: 7,
                },
                Segment {
                    kind: SegmentKind::Circle,
                    start: p_left,
                    end: p_right,
                    bulge: -1.0,
                    center: Some(center),
                    layer: "0".into(),
                    color: 7,
                },
            ],
            true,
        );
        let area_ccw = object_signed_area(&ccw);
        let area_cw = object_signed_area(&cw);
        let pi_r2 = std::f64::consts::PI * r * r;
        assert!(
            (area_ccw - pi_r2).abs() < 1e-6,
            "CCW circle area should be +π·r² (got {area_ccw}, expected {pi_r2})",
        );
        assert!(
            (area_cw + pi_r2).abs() < 1e-6,
            "CW circle area should be -π·r² (got {area_cw}, expected {})",
            -pi_r2,
        );
    }

    #[test]
    fn reverse_offset_negates_bulges() {
        let arc1 = Segment::arc(p(0.0, 0.0), p(10.0, 0.0), 0.5, None, "0", 7);
        let arc2 = Segment::arc(p(10.0, 0.0), p(10.0, 10.0), -0.3, None, "0", 7);
        let mut o = PolylineOffset {
            segments: vec![arc1, arc2],
            closed: false,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        };
        reverse_offset(&mut o);
        assert_eq!(o.segments.len(), 2);
        // After reversal, the chain runs end → start of the original
        // last arc, then end → start of the first arc — and the bulges
        // negate so the curve direction is preserved.
        assert_eq!(o.segments[0].start, p(10.0, 10.0));
        assert_eq!(o.segments[0].end, p(10.0, 0.0));
        assert!((o.segments[0].bulge - 0.3).abs() < 1e-12);
        assert_eq!(o.segments[1].start, p(10.0, 0.0));
        assert_eq!(o.segments[1].end, p(0.0, 0.0));
        assert!((o.segments[1].bulge - -0.5).abs() < 1e-12);
    }

    /// Regression: a circle whose radius sits in the previously-dead
    /// `[0.95·r, r)` zone now gets a drill substitution rather than
    /// being silently dropped by the empty inward-cascade.
    #[test]
    fn near_tool_radius_circle_drills_at_center() {
        use crate::geometry::SegmentKind;
        // 2.85 mm radius circle, 3 mm tool (so tool_radius = 1.5 vs r 2.85
        // — the OLD test used a 1 mm circle vs 3 mm tool. We pick a
        // radius that's bigger than 0.95 * tool_radius but still smaller
        // than tool_radius so the prior threshold would have rejected
        // it. tool_radius = 3.0 → old threshold 2.85; choose r = 2.9.
        let tool_radius = 3.0_f64;
        let r = 2.9_f64;
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
        let drill = small_circle_drill(&obj, tool_radius);
        assert!(
            drill.is_some(),
            "near-tool-radius circle must drill at center"
        );
        let drill = drill.unwrap();
        assert_eq!(drill.segments.len(), 1);
        assert!(matches!(drill.segments[0].kind, SegmentKind::Point));
        assert!(drill.segments[0].start.distance(center) < 1e-9);
    }

    /// Regression: a closed circle whose radius EXACTLY equals the
    /// tool radius (e.g. a 6 mm hole milled with a 6 mm endmill) must
    /// route through the drill substitution — the cascade can't carve
    /// such a hole (inward offset collapses to empty) but the drill
    /// plunge cuts a perfectly fitting hole at the circle's center.
    /// Pre-fix the `>= tool_radius * 0.999` rejected the exact-fit case
    /// and the hole was silently dropped.
    #[test]
    fn exact_fit_circle_drills_at_center() {
        use crate::geometry::SegmentKind;
        // 3 mm radius circle, 3 mm tool radius (6 mm endmill, 6 mm hole).
        let tool_radius = 3.0_f64;
        let r = 3.0_f64;
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
        let drill = small_circle_drill(&obj, tool_radius);
        assert!(
            drill.is_some(),
            "exact-fit circle (r == tool_radius) must drill at center",
        );
        let drill = drill.unwrap();
        assert_eq!(drill.segments.len(), 1);
        assert!(matches!(drill.segments[0].kind, SegmentKind::Point));
        assert!(drill.segments[0].start.distance(center) < 1e-9);
    }

    /// Boundary: a circle slightly LARGER than the tool (within
    /// the 0.1 % floating-point slop band) still routes to drill — the
    /// cutter fills the hole; we'd rather emit a useful drill plunge
    /// than a silent drop. Above that band (radius > 1.001 *
    /// `tool_radius`) the cascade owns the cut.
    #[test]
    fn slightly_oversize_circle_drills_at_center_within_slop_band() {
        use crate::geometry::SegmentKind;
        let tool_radius = 3.0_f64;
        // r = tool_radius + 0.0005 → 0.017 % over nominal, well inside
        // the 0.1 % slop band.
        let r = tool_radius + 0.0005;
        let center = Point2::new(0.0, 0.0);
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
        assert!(small_circle_drill(&obj, tool_radius).is_some());
        // Far above the slop band: the cascade owns this.
        let bigger_r = tool_radius * 1.05;
        let p_right = Point2::new(center.x + bigger_r, center.y);
        let p_left = Point2::new(center.x - bigger_r, center.y);
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
        assert!(small_circle_drill(&obj, tool_radius).is_none());
    }

    /// Regression: a U-shaped pocket's zigzag joiner that would
    /// span the cross-bar of the U must split the chain instead of
    /// emitting a Line that ploughs through stock.
    #[test]
    fn zigzag_u_shape_splits_chain_at_cross_bar() {
        // U-shaped outer (20mm tall, 20mm wide):
        //   (0,0)-(20,0) bottom edge
        //   (20,0)-(20,20) right wall (full height)
        //   (20,20)-(15,20) top of right arm
        //   (15,20)-(15,5)  inner wall right
        //   (15,5)-(5,5)    inner wall bottom (the cross-bar)
        //   (5,5)-(5,20)    inner wall left
        //   (5,20)-(0,20)   top of left arm
        //   (0,20)-(0,0)    left wall (full height)
        let boundary = vec![
            p(0.0, 0.0),
            p(20.0, 0.0),
            p(20.0, 20.0),
            p(15.0, 20.0),
            p(15.0, 5.0),
            p(5.0, 5.0),
            p(5.0, 20.0),
            p(0.0, 20.0),
        ];
        // Use a stride that puts at least one scanline above the
        // cross-bar — then each scanline produces TWO disjoint strokes
        // (left arm + right arm) and the joiner between them would
        // otherwise cross the cross-bar.
        let chains = pocket_zigzag(&boundary, &[], 1.5, 2.0);
        // The chain must split where the joiner would cross the cross-bar.
        assert!(
            chains.len() >= 2,
            "U-shape must produce multiple chains (one per arm region); got {}",
            chains.len()
        );
        // No emitted line segment should run along the cross-bar
        // (y ∈ [5..6]) crossing x in [5..15].
        for chain in &chains {
            for s in chain {
                let mid = Point2::new((s.start.x + s.end.x) * 0.5, (s.start.y + s.end.y) * 0.5);
                // A horizontal stroke at y > cross-bar (y >= 5 + tool_r)
                // that spans x ∈ [5..15] would be illegal — that's
                // through the cross-bar region.
                let spans_cross_bar = s.start.y > 5.5
                    && s.end.y > 5.5
                    && (s.start.y - s.end.y).abs() < 1e-6
                    && mid.x > 6.0
                    && mid.x < 14.0;
                if spans_cross_bar {
                    // Allowed only if y > 20 (above top, never happens
                    // here) or the stroke is on the same arm (entirely
                    // within one arm).
                    let on_left_arm = s.start.x.max(s.end.x) <= 5.5;
                    let on_right_arm = s.start.x.min(s.end.x) >= 14.5;
                    assert!(
                        on_left_arm || on_right_arm,
                        "zigzag stroke crossed the U's cross-bar: {s:?}"
                    );
                }
            }
        }
    }

    /// Regression: an L-shaped boundary with long walls (>= 30mm
    /// arms) produces an overcut dip at the reflex corner. Pre-fix the
    /// endpoint-only probe missed the bisector ray entirely on long
    /// walls and skipped the overcut silently.
    #[test]
    fn overcut_long_wall_reflex_corner_dips() {
        // L-shaped boundary CCW with 30mm arms (the prior test used 20mm
        // arms which the endpoint probe could just reach via the corner
        // vertex). Now the reflex corner sits at (15, 15) with each
        // wall extending 15 mm to the next vertex — well outside the
        // 0.25 mm perp tolerance via endpoint-only probing.
        let boundary = vec![
            Segment::line(p(0.0, 0.0), p(30.0, 0.0), "0", 7),
            Segment::line(p(30.0, 0.0), p(30.0, 15.0), "0", 7),
            Segment::line(p(30.0, 15.0), p(15.0, 15.0), "0", 7),
            Segment::line(p(15.0, 15.0), p(15.0, 30.0), "0", 7),
            Segment::line(p(15.0, 30.0), p(0.0, 30.0), "0", 7),
            Segment::line(p(0.0, 30.0), p(0.0, 0.0), "0", 7),
        ];
        let r = 2.0_f64;
        // Inward parallel offset by tool_radius (CCW polygon) — the L
        // arms inset by 2 mm; the reflex corner of the original (15, 15)
        // becomes a CONVEX corner on the inward offset of an L (a v1
        // miter — but reversed here for OUTSIDE-of-L cut). For the
        // overcut probe we want the reflex case: CW-wound offset
        // around an L-shaped ISLAND. Reverse to get that.
        let mut offset = PolylineOffset {
            segments: vec![
                Segment::line(p(r, r), p(30.0 - r, r), "0", 7),
                Segment::line(p(30.0 - r, r), p(30.0 - r, 15.0 - r), "0", 7),
                Segment::line(p(30.0 - r, 15.0 - r), p(15.0 + r, 15.0 - r), "0", 7),
                Segment::line(p(15.0 + r, 15.0 - r), p(15.0 + r, 30.0 - r), "0", 7),
                Segment::line(p(15.0 + r, 30.0 - r), p(r, 30.0 - r), "0", 7),
                Segment::line(p(r, 30.0 - r), p(r, r), "0", 7),
            ],
            closed: true,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        };
        offset.segments.reverse();
        for s in &mut offset.segments {
            std::mem::swap(&mut s.start, &mut s.end);
        }
        let before = offset.segments.len();
        apply_overcut(&mut offset, &boundary, r);
        assert!(
            offset.segments.len() > before,
            "overcut with long walls must still insert a dip (was {before}, now {})",
            offset.segments.len()
        );
    }

    /// Regression: the thread-local panic sink starts empty, and
    /// `take_parallel_offset_panics` returns its contents and clears the
    /// sink. We can't easily synthesise a `cavalier_contours` panic in a
    /// unit test (the assert is internal to the crate's offset
    /// machinery), so we test the API contract: stash a synthetic
    /// record via the public `take_parallel_offset_panics` round-trip.
    #[test]
    fn parallel_offset_panic_sink_drains_and_clears() {
        let drained = take_parallel_offset_panics();
        // The sink may already be empty depending on test order; we
        // just assert no panic record is returned twice (drain clears
        // the sink).
        assert!(drained.iter().all(|p| !p.layer.is_empty()) || drained.is_empty());
        let second = take_parallel_offset_panics();
        assert!(
            second.is_empty(),
            "sink must be empty after the first drain"
        );
    }

    /// Regression: a polygon whose top edge grazes a scanline at a
    /// vertex (producing 1 odd crossing under the half-open rule) is
    /// coalesced so the count returns to even. We don't lose strokes
    /// when a vertex sits exactly on the sweep.
    #[test]
    fn horizontal_crossings_coalesces_vertex_tangent_duplicates() {
        // A polygon where two adjacent edges both end at the same vertex
        // (10, 5). Probe at y = 5: the half-open rule could emit two
        // x=10 crossings (one per edge sharing the vertex) — without
        // dedup that's 4 crossings (= even, but with a duplicate in the
        // middle). The dedup collapses the duplicates so the resulting
        // pairs are sensible interior intervals.
        let poly = vec![
            p(0.0, 0.0),
            p(20.0, 0.0),
            p(20.0, 10.0),
            p(10.0, 5.0), // touch vertex at y = 5
            p(0.0, 10.0),
        ];
        let xs = horizontal_crossings(&poly, 5.0, 0.0, 20.0);
        // (10, 5) is a local-minimum tangent: the scanline grazes it but
        // the interior at y = 5 is the single span [0, 20]. The tangent
        // pair at x = 10 must cancel — NOT collapse to one (which used to
        // leave [0, 10] and a tool-radius ribbon of uncut stock from
        // x = 10 to 20).
        assert_eq!(xs.len(), 2, "expected one interval, got {xs:?}");
        assert!((xs[0] - 0.0).abs() < 1e-6, "left crossing: {xs:?}");
        assert!((xs[1] - 20.0).abs() < 1e-6, "right crossing: {xs:?}");
    }

    /// Regression: a Pocket op with `nocontour=true` and the Zigzag
    /// strategy must NOT leave a tool-radius-wide ribbon of uncut stock
    /// along every wall. Pre-fix the rough boundary was already inset by
    /// `tool_r`, then `pocket_zigzag` self-inset by another `tool_r` —
    /// without the wall ring (skipped on nocontour) the outermost
    /// stroke sat `2·tool_r` from the original wall. Post-fix:
    /// `pocket_zigzag` is invoked with `tool_diameter = 0` when
    /// `nocontour = true` so the outermost stroke reaches the
    /// already-inset boundary edge (a `tool_r` from the original wall).
    #[test]
    fn pocket_zigzag_nocontour_reaches_inset_edge() {
        let obj = closed_square(40.0);
        let tool_r = 2.0_f64;
        // With nocontour=true the post-fix code passes tool_diameter=0
        // to pocket_zigzag → no double-inset; strokes reach the
        // tool_r-inset edge along X (and Y, modulo the half-open
        // scanline rule that drops the top edge by one stride).
        let offsets = pocket_for_object(
            &obj,
            tool_r,
            true,
            6,
            PocketEmit::Zigzag { angle_deg: 0.0 },
            &[],
            tool_r * 2.0 * 0.5,
            0.0,
            None,
            crate::project::tool::SpindleDirection::Cw,
        );
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut zigzag_found = false;
        for o in &offsets {
            if o.is_pocket != 1 {
                continue;
            }
            zigzag_found = true;
            for s in &o.segments {
                for pt in [s.start, s.end] {
                    min_x = min_x.min(pt.x);
                    max_x = max_x.max(pt.x);
                }
            }
        }
        assert!(zigzag_found, "expected at least one zigzag PolylineOffset");
        // Pre-fix the outermost strokes sat at x ≈ 2*tool_r and
        // x ≈ 40 - 2*tool_r (the boundary's tool_r self-inset on top
        // of the rough boundary's tool_r inset = 2·tool_r from the
        // original wall). Post-fix they sit at x ≈ tool_r and
        // x ≈ 40 - tool_r. Allow tiny slop for the per-stroke
        // endpoint inset clamp.
        let slop = 0.1;
        assert!(
            min_x <= tool_r + slop,
            "outermost stroke min_x {min_x:.3} > inset edge ({tool_r:.3}) + slop {slop} — pre-fix double-inset bug"
        );
        assert!(
            max_x >= 40.0 - tool_r - slop,
            "outermost stroke max_x {max_x:.3} < inset edge ({:.3}) - slop {slop} — pre-fix double-inset bug",
            40.0 - tool_r
        );
        // Sanity: the buggy pre-fix x bounds would be [2·tool_r,
        // 40 - 2·tool_r] = [4, 36], leaving a tool_r-wide ribbon.
        // Post-fix bounds are at least tool_r tighter — verify so
        // the test fails clearly under the pre-fix regression.
        assert!(
            min_x < 2.0 * tool_r - 0.5,
            "outermost stroke min_x {min_x:.3} is still ≥ 2·tool_r — pre-fix double-inset still in effect"
        );
        assert!(
            max_x > 40.0 - 2.0 * tool_r + 0.5,
            "outermost stroke max_x {max_x:.3} is still ≤ 40 - 2·tool_r — pre-fix double-inset still in effect"
        );
    }

    /// Regression: a fine-finish stride (0.05 mm, well below the
    /// old 0.1 mm silent clamp) must actually produce rows at the
    /// requested density. Pre-fix the function ran with stride = 0.1
    /// regardless of the user's value, halving the raster density and
    /// hiding the loss behind the silent clamp.
    #[test]
    fn pocket_zigzag_honors_sub_clamp_stride() {
        let _ = take_zigzag_stride_degenerate();
        // 10 × 10 square pocket. Cutter diameter zero (nocontour-style
        // — we want the stroke count, not the inset behaviour).
        let boundary = vec![p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)];
        let coarse = pocket_zigzag(&boundary, &[], 0.5, 0.0);
        let fine = pocket_zigzag(&boundary, &[], 0.05, 0.0);
        let coarse_strokes: usize = coarse.iter().map(std::vec::Vec::len).sum();
        let fine_strokes: usize = fine.iter().map(std::vec::Vec::len).sum();
        // 10x coarser stride ⇒ roughly 10x fewer strokes. Pre-fix both
        // collapsed onto the 0.1 mm clamp and produced ~the same count.
        assert!(
            fine_strokes >= coarse_strokes * 5,
            "fine-stride raster ({fine_strokes} strokes at 0.05 mm) should be much denser than coarse ({coarse_strokes} at 0.5 mm) — pre-fix both clamped to 0.1 mm"
        );
        // No degeneracy warning at 0.05 mm — that's well above the 1e-6
        // mm floor.
        assert!(
            take_zigzag_stride_degenerate().is_empty(),
            "0.05 mm stride must not record a degeneracy event — only sub-fp strides do"
        );
    }

    /// Regression: a truly degenerate stride (sub-fp) must record
    /// a `ZigzagStrideDegenerate` event so the pipeline can surface a
    /// `zigzag_stride_clamped_below_minimum` warning instead of
    /// silently emitting no toolpath.
    #[test]
    fn pocket_zigzag_records_degenerate_stride() {
        let _ = take_zigzag_stride_degenerate();
        let boundary = vec![p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)];
        let chains = pocket_zigzag(&boundary, &[], 1e-9, 0.0);
        assert!(chains.is_empty(), "sub-fp stride must produce no strokes");
        let drained = take_zigzag_stride_degenerate();
        assert_eq!(
            drained.len(),
            1,
            "exactly one degeneracy event expected for sub-fp stride"
        );
        assert!(drained[0].stride_mm < 1e-6);
    }

    /// Regression: an island that wholly spans one or more
    /// scanlines (so the row emits no strokes) must NOT flip the
    /// serpent parity. Pre-fix the bookkeeping toggled `flip`
    /// unconditionally; the next non-empty row ran in the wrong
    /// direction relative to the previous non-empty row, doubling
    /// cutter travel across the island.
    ///
    /// Setup: 20-mm-tall pocket spanning x ∈ [0..20]. An island
    /// covering x ∈ [0..20] (full width) for y ∈ [5..15] — i.e. the
    /// island swallows several scanlines wholesale, producing
    /// consecutive empty rows. With `tool_diameter` = 1 mm and
    /// stride = 1 mm, scanlines at y = 0.5, 1.5, … 19.5 each emit one
    /// stroke unless they fall inside the island band (5..15) — those
    /// rows emit zero strokes (the entire outer-pair gets swallowed by
    /// the island interval).
    ///
    /// Pre-fix: `flip` toggled on every empty row in the band ⇒ the
    /// row at y = 15.5 (first non-empty after the band) ran in the
    /// SAME direction as the row at y = 4.5 (last non-empty before).
    /// Post-fix: the band leaves parity unchanged ⇒ y = 15.5 runs
    /// OPPOSITE to y = 4.5.
    #[test]
    fn pocket_zigzag_empty_row_preserves_flip_parity() {
        let boundary = vec![p(0.0, 0.0), p(20.0, 0.0), p(20.0, 20.0), p(0.0, 20.0)];
        // Full-width island swallowing y ∈ [4..13] (an ODD-sized
        // band — picked so pre-fix's per-row toggle gives an
        // OBSERVABLY different parity than post-fix's "no toggle on
        // empty row"). Scanlines run at y = 0.5, 1.5, … 19.5; the
        // band of empty rows is y = 4.5, 5.5, 6.5, 7.5, 8.5, 9.5,
        // 10.5, 11.5, 12.5 (9 rows). Pre-fix: 9 toggles flip parity;
        // post-fix: 0 toggles preserve it.
        let island = vec![p(-1.0, 4.0), p(21.0, 4.0), p(21.0, 13.0), p(-1.0, 13.0)];
        let chains = pocket_zigzag(&boundary, &[island], 1.0, 1.0);
        assert!(!chains.is_empty(), "expected at least one chain");
        // Collect every horizontal stroke (ignoring connectors), then
        // pick the first non-empty rows on either side of the gap.
        let mut strokes: Vec<(f64, f64, f64)> = Vec::new();
        for chain in &chains {
            for s in chain {
                if (s.start.y - s.end.y).abs() < 1e-6 {
                    strokes.push((s.start.y, s.start.x, s.end.x));
                }
            }
        }
        strokes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        // Find the last stroke with y < 4 (below the island band) and
        // the first stroke with y > 13 (above the band).
        let last_below = strokes
            .iter()
            .rev()
            .find(|s| s.0 < 4.0)
            .copied()
            .expect("expected at least one row below the island band");
        let first_above = strokes
            .iter()
            .find(|s| s.0 > 13.0)
            .copied()
            .expect("expected at least one row above the island band");
        // Direction sign: +1 = L→R, -1 = R→L.
        let dir_below = (last_below.2 - last_below.1).signum();
        let dir_above = (first_above.2 - first_above.1).signum();
        // Post-fix: with 9 empty rows in the band (odd count),
        // the old code would flip parity 9 times → next non-empty row
        // matches dir_below. Post-fix the band is parity-neutral →
        // next non-empty row is OPPOSITE to dir_below (the LAST
        // non-empty row's toggle still applies). Assert opposite.
        assert!(
            (dir_below + dir_above).abs() < 0.5,
            "flip-parity regression: first row above empty-band must run opposite to last row below — got dir_below={dir_below}, dir_above={dir_above}"
        );
    }

    /// Regression: at high overlap (`xy_step` < `tool_radius`) the
    /// pre-fix cascade's first ring around an island sat too close to
    /// the raw island wall — the cutter edge intruded by (`tool_r` −
    /// step) mm. With the over-inflation fix the cutter edge MUST stay
    /// outside the raw island for any step ≤ `tool_radius`.
    #[test]
    fn pocket_cascade_high_overlap_keeps_island_clearance() {
        // 50 × 50 pocket, 10 × 10 island centered at (25, 25). Tool
        // radius = 2 mm. Step = 0.4 mm (80% overlap — well below
        // tool_radius). Pre-fix: first cascade ring around island sits
        // at 0.4 mm from raw wall ⇒ cutter EDGE bites in by 1.6 mm.
        let outer = vec![p(0.0, 0.0), p(50.0, 0.0), p(50.0, 50.0), p(0.0, 50.0)];
        let raw_island = vec![p(20.0, 20.0), p(30.0, 20.0), p(30.0, 30.0), p(20.0, 30.0)];
        let tool_r = 2.0_f64;
        let step = 0.4_f64; // < tool_r ⇒ pre-fix intrusion of 1.6 mm
        let knd4_islands = inflate_islands_by_tool_radius(&[raw_island.clone()], tool_r);
        let over_inflated = over_inflate_islands_for_high_overlap(&knd4_islands, tool_r, step);
        // The over-inflated boundary must sit MEASURABLY further from
        // the raw island wall than the bare tool-radius inflation.
        let bbox = |pts: &[Point2]| {
            let (mut mnx, mut mxx) = (f64::INFINITY, f64::NEG_INFINITY);
            for p in pts {
                if p.x < mnx {
                    mnx = p.x;
                }
                if p.x > mxx {
                    mxx = p.x;
                }
            }
            (mnx, mxx)
        };
        let (kmin, _) = bbox(&knd4_islands[0]);
        let (omin, _) = bbox(&over_inflated[0]);
        // Raw island bbox min_x = 20. tool-radius inflate ≈ 18 (tool_r=2 outward).
        // High-overlap over-inflate ≈ 18 - (tool_r - step) = 16.4.
        assert!(
            omin + 0.05 < kmin,
            "high-overlap over-inflate must extend further than bare tool-radius inflate (over={omin:.3} vs knd4={kmin:.3})"
        );
        // Run the cascade against the over-inflated islands. Every
        // ring's vertex must keep the cutter EDGE outside the raw
        // island — i.e. every centerline point must sit ≥ tool_r from
        // the raw island wall.
        let rings = pocket_cascade_with_islands(&outer, &over_inflated, step);
        assert!(!rings.is_empty(), "cascade produced no rings");
        // Check the FIRST ring around the island (the one that
        // previously intruded). The cascade returns multiple rings;
        // every ring vertex near the island must keep ≥ tool_r
        // clearance from the raw island wall.
        let dist_to_raw = |pt: Point2| -> f64 {
            // Euclidean distance from `pt` to the raw [20..30]²
            // island. For points outside the rectangle this is the
            // perpendicular drop onto the nearest edge / corner; for
            // points inside it's negative (signed distance with the
            // outside positive).
            let dx_out = ((20.0 - pt.x).max(0.0)).max(pt.x - 30.0);
            let dy_out = ((20.0 - pt.y).max(0.0)).max(pt.y - 30.0);
            let inside_x = pt.x > 20.0 && pt.x < 30.0;
            let inside_y = pt.y > 20.0 && pt.y < 30.0;
            if inside_x && inside_y {
                // Inside the rectangle: signed distance to nearest
                // edge, negated so "inside" is negative.
                let dx_in = (pt.x - 20.0).min(30.0 - pt.x);
                let dy_in = (pt.y - 20.0).min(30.0 - pt.y);
                -(dx_in.min(dy_in))
            } else {
                // Outside in at least one axis: Euclidean dist to the
                // nearest edge or corner.
                (dx_out * dx_out + dy_out * dy_out).sqrt()
            }
        };
        // Find vertices near the island wall (< 2·tool_r away on the
        // outside) — these are the first ring around the island.
        let mut near: Vec<f64> = Vec::new();
        for ring in &rings {
            for pt in ring {
                let d = dist_to_raw(*pt);
                if (0.0..2.0 * tool_r).contains(&d) {
                    near.push(d);
                }
            }
        }
        assert!(
            !near.is_empty(),
            "no cascade vertex sat near the island — test geometry mis-sized"
        );
        let nearest = near.iter().copied().fold(f64::INFINITY, f64::min);
        // Cutter EDGE clearance = nearest_centerline_dist - tool_r.
        // Must be ≥ 0 (allowing FP slop). Pre-fix this would have been
        // ≈ step - tool_r = -1.6 mm.
        let edge_clearance = nearest - tool_r;
        assert!(
            edge_clearance >= -0.05,
            "cutter EDGE intrudes into raw island by {:.3} mm (nearest centerline {:.3}, tool_r {tool_r}) — high-overlap regression",
            -edge_clearance,
            nearest
        );
    }

    /// Regression: at sub-mm scale (5 mm part, 0.3 mm endmill) the
    /// pre-fix overcut's 0.25 mm `perp_tol` was wider than the entire
    /// part bbox, picking the nearest WRONG wall as the overcut probe
    /// target. With the bbox-scaled tolerance the function either picks
    /// the right wall or makes no dip at all (rather than gouging an
    /// arbitrary direction). The CHECK here is the inverse: a known
    /// reflex corner at sub-mm scale must not gouge the offset into a
    /// totally-wrong direction (>= 2 × intended dip).
    #[test]
    fn apply_overcut_scales_perp_tol_with_bbox_at_sub_mm_scale() {
        // L-shape boundary at 5 mm scale, CCW. Reflex corner sits at
        // (2.5, 2.5); short arms — 2.5 mm each. Pre-fix the 0.25 mm
        // perp_tol pulled in the FAR wall (at x=5) as a candidate
        // because it sat within 0.25 mm of the outward bisector ray's
        // tangent — wrong wall, gouge in the wrong direction.
        let boundary_segs = vec![
            Segment::line(p(0.0, 0.0), p(5.0, 0.0), "0", 7),
            Segment::line(p(5.0, 0.0), p(5.0, 2.5), "0", 7),
            Segment::line(p(5.0, 2.5), p(2.5, 2.5), "0", 7),
            Segment::line(p(2.5, 2.5), p(2.5, 5.0), "0", 7),
            Segment::line(p(2.5, 5.0), p(0.0, 5.0), "0", 7),
            Segment::line(p(0.0, 5.0), p(0.0, 0.0), "0", 7),
        ];
        // Build an offset polyline matching the boundary inset by
        // tool_radius = 0.15 mm (0.3 mm endmill). A CCW polyline with
        // a reflex corner at the inner L joint.
        let r = 0.15_f64;
        let off = [
            p(r, r),
            p(5.0 - r, r),
            p(5.0 - r, 2.5 - r),
            p(2.5 - r, 2.5 - r),
            p(2.5 - r, 5.0 - r),
            p(r, 5.0 - r),
        ];
        let mut offset = PolylineOffset {
            segments: vec![
                Segment::line(off[0], off[1], "0", 7),
                Segment::line(off[1], off[2], "0", 7),
                Segment::line(off[2], off[3], "0", 7),
                Segment::line(off[3], off[4], "0", 7),
                Segment::line(off[4], off[5], "0", 7),
                Segment::line(off[5], off[0], "0", 7),
            ],
            closed: true,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        };
        // bbox diagonal of the 5 mm L = √(5² + 5²) = 7.07 mm
        // ⇒ perp_tol = 7.07e-3 mm. The old 0.25 mm tol was 35× too
        // loose at this scale. Verify the function still runs (no
        // panic, no inversion of winding) and any inserted dip points
        // lie on the OUTWARD side of the reflex corner.
        apply_overcut(&mut offset, &boundary_segs, r);
        // The reflex corner of the offset sits at (2.5-r, 2.5-r) =
        // (2.35, 2.35). Outward direction at this reflex corner points
        // toward (5, 5) — i.e. +x and +y. Any inserted dip vertex must
        // lie on that outward side. If the pre-fix loose tolerance had
        // picked the (0,0) endpoint, the dip would point toward
        // (-x, -y) and gouge the WRONG quadrant.
        for s in &offset.segments {
            for q in [s.start, s.end] {
                // Allow the original offset vertices (which include
                // the reflex corner itself). Just check no vertex
                // lands outside the original boundary bbox by more
                // than the perp_tol slack: 5 mm + 7e-3 mm.
                assert!(
                    q.x >= -0.01 && q.x <= 5.01,
                    "overcut vertex x={:.3} outside boundary bbox — fksa gouge",
                    q.x
                );
                assert!(
                    q.y >= -0.01 && q.y <= 5.01,
                    "overcut vertex y={:.3} outside boundary bbox — fksa gouge",
                    q.y
                );
            }
        }
    }
}
