//! V-Carve medial-axis builder.
//!
//! For a closed planar region the medial axis is the locus of points
//! equidistant from at least two boundary points. We approximate it with
//! the Voronoi diagram of densely-sampled boundary points: each Voronoi
//! vertex is the circumcenter of a Delaunay triangle of three boundary
//! samples, and the inscribed-circle radius at that vertex equals the
//! triangle's circumradius. Voronoi edges run between circumcenters of
//! adjacent triangles.
//!
//! The Voronoi diagram of the boundary samples extends beyond the
//! region; we keep only edges whose both endpoints lie inside the
//! region (even-odd test on the densified outer ring with holes).

// # CAM/sim pedantic-lint exemptions
// V-carve medial-axis sampling uses `t`, `dx`, `dy`, `r` from the
// inscribed-circle parametrization; sample counts are bounded by voronator's
// triangulation size.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

use voronator::delaunator::{triangulate, Point as VPointXy, INVALID_INDEX};

use crate::cancel::CancelToken;

#[cfg(test)]
use crate::cam::is_inside_polygon;
#[cfg(test)]
use crate::cam::spatial::point_segment_dist_sq;
use crate::cam::spatial::{RegionInsideIndex, SegmentNearestGrid};
use crate::cam::{segments_to_points, VcObject};
use crate::geometry::Point2;

/// One medial-axis vertex: (x, y, `R_inscribed`). The inscribed-circle
/// radius is the distance from the vertex to the nearest boundary
/// sample.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct VPoint {
    pub x: f64,
    pub y: f64,
    pub r: f64,
}

/// Boundary input for V-Carve. `outer` is the closed outer ring;
/// `holes` are zero-or-more closed inner rings (even-odd region).
#[derive(Debug, Clone)]
pub struct VcRegion {
    pub outer: Vec<Point2>,
    pub holes: Vec<Vec<Point2>>,
}

/// Sample spacing along boundary segments before Voronoi.
/// Smaller = better axis fidelity, more triangles.
const BOUNDARY_SAMPLE_MM: f64 = 0.1;

/// Default minimum branch length (a fraction of `tool_radius`) below
/// which a terminal medial-axis chain is considered a spur and pruned.
/// The 0.1 mm boundary sampling produces ~10× too many vertices
/// on curves; without pruning the medial axis is fuzzy with micro
/// branches pointing at every densified-curve sample. `0.5 *
/// tool_radius` is a defensible default — anything shorter than half
/// the cutter's engaged width is dominated by sampling noise.
pub const PRUNE_MIN_BRANCH_FACTOR: f64 = 0.5;

/// Densify a closed ring to a list of points spaced ≤ `step` apart.
/// The first point is always emitted; the last point is dropped so the
/// ring is "open" (Voronoi sees a cycle of unique sample points).
fn densify_ring(ring: &[Point2], step: f64) -> Vec<Point2> {
    if ring.len() < 2 {
        return ring.to_vec();
    }
    let step = step.max(1e-3);
    let mut out: Vec<Point2> = Vec::new();
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        let len = a.distance(b);
        if len < 1e-9 {
            if out.is_empty() {
                out.push(a);
            }
            continue;
        }
        let n = (len / step).ceil().max(1.0) as usize;
        for j in 0..n {
            let t = (j as f64) / (n as f64);
            out.push(Point2::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y)));
        }
    }
    // Coalesce a duplicate seam if the input ring repeats its first
    // point at the end.
    if out.len() >= 2 && out[0].distance(out[out.len() - 1]) < 1e-9 {
        out.pop();
    }
    out
}

/// A chord (p0, p1) "stays in the region" iff every interior
/// sample along the chord is in the region. Endpoints are excluded
/// (they're circumcenters guaranteed-in-region by `inside[]`).
///
/// Previously this used a fixed 8 strictly-interior samples
/// (t ∈ {1/8, …, 7/8}). For a 50 mm chord that's a 6.25 mm sample
/// spacing — a hole or re-entrant notch narrower than ~1/8 of the chord
/// length could sit ENTIRELY between two consecutive samples and the
/// chord was incorrectly declared safe. The medial-axis stitcher then
/// emitted a chord that ploughed across the notch (the cutter dipped
/// into uncut stock OR carved past a hole the user wanted preserved).
///
/// Density now scales with chord length: one sample every `MAX_SAMPLE_MM`
/// (currently 0.5 mm), with a floor of 8 samples so short chords keep
/// their existing resolution. For most CAD work this is overkill — a
/// chord rarely exceeds a few mm — but for long axis segments through a
/// big region it catches small holes / notches the fixed-count version
/// missed.
fn chord_stays_in_region(region: &RegionInsideIndex, p0: Point2, p1: Point2) -> bool {
    /// Target spacing between chord-interior samples in mm.
    /// 0.5 mm matches the densification used elsewhere in the medial-axis
    /// pipeline (`BOUNDARY_SAMPLE_MM` is the same order) so a hole that
    /// resolves to a few samples on its own ring also resolves to a few
    /// samples on any chord crossing it.
    const MAX_SAMPLE_MM: f64 = 0.5;
    let chord_len = p0.distance(p1);
    // Floor preserves prior behaviour for short chords; ceil ensures we
    // bracket the chord at least every MAX_SAMPLE_MM mm. `samples` here
    // is the same parameter the prior implementation called `samples`
    // (i.e. the number of intervals — we sample t = i / samples for
    // i in 1..samples, so `samples` intervals yield `samples - 1` points).
    let samples = (chord_len / MAX_SAMPLE_MM).ceil() as usize;
    let samples = samples.max(8);
    for i in 1..samples {
        let t = (i as f64) / (samples as f64);
        let s = Point2::new(p0.x + (p1.x - p0.x) * t, p0.y + (p1.y - p0.y) * t);
        if !region.contains(s) {
            return false;
        }
    }
    true
}

/// Even-odd region test honoring holes — point is inside iff it's
/// inside the outer ring AND outside every hole. Brute-force oracle:
/// production now goes through [`RegionInsideIndex`], which reproduces
/// this result edge-for-edge; retained for the equivalence tests.
#[cfg(test)]
fn point_in_region(region: &VcRegion, p: Point2) -> bool {
    if !is_inside_polygon(&region.outer, p) {
        return false;
    }
    for h in &region.holes {
        if is_inside_polygon(h, p) {
            return false;
        }
    }
    true
}

/// Build the medial axis as a `Vec<Vec<VPoint>>`. Each inner vec is one
/// connected polyline of medial-axis vertices, ordered along the chain.
/// Vertices outside the region are excluded; edges with at least one
/// outside endpoint are dropped.
#[must_use]
pub fn medial_axis(region: &VcRegion) -> Vec<Vec<VPoint>> {
    medial_axis_cancellable(region, None)
}

/// Cancellable wrapper around [`medial_axis`]. Checks `cancel` after
/// every Voronoi-vertex traversal; on cancellation returns whatever
/// chains have been emitted so far (often empty). Callers should also
/// inspect the cancel flag after this call to bail out cleanly.
// Medial-axis derivation interleaves Voronoi extraction, edge filtering,
// and inscribed-circle radius sampling — the algorithm reads linearly
// top-to-bottom; splitting would scatter the geometric reasoning.
#[allow(clippy::too_many_lines)]
#[must_use]
pub fn medial_axis_cancellable(
    region: &VcRegion,
    cancel: Option<&CancelToken>,
) -> Vec<Vec<VPoint>> {
    let is_cancelled = || cancel.is_some_and(super::super::pipeline::CancelToken::is_cancelled);
    // Densify all boundaries (outer + holes) into a single sample list.
    // Voronoi-of-points needs >= 3 samples to triangulate at all.
    let mut samples: Vec<Point2> = Vec::new();
    samples.extend(densify_ring(&region.outer, BOUNDARY_SAMPLE_MM));
    for h in &region.holes {
        samples.extend(densify_ring(h, BOUNDARY_SAMPLE_MM));
    }
    if samples.len() < 3 {
        return Vec::new();
    }

    let voronator_pts: Vec<VPointXy> = samples
        .iter()
        .map(|p| VPointXy { x: p.x, y: p.y })
        .collect();
    let Some(tri) = triangulate(&voronator_pts) else {
        return Vec::new();
    };

    // Per-triangle circumcenter. Triangles index into tri.triangles in
    // groups of 3 (vertex indices into samples). The halfedges array
    // maps half-edge i to its twin in an adjacent triangle, or
    // INVALID_INDEX on the convex hull.
    //
    // The circumradius is NOT used as the medial-axis radius. With
    // discrete boundary samples, the circumradius equals the distance
    // from the circumcenter to its three witness samples — but near
    // re-entrant corners or features close to the boundary, the
    // perpendicular distance to a nearby boundary EDGE can be smaller
    // than the distance to those witnesses. Using the circumradius
    // overestimates the inscribed circle, which becomes over-cut depth
    // for V-bit / ball-nose halfpipe ops downstream. We instead compute
    // the actual distance from each circumcenter to the nearest
    // boundary segment below.
    let n_tri = tri.len();
    let mut centers: Vec<Option<Point2>> = Vec::with_capacity(n_tri);
    for t in 0..n_tri {
        let i0 = tri.triangles[3 * t];
        let i1 = tri.triangles[3 * t + 1];
        let i2 = tri.triangles[3 * t + 2];
        let a = samples[i0];
        let b = samples[i1];
        let c = samples[i2];
        if let Some((cx, cy, _)) = circumcircle(a, b, c) {
            centers.push(Some(Point2::new(cx, cy)));
        } else {
            centers.push(None);
        }
    }

    // Boundary segments — used to compute the true inscribed-circle
    // radius at each medial-axis vertex (perpendicular distance to the
    // nearest input edge, not to the discrete sample points).
    let boundary_segments = collect_boundary_segments(region);
    // Build spatial indexes ONCE per region so the per-circumcenter
    // probes below are ~O(1) instead of O(B). Without these the loop is
    // O(n_tri · B) ≈ O(B²): both `point_in_region` (ray cast over every
    // ring edge) and the nearest-segment distance are linear in B, run
    // once per circumcenter (n_tri ≈ 2B). Both indexes reproduce the
    // brute-force result exactly (see `cam::spatial`), so output is
    // unchanged.
    let region_index = RegionInsideIndex::new(&region.outer, &region.holes);
    let boundary_grid = SegmentNearestGrid::new(&boundary_segments);

    // A medial-axis vertex is a circumcenter that lies inside the
    // region. Build parallel arrays aligned to `centers`: `inside` is
    // the bitset of "circumcenter lies in region AND its triangle has
    // a defined circumcenter"; `vpts` stores the inscribed-radius
    // VPoint for valid entries (sentinel `VPoint::default()` for
    // invalid ones — they're never read because the graph below only
    // adds adjacencies where both endpoints are `inside`).
    let mut inside: Vec<bool> = vec![false; n_tri];
    let mut vpts: Vec<VPoint> = vec![VPoint::default(); n_tri];
    for (i, c) in centers.iter().enumerate() {
        if let Some(p) = c {
            if region_index.contains(*p) {
                let r = boundary_grid.nearest_distance(*p);
                inside[i] = true;
                vpts[i] = VPoint { x: p.x, y: p.y, r };
            }
        }
    }

    // Edges between adjacent triangles whose circumcenters are both
    // inside form the medial-axis graph. We deduplicate by halfedge ↔
    // twin pairing — only emit when our halfedge index is the smaller.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n_tri];
    for e in 0..tri.halfedges.len() {
        let twin = tri.halfedges[e];
        if twin == INVALID_INDEX || twin <= e {
            continue;
        }
        let t0 = e / 3;
        let t1 = twin / 3;
        if inside[t0] && inside[t1] {
            // Reject the chord between the two circumcenters when
            // it leaves the region. The prior single-midpoint test
            // missed thin axis-aligned chords across a slot whose
            // midpoint happened to land inside but whose 25%/75%
            // points lay outside — and rejected legitimate chords in
            // the opposite configuration. The replacement samples 8
            // strictly-interior points along the chord; the chord is
            // accepted iff every sample lies inside the region.
            let p0 = Point2::new(vpts[t0].x, vpts[t0].y);
            let p1 = Point2::new(vpts[t1].x, vpts[t1].y);
            if !chord_stays_in_region(&region_index, p0, p1) {
                continue;
            }
            adj[t0].push(t1);
            adj[t1].push(t0);
        }
    }

    // Walk the resulting graph as connected polylines. Start from
    // degree-1 endpoints (so terminal branches stay open) and from any
    // remaining unvisited cycle.
    let mut visited_edge: std::collections::HashSet<(usize, usize)> =
        std::collections::HashSet::new();
    let mut polylines: Vec<Vec<VPoint>> = Vec::new();

    let edge_key = |a: usize, b: usize| if a < b { (a, b) } else { (b, a) };

    // Drain endpoint walks first.
    for start in 0..n_tri {
        if is_cancelled() {
            return polylines;
        }
        if !inside[start] || adj[start].len() != 1 {
            continue;
        }
        // Avoid restarting from the other endpoint of an already-walked
        // chain — only start when the single edge is unwalked.
        let nb = adj[start][0];
        if visited_edge.contains(&edge_key(start, nb)) {
            continue;
        }
        let mut chain: Vec<VPoint> = Vec::new();
        chain.push(vpts[start]);
        let mut prev = start;
        let mut cur = nb;
        loop {
            // `insert` returns false when the edge was already in
            // the set — guarantee at-most-once segment emission per
            // edge across all chains.
            if !visited_edge.insert(edge_key(prev, cur)) {
                chain.push(vpts[cur]);
                break;
            }
            chain.push(vpts[cur]);
            // Pick the next neighbor that isn't `prev` and whose edge
            // isn't visited; if there's a branch, end this chain at the
            // junction (we'll start fresh chains from the junction's
            // other branches in the cycle pass).
            let nexts: Vec<usize> = adj[cur]
                .iter()
                .copied()
                .filter(|&n| n != prev && !visited_edge.contains(&edge_key(cur, n)))
                .collect();
            if nexts.len() != 1 {
                break;
            }
            prev = cur;
            cur = nexts[0];
        }
        if chain.len() >= 2 {
            polylines.push(chain);
        }
    }

    // Cycles + leftover branches: any remaining unvisited edge starts a
    // new chain.
    //
    // The walk's loop bound is purely edge-visited (`visited_edge`)
    // — every iteration consumes at least one fresh edge, so the walk
    // terminates in O(E) total across all chains. We additionally bound
    // each chain by `n_tri` iterations as a belt-and-braces guard against
    // pathological graphs (self-loops, multi-edges) that voronator might
    // theoretically emit on degenerate input. Without that guard a stuck
    // walk would spin instead of erroring; with it, we bail and the chain
    // is dropped (the early-exit cap is way above any realistic chain
    // length).
    let max_chain_len = n_tri + 1;
    for start in 0..n_tri {
        if is_cancelled() {
            return polylines;
        }
        if !inside[start] {
            continue;
        }
        for &nb in &adj[start].clone() {
            if visited_edge.contains(&edge_key(start, nb)) {
                continue;
            }
            let mut chain: Vec<VPoint> = Vec::new();
            chain.push(vpts[start]);
            let mut prev = start;
            let mut cur = nb;
            let mut steps = 0usize;
            loop {
                if steps > max_chain_len {
                    // Defensive bail — should never trip on
                    // well-formed Voronoi graphs; protects against
                    // pathological multi-edge / self-loop input.
                    break;
                }
                steps += 1;
                let edge = edge_key(prev, cur);
                if !visited_edge.insert(edge) {
                    // This edge was already walked by a prior
                    // chain — stop now so we don't emit a duplicate
                    // segment. The `nexts` filter excludes visited edges,
                    // but a re-entrant chain start could land on an
                    // already-walked edge and emit it twice without this guard.
                    chain.push(vpts[cur]);
                    break;
                }
                chain.push(vpts[cur]);
                let nexts: Vec<usize> = adj[cur]
                    .iter()
                    .copied()
                    .filter(|&n| n != prev && !visited_edge.contains(&edge_key(cur, n)))
                    .collect();
                if nexts.len() != 1 {
                    break;
                }
                prev = cur;
                cur = nexts[0];
                if cur == start {
                    visited_edge.insert(edge_key(prev, cur));
                    chain.push(vpts[cur]);
                    break;
                }
            }
            if chain.len() >= 2 {
                polylines.push(chain);
            }
        }
    }

    polylines
}

/// Prune spurious medial-axis branches that are short / never
/// engage the bit past its flat-tip plateau. Without this step the
/// 0.1 mm boundary sampling produces a fuzzy axis with dozens of
/// micro-branches pointing at every curved boundary vertex — the
/// cutter wastes time ratcheting into tiny non-features.
///
/// A chain is dropped when EITHER:
///
/// 1. Its arc-length < `tool_radius * PRUNE_MIN_BRANCH_FACTOR`
///    (default 0.5 — anything shorter than half the cutter's engaged
///    width is dominated by boundary-sampling noise; the cutter
///    physically can't make use of it).
/// 2. Its max inscribed radius across the chain ≤ `tip_radius_mm`
///    (the chain would only produce z=0 cuts — `polyline_to_z` would
///    emit an all-zero-Z toolpath that walks the surface without
///    engaging).
///
/// Chains with fewer than 2 vertices are always dropped. Chains that
/// survive the filter are returned in input order.
#[must_use]
pub fn prune_medial_axis(
    chains: Vec<Vec<VPoint>>,
    tool_radius_mm: f64,
    tip_radius_mm: f64,
) -> Vec<Vec<VPoint>> {
    let tool_r = tool_radius_mm.max(0.0);
    let tip_r = tip_radius_mm.max(0.0);
    let min_branch_len = (tool_r * PRUNE_MIN_BRANCH_FACTOR).max(1e-6);
    chains
        .into_iter()
        .filter(|chain| {
            if chain.len() < 2 {
                return false;
            }
            let mut len = 0.0;
            for w in chain.windows(2) {
                len += (w[0].x - w[1].x).hypot(w[0].y - w[1].y);
            }
            if len < min_branch_len {
                return false;
            }
            let r_max = chain.iter().map(|v| v.r).fold(0.0_f64, f64::max);
            if r_max <= tip_r + 1e-9 {
                return false;
            }
            true
        })
        .collect()
}

/// Convenience builder: turn a closed-VcObject + optional holes into a
/// `VcRegion`. Open objects aren't supported for V-Carve — they have no
/// interior — so they're rejected at the call site (pipeline emits a
/// warning).
#[must_use]
pub fn region_from_object(outer: &VcObject, holes: &[VcObject]) -> Option<VcRegion> {
    if !outer.closed {
        return None;
    }
    let outer_pts = segments_to_points(&outer.segments, 6);
    if outer_pts.len() < 3 {
        return None;
    }
    let hole_pts: Vec<Vec<Point2>> = holes
        .iter()
        .filter(|h| h.closed)
        .map(|h| segments_to_points(&h.segments, 6))
        .filter(|v| v.len() >= 3)
        .collect();
    Some(VcRegion {
        outer: outer_pts,
        holes: hole_pts,
    })
}

/// Map a medial-axis polyline to a per-point Z polyline using the V-bit
/// geometry. Returns `(x, y, z, r)` so callers can keep the inscribed
/// radius around for diagnostics.
///
/// Math (post tool-tip-aware refactor):
/// - `tip_angle_rad` is the FULL apex angle of the V cone.
/// - `tip_radius_mm` is the cone's flat tip (`tool.tip_diameter / 2`).
///   When the inscribed radius `r ≤ tip_radius_mm`, the bit's flat
///   nose rides the surface and `z = 0` — the cone hasn't engaged yet.
///   Past the tip, depth is `-(r - tip_radius_mm) / tan(angle / 2)`.
///   `0` reduces to the ideal pointed-bit formula.
/// - `r_cap` clips `r` from above. Callers typically pass
///   `min(user_carve_max_width_mm, tool.diameter / 2)` so neither the
///   user setting nor the bit's physical reach is exceeded.
/// - `z_cap` clips `|z|` from below (the `OpParams.depth` parameter —
///   itself a negative number; we treat its absolute value as the limit).
///
/// Returns `(polyline, depth_limited, all_zero)`:
/// - `depth_limited` — at least one point hit either cap.
/// - `all_zero` — every emitted Z is zero, i.e. the chain
///   walks the surface without cutting anything. Happens when the
///   effective r-cap drops at-or-below the V-bit's flat tip
///   (`effective_r_cap ≤ tip_radius_mm`), or when every medial-axis
///   point's inscribed radius is below the tip plateau. Callers
///   should skip the chain and surface a `vcarve_below_tip_radius`
///   warning rather than emitting a useless Z=0 traversal that
///   silently looks like a successful cut.
// The 3-tuple return surfaces `all_zero` alongside the existing
// `depth_limited` flag so the op driver can suppress no-op chains
// instead of silently emitting Z=0 traversals. Factoring the return
// shape into a named type would obscure the (poly, lim, all_zero)
// pattern shared with `cam::halfpipe::polyline_to_z`.
#[must_use]
#[allow(clippy::type_complexity)]
pub fn polyline_to_z(
    axis: &[VPoint],
    tip_angle_rad: f64,
    tip_radius_mm: f64,
    r_cap: Option<f64>,
    z_cap: Option<f64>,
) -> (Vec<(f64, f64, f64, f64)>, bool, bool) {
    let tan_half = (tip_angle_rad * 0.5).tan().max(1e-9);
    let tip_r = tip_radius_mm.max(0.0);
    let mut depth_limited = false;
    let mut all_zero = true;
    let mut out = Vec::with_capacity(axis.len());
    for v in axis {
        let mut r = v.r;
        if let Some(c) = r_cap {
            if r > c {
                r = c;
                depth_limited = true;
            }
        }
        // Inside the flat-tip plateau the bit can't engage — z stays 0
        // even when the medial-axis radius is technically smaller.
        let mut z = if r <= tip_r {
            0.0
        } else {
            -(r - tip_r) / tan_half
        };
        if let Some(c) = z_cap {
            let limit = c.abs();
            if z < -limit {
                z = -limit;
                depth_limited = true;
            }
        }
        if z.abs() > 1e-9 {
            all_zero = false;
        }
        out.push((v.x, v.y, z, r));
    }
    if out.is_empty() {
        all_zero = false;
    }
    (out, depth_limited, all_zero)
}

/// Flatten the region's outer ring + every hole into one list of
/// boundary segments. Each segment is `(a, b)` where `a` and `b` are
/// consecutive points along the closed ring. Used by
/// `nearest_boundary_distance` to compute the true inscribed-circle
/// radius at each medial-axis vertex.
fn collect_boundary_segments(region: &VcRegion) -> Vec<(Point2, Point2)> {
    let mut out: Vec<(Point2, Point2)> = Vec::new();
    let push_ring = |ring: &[Point2], out: &mut Vec<(Point2, Point2)>| {
        if ring.len() < 2 {
            return;
        }
        for i in 0..ring.len() {
            let a = ring[i];
            let b = ring[(i + 1) % ring.len()];
            if a.distance(b) > 1e-12 {
                out.push((a, b));
            }
        }
    };
    push_ring(&region.outer, &mut out);
    for h in &region.holes {
        push_ring(h, &mut out);
    }
    out
}

/// Minimum perpendicular distance from `p` to any of `segments` (the
/// inscribed-circle radius at a medial-axis vertex). Brute-force
/// reference: the hot path uses [`SegmentNearestGrid`] (built once per
/// region) instead, but both go through `point_segment_dist_sq`, so the
/// grid is a provable acceleration of this exact computation. Retained
/// as the equivalence oracle in tests.
#[cfg(test)]
fn nearest_boundary_distance(p: Point2, segments: &[(Point2, Point2)]) -> f64 {
    let mut best_sq = f64::INFINITY;
    for &(a, b) in segments {
        let d_sq = point_segment_dist_sq(p, a, b);
        if d_sq < best_sq {
            best_sq = d_sq;
        }
    }
    if best_sq.is_finite() {
        best_sq.sqrt()
    } else {
        0.0
    }
}

/// Circumcircle (center + radius) for triangle `(a, b, c)`. Returns
/// None on degenerate (colinear) input.
fn circumcircle(a: Point2, b: Point2, c: Point2) -> Option<(f64, f64, f64)> {
    let ax = a.x;
    let ay = a.y;
    let bx = b.x;
    let by = b.y;
    let cx = c.x;
    let cy = c.y;
    let d = 2.0 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
    if d.abs() < 1e-18 {
        return None;
    }
    let ux = ((ax * ax + ay * ay) * (by - cy)
        + (bx * bx + by * by) * (cy - ay)
        + (cx * cx + cy * cy) * (ay - by))
        / d;
    let uy = ((ax * ax + ay * ay) * (cx - bx)
        + (bx * bx + by * by) * (ax - cx)
        + (cx * cx + cy * cy) * (bx - ax))
        / d;
    let r = (ux - ax).hypot(uy - ay);
    Some((ux, uy, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    /// Equilateral triangle of side 20 mm, 60° V-bit. Incircle radius
    /// = 20 / (2·√3) ≈ 5.77 mm. Z = -R / tan(30°) ≈ -10 mm.
    #[test]
    fn equilateral_triangle_centroid_inscribed_radius() {
        let side = 20.0_f64;
        let h = side * (3.0_f64.sqrt()) * 0.5;
        let outer = vec![
            Point2::new(0.0, 0.0),
            Point2::new(side, 0.0),
            Point2::new(side * 0.5, h),
        ];
        let region = VcRegion {
            outer,
            holes: Vec::new(),
        };
        let polylines = medial_axis(&region);
        assert!(
            !polylines.is_empty(),
            "expected at least one medial axis polyline"
        );
        // The medial axis of an equilateral triangle is three segments
        // meeting at the incenter. The incenter (= centroid) is at
        // (side/2, h/3); the largest inscribed-circle radius along the
        // axis is the incircle radius.
        let r_max = polylines
            .iter()
            .flat_map(|p| p.iter())
            .map(|v| v.r)
            .fold(0.0_f64, f64::max);
        let expected_r = side / (2.0 * 3.0_f64.sqrt());
        assert!(
            approx(r_max, expected_r, 0.05),
            "max R_inscribed = {r_max}, expected ≈ {expected_r}"
        );

        let tip = (60.0_f64).to_radians();
        let (z_poly, _, _) = polyline_to_z(&polylines[0], tip, 0.0, None, None);
        let z_min = z_poly.iter().map(|t| t.2).fold(0.0_f64, f64::min);
        // The deepest point along ANY of the three axis segments
        // converges on the incenter; verify the deepest one across
        // all polylines.
        let mut z_min_all = 0.0_f64;
        for poly in &polylines {
            let (zp, _, _) = polyline_to_z(poly, tip, 0.0, None, None);
            for t in &zp {
                if t.2 < z_min_all {
                    z_min_all = t.2;
                }
            }
        }
        let _ = z_min;
        let expected_z = -expected_r / (tip * 0.5).tan();
        assert!(
            approx(z_min_all, expected_z, 0.05),
            "min Z = {z_min_all}, expected ≈ {expected_z}"
        );
    }

    /// Regression for the circumradius-vs-inscribed-radius fix:
    /// `nearest_boundary_distance` must measure the perpendicular drop
    /// onto each segment, not the distance to the endpoint sample. For
    /// a vertex sitting directly above the middle of a horizontal
    /// segment, the answer is the perpendicular distance even if the
    /// nearest endpoint is much farther away.
    #[test]
    fn nearest_boundary_distance_uses_perpendicular_foot() {
        let segs = vec![(Point2::new(-50.0, 0.0), Point2::new(50.0, 0.0))];
        let d = nearest_boundary_distance(Point2::new(0.0, 3.0), &segs);
        assert!(approx(d, 3.0, 1e-9), "perpendicular dist = {d}, want 3");
        // Endpoint sample dist would be ≈ √(50²+3²) ≈ 50.09 — confirm
        // we did NOT pick the endpoint distance.
        assert!(d < 5.0);
    }

    /// The spatial indexes that replaced the brute-force probes in the
    /// medial-axis loop must return bit-identical answers on the actual
    /// boundary data of a real region (square with a square hole). This
    /// is the integration guard for the H5 acceleration: dense probe
    /// sweep, `SegmentNearestGrid` vs `nearest_boundary_distance` and
    /// `RegionInsideIndex` vs `point_in_region`, exact equality.
    #[test]
    fn spatial_indexes_match_brute_force_on_real_region() {
        let region = VcRegion {
            outer: vec![
                Point2::new(0.0, 0.0),
                Point2::new(30.0, 0.0),
                Point2::new(30.0, 30.0),
                Point2::new(0.0, 30.0),
            ],
            holes: vec![vec![
                Point2::new(10.0, 10.0),
                Point2::new(20.0, 10.0),
                Point2::new(20.0, 20.0),
                Point2::new(10.0, 20.0),
            ]],
        };
        let boundary = collect_boundary_segments(&region);
        let grid = SegmentNearestGrid::new(&boundary);
        let region_index = RegionInsideIndex::new(&region.outer, &region.holes);
        let steps = 90;
        for i in 0..=steps {
            for j in 0..=steps {
                let p = Point2::new(
                    -3.0 + 36.0 * f64::from(i) / f64::from(steps),
                    -3.0 + 36.0 * f64::from(j) / f64::from(steps),
                );
                assert_eq!(
                    region_index.contains(p),
                    point_in_region(&region, p),
                    "region inside mismatch at {p:?}"
                );
                assert_eq!(
                    grid.nearest_distance(p),
                    nearest_boundary_distance(p, &boundary),
                    "nearest distance mismatch at {p:?}"
                );
            }
        }
    }

    /// Regression: at a re-entrant corner the inscribed-circle
    /// radius along the medial axis must drop to ~0, not the
    /// (potentially much larger) circumradius of the witness samples.
    /// A "+"-shaped region with a 4 mm wide cross has four such
    /// corners; the medial-axis radius near each corner should taper
    /// down toward the corner, never inflate above the local half-width.
    #[test]
    fn plus_shape_radius_does_not_exceed_local_half_width() {
        // 4-mm-thick plus, 20 mm tip-to-tip.
        let w = 2.0; // half-thickness
        let l = 10.0; // half tip-to-tip
        let outer = vec![
            Point2::new(-w, -l),
            Point2::new(w, -l),
            Point2::new(w, -w),
            Point2::new(l, -w),
            Point2::new(l, w),
            Point2::new(w, w),
            Point2::new(w, l),
            Point2::new(-w, l),
            Point2::new(-w, w),
            Point2::new(-l, w),
            Point2::new(-l, -w),
            Point2::new(-w, -w),
        ];
        let region = VcRegion {
            outer,
            holes: Vec::new(),
        };
        let polys = medial_axis(&region);
        assert!(!polys.is_empty());
        // The plus has no inscribed circle larger than the central
        // 2·w-square's incircle, R = w·√2 ≈ 2.83 mm. The old
        // circumradius-based implementation routinely reported larger
        // R values near the re-entrant corners.
        let r_max = polys
            .iter()
            .flat_map(|p| p.iter())
            .map(|v| v.r)
            .fold(0.0_f64, f64::max);
        let r_bound = w * std::f64::consts::SQRT_2 + 0.1;
        assert!(
            r_max <= r_bound,
            "R_max = {r_max} on plus, expected ≤ {r_bound} (= w·√2 + slack)",
        );
    }

    /// A rounded rectangle (20×10 with 2 mm corner radius)
    /// produces a fuzzy medial axis from voronator — many micro-spurs
    /// pointing at every densified-corner sample. After pruning with
    /// `tool_radius = 2 mm` (so min branch length = 1 mm) we expect
    /// the chain count to collapse to a small number — ideally 1 main
    /// spine, definitely not the dozens that pre-pruning emits.
    #[test]
    fn rounded_rect_medial_axis_prunes_corner_spurs() {
        // Approximate a rounded rectangle: 20×10 outer with 2mm
        // quarter-arc corners sampled at ~10 points each.
        let mut outer: Vec<Point2> = Vec::new();
        let r = 2.0_f64;
        let push_corner = |outer: &mut Vec<Point2>, cx: f64, cy: f64, t0: f64| {
            // Quarter arc starting at angle t0, sweeping +PI/2 CCW.
            for i in 0..=10_i32 {
                let t = t0 + (std::f64::consts::PI * 0.5) * f64::from(i) / 10.0;
                outer.push(Point2::new(cx + r * t.cos(), cy + r * t.sin()));
            }
        };
        // Bottom-right corner, then top-right, top-left, bottom-left.
        push_corner(&mut outer, 20.0 - r, r, -std::f64::consts::PI * 0.5);
        push_corner(&mut outer, 20.0 - r, 10.0 - r, 0.0);
        push_corner(&mut outer, r, 10.0 - r, std::f64::consts::PI * 0.5);
        push_corner(&mut outer, r, r, std::f64::consts::PI);
        let region = VcRegion {
            outer,
            holes: Vec::new(),
        };
        let chains_raw = medial_axis(&region);
        let raw_count = chains_raw.len();
        // Pre-pruning, voronator emits dozens of chains (one short
        // spur per densified boundary vertex on the curved corners).
        assert!(
            raw_count > 8,
            "raw medial axis should be hairy with spurs; got {raw_count}",
        );
        // Post-pruning with tool_radius = 8 (min branch length = 4 mm)
        // collapses to a small set — the main spine plus a few
        // corner-region branches survive. With tip_radius = 1 mm any
        // chain whose max inscribed radius is below 1 mm also drops.
        let chains = prune_medial_axis(chains_raw, 8.0, 1.0);
        assert!(
            !chains.is_empty(),
            "pruning should leave at least the main spine",
        );
        assert!(
            chains.len() < raw_count,
            "pruning should drop ≥1 chain (raw {raw_count} → pruned {})",
            chains.len(),
        );
        assert!(
            chains.len() <= 8,
            "rounded rect should prune to a handful of chains, got {}",
            chains.len(),
        );
    }

    /// A chain shorter than `tool_radius * PRUNE_MIN_BRANCH_FACTOR`
    /// is dropped.
    #[test]
    fn prune_drops_short_chain() {
        let chain = vec![
            VPoint {
                x: 0.0,
                y: 0.0,
                r: 0.5,
            },
            VPoint {
                x: 0.1,
                y: 0.0,
                r: 0.5,
            },
        ];
        let pruned = prune_medial_axis(vec![chain], 5.0, 0.0);
        assert!(pruned.is_empty(), "0.1 mm branch should be pruned");
    }

    /// A chain whose max inscribed radius never exceeds the
    /// V-bit's flat tip is dropped (would emit an all-zero-Z toolpath).
    #[test]
    fn prune_drops_chain_below_tip_radius() {
        let mut chain: Vec<VPoint> = Vec::new();
        for i in 0..=20 {
            chain.push(VPoint {
                x: f64::from(i),
                y: 0.0,
                r: 0.4, // shallower than tip 0.5
            });
        }
        let pruned = prune_medial_axis(vec![chain], 1.0, 0.5);
        assert!(pruned.is_empty(), "all-shallow chain should be pruned");
    }

    #[test]
    fn rectangle_axis_runs_along_long_centerline() {
        let outer = vec![
            Point2::new(0.0, 0.0),
            Point2::new(20.0, 0.0),
            Point2::new(20.0, 4.0),
            Point2::new(0.0, 4.0),
        ];
        let region = VcRegion {
            outer,
            holes: Vec::new(),
        };
        let polys = medial_axis(&region);
        assert!(!polys.is_empty());
        // The medial axis of a 20×4 rectangle is the centerline at
        // y = 2 with R = 2 along the straight portion, plus 45° wings
        // at the short ends. The maximum R_inscribed across the axis
        // should be ≈ 2 (the half-width).
        let r_max = polys
            .iter()
            .flat_map(|p| p.iter())
            .map(|v| v.r)
            .fold(0.0_f64, f64::max);
        assert!(
            approx(r_max, 2.0, 0.1),
            "R_max = {r_max} on a 20x4 rectangle, expected ≈ 2.0"
        );
    }

    /// Regression: `chord_stays_in_region` must adapt its sample
    /// density to the chord length. A long chord with a narrow hole
    /// sitting between two fixed-8 sample positions can pass the
    /// in-region check (the hole is invisible to the sampler) — and the
    /// v-carve emitter then draws a chord that crosses the hole.
    #[test]
    fn chord_stays_in_region_catches_narrow_hole_on_long_chord() {
        // Outer ring: 100 × 10 corridor.
        // Hole: a 1 mm wide rectangle centred at x = 50 (i.e. 1/2 of
        //       the chord length). With chord from (0, 5) to (100, 5)
        //       and 8 fixed samples at t ∈ {1/8 … 7/8}, the samples
        //       sit at x = 12.5, 25, 37.5, 50, 62.5, 75, 87.5. One
        //       sample IS at x=50 (the hole centre) so this hole IS
        //       caught even at fixed density. To force the
        //       regression we offset the hole AWAY from any fixed
        //       sample: centred at x=44, width 0.5 mm. Fixed samples
        //       miss it entirely; adaptive density (1 sample
        //       per 0.5 mm ⇒ 200 samples) catches it.
        let outer = vec![
            Point2::new(0.0, 0.0),
            Point2::new(100.0, 0.0),
            Point2::new(100.0, 10.0),
            Point2::new(0.0, 10.0),
        ];
        let hole = vec![
            Point2::new(43.75, 3.0),
            Point2::new(44.25, 3.0),
            Point2::new(44.25, 7.0),
            Point2::new(43.75, 7.0),
        ];
        let region = VcRegion {
            outer,
            holes: vec![hole],
        };
        // Both endpoints sit in the region (y = 5 is in the corridor,
        // and x in (1, 99) is in the outer but far from the hole).
        let p0 = Point2::new(1.0, 5.0);
        let p1 = Point2::new(99.0, 5.0);
        assert!(
            point_in_region(&region, p0),
            "p0 should be in the region (sanity)"
        );
        assert!(
            point_in_region(&region, p1),
            "p1 should be in the region (sanity)"
        );
        // The chord runs along y=5, straight through the hole at x≈44.
        // Pre-fix with 8 fixed samples (x = 12.25, 24.5, 36.75, 49,
        // 61.25, 73.5, 85.75) — NONE of these land in [43.75, 44.25].
        // So the chord was incorrectly declared safe. Post-fix with
        // 1 sample per 0.5 mm (~200 samples spaced ~0.5 mm), at least
        // one sample falls inside the hole and the function returns
        // false.
        let region_index = RegionInsideIndex::new(&region.outer, &region.holes);
        assert!(
            !chord_stays_in_region(&region_index, p0, p1),
            "chord ploughing through a 0.5 mm hole at x=44 must be rejected — no0u regression"
        );
        // Sanity: a chord that AVOIDS the hole (offset in y) IS safe.
        let p0_safe = Point2::new(1.0, 1.0);
        let p1_safe = Point2::new(99.0, 1.0);
        // Both sit inside the outer ring (y=1 is in [0..10]) and
        // outside the hole (y=1 is below y=3, the hole's bottom).
        assert!(point_in_region(&region, p0_safe));
        assert!(point_in_region(&region, p1_safe));
        assert!(
            chord_stays_in_region(&region_index, p0_safe, p1_safe),
            "chord that avoids the hole must be accepted"
        );
    }
}
