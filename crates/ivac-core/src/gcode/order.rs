//! Cut-order selection for offset lists. Honors `Setup::mill::objectorder` (`Unordered` / `Nearest` / `PerObject`).

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    // `min_x`/`min_cx`, `pcx`/`pcy` etc. — point vs cell-coordinate pairs
    // that read clearly in this tight grid arithmetic.
    clippy::similar_names
)]

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::Setup;
use crate::geometry::Point2;

pub(super) fn order_offsets(
    setup: &Setup,
    offsets: &[PolylineOffset],
    start: Point2,
) -> Vec<usize> {
    use crate::project::ObjectOrder;
    let n = offsets.len();
    if n == 0 {
        return Vec::new();
    }
    match setup.mill.objectorder {
        ObjectOrder::Unordered => (0..n).collect(),
        ObjectOrder::Nearest => greedy_nearest(offsets, start),
        ObjectOrder::PerObject => {
            // Group by source_object_idx (preserving first-seen order),
            // run nearest-neighbor inside each group seeded at the
            // previous group's end.
            let mut groups: Vec<Vec<usize>> = Vec::new();
            let mut group_of: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::default();
            for (i, o) in offsets.iter().enumerate() {
                let g = *group_of.entry(o.source_object_idx).or_insert_with(|| {
                    groups.push(Vec::new());
                    groups.len() - 1
                });
                groups[g].push(i);
            }
            let mut out = Vec::with_capacity(n);
            let mut pen = start;
            for group in groups {
                let group_offsets: Vec<&PolylineOffset> =
                    group.iter().map(|&i| &offsets[i]).collect();
                let local = greedy_nearest_among(&group_offsets, pen);
                for li in local {
                    let global = group[li];
                    out.push(global);
                    pen = end_pos(&offsets[global]);
                }
            }
            out
        }
    }
}

pub(super) fn greedy_nearest(offsets: &[PolylineOffset], start: Point2) -> Vec<usize> {
    let refs: Vec<&PolylineOffset> = offsets.iter().collect();
    greedy_nearest_among(&refs, start)
}

/// Above this offset count the O(n²) greedy scan is replaced by a
/// grid-accelerated one. The grid path is byte-identical to the linear
/// one (same comparator, same result), so the threshold is purely a
/// "don't pay grid setup for a handful of holes" cut-off — small jobs
/// (the common case + every fixture-sized test) keep the plain scan.
const GREEDY_GRID_THRESHOLD: usize = 128;

pub(super) fn greedy_nearest_among(offsets: &[&PolylineOffset], start: Point2) -> Vec<usize> {
    let n = offsets.len();
    if n == 0 {
        return Vec::new();
    }
    if n <= GREEDY_GRID_THRESHOLD {
        greedy_nearest_linear(offsets, start)
    } else {
        greedy_nearest_gridded(offsets, start)
    }
}

/// Is candidate `cand` a better next pick than the current `best`?
/// Tie-breakers, in order:
///   1. closer distance wins (only fall through when distances are within
///      tolerance — two computed f64 distances rarely coincide bit-for-
///      bit even at the same nominal point),
///   2. deeper level wins (innermost ring first — pocket cascades unwind
///      inside-out),
///   3. non-finish before finish (the dedicated finish-wall ring runs
///      LAST so surface quality isn't degraded by re-traversing it),
///   4. lower index wins — makes the result order-independent so the
///      grid path matches the linear scan exactly (in the linear scan
///      candidates arrive in index order, so this never flips its
///      result; in the grid path it pins the deterministic winner).
#[inline]
fn offset_better(
    cand_idx: usize,
    cand_d: f64,
    cand_level: u32,
    cand_finish: bool,
    best: Option<(usize, f64, u32, bool)>,
) -> bool {
    match best {
        None => true,
        Some((bi, bd, bl, bf)) => {
            if (cand_d - bd).abs() > 1e-12 {
                cand_d < bd
            } else if cand_level != bl {
                cand_level > bl
            } else if cand_finish != bf {
                !cand_finish
            } else {
                cand_idx < bi
            }
        }
    }
}

/// Classic O(n²) greedy nearest-neighbor. Exact reference order; used
/// directly for small `n` and as the equivalence oracle in tests.
fn greedy_nearest_linear(offsets: &[&PolylineOffset], start: Point2) -> Vec<usize> {
    let n = offsets.len();
    let mut taken = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut pen = start;
    for _ in 0..n {
        let mut best: Option<(usize, f64, u32, bool)> = None;
        for (i, o) in offsets.iter().enumerate() {
            if taken[i] {
                continue;
            }
            let d = pen.distance(start_pos_of(o));
            if offset_better(i, d, o.level, o.is_finish, best) {
                best = Some((i, d, o.level, o.is_finish));
            }
        }
        let (chosen, _, _, _) = best.unwrap();
        taken[chosen] = true;
        order.push(chosen);
        pen = end_pos(offsets[chosen]);
    }
    order
}

/// Grid-accelerated greedy nearest-neighbor for large `n` (drill grids,
/// dense contour sets). A uniform bucket grid over the start points lets
/// each "nearest remaining" step expand rings outward from the pen
/// instead of scanning all remaining offsets, turning the O(n²) scan into
/// ~O(n) amortized. Picked offsets are removed from their cell so later
/// steps don't re-examine them.
///
/// Produces the SAME order as [`greedy_nearest_linear`]: the ring search
/// expands until no unexamined cell could hold a closer (or tie-margin-
/// equal) candidate, then [`offset_better`] picks the exact same winner.
fn greedy_nearest_gridded(offsets: &[&PolylineOffset], start: Point2) -> Vec<usize> {
    let n = offsets.len();
    let starts: Vec<Point2> = offsets.iter().map(|o| start_pos_of(o)).collect();

    // Bounding span → cell size targeting ~1 point per cell per axis.
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for p in &starts {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    let span = (max_x - min_x).max(max_y - min_y).max(1e-6);
    let cells_per_axis = (n as f64).sqrt().clamp(1.0, 2048.0);
    let cell = (span / cells_per_axis).max(1e-6);
    let cell_of =
        |p: Point2| -> (i64, i64) { ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64) };

    let mut grid: std::collections::HashMap<(i64, i64), Vec<u32>> =
        std::collections::HashMap::new();
    let (mut min_cx, mut min_cy, mut max_cx, mut max_cy) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for (i, p) in starts.iter().enumerate() {
        let c = cell_of(*p);
        grid.entry(c).or_default().push(i as u32);
        min_cx = min_cx.min(c.0);
        min_cy = min_cy.min(c.1);
        max_cx = max_cx.max(c.0);
        max_cy = max_cy.max(c.1);
    }

    let mut order = Vec::with_capacity(n);
    let mut pen = start;
    for _ in 0..n {
        let (pcx, pcy) = cell_of(pen);
        let max_r = (pcx - min_cx)
            .abs()
            .max((max_cx - pcx).abs())
            .max((pcy - min_cy).abs())
            .max((max_cy - pcy).abs())
            .max(0)
            + 1;

        let mut best: Option<(usize, f64, u32, bool)> = None;
        let mut r: i64 = 0;
        loop {
            scan_ring(&grid, pcx, pcy, r, pen, &starts, offsets, &mut best);
            if let Some((_, bd, _, _)) = best {
                // Unexamined cells (ring ≥ r+1) sit ≥ r·cell away, so once
                // the best is within r·cell nothing closer remains. Keep a
                // margin ≥ the 1e-12 distance-tie tolerance so any
                // tie-distance candidate that the comparator would prefer
                // on level/finish/index is still examined.
                if (r as f64) * cell > bd + 1e-9 {
                    break;
                }
            }
            if r >= max_r {
                break;
            }
            r += 1;
        }

        let (chosen, _, _, _) = best.expect("an untaken offset must remain");
        if let Some(v) = grid.get_mut(&cell_of(starts[chosen])) {
            v.retain(|&x| x as usize != chosen);
        }
        order.push(chosen);
        pen = end_pos(offsets[chosen]);
    }
    order
}

/// Fold every offset in the cells at exactly Chebyshev distance `r` from
/// `(pcx, pcy)` into `best` via [`offset_better`].
#[allow(clippy::too_many_arguments)]
fn scan_ring(
    grid: &std::collections::HashMap<(i64, i64), Vec<u32>>,
    pcx: i64,
    pcy: i64,
    r: i64,
    pen: Point2,
    starts: &[Point2],
    offsets: &[&PolylineOffset],
    best: &mut Option<(usize, f64, u32, bool)>,
) {
    let scan_cell = |cx: i64, cy: i64, best: &mut Option<(usize, f64, u32, bool)>| {
        if let Some(v) = grid.get(&(cx, cy)) {
            for &iu in v {
                let idx = iu as usize;
                let o = offsets[idx];
                let d = pen.distance(starts[idx]);
                if offset_better(idx, d, o.level, o.is_finish, *best) {
                    *best = Some((idx, d, o.level, o.is_finish));
                }
            }
        }
    };
    if r == 0 {
        scan_cell(pcx, pcy, best);
        return;
    }
    for cx in (pcx - r)..=(pcx + r) {
        scan_cell(cx, pcy - r, best);
        scan_cell(cx, pcy + r, best);
    }
    for cy in (pcy - r + 1)..(pcy + r) {
        scan_cell(pcx - r, cy, best);
        scan_cell(pcx + r, cy, best);
    }
}

pub(super) fn start_pos_of(offset: &PolylineOffset) -> Point2 {
    offset
        .segments
        .first()
        .map_or(Point2::new(0.0, 0.0), |s| s.start)
}

pub(super) fn end_pos(offset: &PolylineOffset) -> Point2 {
    offset
        .segments
        .last()
        .map_or(Point2::new(0.0, 0.0), |s| s.end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Segment;

    struct Lcg(u64);
    impl Lcg {
        fn next_u32(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (self.0 >> 32) as u32
        }
        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (f64::from(self.next_u32()) / f64::from(u32::MAX)) * (hi - lo)
        }
    }

    fn off(start: Point2, end: Point2, level: u32, is_finish: bool) -> PolylineOffset {
        PolylineOffset {
            segments: vec![Segment::line(start, end, "L", 0)],
            closed: false,
            level,
            is_pocket: 0,
            layer: std::sync::Arc::from("L"),
            color: 0,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish,
        }
    }

    /// The grid-accelerated order must be byte-identical to the brute
    /// O(n²) order across many randomized large inputs — including
    /// coincident start points and mixed level / finish flags that
    /// exercise every tie-breaker.
    #[test]
    fn gridded_order_matches_linear_exactly() {
        let mut rng = Lcg(0xC0FF_EE12_3456_789A);
        for trial in 0..30 {
            let n = GREEDY_GRID_THRESHOLD + 1 + (trial * 37) % 600;
            let owned: Vec<PolylineOffset> = (0..n)
                .map(|k| {
                    // Snap some starts to a coarse lattice so distance ties
                    // (and the level/finish/index tie-breakers) actually fire.
                    let sx = (rng.range(0.0, 50.0)).round();
                    let sy = (rng.range(0.0, 50.0)).round();
                    let ex = rng.range(0.0, 50.0);
                    let ey = rng.range(0.0, 50.0);
                    let level = (k % 3) as u32;
                    let is_finish = k % 5 == 0;
                    off(Point2::new(sx, sy), Point2::new(ex, ey), level, is_finish)
                })
                .collect();
            let refs: Vec<&PolylineOffset> = owned.iter().collect();
            let start = Point2::new(rng.range(0.0, 50.0), rng.range(0.0, 50.0));

            let linear = greedy_nearest_linear(&refs, start);
            let gridded = greedy_nearest_gridded(&refs, start);
            assert_eq!(gridded, linear, "grid order diverged from linear at n={n}");

            // Valid permutation: every index exactly once.
            let mut sorted = gridded.clone();
            sorted.sort_unstable();
            assert_eq!(
                sorted,
                (0..n).collect::<Vec<_>>(),
                "grid order dropped/dup'd an index"
            );

            // Deterministic across runs.
            assert_eq!(greedy_nearest_gridded(&refs, start), gridded);
        }
    }

    #[test]
    fn dispatch_threshold_round_trips_small_and_large() {
        let start = Point2::new(0.0, 0.0);
        // Small (linear path) and large (grid path) both yield a valid
        // permutation through the public entry point.
        for n in [1usize, GREEDY_GRID_THRESHOLD, GREEDY_GRID_THRESHOLD + 50] {
            let owned: Vec<PolylineOffset> = (0..n)
                .map(|k| {
                    let x = (k as f64 * 1.3) % 40.0;
                    let y = (k as f64 * 2.7) % 40.0;
                    off(Point2::new(x, y), Point2::new(x + 0.1, y), 0, false)
                })
                .collect();
            let refs: Vec<&PolylineOffset> = owned.iter().collect();
            let order = greedy_nearest_among(&refs, start);
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, (0..n).collect::<Vec<_>>(), "n={n}");
        }
    }
}
