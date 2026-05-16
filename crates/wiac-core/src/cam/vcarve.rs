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

use voronator::delaunator::{triangulate, Point as VPointXy, INVALID_INDEX};

use crate::pipeline::CancelToken;

use crate::cam::{is_inside_polygon, segments_to_points, VcObject};
use crate::geometry::Point2;

/// One medial-axis vertex: (x, y, R_inscribed). The inscribed-circle
/// radius is the distance from the vertex to the nearest boundary
/// sample.
#[derive(Debug, Clone, Copy, PartialEq)]
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

/// Even-odd region test honoring holes — point is inside iff it's
/// inside the outer ring AND outside every hole.
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
pub fn medial_axis(region: &VcRegion) -> Vec<Vec<VPoint>> {
    medial_axis_cancellable(region, None)
}

/// Cancellable wrapper around [`medial_axis`]. Checks `cancel` after
/// every Voronoi-vertex traversal; on cancellation returns whatever
/// chains have been emitted so far (often empty). Callers should also
/// inspect the cancel flag after this call to bail out cleanly.
pub fn medial_axis_cancellable(
    region: &VcRegion,
    cancel: Option<&CancelToken>,
) -> Vec<Vec<VPoint>> {
    let is_cancelled = || cancel.map(|c| c.is_cancelled()).unwrap_or(false);
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

    // A medial-axis vertex is a circumcenter that lies inside the
    // region. Build a parallel list of VPoints aligned to `centers`,
    // each tagged with its true inscribed radius.
    let inside: Vec<Option<VPoint>> = centers
        .iter()
        .map(|c| {
            c.and_then(|p| {
                if point_in_region(region, p) {
                    let r = nearest_boundary_distance(p, &boundary_segments);
                    Some(VPoint { x: p.x, y: p.y, r })
                } else {
                    None
                }
            })
        })
        .collect();

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
        if inside[t0].is_some() && inside[t1].is_some() {
            // Reject the edge between the two triangles' circumcenters
            // if any midpoint along that segment exits the region —
            // protects against thin "narrow neck" cases where the
            // two endpoints sit inside but the chord cuts a hole.
            let p0 = Point2::new(inside[t0].unwrap().x, inside[t0].unwrap().y);
            let p1 = Point2::new(inside[t1].unwrap().x, inside[t1].unwrap().y);
            let mid = Point2::new(0.5 * (p0.x + p1.x), 0.5 * (p0.y + p1.y));
            if !point_in_region(region, mid) {
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
        if inside[start].is_none() || adj[start].len() != 1 {
            continue;
        }
        // Avoid restarting from the other endpoint of an already-walked
        // chain — only start when the single edge is unwalked.
        let nb = adj[start][0];
        if visited_edge.contains(&edge_key(start, nb)) {
            continue;
        }
        let mut chain: Vec<VPoint> = Vec::new();
        chain.push(inside[start].unwrap());
        let mut prev = start;
        let mut cur = nb;
        loop {
            visited_edge.insert(edge_key(prev, cur));
            chain.push(inside[cur].unwrap());
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
    for start in 0..n_tri {
        if is_cancelled() {
            return polylines;
        }
        if inside[start].is_none() {
            continue;
        }
        for &nb in adj[start].clone().iter() {
            if visited_edge.contains(&edge_key(start, nb)) {
                continue;
            }
            let mut chain: Vec<VPoint> = Vec::new();
            chain.push(inside[start].unwrap());
            let mut prev = start;
            let mut cur = nb;
            loop {
                visited_edge.insert(edge_key(prev, cur));
                chain.push(inside[cur].unwrap());
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
                    chain.push(inside[cur].unwrap());
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

/// Convenience builder: turn a closed-VcObject + optional holes into a
/// `VcRegion`. Open objects aren't supported for V-Carve — they have no
/// interior — so they're rejected at the call site (pipeline emits a
/// warning).
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
/// radius around for diagnostics; `z = -r / tan(angle/2)`.
///
/// `tip_angle_rad` is the FULL apex angle of the V cone. `r_cap` clips
/// `r` from above (the `carve_max_width_mm` parameter); `z_cap` clips
/// `|z|` from below (the `OperationParams.depth` parameter — itself a
/// negative number, we treat its absolute value as the limit). Returns
/// `(polyline, depth_limited)` where `depth_limited` is true when at
/// least one point hit the |z| cap.
pub fn polyline_to_z(
    axis: &[VPoint],
    tip_angle_rad: f64,
    r_cap: Option<f64>,
    z_cap: Option<f64>,
) -> (Vec<(f64, f64, f64, f64)>, bool) {
    let tan_half = (tip_angle_rad * 0.5).tan().max(1e-9);
    let mut depth_limited = false;
    let mut out = Vec::with_capacity(axis.len());
    for v in axis {
        let mut r = v.r;
        if let Some(c) = r_cap {
            if r > c {
                r = c;
                depth_limited = true;
            }
        }
        let mut z = -r / tan_half;
        if let Some(c) = z_cap {
            let limit = c.abs();
            if z < -limit {
                z = -limit;
                depth_limited = true;
            }
        }
        out.push((v.x, v.y, z, r));
    }
    (out, depth_limited)
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

/// Minimum perpendicular distance from `p` to any of `segments`. Each
/// segment is `(a, b)`; projection is clamped to `t ∈ [0, 1]` so the
/// distance is to the segment (not the infinite line). Used as the
/// inscribed-circle radius at a medial-axis vertex.
fn nearest_boundary_distance(p: Point2, segments: &[(Point2, Point2)]) -> f64 {
    let mut best_sq = f64::INFINITY;
    for &(a, b) in segments {
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let len_sq = dx * dx + dy * dy;
        let (qx, qy) = if len_sq < 1e-18 {
            (a.x, a.y)
        } else {
            let mut t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / len_sq;
            if t < 0.0 {
                t = 0.0;
            } else if t > 1.0 {
                t = 1.0;
            }
            (a.x + t * dx, a.y + t * dy)
        };
        let ex = p.x - qx;
        let ey = p.y - qy;
        let d_sq = ex * ex + ey * ey;
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
        let (z_poly, _) = polyline_to_z(&polylines[0], tip, None, None);
        let z_min = z_poly.iter().map(|t| t.2).fold(0.0_f64, f64::min);
        // The deepest point along ANY of the three axis segments
        // converges on the incenter; verify the deepest one across
        // all polylines.
        let mut z_min_all = 0.0_f64;
        for poly in &polylines {
            let (zp, _) = polyline_to_z(poly, tip, None, None);
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

    /// Regression for the circumradius-vs-inscribed-radius fix (gjk):
    /// nearest_boundary_distance must measure the perpendicular drop
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

    /// Regression for gjk: at a re-entrant corner the inscribed-circle
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
}
