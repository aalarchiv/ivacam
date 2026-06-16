//! Offset primitives — the cavalier_contours parallel offset for
//! polylines-with-arcs and the clipper2 inward pocket cascade, plus overcut
//! and the VcObject<->Polyline adapters. Split out of `offsets.rs`.
//! Owns the parallel-offset / cascade-truncation / nocontour diagnostic
//! sinks (drained by the parent's `OffsetDiagnostics`).

use super::{signed_area, PolylineOffset};
use crate::cam::VcObject;
use crate::geometry::{Point2, Segment, SegmentKind};
use cavalier_contours::polyline::{PlineSource, PlineSourceMut, PlineVertex, Polyline};
use clipper2_rust::{inflate_paths_d, EndType, JoinType, PathD, PathsD, Point as ClipperPoint};

/// Polygon-signed-area of a closed `VcObject`. Sums the chord-shoelace
/// contribution from each segment plus, for arcs/circles, the lens
/// area between chord and arc carrying `sign(bulge)`. Positive = CCW,
/// negative = CW.
///
/// Bulge correction matters for objects whose curved segments dominate
/// the shape — most importantly a single-Circle encoded as two
/// semicircles where the chord shoelace sums to exactly zero. Without
/// the correction every closed-arc-only object reads as area=0 ⇒ CCW,
/// which silently flips the inward/outward sign for CW-encoded
/// circles and gives wrong-side profile offsets.
///
/// **Precondition:** the bow term `½r²(θ − sinθ)` is the *minor*
/// circular-segment area, exact only for included angles θ ≤ 180°
/// (`|bulge| ≤ 1`). For a major arc (`|bulge| > 1`, θ > 180°) the true
/// enclosed region is the complement (circle minus minor segment), so the
/// magnitude is under-counted here — and a contour dominated by one major
/// arc could in principle flip the winding sign. In practice every arc
/// reaching this function comes from the DXF importer, which keeps
/// CIRCLE/ARC segments ≤ 180° (circles split into two semicircles, arcs
/// subdivided at ≤ 45°), so the precondition holds. Callers that
/// synthesize arbitrary major-arc segments must pre-split them rather than
/// trust this magnitude.
#[must_use]
pub fn object_signed_area(obj: &VcObject) -> f64 {
    let mut sum = 0.0;
    for seg in &obj.segments {
        // Chord shoelace contribution.
        sum += seg.start.x * seg.end.y - seg.end.x * seg.start.y;
        // Arc bow correction. Only meaningful when bulge != 0 — Line
        // and Point fall through with bulge = 0.
        let b = seg.bulge;
        if b.abs() < 1e-12 {
            continue;
        }
        let dx = seg.end.x - seg.start.x;
        let dy = seg.end.y - seg.start.y;
        let chord = (dx * dx + dy * dy).sqrt();
        if chord < 1e-12 {
            continue;
        }
        // sagitta = b * chord / 2 (signed).
        let s = b * chord * 0.5;
        let s_abs = s.abs();
        let r = (chord * chord * 0.25 + s * s) / (2.0 * s_abs);
        // Included angle: 2 * 2*atan(|b|).
        let theta = 4.0 * b.abs().atan();
        let lens = r * r * (theta - theta.sin()) * 0.5;
        // sum is 2*signed_area (we halve at the end); the lens
        // contribution is signed_area-scale, so multiply by 2.
        sum += 2.0 * b.signum() * lens;
    }
    sum * 0.5
}

/// Inward parallel offset by `distance` (positive). Picks the right
/// sign for the underlying `parallel_offset_object` based on the polygon
/// winding so a CW input doesn't flip the meaning.
#[must_use]
pub fn parallel_offset_inward(obj: &VcObject, distance: f64) -> Vec<PolylineOffset> {
    let mag = distance.abs();
    let delta = if object_signed_area(obj) >= 0.0 {
        mag
    } else {
        -mag
    };
    parallel_offset_object(obj, delta)
}

/// Outward parallel offset by `distance` (positive). Mirror of
/// `parallel_offset_inward`.
#[must_use]
pub fn parallel_offset_outward(obj: &VcObject, distance: f64) -> Vec<PolylineOffset> {
    let mag = distance.abs();
    let delta = if object_signed_area(obj) >= 0.0 {
        -mag
    } else {
        mag
    };
    parallel_offset_object(obj, delta)
}

/// Generate parallel offsets of `obj` at `delta`. Cavalier-Contours
/// convention: positive delta = LEFT of tangent. For CCW input that's
/// inward; for CW input that's outward. Most callers should use
/// `parallel_offset_inward` / `_outward` instead — they handle winding.
#[must_use]
pub fn parallel_offset_object(obj: &VcObject, delta: f64) -> Vec<PolylineOffset> {
    if obj.segments.is_empty() {
        return Vec::new();
    }
    let pline = vc_to_pline(obj);
    // cavalier_contours can panic on degenerate inputs ("input assumed
    // to not have repeat position vertexes") for self-touching contours
    // produced by some HATCH boundaries / ELLIPSE flattening. We
    // already dedupe consecutive vertices in vc_to_pline, but the
    // crate's intermediate self-intersection handling can still trip
    // the assert. Catch the panic and skip the offset rather than
    // taking down the whole pipeline — the user gets a partial result
    // plus a warning instead of a 500.
    //
    // Previously the catch_unwind path only emitted a `tracing::warn`,
    // which is invisible to the UI — the operation silently produced
    // empty offsets and the user shipped gcode missing the contour. We
    // now stash a structured `ParallelOffsetPanic` record into a
    // thread-local sink that the pipeline drains into `PipelineWarning`
    // entries (see `take_parallel_offset_panics`). The panic itself is
    // still caught — the pipeline stays alive.
    let Ok(offsets) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pline.parallel_offset(delta)
    })) else {
        record_parallel_offset_panic(obj, delta);
        return Vec::new();
    };
    offsets
        .into_iter()
        .map(|o| PolylineOffset {
            segments: pline_to_segments(&o, &obj.layer, obj.color),
            closed: o.is_closed(),
            level: 0,
            is_pocket: 0,
            layer: obj.layer.clone(),
            color: obj.color,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        })
        .collect()
}

/// Structured record produced when `parallel_offset_object` traps a
/// `cavalier_contours` panic. The pipeline drains the thread-local sink
/// after each op and converts these into `PipelineWarning`s tagged
/// `parallel_offset_panicked` so the UI surfaces them.
#[derive(Debug, Clone)]
pub struct ParallelOffsetPanic {
    pub layer: std::sync::Arc<str>,
    pub color: i32,
    pub bbox_min_x: f64,
    pub bbox_min_y: f64,
    pub bbox_max_x: f64,
    pub bbox_max_y: f64,
    /// Stable hash of the input segment endpoints (low 64 bits of a
    /// FNV-1a walk) so the same offending geometry reports the same
    /// digest across runs — useful for cross-referencing user bug
    /// reports with stored DXFs.
    pub input_digest: u64,
    pub delta: f64,
}

thread_local! {
    static PARALLEL_OFFSET_PANICS: std::cell::RefCell<Vec<ParallelOffsetPanic>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn record_parallel_offset_panic(obj: &VcObject, delta: f64) {
    tracing::warn!(
        "parallel_offset on layer '{}' panicked in cavalier_contours; skipping",
        obj.layer
    );
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    let mix = |x: f64, acc: &mut u64| {
        let bits = x.to_bits();
        for b in bits.to_le_bytes() {
            *acc ^= u64::from(b);
            *acc = acc.wrapping_mul(0x100_0000_01b3);
        }
    };
    for s in &obj.segments {
        for p in [s.start, s.end] {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
            mix(p.x, &mut h);
            mix(p.y, &mut h);
        }
        mix(s.bulge, &mut h);
    }
    mix(delta, &mut h);
    let rec = ParallelOffsetPanic {
        layer: obj.layer.clone(),
        color: obj.color,
        bbox_min_x: min_x,
        bbox_min_y: min_y,
        bbox_max_x: max_x,
        bbox_max_y: max_y,
        input_digest: h,
        delta,
    };
    PARALLEL_OFFSET_PANICS.with(|s| s.borrow_mut().push(rec));
}

/// Drain (and clear) any `ParallelOffsetPanic` entries stashed by
/// `parallel_offset_object` on this thread. The pipeline calls this once
/// per op so panics get attributed to the op that triggered them.
#[must_use]
pub(super) fn take_parallel_offset_panics() -> Vec<ParallelOffsetPanic> {
    PARALLEL_OFFSET_PANICS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// `pocket_cascade_with_islands` hits a hard ring cap (see
/// [`POCKET_CASCADE_RING_CAP`]) to keep adversarial / very-large pockets
/// from blowing the budget. When the cap fires, the cascade stops short
/// of carving out the entire pocket — the user sees a hollow ring near
/// the centre. We record the event in this thread-local so the per-op
/// driver can drain it via [`take_pocket_cascade_truncations`] and
/// surface a `pocket_cascade_truncated` warning attributed to the
/// triggering op.
#[derive(Debug, Clone)]
pub struct PocketCascadeTruncation {
    pub rings_emitted: usize,
    pub ring_cap: usize,
    pub delta: f64,
}

thread_local! {
    static POCKET_CASCADE_TRUNCATIONS: std::cell::RefCell<Vec<PocketCascadeTruncation>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain (and clear) any cascade-truncation records stashed by
/// `pocket_cascade_with_islands` on this thread.
#[must_use]
pub(super) fn take_pocket_cascade_truncations() -> Vec<PocketCascadeTruncation> {
    POCKET_CASCADE_TRUNCATIONS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// Hard cap on the number of rings the cascade can emit before bailing
/// Was 1024 — raised to 4096 to cover larger pockets at fine
/// steps (e.g. a 400×400 mm sign cascaded at 0.5 mm step needs ~800
/// rings, easily fitting the new budget; the old 1024 cap silently
/// truncated some real projects). The cap exists as a runaway / OOM
/// guard, NOT a project setting.
pub const POCKET_CASCADE_RING_CAP: usize = 4096;

/// Inward-cascade pocket offsets. `boundary` is the (already-tool-radius-offset)
/// inner boundary; `delta` is the per-ring step (positive number — caller
/// doesn't need to negate). Returns rings from outermost to innermost.
///
/// Convenience wrapper for the common single-boundary case; calls
/// [`pocket_cascade_with_islands`] with no holes.
#[must_use]
pub fn pocket_cascade(boundary: &[Point2], delta: f64) -> Vec<Vec<Point2>> {
    pocket_cascade_with_islands(boundary, &[], delta)
}

/// Inflate each raw island polygon outward by `tool_radius` so the
/// cutter centerline keeps a tool-radius clearance from the raw island
/// wall. The pocket emitters (`pocket_zigzag`, `pocket_cascade_with_islands`,
/// `stitch_rings_to_polyline`) all document islands as
/// "pre-inflated by `tool_radius`". Pipeline callers hold RAW
/// holes / inner-object polygons, which would let the cutter edge
/// plough into the island by `tool_r` at the boundary; this helper
/// bridges the gap.
///
/// Returns the inflated outer rings (clipper2 may merge overlapping
/// islands, drop a degenerate one, or split one into multiple). On
/// failure for a given island (degenerate input, clipper2 returns
/// empty) the original island is preserved as a fallback — better to
/// approximate than drop the safety geometry entirely.
#[must_use]
pub fn inflate_islands_by_tool_radius(
    islands: &[Vec<Point2>],
    tool_radius: f64,
) -> Vec<Vec<Point2>> {
    inflate_islands_by_delta(islands, tool_radius)
}

/// Internal Minkowski-sum inflation by an arbitrary positive delta.
/// Shared by [`inflate_islands_by_tool_radius`] and the cascade's
/// over-inflate path. Negative / non-finite deltas pass islands
/// through unchanged.
fn inflate_islands_by_delta(islands: &[Vec<Point2>], delta: f64) -> Vec<Vec<Point2>> {
    if !delta.is_finite() || delta <= 1e-9 {
        return islands.to_vec();
    }
    let mut out: Vec<Vec<Point2>> = Vec::with_capacity(islands.len());
    for island in islands {
        if island.len() < 3 {
            // Too small to inflate meaningfully; keep raw so the caller's
            // length checks still see it.
            out.push(island.clone());
            continue;
        }
        // Clipper2 EndType::Polygon with positive delta = outward inflate
        // of a closed polygon. The result is the Minkowski sum with a
        // disc of radius `delta`, i.e. the safe centerline boundary the
        // cutter must stay outside.
        let path: PathD = island.iter().map(|p| ClipperPoint::new(p.x, p.y)).collect();
        let inflated = inflate_paths_d(
            &vec![path],
            delta,
            JoinType::Round,
            EndType::Polygon,
            2.0,
            4,
            0.25,
        );
        if inflated.is_empty() {
            out.push(island.clone());
            continue;
        }
        for ring in inflated {
            if ring.len() >= 3 {
                out.push(ring.iter().map(|pt| Point2::new(pt.x, pt.y)).collect());
            }
        }
    }
    out
}

/// Extra island inflation needed for the inward cascade when the
/// per-pass `step` is smaller than `tool_radius` (high overlap, e.g. 80%
/// engagement ⇒ step ≈ `0.2·tool_radius`). Callers pass islands that are
/// ALREADY pre-inflated by `tool_radius` (the pre-inflation contract); the
/// cascade's first ring then offsets boundary+islands inward by `-step`.
/// When `step < tool_radius`, the cutter centerline lands at `step` mm
/// from the raw island wall — `tool_r − step` short of full clearance.
/// The cutter EDGE then intrudes into the raised feature by `tool_r −
/// step` mm.
///
/// To restore full clearance on the first ring around the island we
/// apply an EXTRA `max(0, tool_r − step)` of outward inflation here.
/// After the cascade's `-step`, the cutter centerline sits at
/// `tool_r + (tool_r − step) − step = 2·(tool_r − step)` from the raw
/// wall when step < `tool_r`; the cutter edge keeps a clean
/// `tool_r − step` clearance from the raw wall (no intrusion). When
/// step ≥ `tool_r` the extra inflation is zero and behaviour matches the
/// non-over-inflate path.
///
/// `islands` MUST already be pre-inflated by `tool_radius` (the cascade /
/// zigzag / spiral contract). `step` is the per-ring cascade inward
/// delta — same value passed as the third argument to
/// [`pocket_cascade_with_islands`].
#[must_use]
pub fn over_inflate_islands_for_high_overlap(
    islands: &[Vec<Point2>],
    tool_radius: f64,
    step: f64,
) -> Vec<Vec<Point2>> {
    let extra = (tool_radius - step).max(0.0);
    if extra <= 1e-9 {
        return islands.to_vec();
    }
    inflate_islands_by_delta(islands, extra)
}

/// Single-step inward offset of a boundary + holes by `delta`.
/// Unlike [`pocket_cascade_with_islands`], stops after ONE inflate so
/// callers that only want the level-0 ring (V-Carve perimeter mode)
/// don't pay for the rest of the cascade. Holes are NOT pre-inflated
/// here — `delta` IS the desired clearance from each hole.
#[must_use]
pub fn boundary_offset_inward(
    boundary: &[Point2],
    holes: &[Vec<Point2>],
    delta: f64,
) -> Vec<Vec<Point2>> {
    if boundary.len() < 3 || delta <= 1e-9 {
        return Vec::new();
    }
    let paths = build_paths(boundary, holes);
    let next = inflate_paths_d(
        &paths,
        -delta,
        JoinType::Round,
        EndType::Polygon,
        2.0,
        4,
        0.25,
    );
    let mut rings = Vec::with_capacity(next.len());
    for ring in &next {
        if ring.len() >= 3 {
            rings.push(ring.iter().map(|pt| Point2::new(pt.x, pt.y)).collect());
        }
    }
    rings
}

/// Inward-cascade pocket offsets that respect islands (closed contours
/// inside the boundary that should be left uncut). Each `island` is a
/// closed polyline already inflated by `tool_radius` outward — the
/// caller is responsible for that pre-inflation, matching the upstream
/// Python `do_pockets` islands branch.
#[must_use]
pub fn pocket_cascade_with_islands(
    boundary: &[Point2],
    islands: &[Vec<Point2>],
    delta: f64,
) -> Vec<Vec<Point2>> {
    if boundary.len() < 3 || delta <= 1e-9 {
        return Vec::new();
    }
    let mut current: PathsD = build_paths(boundary, islands);
    let mut rings = Vec::new();
    loop {
        // clipper2-rust args: (paths, delta, jt, et, miter_limit, precision, arc_tol).
        // precision = 4 → 1e-4 mm internal grid (sub-micrometer). arc_tol is
        // the chord error for round joins, in input units.
        let next = inflate_paths_d(
            &current,
            -delta,
            JoinType::Round,
            EndType::Polygon,
            2.0,
            4,
            0.25,
        );
        if next.is_empty() || next.iter().all(|r| r.len() < 3) {
            break;
        }
        for ring in &next {
            if ring.len() >= 3 {
                rings.push(ring.iter().map(|pt| Point2::new(pt.x, pt.y)).collect());
            }
        }
        current = next;
        if rings.len() > POCKET_CASCADE_RING_CAP {
            // Cap the cascade and stash a thread-local record so
            // the per-op driver can attribute the event to the user's op
            // (drained via `take_pocket_cascade_truncations`). Large
            // pockets at fine steps could otherwise silently lose
            // interior rings here — leaving a hollow doughnut that looks
            // machined but isn't. The cap is 4096 (an OOM/runaway guard,
            // not a project setting).
            POCKET_CASCADE_TRUNCATIONS.with(|s| {
                s.borrow_mut().push(PocketCascadeTruncation {
                    rings_emitted: rings.len(),
                    ring_cap: POCKET_CASCADE_RING_CAP,
                    delta,
                });
            });
            break;
        }
    }
    rings
}

fn build_paths(boundary: &[Point2], islands: &[Vec<Point2>]) -> PathsD {
    // Clipper2 treats CW-wound rings as holes when EndType::Polygon is in
    // play. Force the outer boundary CCW and the islands CW regardless of
    // how the caller hands them in.
    let mut all: PathsD = Vec::with_capacity(islands.len() + 1);
    let outer = if signed_area(boundary) > 0.0 {
        boundary.to_vec()
    } else {
        let mut r = boundary.to_vec();
        r.reverse();
        r
    };
    let outer_path: PathD = outer.iter().map(|p| ClipperPoint::new(p.x, p.y)).collect();
    all.push(outer_path);
    for island in islands {
        if island.len() < 3 {
            continue;
        }
        let hole = if signed_area(island) < 0.0 {
            island.clone()
        } else {
            let mut r = island.clone();
            r.reverse();
            r
        };
        let hole_path: PathD = hole.iter().map(|p| ClipperPoint::new(p.x, p.y)).collect();
        all.push(hole_path);
    }
    all
}

fn vc_to_pline(obj: &VcObject) -> Polyline<f64> {
    let mut pl = if obj.closed {
        Polyline::new_closed()
    } else {
        Polyline::new()
    };
    // cavalier_contours panics on consecutive coincident vertices ("bug:
    // input assumed to not have repeat position vertexes"), so swallow
    // them here. Imported HATCH boundaries and SVG paths can carry
    // duplicates legitimately.
    let mut last: Option<(f64, f64)> = None;
    let push = |pl: &mut Polyline<f64>, last: &mut Option<(f64, f64)>, x: f64, y: f64, b: f64| {
        if let Some((lx, ly)) = *last {
            if (x - lx).abs() < 1e-9 && (y - ly).abs() < 1e-9 {
                return;
            }
        }
        pl.add_vertex(PlineVertex::new(x, y, b));
        *last = Some((x, y));
    };
    for seg in &obj.segments {
        let bulge = if seg.kind == SegmentKind::Line {
            0.0
        } else {
            seg.bulge
        };
        push(&mut pl, &mut last, seg.start.x, seg.start.y, bulge);
    }
    if !obj.closed {
        if let Some(last_seg) = obj.segments.last() {
            push(&mut pl, &mut last, last_seg.end.x, last_seg.end.y, 0.0);
        }
    }
    pl
}

fn pline_to_segments(pl: &Polyline<f64>, layer: &str, color: i32) -> Vec<Segment> {
    let n = pl.vertex_count();
    if n == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n);
    let last = if pl.is_closed() { n } else { n - 1 };
    for i in 0..last {
        let v0 = pl.at(i);
        let v1 = pl.at((i + 1) % n);
        let start = Point2::new(v0.x, v0.y);
        let end = Point2::new(v1.x, v1.y);
        if v0.bulge.abs() > 1e-12 {
            // Populate the arc center at emit time. The offsetter
            // already has everything needed to derive it; carrying it on the
            // Segment spares every downstream consumer (leads, chaining,
            // tabs) from re-deriving it via bulge_to_arc on each access.
            let center = crate::math::bulge_to_arc(start, end, v0.bulge).0;
            out.push(Segment::arc(
                start,
                end,
                v0.bulge,
                Some(center),
                layer,
                color,
            ));
        } else {
            out.push(Segment::line(start, end, layer, color));
        }
    }
    out
}

/// If `obj` is a single closed CIRCLE smaller than the tool, return a
/// drill-only offset whose single segment is a zero-length POINT at the
/// circle's center. The gcode emitter handles this as plunge + retract.
///
/// The prior threshold `r < 0.95 * tool_radius` left a dead zone
/// for circles whose radius sat in `[0.95·r, r)` — too narrow for the
/// inward-offset cascade (which collapsed to empty geometry) but too wide
/// to drill under the strict bound. Result: such holes were silently
/// dropped. The threshold was extended to `r < 0.999 * tool_radius` so
/// any circle that won't pocket gets a drill substitution at its centre.
///
/// The previous strict `<` left an exact-fit case `r == tool_radius`
/// (think a 6 mm hole milled with a 6 mm endmill) where the cascade
/// returned empty geometry AND the drill substitution was rejected — the
/// hole was silently dropped. The cascade can't carve a hole that exactly
/// equals the tool, but the drill substitution can: the cutter plunges to
/// depth at the circle's center and the hole is cut by the tool's full
/// diameter. The threshold is widened to `radius <= tool_radius * 1.001`
/// so the small floating-point slop band (≤ 1 ‰ over nominal) and the
/// exact-fit value both route to the drill substitution.
#[must_use]
pub fn small_circle_drill(obj: &VcObject, tool_radius: f64) -> Option<PolylineOffset> {
    use crate::geometry::SegmentKind;
    if !obj.closed || obj.segments.is_empty() {
        return None;
    }
    let kinds_circle_only = obj.segments.iter().all(|s| s.kind == SegmentKind::Circle);
    if !kinds_circle_only {
        return None;
    }
    let center = obj.segments[0].center?;
    let radius = obj.segments[0].start.distance(center);
    if radius > tool_radius * 1.001 {
        return None;
    }
    Some(PolylineOffset {
        segments: vec![Segment::point(center, obj.layer.clone(), obj.color)],
        closed: false,
        level: 0,
        is_pocket: 0,
        layer: obj.layer.clone(),
        color: obj.color,
        source_object_idx: 0,
        tabs: Vec::new(),
        is_finish: false,
    })
}

/// Apply overcut to every closed offset whose source object exists in
/// `objects`. The dip targets each offset's owning original boundary, not the
/// offset's own corners, so cascade rings still respect the parent shape.
pub fn apply_overcut_to_offsets(
    offsets: &mut [PolylineOffset],
    objects: &[VcObject],
    tool_radius: f64,
) {
    for off in offsets.iter_mut() {
        if !off.closed {
            continue;
        }
        if let Some(obj) = objects.get(off.source_object_idx) {
            apply_overcut(off, &obj.segments, tool_radius);
        }
    }
}

/// Apply overcut to a closed offset polyline whose reflex (concave) corners
/// need a small dip toward the original wall so the cutter (radius
/// `tool_radius`) clears the geometric corner.
///
/// Pre-conditions: `offset.closed`, polyline is wound CCW (interior on left),
/// `boundary_segments` is the original object boundary the offset was derived
/// from. Arcs are skipped (no overcut applied across them).
///
/// At each reflex corner of the offset polyline we cast a ray along the
/// outward bisector and stop at the first boundary endpoint that lies on the
/// ray. The dip length is `dist_to_boundary - tool_radius`; the inserted
/// vertex pattern is `corner, dip, corner` so the cutter swings out and back.
// Linear reflex-corner walk; splitting into helpers would force a
// shared mutable cursor + parallel index lookups across them. Read
// top-to-bottom as one state machine.
#[allow(clippy::too_many_lines)]
pub fn apply_overcut(offset: &mut PolylineOffset, boundary_segments: &[Segment], tool_radius: f64) {
    use std::f64::consts::FRAC_PI_4;
    if !offset.closed || offset.segments.len() < 3 {
        return;
    }
    let r_abs = tool_radius.abs();
    let n = offset.segments.len();
    let pts: Vec<(Point2, f64)> = offset.segments.iter().map(|s| (s.start, s.bulge)).collect();

    // Derive an adaptive `perp_tol` from the boundary's bbox
    // diagonal. The prior fixed 0.25 mm tolerance was tuned for desktop
    // CNC scales (cm/dm); at sub-mm jewelry / engraving scales (5 mm
    // object with a 0.3 mm endmill) it was wider than the entire
    // workpiece, picking the nearest WRONG wall as the dip target.
    // Pattern: max(1e-3 mm, 1e-3 × bbox_diag) — same shape as the
    // chaining fuzzy fix. A 5 mm object gets 5e-3 mm; a 500 mm sign
    // gets 0.5 mm (looser than the old 0.25, which is fine — long
    // walls and far endpoints want extra slack).
    let perp_tol = {
        let (mut mnx, mut mny, mut mxx, mut mxy) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for s in boundary_segments {
            for p in [s.start, s.end] {
                if p.x < mnx {
                    mnx = p.x;
                }
                if p.y < mny {
                    mny = p.y;
                }
                if p.x > mxx {
                    mxx = p.x;
                }
                if p.y > mxy {
                    mxy = p.y;
                }
            }
        }
        if mnx.is_finite() && mny.is_finite() && mxx.is_finite() && mxy.is_finite() {
            let diag = (mxx - mnx).hypot(mxy - mny);
            (diag * 1e-3).max(1e-3)
        } else {
            // Degenerate / empty boundary — keep the floor.
            1e-3_f64
        }
    };

    let mut emitted: Vec<(f64, f64, f64)> = Vec::with_capacity(n * 2);

    for i in 0..n {
        let prev = pts[(i + n - 1) % n].0;
        let cur = pts[i].0;
        let next = pts[(i + 1) % n].0;
        let in_bulge = pts[(i + n - 1) % n].1;
        let out_bulge = pts[i].1;

        // Always emit the corner first.
        emitted.push((cur.x, cur.y, out_bulge));

        // Skip arc-bounded corners; the dip only makes sense between two
        // straight segments.
        if in_bulge.abs() > 1e-12 || out_bulge.abs() > 1e-12 {
            continue;
        }

        let tin = (cur.x - prev.x, cur.y - prev.y);
        let tout = (next.x - cur.x, next.y - cur.y);
        let len_in = (tin.0 * tin.0 + tin.1 * tin.1).sqrt();
        let len_out = (tout.0 * tout.0 + tout.1 * tout.1).sqrt();
        if len_in < 1e-9 || len_out < 1e-9 {
            continue;
        }
        let ti = (tin.0 / len_in, tin.1 / len_in);
        let to_ = (tout.0 / len_out, tout.1 / len_out);

        // Signed turn: positive = left (convex on CCW), negative = right
        // (reflex on CCW). Need a sharp right turn.
        let cross = ti.0 * to_.1 - ti.1 * to_.0;
        let dot = ti.0 * to_.0 + ti.1 * to_.1;
        let turn = cross.atan2(dot);
        if turn >= -FRAC_PI_4 {
            continue;
        }

        // Outward bisector at a reflex corner: opposite of (-tin + tout)
        // (which points into the interior at convex corners). At a reflex
        // corner the geometric "interior" direction sits on the OPPOSITE side
        // of the offset's local interior — i.e. toward the original wall —
        // so we negate.
        let bx = -ti.0 + to_.0;
        let by = -ti.1 + to_.1;
        let blen = (bx * bx + by * by).sqrt();
        if blen < 1e-9 {
            continue;
        }
        let out = (-bx / blen, -by / blen);

        // Probe boundary segments along the outward ray.
        // Prior implementation only tested vertex ENDPOINTS — fine
        // for tiny test geometries where every wall is short enough that
        // a vertex lands near the bisector ray, but real CAD parts have
        // long flat walls whose endpoints sit far from the ray. We now
        // intersect each boundary segment as a line-segment-vs-ray test
        // so long-wall reflex corners get their dip too.
        //
        // `perp_tol` is derived ONCE from the boundary bbox
        // diagonal (see top of fn) so sub-mm jewelry and metre-scale
        // signs both get a tolerance proportional to the working scale.
        // The endpoint loop below still uses `perp_tol` directly — long
        // walls whose closest point on the ray IS their endpoint
        // continue to resolve here; the segment-distance path picks up
        // mid-edge hits.
        let mut nearest: Option<f64> = None;
        let mut consider = |along: f64| {
            if along <= 1e-6 {
                return;
            }
            if nearest.map_or(true, |c| along < c) {
                nearest = Some(along);
            }
        };
        for seg in boundary_segments {
            // 1) Endpoint hits (unchanged behaviour — short segments hit
            //    here exactly like before).
            for p1 in [seg.start, seg.end] {
                let dx = p1.x - cur.x;
                let dy = p1.y - cur.y;
                let along = dx * out.0 + dy * out.1;
                if along <= 1e-6 {
                    continue;
                }
                let perp = (dx * out.1 - dy * out.0).abs();
                if perp <= perp_tol {
                    consider(along);
                }
            }
            // 2) Mid-edge hits: solve the ray (origin = cur, dir = out)
            //    against the segment (a -> b). The ray hits the line
            //    extending the segment when the two are not parallel; we
            //    additionally require the hit point to lie inside the
            //    segment AND in front of the ray (along > 0).
            let (a, b) = (seg.start, seg.end);
            let ex = b.x - a.x;
            let ey = b.y - a.y;
            // Solve cur + t * out = a + u * (b - a) for (t, u).
            //    out.x * t - ex * u = a.x - cur.x
            //    out.y * t - ey * u = a.y - cur.y
            // Cramer's:
            let det = out.0 * (-ey) - out.1 * (-ex);
            if det.abs() < 1e-12 {
                // Parallel / coincident — fall back to a perpendicular
                // foot probe: closest point on segment to the ray's
                // projection. We don't overcut into a wall the cutter is
                // walking parallel to; skip.
                continue;
            }
            let rhs0 = a.x - cur.x;
            let rhs1 = a.y - cur.y;
            let t = (rhs0 * (-ey) - rhs1 * (-ex)) / det;
            let u = (out.0 * rhs1 - out.1 * rhs0) / det;
            // u in [0, 1] keeps the hit inside the segment; the
            // perp_tol slack also rescues hits that lie just past
            // either endpoint (matching the endpoint loop's slack).
            if (-1e-3..=1.0 + 1e-3).contains(&u) {
                consider(t);
            }
        }
        let Some(dist) = nearest else {
            continue;
        };
        let dip = dist - r_abs;
        if dip <= 1e-6 {
            continue;
        }
        let dip_x = cur.x + out.0 * dip;
        let dip_y = cur.y + out.1 * dip;
        // Pattern at the corner: corner, dip, corner. The first `corner` is
        // the one we already pushed (with its outgoing bulge cleared so the
        // dip-to is a straight line); we need to fix that.
        if let Some(last_emit) = emitted.last_mut() {
            // We just pushed (cur, out_bulge). Reset its outgoing bulge to 0
            // so the segment to the dip is straight.
            last_emit.2 = 0.0;
        }
        emitted.push((dip_x, dip_y, 0.0));
        emitted.push((cur.x, cur.y, out_bulge));
    }

    if emitted.len() < 3 || emitted.len() == n {
        return;
    }

    let mut new_segs: Vec<Segment> = Vec::with_capacity(emitted.len());
    let m = emitted.len();
    for i in 0..m {
        let a = emitted[i];
        let b = emitted[(i + 1) % m];
        let kind = if a.2.abs() > 1e-12 {
            SegmentKind::Arc
        } else {
            SegmentKind::Line
        };
        new_segs.push(Segment {
            kind,
            start: Point2 { x: a.0, y: a.1 },
            end: Point2 { x: b.0, y: b.1 },
            bulge: a.2,
            center: None,
            layer: offset.layer.clone(),
            color: offset.color,
        });
    }
    offset.segments = new_segs;
}
