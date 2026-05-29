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

use cavalier_contours::polyline::{PlineSource, PlineSourceMut, PlineVertex, Polyline};
use clipper2_rust::{inflate_paths_d, EndType, JoinType, PathD, PathsD, Point as ClipperPoint};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::cam::{segments_to_points, VcObject};
use crate::geometry::{Point2, Segment, SegmentKind};

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
    /// cutting this offset. Frontend places these via mtm.10; the gcode
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct TabPoint {
    pub x: f64,
    pub y: f64,
    /// Per-tab width override (mm). When `Some`, this tab uses the
    /// override; when `None`, falls back to the op-level setup width
    /// (`setup.tabs.width`). Audit 3wv: was hashed into the cache key
    /// but never consumed by `emit_path_with_tabs` — toggling overrides
    /// produced identical output. Now drives the per-tab crossing
    /// radius + zone half-width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_override_mm: Option<f64>,
    /// Per-tab height override (mm). When `Some`, the cutter lifts to
    /// `cut_z + override` over this tab instead of the op-level
    /// `setup.tabs.height`. None ⇒ inherit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_override_mm: Option<f64>,
}

impl TabPoint {
    /// Effective tab-zone half-width (mm) — per-tab override wins
    /// over the op-level fallback.
    #[must_use]
    pub fn radius(&self, fallback_full_width_mm: f64) -> f64 {
        match self.width_override_mm {
            Some(w) if w > 0.0 => w * 0.5,
            _ => fallback_full_width_mm * 0.5,
        }
    }

    /// Effective lift over this tab (mm). Per-tab override wins.
    #[must_use]
    pub fn lift(&self, fallback_mm: f64) -> f64 {
        self.height_override_mm
            .filter(|v| *v > 0.0)
            .unwrap_or(fallback_mm)
    }
}

/// Project a list of imported-segment-keyed tab points onto a generated
/// offset's tab list. We snap each tab to the closest point on the
/// offset's polyline; tabs that land further than `max_distance` from the
/// nearest segment are dropped (they belong to a different object).
pub fn attach_tabs_to_offsets(
    offsets: &mut [PolylineOffset],
    tabs_by_object: &HashMap<usize, Vec<TabPoint>>,
    max_distance: f64,
) {
    for offset in offsets.iter_mut() {
        let Some(tabs) = tabs_by_object.get(&offset.source_object_idx) else {
            continue;
        };
        for tab in tabs {
            // Snap to closest point on any segment of this offset.
            if let Some(snap) = snap_to_offset(offset, *tab, max_distance) {
                offset.tabs.push(snap);
            }
        }
    }
}

fn snap_to_offset(offset: &PolylineOffset, tab: TabPoint, max_distance: f64) -> Option<TabPoint> {
    let mut best: Option<(TabPoint, f64)> = None;
    for seg in &offset.segments {
        let p = closest_point_on_segment(seg, tab);
        let d = (p.x - tab.x).hypot(p.y - tab.y);
        if d > max_distance {
            continue;
        }
        if best.map_or(true, |(_, bd)| d < bd) {
            best = Some((p, d));
        }
    }
    best.map(|(p, _)| p)
}

fn closest_point_on_segment(seg: &Segment, tab: TabPoint) -> TabPoint {
    let dx = seg.end.x - seg.start.x;
    let dy = seg.end.y - seg.start.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-12 {
        return TabPoint {
            x: seg.start.x,
            y: seg.start.y,
            width_override_mm: tab.width_override_mm,
            height_override_mm: tab.height_override_mm,
        };
    }
    let t = (((tab.x - seg.start.x) * dx + (tab.y - seg.start.y) * dy) / len_sq).clamp(0.0, 1.0);
    TabPoint {
        x: seg.start.x + t * dx,
        y: seg.start.y + t * dy,
        width_override_mm: tab.width_override_mm,
        height_override_mm: tab.height_override_mm,
    }
}

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
/// **Precondition (jz8l):** the bow term `½r²(θ − sinθ)` is the *minor*
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
    // z4t6: previously the catch_unwind path only emitted a `tracing::warn`,
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
pub fn take_parallel_offset_panics() -> Vec<ParallelOffsetPanic> {
    PARALLEL_OFFSET_PANICS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// mdpo: `pocket_cascade_with_islands` hits a hard ring cap (see
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
pub fn take_pocket_cascade_truncations() -> Vec<PocketCascadeTruncation> {
    POCKET_CASCADE_TRUNCATIONS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

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
pub fn take_nocontour_allowance_ignored() -> Vec<NocontourAllowanceIgnored> {
    NOCONTOUR_ALLOWANCE_IGNORED.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// Hard cap on the number of rings the cascade can emit before bailing
/// (mdpo). Was 1024 — raised to 4096 to cover larger pockets at fine
/// steps (e.g. a 400×400 mm sign cascaded at 0.5 mm step needs ~800
/// rings, easily fitting the new budget; the old 1024 cap silently
/// truncated some real projects). The cap exists as a runaway / OOM
/// guard, NOT a project setting.
pub const POCKET_CASCADE_RING_CAP: usize = 4096;

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
pub fn take_zigzag_stride_degenerate() -> Vec<ZigzagStrideDegenerate> {
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
            if point_in_polygon_pts(poly, px, py) {
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

fn horizontal_crossings(poly: &[Point2], y: f64, min_x: f64, max_x: f64) -> Vec<f64> {
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
    // c6ej: collapse duplicate crossings whose x values are within a
    // FUZZY-equivalent tolerance. A scanline that just grazes a vertex
    // produces TWO crossings at the same x (one for each adjacent edge)
    // when both edges happen to satisfy the half-open `y >= hi.y - 1e-12`
    // rule at the same vertex — that's the classic odd-count source the
    // downstream `chunks_exact(2)` silently dropped. Snapping coincident
    // crossings together restores even parity for the common
    // vertex-tangent case; a remaining odd remainder is a genuinely
    // degenerate input that we now log instead of dropping silently.
    if xs.len() >= 2 {
        let snap_tol = 1e-3_f64;
        let mut dedup = Vec::with_capacity(xs.len());
        let mut last = f64::NEG_INFINITY;
        let mut pending: Option<f64> = None;
        for &x in &xs {
            if (x - last).abs() <= snap_tol && pending.is_none() {
                // Coincident pair (vertex-tangent): swallow the duplicate
                // so the entry-exit count stays even.
                pending = Some(last);
                continue;
            }
            if let Some(p) = pending.take() {
                // Drop the swallowed duplicate; emit the new crossing
                // fresh. (We keep one of the two coincident hits in
                // `dedup` already.)
                let _ = p;
            }
            dedup.push(x);
            last = x;
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

/// knd4: inflate each raw island polygon outward by `tool_radius` so the
/// cutter centerline keeps a tool-radius clearance from the raw island
/// wall. The pocket emitters (`pocket_zigzag`, `pocket_cascade_with_islands`,
/// `stitch_rings_to_polyline`) all document islands as
/// "pre-inflated by `tool_radius`" — pipeline callers used to pass RAW
/// holes / inner-object polygons, so the cutter edge ploughed into the
/// island by `tool_r` at the boundary. This helper bridges the gap.
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
/// over-inflate path (sbtf). Negative / non-finite deltas pass islands
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

/// sbtf: extra island inflation needed for the inward cascade when the
/// per-pass `step` is smaller than `tool_radius` (high overlap, e.g. 80%
/// engagement ⇒ step ≈ `0.2·tool_radius`). Callers pass islands that are
/// ALREADY pre-inflated by `tool_radius` (the knd4 contract); the
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
/// pre-sbtf path.
///
/// `islands` MUST already be the knd4-inflated polygons (the cascade /
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

/// Single-step inward offset of a boundary + holes by `delta` (r8ut).
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
            // mdpo: cap the cascade and stash a thread-local record so
            // the per-op driver can attribute the event to the user's op
            // (drained via `take_pocket_cascade_truncations`). Large
            // pockets at fine steps used to silently lose interior rings
            // here — leaving a hollow doughnut that looked machined but
            // wasn't. The cap was 1024 pre-mdpo; we raise to 4096 (an
            // OOM/runaway guard, not a project setting).
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

/// Side of the workpiece the cutter sits on for a given offset:
/// * `Outer` — cutter is outside the part (external profile, or walking
///   around a pocket island).
/// * `Inner` — cutter is inside the part / pocket (pocket boundary,
///   pocket cascade ring, internal profile).
/// * `Skip` — winding doesn't matter (Engrave / `DragKnife` / Profile-On).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutContext {
    Outer,
    Inner,
    Skip,
}

/// Apply a desired cut direction to a closed offset by reversing its
/// traversal if the resulting winding doesn't match the convention.
///
/// For a right-hand spindle (standard CW from above, `SpindleDirection::Cw`):
///
/// |  context |   conventional   |     climb        |
/// |----------|------------------|------------------|
/// |  outer   |  CW (area < 0)   |  CCW (area > 0)  |
/// |  inner   |  CCW (area > 0)  |  CW (area < 0)   |
///
/// The "outer" and "inner" labels refer to where the *cutter* sits, not
/// the geometry's role in the program. A cutter walking around the
/// outside of a part = Outer; walking inside a pocket = Inner; walking
/// around an island inside a pocket = Outer (the cutter is outside the
/// island).
///
/// q57s: for a LEFT-hand spindle (`SpindleDirection::Ccw`, M4 mode — left-
/// hand cutter, mirror tooling), climb and conventional are physically
/// flipped because the cutting edge rotates the other way. The truth table
/// above is XOR'd with the spindle bit so that the requested intent
/// ("climb" / "conventional") matches the physical cut on either spindle.
/// Pre-q57s, climb-vs-conventional was silently inverted on M4 spindles —
/// "climb" picked CCW geometry on inner-pocket regardless of which way
/// the cutter was rotating.
pub fn enforce_winding(
    offset: &mut PolylineOffset,
    context: CutContext,
    direction: crate::project::CutDirection,
    spindle: crate::project::tool::SpindleDirection,
) {
    use crate::project::tool::SpindleDirection;
    use crate::project::CutDirection;
    if !offset.closed || matches!(context, CutContext::Skip) {
        return;
    }
    let area = offset_signed_area(offset);
    if area.abs() < 1e-9 {
        return;
    }
    // Geometric want_ccw for a right-hand (CW) spindle.
    let want_ccw_rh = match (context, direction) {
        (CutContext::Inner, CutDirection::Conventional) => true,
        (CutContext::Inner, CutDirection::Climb) => false,
        (CutContext::Outer, CutDirection::Conventional) => false,
        (CutContext::Outer, CutDirection::Climb) => true,
        (CutContext::Skip, _) => return,
    };
    // q57s: flip the geometric winding for left-hand spindles so the
    // physical chipload direction matches the user's climb/conventional
    // intent regardless of M3/M4.
    let want_ccw = match spindle {
        SpindleDirection::Cw => want_ccw_rh,
        SpindleDirection::Ccw => !want_ccw_rh,
    };
    let is_ccw = area > 0.0;
    if is_ccw != want_ccw {
        reverse_offset(offset);
    }
}

/// kzz9: any closed offset whose nearest segment-start lands more than
/// [`APPROACH_POINT_WARN_MM`] from the user-picked approach point gets
/// rotated anyway (preserving the prior behaviour), but the distance is
/// recorded in this thread-local so the per-op driver can surface a
/// `rotate_offsets_far_from_approach` warning. Typical cause: stale
/// approach point left over after the user moved the source contour.
#[derive(Debug, Clone)]
pub struct ApproachPointFarRotation {
    pub distance_mm: f64,
    pub approach: (f64, f64),
}

thread_local! {
    static APPROACH_POINT_FAR: std::cell::RefCell<Vec<ApproachPointFarRotation>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain (and clear) any far-approach-point records stashed by
/// [`rotate_offsets_to_approach_point`] on this thread.
#[must_use]
pub fn take_approach_point_far_rotations() -> Vec<ApproachPointFarRotation> {
    APPROACH_POINT_FAR.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// kzz9: distance threshold (mm) above which [`rotate_offsets_to_approach_point`]
/// records a far-rotation event. The chosen value is a rule-of-thumb
/// — most users place the approach point right on the boundary, so any
/// hit > 10 mm is almost certainly stale geometry (the user moved the
/// shape after picking the approach point).
pub const APPROACH_POINT_WARN_MM: f64 = 10.0;

/// Rotate each CLOSED offset's segment list so the first segment's
/// start is closest to `ap` (rt1.26 / Estlcam Anfahrpunkt). Open
/// offsets (zigzag / spiral / trochoidal strokes) are left alone —
/// their winding has no rotational symmetry to exploit. The cutter's
/// plunge / lead-in then happens at the user-picked entry XY.
///
/// kzz9: when the chosen `ap` ends up farther than
/// [`APPROACH_POINT_WARN_MM`] from EVERY closed offset's nearest vertex
/// the rotation still picks the nearest start (preserving the prior
/// behaviour for back-compat with existing tests), but a record is
/// stashed in the thread-local drained by
/// [`take_approach_point_far_rotations`]. The pipeline turns that into
/// a `rotate_offsets_far_from_approach` warning attributed to the op.
pub fn rotate_offsets_to_approach_point(offsets: &mut [PolylineOffset], ap: (f64, f64)) {
    let ap_pt = Point2::new(ap.0, ap.1);
    let mut min_d_overall = f64::INFINITY;
    for offset in offsets.iter_mut() {
        if !offset.closed || offset.segments.len() < 2 {
            continue;
        }
        let mut best: Option<(usize, f64)> = None;
        for (i, seg) in offset.segments.iter().enumerate() {
            let d = seg.start.distance(ap_pt);
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        if let Some((i, d)) = best {
            if d < min_d_overall {
                min_d_overall = d;
            }
            if i > 0 {
                offset.segments.rotate_left(i);
            }
        }
    }
    if min_d_overall.is_finite() && min_d_overall > APPROACH_POINT_WARN_MM {
        APPROACH_POINT_FAR.with(|s| {
            s.borrow_mut().push(ApproachPointFarRotation {
                distance_mm: min_d_overall,
                approach: ap,
            });
        });
    }
}

/// Walk a per-op offset list and enforce climb/conventional on each
/// closed offset. The op's main `cut_direction` applies to roughing
/// passes (cascade level ≥ 1); the `finish_direction` applies to the
/// finishing pass (level = 0 — the offset that defines the wall
/// surface).
///
/// Context is derived from the op kind and per-offset `signed_area`:
/// * Profile + `ToolOffset::Outside` → all offsets are Outer
/// * Profile + `ToolOffset::Inside`  → all offsets are Inner
/// * Profile + `ToolOffset::On/None` → Skip (no winding choice)
/// * Pocket → CCW offsets are Inner (cutter inside the pocket), CW
///   offsets are Outer (cutter going around an island)
/// * Engrave / `DragKnife` → Skip
pub fn apply_cut_direction(
    offsets: &mut [PolylineOffset],
    op: &crate::project::Op,
    finish_default_for_outside_profile_only: bool,
    spindle: crate::project::tool::SpindleDirection,
) {
    use crate::cam::setup::ToolOffset;
    use crate::project::OpKind;
    let _ = finish_default_for_outside_profile_only; // currently unused; kept for future hook
                                                     // kbx5 step 2: cut directions live on ContourParams. Non-contour
                                                     // ops fall back to Conventional (the existing default).
    let (main, finish) = op.contour_params().map_or(
        (
            crate::project::CutDirection::Conventional,
            crate::project::CutDirection::Conventional,
        ),
        |c| (c.cut_direction, c.finish_cut_direction),
    );
    let context_for = |offset: &PolylineOffset| -> CutContext {
        match &op.kind {
            OpKind::Profile {
                offset: tool_offset,
                ..
            } => match tool_offset {
                ToolOffset::Outside => CutContext::Outer,
                ToolOffset::Inside => CutContext::Inner,
                ToolOffset::None | ToolOffset::On => CutContext::Skip,
            },
            OpKind::Pocket { .. } => {
                if offset_signed_area(offset) > 0.0 {
                    CutContext::Inner
                } else {
                    CutContext::Outer
                }
            }
            OpKind::Engrave { .. }
            | OpKind::DragKnife { .. }
            | OpKind::Drill { .. }
            | OpKind::Thread { .. }
            | OpKind::Chamfer { .. }
            | OpKind::Helix
            // 8n4k / rxm9: program-only kinds (Pause / Homing /
            // Probe / CycleMarker / GcodeInclude) never reach this
            // winding pass — they emit inline above run_per_op's
            // body marker — but list them explicitly so a future
            // kind doesn't fall through to a stale arm.
            | OpKind::Pause { .. }
            | OpKind::Homing { .. }
            | OpKind::Probe { .. }
            | OpKind::CycleMarker { .. }
            | OpKind::GcodeInclude { .. }
            | OpKind::VCarve { .. }
            // 3g6u/b7qz: T-slot and dovetail ride the centerline (no
            // inside/outside winding to enforce) just like Engrave.
            | OpKind::TSlot { .. }
            | OpKind::Dovetail { .. }
            // f60x: relief surfacing has its own drop-cutter driver and
            // never enters the offset cascade — no winding to enforce.
            | OpKind::ReliefMill { .. } => CutContext::Skip,
        }
    };
    for offset in offsets.iter_mut() {
        let ctx = context_for(offset);
        // level=0 is the wall-defining pass for both Pocket and Profile
        // (single-pass profile is itself the finishing pass).
        let dir = if offset.level == 0 { finish } else { main };
        enforce_winding(offset, ctx, dir, spindle);
    }
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
        if !point_in_polygon_pts(polygon, px, py) {
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
            if point_in_polygon_pts(isl, px, py) {
                return true;
            }
        }
        if segment_intersects_polygon_edges(a, b, isl) {
            return true;
        }
    }
    false
}

pub(crate) fn point_in_polygon_pts(verts: &[Point2], x: f64, y: f64) -> bool {
    let n = verts.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        if (a.y - b.y).abs() < 1e-12 {
            continue;
        }
        let (lo, hi) = if a.y < b.y { (a, b) } else { (b, a) };
        if y < lo.y - 1e-12 || y >= hi.y - 1e-12 {
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

// ─── conversions ────────────────────────────────────────────────────────────

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
            out.push(Segment::arc(start, end, v0.bulge, None, layer, color));
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
/// dtf1: the prior threshold `r < 0.95 * tool_radius` left a dead zone
/// for circles whose radius sat in `[0.95·r, r)` — too narrow for the
/// inward-offset cascade (which collapsed to empty geometry) but too wide
/// to drill under the strict bound. Result: such holes were silently
/// dropped. The threshold was extended to `r < 0.999 * tool_radius` so
/// any circle that won't pocket gets a drill substitution at its centre.
///
/// hnc1: the previous strict `<` left an exact-fit case `r == tool_radius`
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
// juvx: linear reflex-corner walk; splitting into helpers would force a
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

    // fksa: derive an adaptive `perp_tol` from the boundary's bbox
    // diagonal. The prior fixed 0.25 mm tolerance was tuned for desktop
    // CNC scales (cm/dm); at sub-mm jewelry / engraving scales (5 mm
    // object with a 0.3 mm endmill) it was wider than the entire
    // workpiece, picking the nearest WRONG wall as the dip target.
    // Pattern: max(1e-3 mm, 1e-3 × bbox_diag) — same shape as the sj4t
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
        // 5nij: prior implementation only tested vertex ENDPOINTS — fine
        // for tiny test geometries where every wall is short enough that
        // a vertex lands near the bisector ray, but real CAD parts have
        // long flat walls whose endpoints sit far from the ray. We now
        // intersect each boundary segment as a line-segment-vs-ray test
        // so long-wall reflex corners get their dip too.
        //
        // fksa: perp_tol is now derived ONCE from the boundary bbox
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point2;

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

    /// hx74: a short (< 3 pts) ring inside the cascade was previously
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

    /// kqsl + kc86: a spiral pocket with an island in the bridge path
    /// must NOT carve through the island. The bridge-containment guard
    /// rejects bridges that cross any island; on rejection
    /// `stitch_rings_to_polyline` sweeps OTHER candidate start vertices
    /// on the next ring (kc86), and only when EVERY candidate fails
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

    /// kc86: when the FIRST candidate start vertex would put a bridge
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

    /// kqsl helper: `bridge_crosses_any_island` detects a bridge that
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

    /// knd4 helper: `inflate_islands_by_tool_radius` produces an
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
            point_in_polygon_pts(&inflated[0], 0.0, 0.0),
            "island center (0,0) must lie inside the inflated boundary"
        );
    }

    /// knd4 unit test: the cam-layer `pocket_zigzag` documents its
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

        // RAW island fed in (the pre-knd4 broken contract): scanlines
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

        // INFLATED island fed in (the post-knd4 fixed contract): no
        // centerline endpoint sits within tool_radius - eps of the raw
        // wall. The pocket emitter trims scanlines to a Minkowski-sum
        // boundary that's tool_r outboard of the raw wall.
        //
        // Slack budget: clipper2 inflates with EndType::Polygon +
        // JoinType::Round at `arc_tol = 0.25`, so the rounded corners
        // of the inflated polygon are chord-approximated. A scanline
        // endpoint sampled along a chord between two arc vertices
        // sits up to `arc_tol` inside the true tool_radius circle —
        // a sub-mm manufacturing approximation, not a knd4 regression.
        // We allow ~arc_tol of slack. Pre-knd4 the gouge was a full
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

    /// q57s: a left-hand spindle (`Ccw`, M4 mode) flips the geometric
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

    /// q57s symmetric case: outer+conventional on a left-hand spindle
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

    /// rt1.9: angled zigzag produces strokes oriented at the given
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

    /// gp2a: a 50×50 pocket with a 10×10 island in the centre — the
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

    /// dtf1 regression: a circle whose radius sits in the previously-dead
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

    /// hnc1 regression: a closed circle whose radius EXACTLY equals the
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

    /// hnc1 boundary: a circle slightly LARGER than the tool (within
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

    /// axhd regression: a U-shaped pocket's zigzag joiner that would
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

    /// 5nij regression: an L-shaped boundary with long walls (>= 30mm
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

    /// z4t6 regression: the thread-local panic sink starts empty, and
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

    /// c6ej regression: a polygon whose top edge grazes a scanline at a
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
        // After coalescing the result must be even.
        assert_eq!(
            xs.len() % 2,
            0,
            "expected even count after coalesce, got {}: {xs:?}",
            xs.len()
        );
    }

    /// 06m5 regression: a Pocket op with `nocontour=true` and the Zigzag
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

    /// cpym regression: a fine-finish stride (0.05 mm, well below the
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
            "fine-stride raster ({fine_strokes} strokes at 0.05 mm) should be much denser than coarse ({coarse_strokes} at 0.5 mm) — pre-cpym both clamped to 0.1 mm"
        );
        // No degeneracy warning at 0.05 mm — that's well above the 1e-6
        // mm floor.
        assert!(
            take_zigzag_stride_degenerate().is_empty(),
            "0.05 mm stride must not record a degeneracy event — only sub-fp strides do"
        );
    }

    /// cpym regression: a truly degenerate stride (sub-fp) must record
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

    /// a7v4 regression: an island that wholly spans one or more
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
    /// Pre-a7v4: `flip` toggled on every empty row in the band ⇒ the
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
        // Post-a7v4: with 9 empty rows in the band (odd count),
        // pre-fix would flip parity 9 times → next non-empty row
        // matches dir_below. Post-fix the band is parity-neutral →
        // next non-empty row is OPPOSITE to dir_below (the LAST
        // non-empty row's toggle still applies). Assert opposite.
        assert!(
            (dir_below + dir_above).abs() < 0.5,
            "a7v4 regression: first row above empty-band must run opposite to last row below — got dir_below={dir_below}, dir_above={dir_above}"
        );
    }

    /// sbtf regression: at high overlap (`xy_step` < `tool_radius`) the
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
        // the raw island wall than the bare knd4 inflation.
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
        // Raw island bbox min_x = 20. knd4 ≈ 18 (tool_r=2 outward).
        // sbtf over-inflate ≈ 18 - (tool_r - step) = 16.4.
        assert!(
            omin + 0.05 < kmin,
            "sbtf over-inflate must extend further than knd4 (over={omin:.3} vs knd4={kmin:.3})"
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
            "cutter EDGE intrudes into raw island by {:.3} mm (nearest centerline {:.3}, tool_r {tool_r}) — sbtf regression",
            -edge_clearance,
            nearest
        );
    }

    /// fksa regression: at sub-mm scale (5 mm part, 0.3 mm endmill) the
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
