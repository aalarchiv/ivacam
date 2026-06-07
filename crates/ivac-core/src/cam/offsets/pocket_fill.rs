//! Pocket fill — zigzag / spiral raster fills and the per-object pocket
//! orchestrator `pocket_for_object`, plus the segment-intersection and
//! ring-stitch / bridge helpers they use. Split out of `offsets.rs` (6yst).
//! Owns the zigzag-stride and nocontour-allowance diagnostic sinks (drained
//! by the parent's `OffsetDiagnostics`). Offset primitives it builds on
//! (parallel_offset_object, pocket_cascade_with_islands, …) live in the
//! sibling `parallel` module, reached via `super::` (re-exported there).

use super::{
    over_inflate_islands_for_high_overlap, parallel_offset_inward, small_circle_drill,
    PolylineOffset,
};
use crate::cam::{segments_to_points, VcObject};
use crate::geometry::{point_in_polygon, Point2, Segment};

/// 0tsy: `pocket_for_object` records this when the caller sets
/// `nocontour=true` together with a non-zero `xy_allowance`. Those flags
/// are mutually exclusive in practice: no wall ring means there's no
/// dedicated finish pass to consume the allowance, so the rough cascade
/// would otherwise leave `allowance` mm of stock on every wall (the
/// part would come out undersized). The function folds allowance back
/// to 0 in that case and records this entry so the pipeline can surface
/// a `nocontour_ignores_finish_allowance` warning attributed to the op.
#[derive(Debug, Clone, Copy)]
pub struct NocontourAllowanceIgnored {
    pub allowance_mm: f64,
}

thread_local! {
    static NOCONTOUR_ALLOWANCE_IGNORED: std::cell::RefCell<Vec<NocontourAllowanceIgnored>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain (and clear) any `NocontourAllowanceIgnored` events stashed by
/// `pocket_for_object` on this thread.
#[must_use]
pub(super) fn take_nocontour_allowance_ignored() -> Vec<NocontourAllowanceIgnored> {
    NOCONTOUR_ALLOWANCE_IGNORED.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// cpym: recorded when [`pocket_zigzag`] bails because the requested
/// stride is degenerate (≤ 1e-6 mm, non-finite, or NaN). The pipeline
/// drains this via [`take_zigzag_stride_degenerate`] and emits a
/// `zigzag_stride_clamped_below_minimum` warning attributed to the op.
/// Pre-cpym the stride was silently clamped to 0.1 mm and the user got
/// coarser scallops than requested with no signal.
#[derive(Debug, Clone, Copy)]
pub struct ZigzagStrideDegenerate {
    pub stride_mm: f64,
}

thread_local! {
    static ZIGZAG_STRIDE_DEGENERATE: std::cell::RefCell<Vec<ZigzagStrideDegenerate>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain (and clear) any `ZigzagStrideDegenerate` records stashed by
/// [`pocket_zigzag`] on this thread.
#[must_use]
pub(super) fn take_zigzag_stride_degenerate() -> Vec<ZigzagStrideDegenerate> {
    ZIGZAG_STRIDE_DEGENERATE.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// Generate a zigzag (raster) pocket fill within `boundary`. The fill is
/// a series of horizontal sweep lines at the given Y `stride`, each
/// segment trimmed to the polygon's interior. Adjacent strokes are
/// joined at their endpoints to form a single open polyline (returns a
/// chain of segments). `stride` is the lateral distance between
/// consecutive raster lines — typically `tool_diameter * (1 - overlap)`.
/// `tool_diameter` is needed separately to inset the rasters by half a
/// tool diameter from the polygon edges so the cutter doesn't carve
/// past the boundary.
///
/// rt1.9: angled raster wrapper around `pocket_zigzag` (see below).
/// Rotates the boundary by `-angle_deg` around its bbox centre, runs
/// the axis-aligned zigzag, then rotates the emitted segments back by
/// `+angle_deg`. Identity short-circuits when `angle_deg.abs() < 1e-9`
/// so the 0° case has no additional cost.
///
/// gp2a: `islands` are closed contours pre-inflated by `tool_radius`
/// (matches the `pocket_cascade_with_islands` contract). Every scanline
/// is split at island crossings so the cutter lifts over raised
/// features instead of ploughing straight through them. Returns
/// `Vec<Vec<Segment>>` — one inner Vec per connected sub-chain. The
/// caller wraps each in its own `PolylineOffset` so the gcode emitter
/// inserts a real lift / rapid / re-plunge between island-split
/// sub-chains.
#[must_use]
pub fn pocket_zigzag_angled(
    boundary: &[Point2],
    islands: &[Vec<Point2>],
    stride: f64,
    tool_diameter: f64,
    angle_deg: f64,
) -> Vec<Vec<Segment>> {
    let a = angle_deg.rem_euclid(180.0);
    if a.abs() < 1e-9 {
        return pocket_zigzag(boundary, islands, stride, tool_diameter);
    }
    // Pivot: bbox centre of the input boundary.
    let (min_x, max_x, min_y, max_y) = boundary.iter().fold(
        (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ),
        |(lx, hx, ly, hy), p| (lx.min(p.x), hx.max(p.x), ly.min(p.y), hy.max(p.y)),
    );
    let pivot = Point2::new((min_x + max_x) * 0.5, (min_y + max_y) * 0.5);
    let rad = a.to_radians();
    let (cos, sin) = (rad.cos(), rad.sin());
    let rotate = |p: Point2, sign: f64| -> Point2 {
        let dx = p.x - pivot.x;
        let dy = p.y - pivot.y;
        let s = sign * sin;
        Point2::new(pivot.x + dx * cos - dy * s, pivot.y + dx * s + dy * cos)
    };
    let rotated: Vec<Point2> = boundary.iter().map(|p| rotate(*p, -1.0)).collect();
    let rotated_islands: Vec<Vec<Point2>> = islands
        .iter()
        .map(|isl| isl.iter().map(|p| rotate(*p, -1.0)).collect())
        .collect();
    let mut chains = pocket_zigzag(&rotated, &rotated_islands, stride, tool_diameter);
    for chain in &mut chains {
        for s in chain.iter_mut() {
            s.start = rotate(s.start, 1.0);
            s.end = rotate(s.end, 1.0);
        }
    }
    chains
}

/// Generate a zigzag (raster) pocket fill within `boundary`, splitting
/// each scanline stroke at every `island` crossing so raised features
/// are left uncut.
///
/// gp2a: prior to this fix the function ignored islands entirely and
/// the cutter ploughed straight through any island that fell across a
/// scanline (a P1 correctness bug — the user's "leave this raised"
/// feature was silently gouged out). Each scanline's even-odd crossings
/// against the outer boundary are intersected with each island's
/// crossings to produce "in-pocket but outside-every-island"
/// sub-strokes. Whenever the cutter would have to skip across an
/// island to reach the next stroke, the current chain ends and a new
/// chain begins — the caller emits each chain as its own
/// `PolylineOffset` so the gcode lifts to clearance between them.
// Length budget waived: per-row scanline plus per-stroke island
// subtraction plus per-stroke chain-break logic read top-to-bottom as
// one state machine; splitting would scatter the island-interval /
// split-mark tracking across helpers.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn pocket_zigzag(
    boundary: &[Point2],
    islands: &[Vec<Point2>],
    stride: f64,
    tool_diameter: f64,
) -> Vec<Vec<Segment>> {
    if boundary.len() < 3 || stride <= 0.0 {
        return Vec::new();
    }
    // cpym: previously clamped stride.max(0.1) silently, so a 0.05 mm
    // mirror-finish raster was bumped to 0.1 mm — user-set scallop
    // bounds went unenforced and the only diagnosis was measuring the
    // finished part. The zigzag algorithm tolerates arbitrarily small
    // strides (it just emits more rows); only a strictly zero / NaN
    // stride is degenerate. We bail to the no-strokes path for sub-fp
    // sizes and stash a thread-local record so the pipeline driver can
    // surface a `zigzag_stride_clamped_below_minimum` warning rather
    // than burying the toolpath silently.
    if !stride.is_finite() || stride < 1e-6 {
        ZIGZAG_STRIDE_DEGENERATE.with(|s| {
            s.borrow_mut()
                .push(ZigzagStrideDegenerate { stride_mm: stride });
        });
        return Vec::new();
    }
    let (min_y, max_y) = boundary
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.y), hi.max(p.y))
        });
    let (min_x, max_x) = boundary
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.x), hi.max(p.x))
        });

    let mut chains: Vec<Vec<Segment>> = Vec::new();
    let mut current: Vec<Segment> = Vec::new();
    let mut prev_end: Option<Point2> = None;
    let mut flip = false;
    let tool_r = tool_diameter * 0.5;
    let mut y = min_y + tool_r;
    while y <= max_y - tool_r + 1e-9 {
        let outer = horizontal_crossings(boundary, y, min_x, max_x);
        // Per-island crossings at this Y. Each island's crossings come
        // in even-odd pairs (entry/exit of the island interior). We
        // subtract those interior intervals from the outer-boundary
        // intervals below.
        let mut island_intervals: Vec<(f64, f64)> = Vec::new();
        for isl in islands {
            if isl.len() < 3 {
                continue;
            }
            let xs = horizontal_crossings(isl, y, f64::NEG_INFINITY, f64::INFINITY);
            for pair in xs.chunks_exact(2) {
                let lo = pair[0].min(pair[1]);
                let hi = pair[0].max(pair[1]);
                if hi > lo + 1e-9 {
                    island_intervals.push((lo, hi));
                }
            }
        }

        // Group outer crossings into entry/exit pairs (even-odd rule);
        // each pair is a candidate stroke trimmed to the pocket's
        // interior. We then subtract island intervals from each
        // candidate to get the final cut sub-strokes.
        let mut strokes: Vec<(Point2, Point2)> = Vec::new();
        let mut split_marks: Vec<bool> = Vec::new(); // true ⇒ this stroke was preceded by an island gap on this row
        for pair in outer.chunks_exact(2) {
            let (a, b) = (pair[0], pair[1]);
            // Inset both ends by half a tool diameter so we don't carve
            // outside the polygon interior at the row endpoints. The
            // inset is clamped to half the stroke length so a narrow
            // crossing collapses to a single point rather than going
            // negative.
            let lo = a.min(b);
            let hi = a.max(b);
            let inset = tool_r.min((hi - lo) * 0.5);
            let new_a = lo + inset;
            let new_b = hi - inset;
            if new_b <= new_a + 1e-6 {
                continue;
            }
            // Subtract every island clearance band from [new_a, new_b].
            // The islands handed in here are already pre-inflated by
            // the tool radius (matches the cascade contract), so we use
            // their crossings as-is — re-applying tool_r would
            // double-inflate.
            let mut sub: Vec<(f64, f64)> = vec![(new_a, new_b)];
            for (ilo, ihi) in &island_intervals {
                let cut_lo = *ilo;
                let cut_hi = *ihi;
                let mut next: Vec<(f64, f64)> = Vec::with_capacity(sub.len() + 1);
                for &(sa, sb) in &sub {
                    if cut_hi <= sa + 1e-9 || cut_lo >= sb - 1e-9 {
                        next.push((sa, sb));
                        continue;
                    }
                    if cut_lo > sa + 1e-6 {
                        next.push((sa, cut_lo));
                    }
                    if cut_hi < sb - 1e-6 {
                        next.push((cut_hi, sb));
                    }
                    // Otherwise the entire [sa, sb] is swallowed by
                    // the island's clearance band — emit nothing.
                }
                sub = next;
                if sub.is_empty() {
                    break;
                }
            }
            // First sub-stroke from this outer-pair extends the previous
            // chain; subsequent sub-strokes (created by island splits)
            // start a new chain.
            for (i, (sa, sb)) in sub.iter().enumerate() {
                if *sb > *sa + 1e-6 {
                    strokes.push((Point2::new(*sa, y), Point2::new(*sb, y)));
                    split_marks.push(i > 0);
                }
            }
        }
        if flip {
            strokes.reverse();
            split_marks.reverse();
            for s in &mut strokes {
                std::mem::swap(&mut s.0, &mut s.1);
            }
        }
        // a7v4: only flip parity if the current row actually emitted a
        // stroke. Empty rows (single-vertex polygon point, scanline
        // tangent to a corner, every interval swallowed by an island)
        // used to flip anyway — when the next non-empty row arrived it
        // ran in the "wrong" direction, doubling cutter travel between
        // rows and breaking the serpent topology in places where the
        // user could see it.
        let row_emitted = !strokes.is_empty();
        if row_emitted {
            flip = !flip;
        }
        for ((a, b), force_break) in strokes.into_iter().zip(split_marks.into_iter()) {
            let mut needs_break = force_break;
            // Cross-row bridge sanity: when prev_end is on one side of
            // an island and `a` is on the other side, joining them with
            // a straight cut would cross the island. Break instead.
            // axhd: also break when the joiner LEAVES the outer pocket
            // boundary — non-convex outer shapes (U, +, donut) can put
            // two strokes on the same scanline that belong to disjoint
            // arms; a straight line between them ploughs across uncut
            // stock (the cross-bar of the U, the corner of the L).
            if !needs_break {
                if let Some(prev) = prev_end {
                    if prev.distance(a) > 1e-6 {
                        let crosses_island =
                            !islands.is_empty() && segment_crosses_any_polygon(prev, a, islands);
                        let leaves_outer = !bridge_stays_inside_polygon(prev, a, boundary);
                        if crosses_island || leaves_outer {
                            needs_break = true;
                        }
                    }
                }
            }
            if needs_break {
                if !current.is_empty() {
                    chains.push(std::mem::take(&mut current));
                }
                prev_end = None;
            }
            if let Some(prev) = prev_end {
                if prev.distance(a) > 1e-6 {
                    current.push(Segment::line(prev, a, "0", 7));
                }
            }
            current.push(Segment::line(a, b, "0", 7));
            prev_end = Some(b);
        }
        y += stride;
    }
    if !current.is_empty() {
        chains.push(current);
    }
    chains
}

/// True iff the open segment (a, b) crosses the boundary of ANY of the
/// polygons (or has its midpoint inside one). Used by the islands-aware
/// zigzag to detect cross-row bridges that would gouge a raised island.
fn segment_crosses_any_polygon(a: Point2, b: Point2, polys: &[Vec<Point2>]) -> bool {
    for poly in polys {
        if poly.len() < 3 {
            continue;
        }
        // Sample the open segment; if any interior sample lies inside
        // the polygon, the bridge crosses it.
        let samples = 8;
        for i in 1..samples {
            let t = f64::from(i) / f64::from(samples);
            let px = a.x + (b.x - a.x) * t;
            let py = a.y + (b.y - a.y) * t;
            if point_in_polygon(poly, px, py) {
                return true;
            }
        }
        // Edge-to-edge intersection: a bridge can clip a corner of the
        // island even when no sample lands inside (skinny island).
        if segment_intersects_polygon_edges(a, b, poly) {
            return true;
        }
    }
    false
}

fn segment_intersects_polygon_edges(a: Point2, b: Point2, poly: &[Point2]) -> bool {
    let n = poly.len();
    for i in 0..n {
        let c = poly[i];
        let d = poly[(i + 1) % n];
        if segments_intersect(a, b, c, d) {
            return true;
        }
    }
    false
}

fn segments_intersect(p1: Point2, p2: Point2, p3: Point2, p4: Point2) -> bool {
    let d1 = orient(p3, p4, p1);
    let d2 = orient(p3, p4, p2);
    let d3 = orient(p1, p2, p3);
    let d4 = orient(p1, p2, p4);
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true;
    }
    false
}

fn orient(a: Point2, b: Point2, c: Point2) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

pub(super) fn horizontal_crossings(poly: &[Point2], y: f64, min_x: f64, max_x: f64) -> Vec<f64> {
    let mut xs = Vec::new();
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        // Pure horizontal edge: skip; handled by neighbors.
        if (a.y - b.y).abs() < 1e-12 {
            continue;
        }
        let (lo, hi) = if a.y < b.y { (a, b) } else { (b, a) };
        // Half-open interval: [lo.y, hi.y) so we don't double-count
        // corner crossings.
        if y < lo.y - 1e-12 || y >= hi.y - 1e-12 {
            continue;
        }
        let t = (y - lo.y) / (hi.y - lo.y);
        let x = lo.x + t * (hi.x - lo.x);
        if x >= min_x - 1e-3 && x <= max_x + 1e-3 {
            xs.push(x);
        }
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // c6ej: collapse coincident crossings whose x values are within a
    // FUZZY-equivalent tolerance. A scanline that just grazes a vertex
    // produces TWO crossings at the same x (one per adjacent edge) when
    // both edges share that vertex as their lower endpoint — a local-min
    // tangent. With the half-open `[lo.y, hi.y)` rule, a monotone-through
    // shared vertex yields one crossing and a local-max yields none, so a
    // coincident run is exactly a tangent: it touches the boundary without
    // a net inside/outside toggle. Such crossings must be removed in PAIRS
    // (not collapsed to one — that both breaks parity AND plants a spurious
    // fill boundary at the vertex). An even run vanishes; an odd run is a
    // genuinely degenerate input that leaves one crossing (and trips the
    // odd-count warning below) rather than being dropped silently.
    if xs.len() >= 2 {
        let snap_tol = 1e-3_f64;
        let mut dedup = Vec::with_capacity(xs.len());
        let mut i = 0;
        while i < xs.len() {
            let mut j = i + 1;
            while j < xs.len() && (xs[j] - xs[i]).abs() <= snap_tol {
                j += 1;
            }
            // Keep one crossing only when the coincident run has odd length
            // (a residual real crossing); pure tangent pairs cancel out.
            if (j - i) % 2 == 1 {
                dedup.push(xs[i]);
            }
            i = j;
        }
        xs = dedup;
    }
    if xs.len() % 2 == 1 {
        // The snap pass couldn't bring the count to even (genuinely
        // degenerate input — e.g. an open contour or self-intersecting
        // ring). Surface a warning so the user sees uncut stock instead
        // of shipping silently. Pocket emitters skip the trailing
        // unpaired crossing for THIS scanline only.
        tracing::warn!(
            "horizontal_crossings: odd crossing count {} at y = {:.3}; trailing crossing dropped (degenerate polygon? open contour?)",
            xs.len(),
            y
        );
        xs.pop();
    }
    xs
}

/// Combine a parallel-offset boundary pass with an inward cascade. Returns
/// the boundary ring first (if `nocontour=false`), then progressively-inward
/// pocket rings. When `zigzag` is true the inward cascade is replaced with
/// a single back-and-forth raster fill (one open polyline per offset).
///
/// `islands` are closed contours that should be left uncut inside the
/// pocket (e.g. raised features). Each island gets pre-inflated by the
/// tool radius before being subtracted from the cascade.
///
/// Special case: if `obj` is a single CIRCLE/POINT segment with radius
/// smaller than the tool radius, we can't carve a pocket — emit a drill
/// at center instead (a zero-length cut that the gcode emitter will turn
/// into a plunge).
/// Pocket emission strategy. Chosen by the caller based on the user's
/// `PocketStrategy` setting. Cascade emits N concentric closed rings;
/// Zigzag emits a back-and-forth raster fill; Spiral threads the
/// cascade rings into ONE continuous open polyline so the cutter never
/// lifts between rings (cleaner finish, slightly faster than cascade).
/// Trochoidal walks a stitched centerline (the cascade rings, joined)
/// while looping the cutter on small circles to bound radial engagement
/// — used for hard-material stock removal on hobby-rigidity machines.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PocketEmit {
    Cascade,
    /// Raster fill. rt1.9: `angle_deg` rotates the sweep direction — 0 =
    /// horizontal sweeps (original behaviour), 90 = vertical, 45 =
    /// diagonal, etc. Wrap-around is at 180° (the algorithm is
    /// direction-symmetric).
    Zigzag {
        angle_deg: f64,
    },
    Spiral,
    Trochoidal {
        engagement_angle_deg: f64,
        loop_radius_factor: f64,
        /// True = climb (CCW loops). False = conventional (CW loops).
        climb: bool,
    },
}

// Pocket-for-object computes the full inward cascade for a single source
// VcObject: parametrisation passes through depth / step / tabs / overcut /
// finish-radius / dual-tool. The geometric pipeline reads linearly.
//
// q57s: `spindle` is forwarded into `pocket_trochoidal` so the loop
// winding flips on left-hand spindles. Cascade / Spiral / Zigzag are
// unaffected — their winding is fixed up later by `enforce_winding`,
// which gets the spindle direction via `apply_cut_direction`.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn pocket_for_object(
    obj: &VcObject,
    tool_radius: f64,
    nocontour: bool,
    interpolate: usize,
    emit: PocketEmit,
    islands: &[Vec<Point2>],
    xy_step: f64,
    xy_allowance: f64,
    finish_ring_radius: Option<f64>,
    spindle: crate::project::tool::SpindleDirection,
) -> Vec<PolylineOffset> {
    let mut out = Vec::new();

    if let Some(drill) = small_circle_drill(obj, tool_radius) {
        out.push(drill);
        return out;
    }

    // Always go inward regardless of the source polygon's winding —
    // CW DXF boundaries used to take the cutter OUTSIDE the shape
    // because positive delta = LEFT of tangent for cavc, which is
    // outward on CW polygons. parallel_offset_inward picks the sign.
    //
    // Schlichtzugabe / xy_allowance (rt1.24): when > 0, the rough
    // cascade walks an INSET boundary at (tool_radius + allowance)
    // — that leaves `allowance` mm of stock at the wall — and a
    // dedicated finish ring at tool_radius removes it. allowance == 0
    // collapses both rings onto the tool_radius offset (current
    // behavior).
    //
    // Dual-tool / finish_ring_radius (rt1.33): when Some(r), the
    // dedicated finish wall ring is emitted at radius `r` (the FINISH
    // tool's radius) instead of `tool_radius`. The pipeline op-driver
    // splits the offsets list by is_finish and emits the finish block
    // with the finish tool's setup after an auto toolchange. When None,
    // the finish ring (if any — see allowance) uses `tool_radius`.
    // 0tsy: `nocontour=true` means there will be no wall ring — neither
    // the rough boundary (skipped below) nor the dedicated finish ring
    // (gated on `!nocontour && needs_finish_ring`). A non-zero XY
    // allowance is therefore meaningless: the rough cascade would walk
    // an extra `allowance` inboard and the finish ring that should
    // remove that stock never runs, so every wall comes out undersized
    // by `allowance`. Fold allowance back to 0 in this case and record
    // a `nocontour_ignores_finish_allowance` event so the pipeline
    // surfaces a warning attributed to the op. The dual-tool finish
    // ring is its own pass and stays untouched — when the user provides
    // an explicit finish_ring_radius, the cascade still walks at
    // tool_radius and the finish-radius ring runs below.
    let raw_allowance = xy_allowance.max(0.0);
    let has_dual_tool_finish = finish_ring_radius.is_some();
    let allowance = if nocontour && raw_allowance > 1e-9 && !has_dual_tool_finish {
        NOCONTOUR_ALLOWANCE_IGNORED.with(|s| {
            s.borrow_mut().push(NocontourAllowanceIgnored {
                allowance_mm: raw_allowance,
            });
        });
        0.0
    } else {
        raw_allowance
    };
    let rough_delta = tool_radius.abs() + allowance;
    let boundary = parallel_offset_inward(obj, rough_delta);
    if boundary.is_empty() {
        return out;
    }
    // Effective step in mm: lateral distance between consecutive cuts.
    // The caller passes the step (typically tool_diameter * (1 - overlap));
    // we clamp to a safe minimum so a 100% overlap doesn't loop forever.
    let step = xy_step.max(tool_radius * 0.05);
    // sbtf: islands handed in are already knd4-inflated by tool_radius
    // (cutter-centerline safe boundary). When the cascade per-pass step
    // is SMALLER than tool_radius (high overlap, e.g. 80% engagement ⇒
    // step ≈ 0.2·tool_r), the cascade's first inward step around the
    // island lands the cutter centerline only `step` away from the raw
    // island wall — the cutter edge intrudes by `tool_r − step`. Apply
    // an EXTRA outward inflation of `max(0, tool_r − step)` for the
    // cascade / spiral paths so the first ring keeps clearance. Zigzag
    // /trochoidal stay on the bare knd4 inflation; zigzag's per-row
    // inset is already tool_r and trochoidal's loop disc enforces its
    // own engagement bound.
    let cascade_islands = over_inflate_islands_for_high_overlap(islands, tool_radius, step);
    for offset in &boundary {
        if !nocontour {
            // When there's no XY allowance AND no dual-tool finish,
            // the rough boundary IS the wall — tag it as finish so
            // emit_offset swaps in the tool's finish-set rates
            // (rt1.27). When allowance > 0 OR dual-tool, a dedicated
            // finish ring is emitted below and the rough boundary
            // stays rough.
            let mut wall = offset.clone();
            wall.is_finish = allowance <= 1e-9 && !has_dual_tool_finish;
            out.push(wall);
        }
        let pts = segments_to_points(&offset.segments, interpolate);

        match emit {
            PocketEmit::Zigzag { angle_deg } => {
                // Zigzag stride is the same step semantics — distance
                // between raster lines. Default ~50% overlap.
                // rt1.9: angle_deg rotates the raster direction. We
                // implement it by rotating the boundary into a frame
                // where the sweep is horizontal, running the existing
                // pocket_zigzag, then rotating the output back. Pivot is
                // the boundary's bbox centre — keeps the result on the
                // same canvas.
                // gp2a: pocket_zigzag now respects islands and returns
                // one chain per connected sub-region (a chain ends
                // wherever a row gets chopped by an island). Each chain
                // becomes a separate PolylineOffset so the gcode emitter
                // lifts to clearance and re-plunges between sub-chains
                // — instead of cutting straight through the island.
                // 06m5: when nocontour=true there's no wall ring laid down,
                // so pocket_zigzag's self-inset by tool_r would leave a
                // tool_radius-wide ribbon of uncut stock along every wall
                // (the boundary `pts` is already inset by tool_r — a
                // second inset stacks). Pass tool_diameter=0 in that case
                // so the outermost stroke reaches the inset edge. When
                // there IS a wall ring, the self-inset is needed: the
                // ring already touches the inset edge and the raster
                // strokes should sit a tool_r inboard so they don't
                // overlap the wall.
                let zigzag_tool_d = if nocontour { 0.0 } else { tool_radius * 2.0 };
                // cpym: pre-fix this wrapper clamped step to 0.1 mm
                // before handing to pocket_zigzag, defeating fine-finish
                // strides (e.g. 0.05 mm mirror finish). pocket_zigzag now
                // tolerates arbitrarily small finite strides and records
                // a ZigzagStrideDegenerate event only when the stride is
                // truly non-finite or below the FP working precision —
                // the pipeline drains that into a user-visible warning.
                let chains = pocket_zigzag_angled(&pts, islands, step, zigzag_tool_d, angle_deg);
                for strokes in chains {
                    if strokes.is_empty() {
                        continue;
                    }
                    out.push(PolylineOffset {
                        segments: strokes,
                        closed: false,
                        level: 1,
                        is_pocket: 1,
                        layer: offset.layer.clone(),
                        color: offset.color,
                        source_object_idx: offset.source_object_idx,
                        tabs: Vec::new(),
                        is_finish: false,
                    });
                }
                continue;
            }
            PocketEmit::Spiral => {
                // Spiral: thread the cascade rings into ONE continuous
                // open polyline. Each ring's start point is rotated so
                // it's nearest to the previous ring's end point; that
                // shortens the bridge segment between rings and gives
                // the path a natural "spiral inward" shape. Approximates
                // an Archimedean spiral well enough for pocket clearing.
                //
                // sbtf: pass the over-inflated islands so the first ring
                // doesn't intrude when step < tool_radius (high overlap).
                let rings = crate::cam::geometry_cache::pocket_cascade_with_islands_cached(
                    &pts,
                    &cascade_islands,
                    step,
                );
                if rings.is_empty() {
                    continue;
                }
                // Containment guard (bd w91): straight bridges between
                // consecutive cascade rings can cross a re-entrant pocket
                // wall on non-convex shapes (L / U / +). The outer ring
                // (rings[0] = inset boundary) defines the safe interior;
                // every bridge must stay inside it. If any bridge fails
                // the test we abandon spiral and let the caller fall back
                // to cascade emission, which doesn't cut bridges.
                //
                // sbtf: the stitcher tests bridges against the islands
                // it walks around — pass the same over-inflated set the
                // rings were generated against so the safe-bridge check
                // sees the same geometry.
                match stitch_rings_to_spiral(&rings, &cascade_islands, &offset.layer, offset.color)
                {
                    Some(segs) if !segs.is_empty() => {
                        out.push(PolylineOffset {
                            segments: segs,
                            closed: false,
                            level: 1,
                            is_pocket: 2,
                            layer: offset.layer.clone(),
                            color: offset.color,
                            source_object_idx: offset.source_object_idx,
                            tabs: Vec::new(),
                            is_finish: false,
                        });
                        continue;
                    }
                    _ => {
                        tracing::debug!(
                            "spiral pocket: bridge crosses pocket boundary or island, falling back to cascade (no bridges)"
                        );
                        // Fall through to cascade emission below using
                        // the rings we already computed.
                    }
                }
            }
            PocketEmit::Trochoidal {
                engagement_angle_deg,
                loop_radius_factor,
                climb,
            } => {
                if let Some(segs) = crate::cam::trochoidal::pocket_trochoidal(
                    &pts,
                    islands,
                    tool_radius,
                    engagement_angle_deg,
                    loop_radius_factor,
                    climb,
                    &offset.layer,
                    offset.color,
                    spindle,
                ) {
                    if !segs.is_empty() {
                        out.push(PolylineOffset {
                            segments: segs,
                            closed: false,
                            level: 1,
                            is_pocket: 2,
                            layer: offset.layer.clone(),
                            color: offset.color,
                            source_object_idx: offset.source_object_idx,
                            tabs: Vec::new(),
                            is_finish: false,
                        });
                    }
                }
                continue;
            }
            PocketEmit::Cascade => {}
        }

        // sbtf: cascade emission uses the over-inflated island set so the
        // first ring keeps clearance from raw island walls when
        // step < tool_radius (high overlap). The over-inflation is a
        // no-op when step >= tool_radius (matches the pre-sbtf path).
        let rings = crate::cam::geometry_cache::pocket_cascade_with_islands_cached(
            &pts,
            &cascade_islands,
            step,
        );
        // No silent fallback to zigzag here: the user picked cascade or
        // spiral explicitly, and substituting zigzag when no ring fits
        // is surprising. The pocket_fill_incomplete warning fires
        // instead, telling the user the tool is too wide and to pick a
        // smaller tool or switch to zigzag explicitly.
        for (i, ring) in rings.iter().enumerate() {
            if ring.len() < 2 {
                continue;
            }
            let mut segs = Vec::with_capacity(ring.len());
            for win in ring.windows(2) {
                segs.push(Segment::line(
                    win[0],
                    win[1],
                    offset.layer.clone(),
                    offset.color,
                ));
            }
            // Close the ring.
            if let (Some(first), Some(last)) = (ring.first(), ring.last()) {
                if first.distance(*last) > 1e-6 {
                    segs.push(Segment::line(
                        *last,
                        *first,
                        offset.layer.clone(),
                        offset.color,
                    ));
                }
            }
            out.push(PolylineOffset {
                segments: segs,
                closed: true,
                level: (i + 1) as u32,
                is_pocket: 2,
                layer: offset.layer.clone(),
                color: offset.color,
                source_object_idx: offset.source_object_idx,
                tabs: Vec::new(),
                is_finish: false,
            });
        }
    }
    // rt1.24 / rt1.33: emit a dedicated finish-wall pass when either
    // an XY allowance is set (single-tool finishing pass) or a
    // dual-tool finish radius is set (smaller tool walks the wall).
    // The dual-tool branch uses `finish_ring_radius`; the single-tool
    // branch uses `tool_radius`.
    let needs_finish_ring = allowance > 1e-9 || has_dual_tool_finish;
    if !nocontour && needs_finish_ring {
        let finish_r = finish_ring_radius.map_or_else(|| tool_radius.abs(), f64::abs);
        let finish_boundary = parallel_offset_inward(obj, finish_r);
        for o in &finish_boundary {
            let mut wall = o.clone();
            wall.is_finish = true;
            out.push(wall);
        }
    }
    out
}

/// Thread cascade rings into one continuous spiral polyline. For each
/// ring after the first: rotate the ring's start so it's nearest to
/// the previous ring's end point, walk the ring forward, repeat. The
/// bridge between rings is a straight Line segment (the cutter steps
/// inward by ~one `xy_step`).
///
/// Returns None when a bridge between rings would cross the pocket
/// boundary or any island — happens on non-convex shapes (L / U / +)
/// where a straight bridge can leave the safe interior, or on pockets
/// containing islands where a bridge can plough straight through a
/// raised feature. The caller should fall back to cascade emission
/// (separate closed rings, no bridges).
fn stitch_rings_to_spiral(
    rings: &[Vec<Point2>],
    islands: &[Vec<Point2>],
    layer: &str,
    color: i32,
) -> Option<Vec<Segment>> {
    let pts = stitch_rings_to_polyline(rings, islands)?;
    let mut out: Vec<Segment> = Vec::with_capacity(pts.len().saturating_sub(1));
    for w in pts.windows(2) {
        if w[0].distance(w[1]) > 1e-9 {
            out.push(Segment::line(w[0], w[1], layer, color));
        }
    }
    Some(out)
}

/// Stitch cascade rings into one continuous polyline of points (the
/// shared core of `stitch_rings_to_spiral` and the trochoidal
/// centerline). Rotates each ring's start vertex to be nearest the
/// previous ring's end so bridges between rings are short.
///
/// Bridge containment is the safety guard: a bridge must (a) stay
/// inside the outer ring (the inset pocket boundary), and (b) NOT
/// cross any island. Returns None on any violation; the caller falls
/// back to cascade emission (separate closed rings, no bridges).
///
/// kqsl: prior to this fix islands were ignored — a bridge could carve
/// straight through a raised feature on pockets-with-islands. The
/// per-bridge check now considers every island polygon too.
///
/// kc86: with ≥3 rings the closest start vertex on ring N+1 to ring
/// N's end can produce a bridge that grazes an inflated island sitting
/// between ring N+1 and ring N+2 — the closest vertex isn't always the
/// safest. We now sweep ALL vertices in the next ring, ordered by
/// distance from the previous end, and pick the first one whose bridge
/// passes the containment + island guard. Only after every candidate
/// fails do we fall back to cascade emission. This recovers spiral
/// emission on deep pockets with multiple islands where the prior
/// "first candidate wins" approach silently degraded the toolpath.
pub(crate) fn stitch_rings_to_polyline(
    rings: &[Vec<Point2>],
    islands: &[Vec<Point2>],
) -> Option<Vec<Point2>> {
    if rings.is_empty() {
        return Some(Vec::new());
    }
    let outer = &rings[0];
    let mut out: Vec<Point2> = Vec::new();
    let mut last_end: Option<Point2> = None;
    for (idx, ring) in rings.iter().enumerate() {
        if ring.len() < 3 {
            // hx74: a ring with fewer than 3 points can't be traversed
            // meaningfully. Pre-fix we silently `continue`d past it,
            // leaving a bridge gap (the next ring's first vertex was
            // stitched to the PREVIOUS ring's last vertex across the
            // dropped ring, often jumping far enough to cross islands
            // / leave the pocket). Now: if it's the first ring we
            // can't establish a starting vertex — fail. If it's a
            // later ring, the cascade emitted a degenerate result
            // (likely clipper2 collapsed a sliver) — also fail, the
            // caller falls back to ring-cascade emission (no bridges).
            if idx == 0 {
                return None;
            }
            return None;
        }
        let n = ring.len();
        let start_idx = if let Some(end) = last_end {
            // kc86: rank candidate start vertices by distance from the
            // previous ring's end, then sweep until the bridge guard
            // passes. The closest vertex is tried first (short bridge
            // = the original heuristic); when that bridge would cross
            // the outer ring or an island we slide to the next-best,
            // and so on. We fall through to None only when EVERY
            // vertex on this ring yields an unsafe bridge — only then
            // is cascade fallback truly the right answer.
            let mut ranked: Vec<(usize, f64)> = ring
                .iter()
                .enumerate()
                .map(|(i, p)| (i, p.distance(end)))
                .collect();
            ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            let mut chosen: Option<usize> = None;
            for (cand, _) in &ranked {
                let cand_pt = ring[*cand];
                // Zero-length bridge always passes — same ring boundary,
                // no straight segment to test.
                if end.distance(cand_pt) <= 1e-6 {
                    chosen = Some(*cand);
                    break;
                }
                if bridge_stays_inside_polygon(end, cand_pt, outer)
                    && !bridge_crosses_any_island(end, cand_pt, islands)
                {
                    chosen = Some(*cand);
                    break;
                }
            }
            chosen?
        } else {
            0
        };
        let first = ring[start_idx];
        if let Some(end) = last_end {
            if end.distance(first) > 1e-6 {
                // Bridge already validated against outer + islands during
                // the start_idx sweep above; just emit the vertex.
                out.push(first);
            }
        } else {
            out.push(first);
        }
        for k in 1..=n {
            let p = ring[(start_idx + k) % n];
            if let Some(prev) = out.last() {
                if prev.distance(p) > 1e-9 {
                    out.push(p);
                }
            }
        }
        last_end = Some(first);
    }
    Some(out)
}

/// Sample along the bridge segment (a, b) and verify every interior
/// sample lies inside the polygon. The endpoint a typically sits
/// exactly on `polygon` (it's the ring start vertex from a cascade
/// ring), so we skip it under the half-open ray-cast convention by
/// sampling at strictly interior parameters t ∈ (0, 1). 8 samples is
/// enough to catch a bridge crossing through a re-entrant corner of a
/// reasonable pocket; the failure mode this guards against is a
/// straight line that exits and re-enters the polygon, which spans a
/// finite arc inside the gap.
pub(crate) fn bridge_stays_inside_polygon(a: Point2, b: Point2, polygon: &[Point2]) -> bool {
    if polygon.len() < 3 {
        return true;
    }
    let samples = 8;
    for i in 1..samples {
        let t = f64::from(i) / f64::from(samples);
        let px = a.x + (b.x - a.x) * t;
        let py = a.y + (b.y - a.y) * t;
        if !point_in_polygon(polygon, px, py) {
            return false;
        }
    }
    true
}

/// kqsl: true iff the open segment (a, b) crosses the interior of ANY
/// island, i.e. a sample on the interior of the segment lies inside an
/// island polygon, or the segment intersects an island edge. Endpoints
/// are excluded (they may legitimately sit on an inflated island ring).
pub(crate) fn bridge_crosses_any_island(a: Point2, b: Point2, islands: &[Vec<Point2>]) -> bool {
    if islands.is_empty() {
        return false;
    }
    for isl in islands {
        if isl.len() < 3 {
            continue;
        }
        let samples = 8;
        for i in 1..samples {
            let t = f64::from(i) / f64::from(samples);
            let px = a.x + (b.x - a.x) * t;
            let py = a.y + (b.y - a.y) * t;
            if point_in_polygon(isl, px, py) {
                return true;
            }
        }
        if segment_intersects_polygon_edges(a, b, isl) {
            return true;
        }
    }
    false
}

// ─── conversions ────────────────────────────────────────────────────────────
