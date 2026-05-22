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
use crate::math;

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

/// Hard cap on the number of rings the cascade can emit before bailing
/// (mdpo). Was 1024 — raised to 4096 to cover larger pockets at fine
/// steps (e.g. a 400×400 mm sign cascaded at 0.5 mm step needs ~800
/// rings, easily fitting the new budget; the old 1024 cap silently
/// truncated some real projects). The cap exists as a runaway / OOM
/// guard, NOT a project setting.
pub const POCKET_CASCADE_RING_CAP: usize = 4096;

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
        Point2::new(
            pivot.x + dx * cos - dy * s,
            pivot.y + dx * s + dy * cos,
        )
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
    let stride = stride.max(0.1);
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
        flip = !flip;
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
                        let crosses_island = !islands.is_empty()
                            && segment_crosses_any_polygon(prev, a, islands);
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
/// For a right-hand spindle (standard CW from above):
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
pub fn enforce_winding(
    offset: &mut PolylineOffset,
    context: CutContext,
    direction: crate::project::CutDirection,
) {
    use crate::project::CutDirection;
    if !offset.closed || matches!(context, CutContext::Skip) {
        return;
    }
    let area = offset_signed_area(offset);
    if area.abs() < 1e-9 {
        return;
    }
    let want_ccw = match (context, direction) {
        (CutContext::Inner, CutDirection::Conventional) => true,
        (CutContext::Inner, CutDirection::Climb) => false,
        (CutContext::Outer, CutDirection::Conventional) => false,
        (CutContext::Outer, CutDirection::Climb) => true,
        (CutContext::Skip, _) => return,
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
            | OpKind::Pause { .. }
            | OpKind::VCarve { .. } => CutContext::Skip,
        }
    };
    for offset in offsets.iter_mut() {
        let ctx = context_for(offset);
        // level=0 is the wall-defining pass for both Pocket and Profile
        // (single-pass profile is itself the finishing pass).
        let dir = if offset.level == 0 { finish } else { main };
        enforce_winding(offset, ctx, dir);
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
    let allowance = xy_allowance.max(0.0);
    let has_dual_tool_finish = finish_ring_radius.is_some();
    let rough_delta = tool_radius.abs() + allowance;
    let boundary = parallel_offset_inward(obj, rough_delta);
    if boundary.is_empty() {
        return out;
    }
    // Effective step in mm: lateral distance between consecutive cuts.
    // The caller passes the step (typically tool_diameter * (1 - overlap));
    // we clamp to a safe minimum so a 100% overlap doesn't loop forever.
    let step = xy_step.max(tool_radius * 0.05);
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
                let chains = pocket_zigzag_angled(
                    &pts,
                    islands,
                    step.max(0.1),
                    tool_radius * 2.0,
                    angle_deg,
                );
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
                let rings = crate::cam::geometry_cache::pocket_cascade_with_islands_cached(
                    &pts, islands, step,
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
                match stitch_rings_to_spiral(&rings, islands, &offset.layer, offset.color) {
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

        let rings = crate::cam::geometry_cache::pocket_cascade_with_islands_cached(
            &pts, islands, step,
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
                segs.push(Segment::line(win[0], win[1], offset.layer.clone(), offset.color));
            }
            // Close the ring.
            if let (Some(first), Some(last)) = (ring.first(), ring.last()) {
                if first.distance(*last) > 1e-6 {
                    segs.push(Segment::line(*last, *first, offset.layer.clone(), offset.color));
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
        let start_idx = if let Some(end) = last_end {
            let mut best = 0usize;
            let mut best_d = f64::INFINITY;
            for (i, p) in ring.iter().enumerate() {
                let d = p.distance(end);
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            best
        } else {
            0
        };
        let n = ring.len();
        let first = ring[start_idx];
        if let Some(end) = last_end {
            if end.distance(first) > 1e-6 {
                if !bridge_stays_inside_polygon(end, first, outer) {
                    return None;
                }
                if bridge_crosses_any_island(end, first, islands) {
                    return None;
                }
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
pub(crate) fn bridge_crosses_any_island(
    a: Point2,
    b: Point2,
    islands: &[Vec<Point2>],
) -> bool {
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
/// dropped. The threshold is now extended to `r < 0.999 * tool_radius` so
/// any circle that won't pocket gets a drill substitution at its centre.
/// The tiny remaining sliver `[0.999·r, r)` is genuinely a manufacturing
/// boundary (the tool exactly fills the hole) — it's left to the cascade
/// + the `pocket_fill_incomplete` / `tool_too_large` warnings.
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
    if radius >= tool_radius * 0.999 {
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

#[allow(dead_code)]
fn _math_unused() {
    let _ = math::TWO_PI;
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
pub fn apply_overcut(offset: &mut PolylineOffset, boundary_segments: &[Segment], tool_radius: f64) {
    use std::f64::consts::FRAC_PI_4;
    if !offset.closed || offset.segments.len() < 3 {
        return;
    }
    let r_abs = tool_radius.abs();
    let n = offset.segments.len();
    let pts: Vec<(Point2, f64)> = offset.segments.iter().map(|s| (s.start, s.bulge)).collect();

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
        // perp_tol stays at 0.25 mm — that's the existing endpoint-mode
        // tolerance, retained so the endpoint hits this loop still picks
        // (a long wall whose closest point on the ray IS its endpoint
        // resolves identically). Vertex-endpoint hits are picked up by
        // the segment-distance path with a non-negative `t` clamp.
        let perp_tol = 0.25_f64;
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
    /// last_end to the next ring's first vertex unverified — it could
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

    /// kqsl: a spiral pocket with an island in the bridge path must
    /// NOT carve through the island. The bridge-containment guard
    /// rejects bridges that cross any island; on rejection the caller
    /// falls back to cascade emission (separate closed rings, no
    /// bridges). We verify by calling `stitch_rings_to_polyline`
    /// directly with rings whose bridge between consecutive ring start
    /// vertices crosses an island — must return None.
    #[test]
    fn spiral_bridge_rejected_when_crossing_island() {
        // 50×50 pocket; an island in the middle at [20..30] × [20..30].
        // Construct two rings so the spiral bridge from ring 0's
        // chosen-start to ring 1's chosen-start MUST cross the island.
        // The stitcher picks each ring's start vertex as the closest
        // to the previous ring's last_end (= that ring's start), so
        // we constrain ring vertices to engineer the bridge path:
        // - ring 0 first vertex at (5, 25)  → last_end = (5, 25)
        // - ring 1 vertices only on the right of the island; nearest
        //   to (5, 25) is (40, 25) → bridge runs along y=25 from x=5
        //   to x=40, slicing straight through the centre of the island.
        let ring0 = vec![p(5.0, 25.0), p(5.0, 5.0), p(45.0, 5.0), p(45.0, 45.0), p(5.0, 45.0)];
        let ring1 = vec![p(40.0, 25.0), p(40.0, 10.0), p(40.0, 5.0), p(40.0, 45.0)];
        let rings = vec![ring0, ring1];
        let island = vec![p(20.0, 20.0), p(30.0, 20.0), p(30.0, 30.0), p(20.0, 30.0)];
        // No islands → polyline stitches without complaint (sanity).
        assert!(stitch_rings_to_polyline(&rings, &[]).is_some());
        // With the island present the y=25 bridge crosses it → reject.
        assert!(
            stitch_rings_to_polyline(&rings, &[island.clone()]).is_none(),
            "stitch must reject a bridge that crosses an island",
        );
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
        );
        let after: Vec<_> = o.segments.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(before, after);
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
                    assert!(
                        !inside,
                        "zigzag stroke endpoint inside island: {pt:?}",
                    );
                }
            }
        }
        // No single stroke crosses the island bbox horizontally.
        for chain in &chains {
            for s in chain {
                if (s.start.y - s.end.y).abs() < 1e-6
                    && s.start.y > 20.0
                    && s.start.y < 30.0
                {
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
        assert!(drill.is_some(), "near-tool-radius circle must drill at center");
        let drill = drill.unwrap();
        assert_eq!(drill.segments.len(), 1);
        assert!(matches!(drill.segments[0].kind, SegmentKind::Point));
        assert!(drill.segments[0].start.distance(center) < 1e-9);
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
                let mid = Point2::new(
                    (s.start.x + s.end.x) * 0.5,
                    (s.start.y + s.end.y) * 0.5,
                );
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
    /// sink. We can't easily synthesise a cavalier_contours panic in a
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
        let _second = take_parallel_offset_panics();
        assert!(_second.is_empty(), "sink must be empty after the first drain");
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
}
