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
    pub layer: String,
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
    let layer = obj.layer.clone();
    let Ok(offsets) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pline.parallel_offset(delta)
    })) else {
        tracing::warn!(
            "parallel_offset on layer '{}' panicked in cavalier_contours; skipping",
            layer
        );
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

/// Generate a zigzag (raster) pocket fill within `boundary`. The fill is
/// a series of horizontal sweep lines at the given Y `stride`, each
/// segment trimmed to the polygon's interior. Adjacent strokes are
/// joined at their endpoints to form a single open polyline (returns a
/// chain of segments). `stride` is the lateral distance between
/// consecutive raster lines — typically `tool_diameter * (1 - overlap)`.
/// `tool_diameter` is needed separately to inset the rasters by half a
/// tool diameter from the polygon edges so the cutter doesn't carve
/// past the boundary.
#[must_use]
pub fn pocket_zigzag(boundary: &[Point2], stride: f64, tool_diameter: f64) -> Vec<Segment> {
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

    let mut out = Vec::new();
    let mut prev_end: Option<Point2> = None;
    let mut flip = false;
    let mut y = min_y + tool_diameter * 0.5;
    while y <= max_y - tool_diameter * 0.5 + 1e-9 {
        let crossings = horizontal_crossings(boundary, y, min_x, max_x);
        // Group into entry/exit pairs (even-odd rule).
        let mut iter = crossings.chunks_exact(2);
        let mut strokes: Vec<(Point2, Point2)> = Vec::new();
        for pair in iter.by_ref() {
            let (a, b) = (pair[0], pair[1]);
            // Inset both ends by half a tool diameter so we don't carve
            // outside the polygon interior at the row endpoints. The
            // inset is clamped to half the stroke length so a narrow
            // crossing collapses to a single point rather than going
            // negative.
            let lo = a.min(b);
            let hi = a.max(b);
            let inset = (tool_diameter * 0.5).min((hi - lo) * 0.5);
            let new_a = lo + inset;
            let new_b = hi - inset;
            if new_b <= new_a + 1e-6 {
                continue;
            }
            strokes.push((Point2::new(new_a, y), Point2::new(new_b, y)));
        }
        if flip {
            strokes.reverse();
            for s in &mut strokes {
                std::mem::swap(&mut s.0, &mut s.1);
            }
        }
        flip = !flip;
        for (a, b) in strokes {
            if let Some(prev) = prev_end {
                if prev.distance(a) > 1e-6 {
                    out.push(Segment::line(prev, a, "0", 7));
                }
            }
            out.push(Segment::line(a, b, "0", 7));
            prev_end = Some(b);
        }
        y += stride;
    }
    out
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
        if rings.len() > 1024 {
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

/// Rotate each CLOSED offset's segment list so the first segment's
/// start is closest to `ap` (rt1.26 / Estlcam Anfahrpunkt). Open
/// offsets (zigzag / spiral / trochoidal strokes) are left alone —
/// their winding has no rotational symmetry to exploit. The cutter's
/// plunge / lead-in then happens at the user-picked entry XY.
///
/// When the chosen `ap` is far from every closed offset, the function
/// still rotates (picks the nearest vertex regardless of distance);
/// the caller can validate or warn separately.
pub fn rotate_offsets_to_approach_point(offsets: &mut [PolylineOffset], ap: (f64, f64)) {
    let ap_pt = Point2::new(ap.0, ap.1);
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
        if let Some((i, _)) = best {
            if i > 0 {
                offset.segments.rotate_left(i);
            }
        }
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
    Zigzag,
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
            PocketEmit::Zigzag => {
                // Zigzag stride is the same step semantics — distance
                // between raster lines. Default ~50% overlap.
                let strokes = pocket_zigzag(&pts, step.max(0.1), tool_radius * 2.0);
                if !strokes.is_empty() {
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
                let rings = pocket_cascade_with_islands(&pts, islands, step);
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
                match stitch_rings_to_spiral(&rings, &offset.layer, offset.color) {
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
                            "spiral pocket: bridge crosses pocket boundary in non-convex shape, falling back to cascade"
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

        let rings = pocket_cascade_with_islands(&pts, islands, step);
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
                segs.push(Segment::line(win[0], win[1], &offset.layer, offset.color));
            }
            // Close the ring.
            if let (Some(first), Some(last)) = (ring.first(), ring.last()) {
                if first.distance(*last) > 1e-6 {
                    segs.push(Segment::line(*last, *first, &offset.layer, offset.color));
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
/// boundary — happens on non-convex shapes (L / U / +) where a
/// straight bridge can leave the safe interior. The caller should fall
/// back to cascade emission (separate closed rings, no bridges).
fn stitch_rings_to_spiral(rings: &[Vec<Point2>], layer: &str, color: i32) -> Option<Vec<Segment>> {
    let pts = stitch_rings_to_polyline(rings)?;
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
/// previous ring's end so bridges between rings are short. Returns
/// None when any bridge would cross the outer ring (the inset pocket
/// boundary) — same containment guard that protects the spiral
/// strategy on non-convex shapes.
pub(crate) fn stitch_rings_to_polyline(rings: &[Vec<Point2>]) -> Option<Vec<Point2>> {
    if rings.is_empty() {
        return Some(Vec::new());
    }
    let outer = &rings[0];
    let mut out: Vec<Point2> = Vec::new();
    let mut last_end: Option<Point2> = None;
    for ring in rings {
        if ring.len() < 3 {
            continue;
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
    if radius >= tool_radius * 0.95 {
        return None;
    }
    Some(PolylineOffset {
        segments: vec![Segment::point(center, &obj.layer, obj.color)],
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

        // Probe boundary endpoints along the outward ray.
        let mut nearest: Option<f64> = None;
        for seg in boundary_segments {
            for p1 in [seg.start, seg.end] {
                let dx = p1.x - cur.x;
                let dy = p1.y - cur.y;
                let along = dx * out.0 + dy * out.1;
                if along <= 1e-6 {
                    continue;
                }
                let perp = (dx * out.1 - dy * out.0).abs();
                if perp > 0.25 {
                    continue;
                }
                if nearest.map_or(true, |c| along < c) {
                    nearest = Some(along);
                }
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
        let segs = pocket_zigzag(&boundary, 1.8, 2.0);
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
        for s in &segs {
            for pt in [s.start, s.end] {
                assert!(pt.x >= -0.01 && pt.x <= 20.01);
                assert!(pt.y >= -0.01 && pt.y <= 20.01);
            }
        }
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
        let segs = pocket_zigzag(&boundary, 2.0, 3.0);
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
}
