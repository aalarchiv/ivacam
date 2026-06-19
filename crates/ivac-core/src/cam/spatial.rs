//! Uniform spatial indexes over 2D boundary geometry, built once and
//! queried many times. They turn the V-carve medial-axis inner loop from
//! all-pairs O(B) probes (per circumcenter, against every boundary
//! segment) into ~O(1) amortized lookups — the whole loop drops from
//! O(n_tri · B) ≈ O(B²) to roughly O(n_tri).
//!
//! Two structures, both pure accelerators — they reproduce the exact
//! float / boolean result of the brute-force routines they replace
//! (`nearest_boundary_distance` and `is_inside_polygon` / `point_in_region`),
//! so V-carve output is unchanged within their respective definitions:
//!
//! * [`SegmentNearestGrid`] — nearest distance from a probe point to a
//!   set of line segments (a uniform 2D bucket grid + expanding-ring
//!   search). Equivalence is exact because the minimum over a set is the
//!   same value however the set is traversed, and the per-segment
//!   distance uses the identical arithmetic.
//! * [`PolygonRayIndex`] / [`RegionInsideIndex`] — even-odd point-in-
//!   polygon, bucketing each edge by the grid rows its y-span covers so a
//!   query only visits edges that can straddle the probe's scanline.
//!   Equivalence is exact because the parity flip is commutative and
//!   every straddling edge lands in the probe's row bucket.

// Pedantic lints that are idiomatic for this grid/geometry code and fire
// only under the pre-release `-W clippy::pedantic` gate (the workspace
// allows are overridden there): single-char x/y/r/cx/cy coordinate names,
// a helper `const` placed next to the statement that uses it, and segment
// indices cast to `u32` for the compact grid buckets (a CAM job never
// holds >4 billion segments).
#![allow(
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_possible_truncation
)]

use std::collections::HashMap;

use crate::geometry::Point2;

/// Squared distance from `p` to segment `(a, b)`, clamped to the segment
/// (not the infinite line). Factored out so the brute-force
/// `nearest_boundary_distance` and [`SegmentNearestGrid`] compute
/// bit-identical per-segment distances — the grid is then provably just
/// an acceleration, never a different answer.
#[must_use]
pub fn point_segment_dist_sq(p: Point2, a: Point2, b: Point2) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len_sq = dx * dx + dy * dy;
    let (qx, qy) = if len_sq < 1e-18 {
        (a.x, a.y)
    } else {
        let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len_sq).clamp(0.0, 1.0);
        (a.x + t * dx, a.y + t * dy)
    };
    let ex = p.x - qx;
    let ey = p.y - qy;
    ex * ex + ey * ey
}

/// Map a world coordinate to its integer grid cell. CAM coordinates are
/// mm bounded by stock dimensions (≪ i64 range), so `.floor() as i64`
/// cannot truncate within the supported scale — the same pattern
/// `chaining::cell_of` uses.
#[allow(clippy::cast_possible_truncation)]
fn cell_floor(v: f64, cell: f64) -> i64 {
    (v / cell).floor() as i64
}

/// Pick a cell size that targets a handful of items per cell: roughly
/// one grid cell per item along each axis over the data's bounding span.
/// Correctness is independent of this value (both queries find the true
/// answer regardless); it only trades grid memory against probe fan-out.
fn auto_cell(span: f64, count: usize) -> f64 {
    let cells_per_axis = (count as f64).sqrt().clamp(1.0, 2048.0);
    (span / cells_per_axis).max(1e-6)
}

// ─── nearest-segment grid ───────────────────────────────────────────────

/// Uniform 2D grid over line segments for nearest-distance queries. Each
/// segment is filed under every cell its bounding box overlaps; a query
/// searches the probe's cell and expands ring by ring until no
/// unexamined cell could hold a closer segment.
#[derive(Debug)]
pub struct SegmentNearestGrid {
    cell: f64,
    grid: HashMap<(i64, i64), Vec<u32>>,
    segs: Vec<(Point2, Point2)>,
    min_cell: (i64, i64),
    max_cell: (i64, i64),
}

impl SegmentNearestGrid {
    #[must_use]
    pub fn new(segments: &[(Point2, Point2)]) -> Self {
        let segs = segments.to_vec();
        if segs.is_empty() {
            return Self {
                cell: 1.0,
                grid: HashMap::new(),
                segs,
                min_cell: (0, 0),
                max_cell: (0, 0),
            };
        }
        // Bounding span over every endpoint to size the cell.
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for &(a, b) in &segs {
            for q in [a, b] {
                min_x = min_x.min(q.x);
                min_y = min_y.min(q.y);
                max_x = max_x.max(q.x);
                max_y = max_y.max(q.y);
            }
        }
        let span = (max_x - min_x).max(max_y - min_y).max(1e-6);
        let cell = auto_cell(span, segs.len());

        let mut grid: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
        let (mut min_cell, mut max_cell) = ((i64::MAX, i64::MAX), (i64::MIN, i64::MIN));
        for (i, &(a, b)) in segs.iter().enumerate() {
            let cx0 = cell_floor(a.x.min(b.x), cell);
            let cx1 = cell_floor(a.x.max(b.x), cell);
            let cy0 = cell_floor(a.y.min(b.y), cell);
            let cy1 = cell_floor(a.y.max(b.y), cell);
            for cx in cx0..=cx1 {
                for cy in cy0..=cy1 {
                    grid.entry((cx, cy)).or_default().push(i as u32);
                    min_cell = (min_cell.0.min(cx), min_cell.1.min(cy));
                    max_cell = (max_cell.0.max(cx), max_cell.1.max(cy));
                }
            }
        }
        Self {
            cell,
            grid,
            segs,
            min_cell,
            max_cell,
        }
    }

    /// Minimum distance from `p` to any segment, or `0.0` when there are
    /// no segments — matching the brute-force `nearest_boundary_distance`
    /// fallback. The result equals the brute-force minimum exactly.
    #[must_use]
    pub fn nearest_distance(&self, p: Point2) -> f64 {
        if self.segs.is_empty() {
            return 0.0;
        }
        let pcx = cell_floor(p.x, self.cell);
        let pcy = cell_floor(p.y, self.cell);
        // Largest ring radius that can still reach an occupied cell from
        // the probe; bounds the loop so an empty direction terminates.
        let max_r = (pcx - self.min_cell.0)
            .abs()
            .max((self.max_cell.0 - pcx).abs())
            .max((pcy - self.min_cell.1).abs())
            .max((self.max_cell.1 - pcy).abs())
            .max(0)
            + 1;

        // If the probe sits absurdly far from the occupied cells in
        // cell-units, the ring walk would scan a huge number of empty
        // rings before reaching any segment. That only happens when the
        // grid is mis-sized for this probe (a near-zero-span segment set,
        // or a probe far outside a tiny part) — brute force is then both
        // faster and identical. The threshold is well above any real
        // V-carve probe, which sits within a few hundred cells of the
        // boundary.
        const RING_CAP: i64 = 4096;
        if max_r > RING_CAP {
            return self.brute_force_nearest(p);
        }

        let mut best_sq = f64::INFINITY;
        let mut r: i64 = 0;
        while r <= max_r {
            self.scan_ring(p, pcx, pcy, r, &mut best_sq);
            // Every unexamined segment now lies in a cell at Chebyshev
            // distance ≥ r+1, whose nearest point to `p` is ≥ r·cell
            // away. So once the best found is within r·cell, nothing
            // further can beat it.
            if best_sq.is_finite() {
                let reach = r as f64 * self.cell;
                if best_sq <= reach * reach {
                    break;
                }
            }
            r += 1;
        }
        if best_sq.is_finite() {
            best_sq.sqrt()
        } else {
            0.0
        }
    }

    /// Exact nearest distance by scanning every segment. The fallback
    /// for probes the grid can't serve cheaply; identical result to the
    /// ring walk.
    fn brute_force_nearest(&self, p: Point2) -> f64 {
        let mut best_sq = f64::INFINITY;
        for &(a, b) in &self.segs {
            let d = point_segment_dist_sq(p, a, b);
            if d < best_sq {
                best_sq = d;
            }
        }
        if best_sq.is_finite() {
            best_sq.sqrt()
        } else {
            0.0
        }
    }

    /// Fold every segment in the cells at exactly Chebyshev distance `r`
    /// from `(pcx, pcy)` into `best_sq`.
    fn scan_ring(&self, p: Point2, pcx: i64, pcy: i64, r: i64, best_sq: &mut f64) {
        let visit = |cx: i64, cy: i64, best_sq: &mut f64| {
            if let Some(list) = self.grid.get(&(cx, cy)) {
                for &si in list {
                    let (a, b) = self.segs[si as usize];
                    let d = point_segment_dist_sq(p, a, b);
                    if d < *best_sq {
                        *best_sq = d;
                    }
                }
            }
        };
        if r == 0 {
            visit(pcx, pcy, best_sq);
            return;
        }
        // Top and bottom rows of the ring (full width).
        for cx in (pcx - r)..=(pcx + r) {
            visit(cx, pcy - r, best_sq);
            visit(cx, pcy + r, best_sq);
        }
        // Left and right columns (excluding the corners already done).
        for cy in (pcy - r + 1)..(pcy + r) {
            visit(pcx - r, cy, best_sq);
            visit(pcx + r, cy, best_sq);
        }
    }
}

// ─── ray-vs-segment first-hit grid ──────────────────────────────────────

/// Uniform 2D grid for outward-ray probes against a set of line segments,
/// built for `cam::offsets::parallel::apply_overcut`'s reflex-corner walk.
///
/// `apply_overcut` casts a ray from each reflex corner of an offset
/// polyline and finds the nearest boundary hit, where "hit" is the
/// minimum over an exact per-segment test (endpoint projection within a
/// `perp_tol` corridor, plus a Cramer ray-vs-edge intersection with a
/// small `u`-slack). Brute force runs that test against every boundary
/// segment — O(corners · boundary).
///
/// This index files each segment under every cell its bounding box —
/// inflated by `margin` — overlaps, then [`collect_candidates`] walks the
/// ray's cells (a DDA / Amanatides-Woo traversal) and returns the segments
/// filed in them. The caller re-runs its *identical* per-segment
/// arithmetic over that set, so the result is bit-identical to the
/// brute-force scan: the returned set is a **superset** of every segment
/// that brute force would find a passing candidate for (see
/// [`collect_candidates`] for the coverage argument), and the minimum over
/// a superset — computed with the same arithmetic — is the same value, as
/// extra non-minimal candidates can never lower it.
///
/// Early "stop at the first hit" termination is deliberately *not* done
/// here: it would couple the walk to the caller's running minimum and the
/// `perp_tol` corridor in a way that's easy to get subtly wrong, and the
/// asymptotic win already comes from visiting only the ray-local cells
/// (~O(√boundary) per corner) rather than all segments.
///
/// [`collect_candidates`]: SegmentRayGrid::collect_candidates
#[derive(Debug)]
pub struct SegmentRayGrid {
    cell: f64,
    grid: HashMap<(i64, i64), Vec<u32>>,
    min_cell: (i64, i64),
    max_cell: (i64, i64),
    n: usize,
}

impl SegmentRayGrid {
    /// Build the grid over `segments` (each a `(start, end)` chord),
    /// inflating every segment's footprint by `margin` so a ray that
    /// passes within `margin` of an endpoint — or whose Cramer hit lies up
    /// to `margin` past an edge end — still lands the segment in a cell the
    /// ray crosses. `margin` should be the caller's `perp_tol` (the same
    /// corridor half-width its endpoint test uses).
    #[must_use]
    pub fn new(segments: &[(Point2, Point2)], margin: f64) -> Self {
        let n = segments.len();
        if n == 0 {
            return Self {
                cell: 1.0,
                grid: HashMap::new(),
                min_cell: (0, 0),
                max_cell: (0, 0),
                n,
            };
        }
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for &(a, b) in segments {
            for q in [a, b] {
                min_x = min_x.min(q.x);
                min_y = min_y.min(q.y);
                max_x = max_x.max(q.x);
                max_y = max_y.max(q.y);
            }
        }
        let span = (max_x - min_x).max(max_y - min_y).max(1e-6);
        let cell = auto_cell(span, n);
        let m = margin.max(0.0);

        let mut grid: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
        let (mut min_cell, mut max_cell) = ((i64::MAX, i64::MAX), (i64::MIN, i64::MIN));
        for (i, &(a, b)) in segments.iter().enumerate() {
            let cx0 = cell_floor(a.x.min(b.x) - m, cell);
            let cx1 = cell_floor(a.x.max(b.x) + m, cell);
            let cy0 = cell_floor(a.y.min(b.y) - m, cell);
            let cy1 = cell_floor(a.y.max(b.y) + m, cell);
            for cx in cx0..=cx1 {
                for cy in cy0..=cy1 {
                    grid.entry((cx, cy)).or_default().push(i as u32);
                    min_cell = (min_cell.0.min(cx), min_cell.1.min(cy));
                    max_cell = (max_cell.0.max(cx), max_cell.1.max(cy));
                }
            }
        }
        Self {
            cell,
            grid,
            min_cell,
            max_cell,
            n,
        }
    }

    /// Fill `out` (cleared first) with the de-duplicated indices of every
    /// segment filed in a cell the ray `(origin, dir)` passes through,
    /// walked from the origin until the ray leaves the grid's occupied
    /// region. `dir` need not be unit-length (only the cells the ray
    /// crosses matter, which are scale-invariant).
    ///
    /// Coverage (why the result is a superset of brute force's hits): any
    /// segment brute force scores has a "foot" point ON the ray — the
    /// projection foot for an endpoint candidate, or the intersection point
    /// for a Cramer candidate — at the candidate's distance along the ray.
    /// That foot lies within `margin` of the segment's true footprint
    /// (≤ `perp_tol` laterally for an endpoint within the corridor; ≤ the
    /// `u`-slack ⋅ edge-length ≤ `perp_tol` for a Cramer hit just past an
    /// edge end), so the segment is filed in the foot's cell. The walk
    /// visits every cell the ray crosses inside the occupied bbox, so it
    /// visits the foot's cell and collects the segment.
    pub fn collect_candidates(&self, origin: Point2, dir: (f64, f64), out: &mut Vec<u32>) {
        out.clear();
        if self.n == 0 || (dir.0 == 0.0 && dir.1 == 0.0) {
            return;
        }
        let (dx, dy) = dir;
        let mut cx = cell_floor(origin.x, self.cell);
        let mut cy = cell_floor(origin.y, self.cell);

        // Amanatides-Woo setup. `t_max_*` is the ray parameter at which the
        // walk next crosses a cell boundary on that axis; `t_delta_*` is
        // the parameter step between successive crossings. A zero
        // direction component never crosses on that axis (t_max = ∞).
        let (step_x, mut t_max_x, t_delta_x) = axis_setup(origin.x, dx, cx, self.cell);
        let (step_y, mut t_max_y, t_delta_y) = axis_setup(origin.y, dy, cy, self.cell);

        // Iteration backstop: the ray can cross at most width+height cell
        // boundaries inside the bbox plus a small approach margin. If a
        // float pathology blows past that, fall back to the full set — a
        // safe (if slow) superset — rather than risk truncating early.
        let width = (self.max_cell.0 - self.min_cell.0).max(0);
        let height = (self.max_cell.1 - self.min_cell.1).max(0);
        let cap = (width + height) * 2 + 64;
        let mut iters: i64 = 0;

        loop {
            if let Some(list) = self.grid.get(&(cx, cy)) {
                out.extend_from_slice(list);
            }
            iters += 1;
            if iters > cap {
                // Pathological walk — return everything (still a superset).
                out.clear();
                out.extend(0..self.n as u32);
                return;
            }
            // Step to the next cell along the ray.
            if t_max_x < t_max_y {
                cx += step_x;
                t_max_x += t_delta_x;
            } else {
                cy += step_y;
                t_max_y += t_delta_y;
            }
            // The ray is straight, so once a coordinate passes the occupied
            // bbox in the travel direction it can never re-enter — stop.
            if (step_x > 0 && cx > self.max_cell.0)
                || (step_x < 0 && cx < self.min_cell.0)
                || (step_y > 0 && cy > self.max_cell.1)
                || (step_y < 0 && cy < self.min_cell.1)
                || (step_x == 0 && (cx < self.min_cell.0 || cx > self.max_cell.0))
                || (step_y == 0 && (cy < self.min_cell.1 || cy > self.max_cell.1))
            {
                break;
            }
        }
        out.sort_unstable();
        out.dedup();
    }
}

/// Per-axis Amanatides-Woo init: returns `(step, t_max, t_delta)` for the
/// ray component `d` starting at world coord `o` in cell `c` (cell size
/// `cell`). A zero component yields `(0, ∞, ∞)` — that axis never advances.
fn axis_setup(o: f64, d: f64, c: i64, cell: f64) -> (i64, f64, f64) {
    if d > 0.0 {
        let next_boundary = (c + 1) as f64 * cell;
        ((1), (next_boundary - o) / d, cell / d)
    } else if d < 0.0 {
        let boundary = c as f64 * cell;
        ((-1), (boundary - o) / d, -cell / d)
    } else {
        (0, f64::INFINITY, f64::INFINITY)
    }
}

// ─── point-in-polygon by scanline row buckets ──────────────────────────

/// Even-odd point-in-polygon accelerator. Each polygon edge is filed
/// under every grid row (`cell_floor(y, cell)`) its y-extent covers; a
/// query only walks the edges in the probe's row, which are the only ones
/// that can straddle its horizontal scanline. Reproduces
/// [`crate::geometry::is_inside_polygon`] exactly (same crossing formula,
/// same `< 3` short-circuit).
#[derive(Debug)]
pub struct PolygonRayIndex {
    cell: f64,
    rows: HashMap<i64, Vec<(Point2, Point2)>>,
    n_points: usize,
}

impl PolygonRayIndex {
    #[must_use]
    pub fn new(points: &[Point2]) -> Self {
        let n_points = points.len();
        if n_points < 3 {
            return Self {
                cell: 1.0,
                rows: HashMap::new(),
                n_points,
            };
        }
        let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
        for q in points {
            min_y = min_y.min(q.y);
            max_y = max_y.max(q.y);
        }
        let cell = auto_cell((max_y - min_y).max(1e-6), n_points);

        let mut rows: HashMap<i64, Vec<(Point2, Point2)>> = HashMap::new();
        let n = n_points;
        let mut j = n - 1;
        for i in 0..n {
            // Match is_inside_polygon's edge orientation exactly: edge is
            // (pi, pj) with pj the previous vertex.
            let pi = points[i];
            let pj = points[j];
            let r0 = cell_floor(pi.y.min(pj.y), cell);
            let r1 = cell_floor(pi.y.max(pj.y), cell);
            for row in r0..=r1 {
                rows.entry(row).or_default().push((pi, pj));
            }
            j = i;
        }
        Self {
            cell,
            rows,
            n_points,
        }
    }

    /// `true` iff `p` is inside the polygon by the even-odd rule — the
    /// same value [`crate::geometry::is_inside_polygon`] returns.
    #[must_use]
    pub fn contains(&self, p: Point2) -> bool {
        if self.n_points < 3 {
            return false;
        }
        let row = cell_floor(p.y, self.cell);
        let Some(edges) = self.rows.get(&row) else {
            return false;
        };
        let mut inside = false;
        for &(pi, pj) in edges {
            let crosses_y = (pi.y > p.y) != (pj.y > p.y);
            if crosses_y {
                let x_at = pi.x + (p.y - pi.y) * (pj.x - pi.x) / (pj.y - pi.y);
                if p.x < x_at {
                    inside = !inside;
                }
            }
        }
        inside
    }
}

/// Even-odd region test honoring holes, accelerated. Inside iff inside
/// the outer ring AND outside every hole — the same definition
/// `vcarve::point_in_region` uses, edge for edge.
#[derive(Debug)]
pub struct RegionInsideIndex {
    outer: PolygonRayIndex,
    holes: Vec<PolygonRayIndex>,
}

impl RegionInsideIndex {
    #[must_use]
    pub fn new(outer: &[Point2], holes: &[Vec<Point2>]) -> Self {
        Self {
            outer: PolygonRayIndex::new(outer),
            holes: holes.iter().map(|h| PolygonRayIndex::new(h)).collect(),
        }
    }

    #[must_use]
    pub fn contains(&self, p: Point2) -> bool {
        if !self.outer.contains(p) {
            return false;
        }
        !self.holes.iter().any(|h| h.contains(p))
    }
}

// ─── boundary-robust point-in-polygon by scanline row buckets ───────────

/// Boundary-robust (epsilon) point-in-polygon accelerator. Reproduces
/// [`crate::geometry::point_in_polygon`] exactly — the variant the
/// offset / pocket / trochoidal guards rely on, distinct from the plain
/// no-epsilon [`PolygonRayIndex`] above (which mirrors `is_inside_polygon`).
///
/// Same row-bucket idea: each edge `(verts[i], verts[i+1 % n])` is filed
/// under every grid row its y-extent covers, and a query walks only the
/// edges in the probe's row. Correctness is by construction — the query
/// applies `point_in_polygon`'s exact per-edge arithmetic (the
/// `1e-12`-slop y-range test and the `xi > x` crossing flip), so filing
/// an edge under *extra* rows is harmless (the formula simply skips it);
/// the only requirement is that no edge a probe could cross is ever
/// missed. The y-range an edge is active over is `[lo.y - 1e-12,
/// hi.y - 1e-12)`, so it is filed over `cell_floor(lo.y - 1e-12)
/// ..= cell_floor(hi.y)` — a safe superset of the rows any such probe
/// can land in.
#[derive(Debug)]
pub struct PolygonRayIndexEps {
    cell: f64,
    rows: HashMap<i64, Vec<(Point2, Point2)>>,
    n_points: usize,
}

impl PolygonRayIndexEps {
    /// Slop used by [`crate::geometry::point_in_polygon`]'s y-range test;
    /// kept identical here so the bucket range can't drop an active edge.
    const Y_EPS: f64 = 1e-12;

    #[must_use]
    pub fn new(verts: &[Point2]) -> Self {
        let n_points = verts.len();
        if n_points < 3 {
            return Self {
                cell: 1.0,
                rows: HashMap::new(),
                n_points,
            };
        }
        let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
        for q in verts {
            min_y = min_y.min(q.y);
            max_y = max_y.max(q.y);
        }
        let cell = auto_cell((max_y - min_y).max(1e-6), n_points);

        let mut rows: HashMap<i64, Vec<(Point2, Point2)>> = HashMap::new();
        let n = n_points;
        for i in 0..n {
            // Edge orientation matches point_in_polygon: (verts[i],
            // verts[i+1 mod n]), the closing edge implied.
            let a = verts[i];
            let b = verts[(i + 1) % n];
            let lo_y = a.y.min(b.y);
            let hi_y = a.y.max(b.y);
            // Active over [lo_y - Y_EPS, hi_y - Y_EPS); file the superset
            // [cell_floor(lo_y - eps), cell_floor(hi_y)].
            let r0 = cell_floor(lo_y - Self::Y_EPS, cell);
            let r1 = cell_floor(hi_y, cell);
            for row in r0..=r1 {
                rows.entry(row).or_default().push((a, b));
            }
        }
        Self {
            cell,
            rows,
            n_points,
        }
    }

    /// `true` iff `(x, y)` is inside by the boundary-robust even-odd rule —
    /// the same value [`crate::geometry::point_in_polygon`] returns.
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        if self.n_points < 3 {
            return false;
        }
        let row = cell_floor(y, self.cell);
        let Some(edges) = self.rows.get(&row) else {
            return false;
        };
        let mut inside = false;
        for &(a, b) in edges {
            if (a.y - b.y).abs() < Self::Y_EPS {
                continue;
            }
            let (lo, hi) = if a.y < b.y { (a, b) } else { (b, a) };
            if y < lo.y - Self::Y_EPS || y >= hi.y - Self::Y_EPS {
                continue;
            }
            let t = (y - lo.y) / (hi.y - lo.y);
            let xi = lo.x + t * (hi.x - lo.x);
            if xi > x {
                inside = !inside;
            }
        }
        inside
    }
}

#[cfg(test)]
mod tests {
    // These tests assert the indexed result equals the brute-force routine
    // EXACTLY — exact float equality is the invariant under test, not a
    // tolerance, so float_cmp is intentional here.
    #![allow(clippy::float_cmp)]
    use super::*;
    use crate::geometry::{is_inside_polygon, point_in_polygon};

    /// Tiny deterministic LCG so the randomized sweeps are reproducible
    /// (and don't pull in a dev-dependency).
    struct Lcg(u64);
    impl Lcg {
        fn next_u32(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (self.0 >> 32) as u32
        }
        /// Uniform f64 in `[lo, hi)`.
        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (f64::from(self.next_u32()) / f64::from(u32::MAX)) * (hi - lo)
        }
    }

    /// Brute-force nearest distance — the oracle the grid must match.
    fn brute_nearest(p: Point2, segs: &[(Point2, Point2)]) -> f64 {
        let mut best = f64::INFINITY;
        for &(a, b) in segs {
            best = best.min(point_segment_dist_sq(p, a, b));
        }
        if best.is_finite() {
            best.sqrt()
        } else {
            0.0
        }
    }

    #[test]
    fn nearest_grid_matches_brute_force_exactly() {
        let mut rng = Lcg(0x1234_5678_9abc_def0);
        for trial in 0..40 {
            // Random short segments (boundary-sample scale) over a patch.
            let n = 5 + (trial % 60);
            let segs: Vec<(Point2, Point2)> = (0..n)
                .map(|_| {
                    let ax = rng.range(0.0, 100.0);
                    let ay = rng.range(0.0, 100.0);
                    (
                        Point2::new(ax, ay),
                        Point2::new(ax + rng.range(-1.0, 1.0), ay + rng.range(-1.0, 1.0)),
                    )
                })
                .collect();
            let grid = SegmentNearestGrid::new(&segs);
            for _ in 0..200 {
                // Probe both inside the patch and well outside it.
                let p = Point2::new(rng.range(-20.0, 120.0), rng.range(-20.0, 120.0));
                assert_eq!(
                    grid.nearest_distance(p),
                    brute_nearest(p, &segs),
                    "grid nearest must equal brute force exactly at {p:?}"
                );
            }
        }
    }

    #[test]
    fn nearest_grid_empty_is_zero() {
        let grid = SegmentNearestGrid::new(&[]);
        assert_eq!(grid.nearest_distance(Point2::new(3.0, 4.0)), 0.0);
    }

    #[test]
    fn nearest_grid_handles_degenerate_zero_length_segment() {
        let segs = [(Point2::new(5.0, 5.0), Point2::new(5.0, 5.0))];
        let grid = SegmentNearestGrid::new(&segs);
        let p = Point2::new(8.0, 9.0);
        assert_eq!(grid.nearest_distance(p), brute_nearest(p, &segs));
    }

    /// Dense probe sweep over a polygon's bbox, asserting the indexed
    /// inside-test matches `is_inside_polygon` cell for cell — catches
    /// scanline / vertex-on-ray edge cases the row bucketing must
    /// preserve.
    fn assert_polygon_index_matches(poly: &[Point2]) {
        let idx = PolygonRayIndex::new(poly);
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for q in poly {
            min_x = min_x.min(q.x);
            min_y = min_y.min(q.y);
            max_x = max_x.max(q.x);
            max_y = max_y.max(q.y);
        }
        let steps = 73;
        for i in 0..=steps {
            for j in 0..=steps {
                let p = Point2::new(
                    min_x - 2.0 + (max_x - min_x + 4.0) * f64::from(i) / f64::from(steps),
                    min_y - 2.0 + (max_y - min_y + 4.0) * f64::from(j) / f64::from(steps),
                );
                assert_eq!(
                    idx.contains(p),
                    is_inside_polygon(poly, p),
                    "polygon index disagrees with is_inside_polygon at {p:?}"
                );
            }
        }
    }

    #[test]
    fn polygon_index_matches_is_inside_polygon() {
        // Square.
        assert_polygon_index_matches(&[
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(10.0, 10.0),
            Point2::new(0.0, 10.0),
        ]);
        // Non-convex L-shape.
        assert_polygon_index_matches(&[
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(10.0, 4.0),
            Point2::new(4.0, 4.0),
            Point2::new(4.0, 10.0),
            Point2::new(0.0, 10.0),
        ]);
        // Concave star-ish polygon with shared scanline y-values.
        assert_polygon_index_matches(&[
            Point2::new(5.0, 0.0),
            Point2::new(6.5, 3.5),
            Point2::new(10.0, 3.5),
            Point2::new(7.0, 6.0),
            Point2::new(8.5, 10.0),
            Point2::new(5.0, 7.5),
            Point2::new(1.5, 10.0),
            Point2::new(3.0, 6.0),
            Point2::new(0.0, 3.5),
            Point2::new(3.5, 3.5),
        ]);
    }

    #[test]
    fn polygon_index_degenerate_under_three_points() {
        let idx = PolygonRayIndex::new(&[Point2::new(0.0, 0.0), Point2::new(1.0, 1.0)]);
        assert!(!idx.contains(Point2::new(0.5, 0.5)));
    }

    /// Dense sweep asserting the epsilon index matches `point_in_polygon`
    /// cell for cell, plus an extra sweep along every vertex y-value
    /// (where the `1e-12` slop in the y-range test bites) so the row
    /// bucketing can't drop an edge active only within that epsilon band.
    fn assert_polygon_eps_index_matches(poly: &[Point2]) {
        let idx = PolygonRayIndexEps::new(poly);
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for q in poly {
            min_x = min_x.min(q.x);
            min_y = min_y.min(q.y);
            max_x = max_x.max(q.x);
            max_y = max_y.max(q.y);
        }
        let steps = 73;
        for i in 0..=steps {
            for j in 0..=steps {
                let x = min_x - 2.0 + (max_x - min_x + 4.0) * f64::from(i) / f64::from(steps);
                let y = min_y - 2.0 + (max_y - min_y + 4.0) * f64::from(j) / f64::from(steps);
                assert_eq!(
                    idx.contains(x, y),
                    point_in_polygon(poly, x, y),
                    "eps index disagrees with point_in_polygon at ({x}, {y})"
                );
            }
        }
        // Probe AT and just-around each vertex y across the x range —
        // the scanline values the epsilon y-range test is sensitive to.
        for v in poly {
            for &dy in &[-2e-12, -1e-12, 0.0, 1e-12, 2e-12] {
                let y = v.y + dy;
                for i in 0..=steps {
                    let x = min_x - 2.0 + (max_x - min_x + 4.0) * f64::from(i) / f64::from(steps);
                    assert_eq!(
                        idx.contains(x, y),
                        point_in_polygon(poly, x, y),
                        "eps index disagrees at vertex-y scanline ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn polygon_eps_index_matches_point_in_polygon() {
        // Square.
        assert_polygon_eps_index_matches(&[
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(10.0, 10.0),
            Point2::new(0.0, 10.0),
        ]);
        // Non-convex L-shape.
        assert_polygon_eps_index_matches(&[
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(10.0, 4.0),
            Point2::new(4.0, 4.0),
            Point2::new(4.0, 10.0),
            Point2::new(0.0, 10.0),
        ]);
        // Concave star-ish polygon with shared scanline y-values.
        assert_polygon_eps_index_matches(&[
            Point2::new(5.0, 0.0),
            Point2::new(6.5, 3.5),
            Point2::new(10.0, 3.5),
            Point2::new(7.0, 6.0),
            Point2::new(8.5, 10.0),
            Point2::new(5.0, 7.5),
            Point2::new(1.5, 10.0),
            Point2::new(3.0, 6.0),
            Point2::new(0.0, 3.5),
            Point2::new(3.5, 3.5),
        ]);
    }

    #[test]
    fn polygon_eps_index_degenerate_under_three_points() {
        let idx = PolygonRayIndexEps::new(&[Point2::new(0.0, 0.0), Point2::new(1.0, 1.0)]);
        assert!(!idx.contains(0.5, 0.5));
    }

    #[test]
    fn region_index_matches_outer_minus_holes() {
        let outer = vec![
            Point2::new(0.0, 0.0),
            Point2::new(20.0, 0.0),
            Point2::new(20.0, 20.0),
            Point2::new(0.0, 20.0),
        ];
        let hole = vec![
            Point2::new(6.0, 6.0),
            Point2::new(14.0, 6.0),
            Point2::new(14.0, 14.0),
            Point2::new(6.0, 14.0),
        ];
        let holes = vec![hole.clone()];
        let idx = RegionInsideIndex::new(&outer, &holes);
        let steps = 80;
        for i in 0..=steps {
            for j in 0..=steps {
                let p = Point2::new(
                    -2.0 + 24.0 * f64::from(i) / f64::from(steps),
                    -2.0 + 24.0 * f64::from(j) / f64::from(steps),
                );
                let reference = is_inside_polygon(&outer, p) && !is_inside_polygon(&hole, p);
                assert_eq!(idx.contains(p), reference, "region index mismatch at {p:?}");
            }
        }
    }

    // ── SegmentRayGrid: the outward-ray first-hit probe ──────────────────

    /// The exact per-segment arithmetic `apply_overcut` runs over each
    /// candidate boundary segment, computing the nearest `along` distance
    /// of the ray `(origin, dir)` (dir unit) — replicated here verbatim so
    /// the test can score the brute-force full scan and the grid's
    /// candidate subset with the *identical* float ops. The grid is a
    /// pure accelerator iff these two scores match exactly.
    fn probe_nearest(
        origin: Point2,
        dir: (f64, f64),
        perp_tol: f64,
        segs: &[(Point2, Point2)],
        indices: &[u32],
    ) -> Option<f64> {
        let mut nearest: Option<f64> = None;
        let mut consider = |along: f64| {
            if along <= 1e-6 {
                return;
            }
            if nearest.map_or(true, |c| along < c) {
                nearest = Some(along);
            }
        };
        for &si in indices {
            let (a, b) = segs[si as usize];
            for p1 in [a, b] {
                let dx = p1.x - origin.x;
                let dy = p1.y - origin.y;
                let along = dx * dir.0 + dy * dir.1;
                if along <= 1e-6 {
                    continue;
                }
                let perp = (dx * dir.1 - dy * dir.0).abs();
                if perp <= perp_tol {
                    consider(along);
                }
            }
            let ex = b.x - a.x;
            let ey = b.y - a.y;
            let det = dir.0 * (-ey) - dir.1 * (-ex);
            if det.abs() < 1e-12 {
                continue;
            }
            let rhs0 = a.x - origin.x;
            let rhs1 = a.y - origin.y;
            let t = (rhs0 * (-ey) - rhs1 * (-ex)) / det;
            let u = (dir.0 * rhs1 - dir.1 * rhs0) / det;
            if (-1e-3..=1.0 + 1e-3).contains(&u) {
                consider(t);
            }
        }
        nearest
    }

    #[test]
    fn ray_grid_matches_full_scan_exactly() {
        let mut rng = Lcg(0x0bad_f00d_dead_beef);
        for trial in 0..60 {
            // A ring of short boundary chords around the origin patch, the
            // shape of a real offset boundary.
            let n = 8 + (trial % 80);
            let span = 100.0;
            let segs: Vec<(Point2, Point2)> = (0..n)
                .map(|_| {
                    let ax = rng.range(0.0, span);
                    let ay = rng.range(0.0, span);
                    (
                        Point2::new(ax, ay),
                        Point2::new(ax + rng.range(-3.0, 3.0), ay + rng.range(-3.0, 3.0)),
                    )
                })
                .collect();
            // perp_tol as apply_overcut derives it: 1e-3 × bbox diagonal.
            let perp_tol = (span.hypot(span) * 1e-3).max(1e-3);
            let all: Vec<u32> = (0..n as u32).collect();
            let grid = SegmentRayGrid::new(&segs, perp_tol);

            let mut cands = Vec::new();
            for _ in 0..300 {
                let origin =
                    Point2::new(rng.range(-20.0, span + 20.0), rng.range(-20.0, span + 20.0));
                // Random unit direction.
                let ang = rng.range(0.0, std::f64::consts::TAU);
                let dir = (ang.cos(), ang.sin());
                grid.collect_candidates(origin, dir, &mut cands);
                let by_grid = probe_nearest(origin, dir, perp_tol, &segs, &cands);
                let by_scan = probe_nearest(origin, dir, perp_tol, &segs, &all);
                assert_eq!(
                    by_grid, by_scan,
                    "ray-grid probe must equal full scan exactly: origin={origin:?} dir={dir:?}"
                );
            }
        }
    }

    #[test]
    fn ray_grid_candidates_are_a_superset_of_hit_segments() {
        // Axis-aligned ray straight at a known wall: the wall segment must
        // appear in the collected candidates.
        let segs = vec![
            (Point2::new(10.0, -5.0), Point2::new(10.0, 5.0)), // vertical wall at x=10
            (Point2::new(-5.0, 20.0), Point2::new(5.0, 20.0)), // far horizontal wall
        ];
        let grid = SegmentRayGrid::new(&segs, 0.05);
        let mut cands = Vec::new();
        grid.collect_candidates(Point2::new(0.0, 0.0), (1.0, 0.0), &mut cands);
        assert!(
            cands.contains(&0),
            "ray toward x=10 wall must collect it: {cands:?}"
        );
    }

    #[test]
    fn ray_grid_empty_and_zero_dir_are_safe() {
        let grid = SegmentRayGrid::new(&[], 0.1);
        let mut cands = vec![7, 8, 9];
        grid.collect_candidates(Point2::new(0.0, 0.0), (1.0, 0.0), &mut cands);
        assert!(cands.is_empty(), "empty grid yields no candidates");

        let segs = vec![(Point2::new(1.0, 1.0), Point2::new(2.0, 2.0))];
        let grid = SegmentRayGrid::new(&segs, 0.1);
        grid.collect_candidates(Point2::new(0.0, 0.0), (0.0, 0.0), &mut cands);
        assert!(cands.is_empty(), "zero direction yields no candidates");
    }
}
