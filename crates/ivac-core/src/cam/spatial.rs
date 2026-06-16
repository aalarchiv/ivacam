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
        for cy in (pcy - r + 1)..=(pcy + r - 1) {
            visit(pcx - r, cy, best_sq);
            visit(pcx + r, cy, best_sq);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::is_inside_polygon;

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
}
