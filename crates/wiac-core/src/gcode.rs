//! Gcode generation — port of viaConstructor's `machine_cmd.py` and the
//! three output plugins (gcode_grbl, gcode_linuxcnc, hpgl).
//!
//! `PostProcessor` is the trait every dialect implements; `emit_polylines`
//! is the dialect-agnostic orchestrator that walks offsets and writes
//! gcode through the trait.

use serde::{Deserialize, Serialize};

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::{LeadKind, MachineMode, Setup, ToolOffset, UnitSystem};
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::math;

pub mod grbl;
pub mod hpgl;
pub mod linuxcnc;
pub mod preview;

/// Generic post-processor trait. Stateful — implementations track the last
/// emitted XYZ/feedrate/spindle so they can delta-encode output.
pub trait PostProcessor {
    fn separation(&mut self) {}
    fn raw(&mut self, _cmd: &str) {}
    fn comment(&mut self, _text: &str) {}

    fn unit(&mut self, _unit: UnitSystem);
    fn absolute(&mut self, _active: bool) {}
    fn feedrate(&mut self, rate: u32);

    fn program_start(&mut self) {}
    fn program_end(&mut self) {}

    fn tool(&mut self, _number: u32) {}
    fn tool_offsets(&mut self, _offset: ToolOffset) {}
    fn machine_offsets(&mut self, _offsets: (f64, f64, f64), _soft: bool) {}

    fn coolant_mist(&mut self) {}
    fn coolant_flood(&mut self) {}
    fn coolant_off(&mut self) {}

    fn spindle_off(&mut self) {}
    fn spindle_cw(&mut self, speed: u32, pause_seconds: u32);
    fn spindle_ccw(&mut self, speed: u32, pause_seconds: u32);

    fn move_to(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>);
    fn linear(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>);
    fn arc_cw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    );
    fn arc_ccw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    );

    fn finish(&self) -> String;
}

/// Top-level orchestrator. Walks `offsets` and emits gcode through `post`.
/// Replaces `polylines2machine_cmd` from machine_cmd.py.
pub fn emit_polylines<P: PostProcessor>(
    setup: &Setup,
    offsets: &[PolylineOffset],
    post: &mut P,
) -> String {
    program_begin(setup, post);
    let mut last_pos = Point2::new(0.0, 0.0);
    emit_polylines_block(setup, offsets, post, &mut last_pos);
    program_end(setup, post);
    post.finish()
}

/// Header-only emit. Per-op pipeline drivers call this once at the start
/// of the program, then loop through each op calling
/// [`emit_polylines_block`], then close with [`emit_program_end`].
pub fn emit_program_begin<P: PostProcessor>(setup: &Setup, post: &mut P) {
    program_begin(setup, post);
}

/// Footer-only emit. Counterpart to [`emit_program_begin`].
pub fn emit_program_end<P: PostProcessor>(setup: &Setup, post: &mut P) {
    program_end(setup, post);
}

/// Cut-block emit — the per-offset loop without program-begin / -end. The
/// per-op driver calls this once per operation; the `setup` passed is the
/// op's *synthesized* setup (its tool + params), and `last_pos` is shared
/// across calls so the next op continues from where the previous one
/// finished.
pub fn emit_polylines_block<P: PostProcessor>(
    setup: &Setup,
    offsets: &[PolylineOffset],
    post: &mut P,
    last_pos: &mut Point2,
) {
    let order = order_offsets(setup, offsets, *last_pos);
    for &idx in &order {
        emit_offset(setup, &offsets[idx], post, last_pos);
    }
}

/// Decide the cut order for the offsets. Honors `setup.mill.objectorder`:
/// - `Unordered`  — input order, matches the upstream Python tool.
/// - `Nearest`    — greedy nearest-neighbor from current pen position;
///                  ties broken by deepest level (innermost) first so
///                  pocket cascades unwind from the inside out.
/// - `PerObject`  — group all offsets sharing source_object_idx, finish
///                  one object before starting the next; within a group
///                  use Nearest.
fn order_offsets(setup: &Setup, offsets: &[PolylineOffset], start: Point2) -> Vec<usize> {
    use crate::cam::setup::ObjectOrder;
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
            let mut group_of: std::collections::HashMap<usize, usize> = Default::default();
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

fn greedy_nearest(offsets: &[PolylineOffset], start: Point2) -> Vec<usize> {
    let refs: Vec<&PolylineOffset> = offsets.iter().collect();
    greedy_nearest_among(&refs, start)
}

fn greedy_nearest_among(offsets: &[&PolylineOffset], start: Point2) -> Vec<usize> {
    let n = offsets.len();
    if n == 0 {
        return Vec::new();
    }
    let mut taken = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut pen = start;
    for _ in 0..n {
        let mut best: Option<(usize, f64, u32)> = None;
        for (i, o) in offsets.iter().enumerate() {
            if taken[i] {
                continue;
            }
            let d = pen.distance(start_pos_of(o));
            // Tie-breaker: deeper levels first so pocket cascades unwind
            // inside-out (innermost ring before its parent contour).
            let level = o.level;
            let better = match best {
                None => true,
                Some((_, bd, bl)) => d < bd || (d == bd && level > bl),
            };
            if better {
                best = Some((i, d, level));
            }
        }
        let (chosen, _, _) = best.unwrap();
        taken[chosen] = true;
        order.push(chosen);
        pen = end_pos(offsets[chosen]);
    }
    order
}

fn start_pos_of(offset: &PolylineOffset) -> Point2 {
    offset
        .segments
        .first()
        .map(|s| s.start)
        .unwrap_or(Point2::new(0.0, 0.0))
}

fn end_pos(offset: &PolylineOffset) -> Point2 {
    offset
        .segments
        .last()
        .map(|s| s.end)
        .unwrap_or(Point2::new(0.0, 0.0))
}

fn program_begin<P: PostProcessor>(setup: &Setup, post: &mut P) {
    post.program_start();
    post.unit(setup.machine.unit);
    post.absolute(true);
    post.feedrate(setup.tool.rate_h);
    post.move_to(None, None, Some(setup.mill.fast_move_z));
}

fn program_end<P: PostProcessor>(setup: &Setup, post: &mut P) {
    post.move_to(None, None, Some(setup.mill.fast_move_z));
    post.spindle_off();
    if setup.tool.flood || setup.tool.mist {
        post.coolant_off();
    }
    post.program_end();
    let _ = setup;
}

/// Emit a single polyline offset (one cut pass per multi-pass step).
fn emit_offset<P: PostProcessor>(
    setup: &Setup,
    offset: &PolylineOffset,
    post: &mut P,
    last_pos: &mut Point2,
) {
    if offset.segments.is_empty() {
        return;
    }
    if setup.machine.comments {
        post.separation();
        post.comment(&format!(
            "object={} level={} pocket={} segments={}",
            offset.source_object_idx,
            offset.level,
            offset.is_pocket,
            offset.segments.len()
        ));
    }
    if setup.machine.mode == MachineMode::Mill {
        post.spindle_cw(setup.tool.speed, setup.tool.pause);
    }
    if setup.tool.flood {
        post.coolant_flood();
    }
    if setup.tool.mist {
        post.coolant_mist();
    }
    let start = offset.segments[0].start;
    // Lead-in (straight or arc) before the first cut.
    let approach = lead_in_point(setup, &offset.segments);
    if let Some(pre) = approach {
        post.move_to(Some(pre.x), Some(pre.y), Some(setup.mill.fast_move_z));
        post.linear(None, None, Some(0.0));
    } else {
        post.move_to(Some(start.x), Some(start.y), Some(setup.mill.fast_move_z));
        post.linear(None, None, Some(0.0));
    }

    multi_pass(setup, &offset.segments, &offset.tabs, post);

    if let Some(out) = lead_out_point(setup, &offset.segments) {
        post.linear(Some(out.x), Some(out.y), None);
    }
    post.linear(None, None, Some(setup.mill.fast_move_z));

    *last_pos = offset.segments.last().map(|s| s.end).unwrap_or(start);
}

fn multi_pass<P: PostProcessor>(
    setup: &Setup,
    segments: &[Segment],
    tabs: &[crate::cam::offsets::TabPoint],
    post: &mut P,
) {
    let total_depth = setup.mill.depth;
    let step = if setup.mill.step.abs() < 1e-9 {
        total_depth
    } else if setup.mill.step > 0.0 {
        -setup.mill.step
    } else {
        setup.mill.step
    };
    let tabs_z = total_depth + setup.tabs.height.abs();
    let tab_radius = (setup.tool.diameter * 0.5).max(0.5);
    // Ramp profile only applies when tab_type=Ramp. ramp_length is the
    // horizontal distance over which Z transitions between cut_z and
    // tabs_z at the configured angle. Computed once per pass below.
    use crate::cam::setup::TabType;
    let tab_ramp_angle_deg = match setup.tabs.tab_type {
        TabType::Ramp => Some(setup.tabs.ramp_angle_deg.clamp(0.5, 89.0)),
        TabType::Rectangle => None,
    };

    // Helix mode replaces the straight Z plunge between passes with a
    // spiral down the contour — gentler on small-diameter tools and
    // produces cleaner closed-contour entries. Only meaningful for
    // closed paths; for open paths we silently fall back to straight.
    let closed_path = is_closed_path(segments);
    let helix = setup.mill.helix_mode && closed_path;
    // Ramp plunge: descend Z while walking the first `ramp_length` of
    // the path, then continue at depth. Computed once per pass from
    // `step / tan(angle)`. Disabled when helix is active (the helix
    // already provides a ramped descent over the full path).
    use crate::cam::setup::PlungeStrategy;
    // Helix-entry plunge: a start-of-cut spiral descent on a small
    // circle inside the closed pocket boundary, distinct from the
    // path-wide `helix_mode` above. Only meaningful for closed paths
    // when the helix circle (radius ≥ tool_radius) fits inside the
    // boundary polygon — otherwise we fall back to Ramp / Direct.
    let helix_entry: Option<HelixEntry> = match setup.mill.plunge {
        PlungeStrategy::Helix {
            angle_deg,
            radius_mm,
        } if closed_path => {
            let tool_radius = setup.tool.diameter * 0.5;
            plan_helix_entry(segments, radius_mm, tool_radius, angle_deg)
        }
        _ => None,
    };
    let ramp_angle_deg = match setup.mill.plunge {
        PlungeStrategy::Ramp { angle_deg } => Some(angle_deg.clamp(0.5, 45.0)),
        PlungeStrategy::Helix { angle_deg, .. } if helix_entry.is_none() => {
            // Helix didn't fit (radius too small or circle outside
            // boundary) — fall back to Ramp at the same angle so the
            // user still gets a non-vertical entry.
            Some(angle_deg.clamp(0.5, 45.0))
        }
        _ => None,
    };
    let total_path_len: f64 = segments
        .iter()
        .map(|s| match s.kind {
            SegmentKind::Line | SegmentKind::Point => s.start.distance(s.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(s),
        })
        .sum();

    // For the helix-vs-direct decision we treat the first pass as
    // having no prev_z (no spiral from somewhere), but the ramp plunge
    // wants to descend from start_depth on the first pass too — that's
    // when it matters most. We track them with separate state.
    let mut prev_z: Option<f64> = None;
    let mut ramp_from: f64 = setup.mill.start_depth;
    let mut z = (setup.mill.start_depth + step).max(total_depth);
    loop {
        let pass_uses_tabs = setup.tabs.active && !tabs.is_empty() && z < tabs_z;
        if let (true, Some(pz)) = (helix, prev_z) {
            // Spiral from prev_z down to z while tracing the segments.
            post.feedrate(setup.tool.rate_h);
            emit_helix_pass(segments, pz, z, post);
        } else if let Some(plan) = helix_entry.as_ref().filter(|_| !pass_uses_tabs) {
            // Start-of-cut helical entry: spiral down on a small
            // circle inside the pocket boundary, then walk to the
            // path start and continue normally. Only the descent
            // portion is helix-driven; the rest of the pass uses the
            // ordinary path emit at constant z.
            let pz = ramp_from;
            post.feedrate(setup.tool.rate_h);
            emit_helix_entry(plan, pz, z, post);
            // Cut from helix landing point to the path's actual start.
            let start = segments.first().map(|s| s.start).unwrap_or(plan.center);
            post.linear(Some(start.x), Some(start.y), Some(z));
            let dragoff = setup.tool.dragoff.unwrap_or(0.0);
            emit_path_with_dragoff(segments, dragoff, post);
        } else if let Some(angle) = ramp_angle_deg.filter(|_| !pass_uses_tabs) {
            // Ramp plunge: descend from pz to z over the first
            // ramp_length of arc length, then continue at z for the
            // remainder. emit_ramp_pass walks ALL segments — the ramp
            // IS the full pass — so we don't follow it with another
            // path emit. Tabs-needed passes fall through to the direct
            // branch below to keep the tabs walker authoritative.
            let pz = ramp_from;
            let dz = (pz - z).abs();
            let ramp_length = if dz < 1e-9 {
                0.0
            } else {
                dz / angle.to_radians().tan()
            };
            if ramp_length > 1e-6 && total_path_len >= ramp_length {
                post.feedrate(setup.tool.rate_h);
                emit_ramp_pass(segments, pz, z, ramp_length, post);
            } else {
                // Path too short for the ramp → fall back to straight
                // plunge so the user still gets a valid program.
                post.feedrate(setup.tool.rate_v);
                post.linear(None, None, Some(z));
                post.feedrate(setup.tool.rate_h);
                let dragoff = setup.tool.dragoff.unwrap_or(0.0);
                emit_path_with_dragoff(segments, dragoff, post);
            }
        } else {
            post.feedrate(setup.tool.rate_v);
            post.linear(None, None, Some(z));
            post.feedrate(setup.tool.rate_h);
            if pass_uses_tabs {
                emit_path_with_tabs(
                    segments,
                    tabs,
                    tabs_z,
                    z,
                    tab_radius,
                    tab_ramp_angle_deg,
                    post,
                );
            } else {
                let dragoff = setup.tool.dragoff.unwrap_or(0.0);
                emit_path_with_dragoff(segments, dragoff, post);
            }
        }
        prev_z = Some(z);
        ramp_from = z;
        if z <= total_depth + 1e-9 {
            break;
        }
        z = (z + step).max(total_depth);
    }
    // Ramp plunge leaves a sloped section at the start of every pass —
    // the cells under the ramp sit at progressively descending Z, NOT
    // at the pass's final depth. Earlier passes' slopes are re-cut by
    // later passes (which start at the previous z and ramp deeper),
    // but the LAST pass's slope persists as material left in the
    // pocket. Add a constant-depth cleanup walk at total_depth to
    // sweep that slope flat. Skipped on tabs-active paths because the
    // tabs walker already lifts/lowers Z based on its own logic and a
    // bonus pass would double-cut.
    let needs_ramp_cleanup = ramp_angle_deg.is_some()
        && !(setup.tabs.active && !tabs.is_empty())
        && total_path_len > 1e-6;
    if needs_ramp_cleanup {
        post.feedrate(setup.tool.rate_h);
        let dragoff = setup.tool.dragoff.unwrap_or(0.0);
        emit_path_with_dragoff(segments, dragoff, post);
    }
}

/// Walk `segments` while linearly descending Z from `from_z` to `to_z`
/// over the first `ramp_length` of arc length, then continue at `to_z`
/// for the remainder.
///
/// Line segments are *split* when they cross the ramp_length boundary
/// so the ramp angle is honored even if the first segment is longer
/// than ramp_length. Arc segments aren't split mid-arc (the math gets
/// fiddly); the ramp simply finishes at the first arc boundary that
/// crosses ramp_length and the rest of the path proceeds at to_z.
fn emit_ramp_pass<P: PostProcessor>(
    segments: &[Segment],
    from_z: f64,
    to_z: f64,
    ramp_length: f64,
    post: &mut P,
) {
    if ramp_length < 1e-9 {
        post.linear(None, None, Some(to_z));
        return;
    }
    let mut consumed = 0.0;
    let interp_z = |consumed: f64| -> f64 {
        let t = (consumed / ramp_length).min(1.0);
        from_z + (to_z - from_z) * t
    };
    for seg in segments {
        let seg_len = match seg.kind {
            SegmentKind::Line | SegmentKind::Point => seg.start.distance(seg.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(seg),
        };
        // Split this segment at ramp_length boundary if it's a line
        // and it crosses the boundary.
        let crosses_boundary = consumed < ramp_length
            && consumed + seg_len > ramp_length
            && matches!(seg.kind, SegmentKind::Line);
        if crosses_boundary {
            let remaining_ramp = ramp_length - consumed;
            let frac = remaining_ramp / seg_len;
            let mid_x = seg.start.x + (seg.end.x - seg.start.x) * frac;
            let mid_y = seg.start.y + (seg.end.y - seg.start.y) * frac;
            // Emit the ramp portion at to_z (we just arrived at depth)
            // then continue to the segment end at to_z.
            post.linear(Some(mid_x), Some(mid_y), Some(to_z));
            post.linear(Some(seg.end.x), Some(seg.end.y), Some(to_z));
            consumed += seg_len;
            continue;
        }
        consumed += seg_len;
        let z = interp_z(consumed);
        match seg.kind {
            SegmentKind::Line => post.linear(Some(seg.end.x), Some(seg.end.y), Some(z)),
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), Some(z)),
            SegmentKind::Arc | SegmentKind::Circle => {
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if seg.bulge > 0.0 {
                    post.arc_ccw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                } else {
                    post.arc_cw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                }
            }
        }
    }
}

fn is_closed_path(segments: &[Segment]) -> bool {
    if segments.len() < 3 {
        return false;
    }
    let first = segments.first().unwrap().start;
    let last = segments.last().unwrap().end;
    first.distance(last) < 1e-3
}

/// Emit one revolution around `segments` while linearly descending Z from
/// `from_z` to `to_z`. Each segment endpoint gets the interpolated Z so
/// the spiral stays smooth even with arc segments.
fn emit_helix_pass<P: PostProcessor>(segments: &[Segment], from_z: f64, to_z: f64, post: &mut P) {
    let total_len: f64 = segments
        .iter()
        .map(|s| match s.kind {
            SegmentKind::Line | SegmentKind::Point => s.start.distance(s.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(s),
        })
        .sum();
    if total_len < 1e-9 {
        post.linear(None, None, Some(to_z));
        return;
    }
    let mut consumed = 0.0;
    for seg in segments {
        let seg_len = match seg.kind {
            SegmentKind::Line | SegmentKind::Point => seg.start.distance(seg.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(seg),
        };
        consumed += seg_len;
        let t = consumed / total_len;
        let z = from_z + (to_z - from_z) * t;
        match seg.kind {
            SegmentKind::Line => post.linear(Some(seg.end.x), Some(seg.end.y), Some(z)),
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), Some(z)),
            SegmentKind::Arc | SegmentKind::Circle => {
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if seg.bulge > 0.0 {
                    post.arc_ccw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                } else {
                    post.arc_cw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                }
            }
        }
    }
}

/// Plan for a start-of-cut helical entry: where to drop, how far
/// horizontally, how deep per revolution. Produced by
/// `plan_helix_entry` and consumed by `emit_helix_entry`.
#[derive(Debug, Clone, Copy)]
struct HelixEntry {
    /// XY center of the helix circle.
    center: Point2,
    /// Helix radius in mm.
    radius: f64,
    /// Z drop per full revolution (always positive).
    dz_per_rev: f64,
    /// True if the helix winds CCW around `center` when viewed from +Z.
    /// Matches the polygon winding so the cutter spirals "into" the
    /// material in the same direction the path will run.
    ccw: bool,
    /// Starting angle of the helix on the circle (radians, atan2 of
    /// (path_start - center)). Helix returns to this angle at landing
    /// so the post-helix walk to path_start is the shortest.
    start_angle: f64,
}

/// Build a helix entry plan for `segments` if the geometry supports it.
/// Returns None when:
///   - radius < tool_radius (helix would carve nothing the cutter
///     doesn't already cover from the path)
///   - the helix circle doesn't fit inside the polygon (any of 8
///     sample points lies outside the boundary)
///   - the path is too short / not closed (caller already checks
///     closed; this is defensive)
///
/// The helix center is the polygon centroid offset back toward the
/// path start so the cutter lands near where the cut begins (and the
/// post-helix walk to path-start is short). The helix circle must fit
/// entirely inside the polygon — otherwise the spiral would carve into
/// the wall on its way down.
fn plan_helix_entry(
    segments: &[Segment],
    radius_mm: f64,
    tool_radius: f64,
    angle_deg: f64,
) -> Option<HelixEntry> {
    if segments.is_empty() {
        return None;
    }
    if radius_mm < tool_radius - 1e-9 {
        return None;
    }
    let radius = radius_mm.max(1e-6);
    let angle = angle_deg.clamp(0.5, 45.0).to_radians();
    let dz_per_rev = (2.0 * std::f64::consts::PI * radius * angle.tan()).abs();
    if dz_per_rev < 1e-9 {
        return None;
    }
    // Polygon vertices (line endpoints; arc endpoints, no mid-arc
    // sampling). Sufficient for the shoelace + ray-cast checks below.
    let verts = polygon_vertices(segments);
    if verts.len() < 3 {
        return None;
    }
    let area = polygon_signed_area(&verts);
    let ccw = area > 0.0;
    // Centroid as the helix center. Robust default for convex
    // pockets; for skinny / non-convex shapes the point-in-polygon
    // sampling below catches the bad cases and we fall back to Ramp.
    // We don't try to pull the center toward the path start — doing so
    // can push the helix circle into a wall on small or
    // sharply-cornered pockets, which is exactly the failure mode we
    // need helical entry to avoid. The post-helix walk to the path
    // start runs at constant z through the pocket interior, which is
    // safe because the boundary path itself is already inset from the
    // walls by tool_radius.
    let centroid = polygon_centroid(&verts);
    let path_start = segments[0].start;
    let center = centroid;
    // Sample 8 points on the helix circle; all must be inside the
    // polygon for the helix to be safe.
    let samples = 8;
    for i in 0..samples {
        let theta = (i as f64) * std::f64::consts::TAU / (samples as f64);
        let px = center.x + radius * theta.cos();
        let py = center.y + radius * theta.sin();
        if !point_in_polygon(&verts, px, py) {
            return None;
        }
    }
    // Start angle: vector from helix center toward the path start.
    // The helix lands at (center + radius·(cosθ, sinθ)) where θ =
    // start_angle, then walks the short remaining distance to the
    // path start.
    let start_angle = (path_start.y - center.y).atan2(path_start.x - center.x);
    Some(HelixEntry {
        center,
        radius,
        dz_per_rev,
        ccw,
        start_angle,
    })
}

/// Polygon centroid via the shoelace formula. For a degenerate
/// (zero-area) polygon, returns the average of the vertices.
fn polygon_centroid(verts: &[Point2]) -> Point2 {
    let n = verts.len();
    if n == 0 {
        return Point2::new(0.0, 0.0);
    }
    let mut a = 0.0;
    let mut cx = 0.0;
    let mut cy = 0.0;
    for i in 0..n {
        let p = verts[i];
        let q = verts[(i + 1) % n];
        let cross = p.x * q.y - q.x * p.y;
        a += cross;
        cx += (p.x + q.x) * cross;
        cy += (p.y + q.y) * cross;
    }
    a *= 0.5;
    if a.abs() < 1e-9 {
        let mut sx = 0.0;
        let mut sy = 0.0;
        for p in verts {
            sx += p.x;
            sy += p.y;
        }
        return Point2::new(sx / n as f64, sy / n as f64);
    }
    Point2::new(cx / (6.0 * a), cy / (6.0 * a))
}

/// Emit the helical entry: descend from `from_z` to `to_z` on a circle
/// of radius `plan.radius` around `plan.center`. Each revolution drops
/// Z by `plan.dz_per_rev`; partial revolutions linearly interpolate Z.
/// The final point lands at the path-start angle so the caller's
/// follow-up `linear(start.x, start.y, to_z)` is a straight line of
/// length zero (or near-zero in the Helix circle's tangent frame).
fn emit_helix_entry<P: PostProcessor>(plan: &HelixEntry, from_z: f64, to_z: f64, post: &mut P) {
    let dz = (from_z - to_z).abs();
    if dz < 1e-9 {
        return;
    }
    // Number of full revolutions needed (always at least one — if the
    // user picks a tiny step the helix still completes a full lap so
    // the cutter doesn't dive on a chord).
    let revs_full = (dz / plan.dz_per_rev).ceil().max(1.0);
    // Each revolution drops Z by dz/revs_full so the descent is
    // distributed evenly.
    let dz_each = -(from_z - to_z).abs() / revs_full; // negative (going down)
    let n = revs_full as usize;
    // Helix start: cutter at start angle, current Z = from_z.
    let start_x = plan.center.x + plan.radius * plan.start_angle.cos();
    let start_y = plan.center.y + plan.radius * plan.start_angle.sin();
    // Move to start of helix at fast_move_z would be done by caller —
    // here we assume the cutter is already above the helix start. The
    // first emit is a linear move to the helix start at from_z so the
    // tool steps off the path-start XY (where the rapid landed it)
    // onto the helix circle at z=from_z.
    post.linear(Some(start_x), Some(start_y), Some(from_z));
    let mut cur_z = from_z;
    for i in 0..n {
        let next_z = if i + 1 == n { to_z } else { cur_z + dz_each };
        // Each revolution is two semicircles so a single G2/G3 with
        // i, j vector to center stays within the post processor's
        // arc capabilities (some posts reject full-circle arcs whose
        // endpoint == startpoint).
        let half_dz = (next_z - cur_z) * 0.5;
        let mid_angle = plan.start_angle + std::f64::consts::PI;
        let mid_x = plan.center.x + plan.radius * mid_angle.cos();
        let mid_y = plan.center.y + plan.radius * mid_angle.sin();
        // Arc 1: start → midpoint (semicircle). i, j are the offset
        // from the arc's start point to the helix center.
        let i1 = -plan.radius * plan.start_angle.cos();
        let j1 = -plan.radius * plan.start_angle.sin();
        if plan.ccw {
            post.arc_ccw(
                Some(mid_x),
                Some(mid_y),
                Some(cur_z + half_dz),
                Some(i1),
                Some(j1),
            );
        } else {
            post.arc_cw(
                Some(mid_x),
                Some(mid_y),
                Some(cur_z + half_dz),
                Some(i1),
                Some(j1),
            );
        }
        // Arc 2: midpoint → start (semicircle, completing the lap).
        let i2 = -plan.radius * mid_angle.cos();
        let j2 = -plan.radius * mid_angle.sin();
        let end_x = plan.center.x + plan.radius * plan.start_angle.cos();
        let end_y = plan.center.y + plan.radius * plan.start_angle.sin();
        if plan.ccw {
            post.arc_ccw(Some(end_x), Some(end_y), Some(next_z), Some(i2), Some(j2));
        } else {
            post.arc_cw(Some(end_x), Some(end_y), Some(next_z), Some(i2), Some(j2));
        }
        cur_z = next_z;
    }
}

/// Extract polygon vertices from a segment chain (line endpoints; arc
/// endpoints — arc midpoints aren't sampled, the polygon is just the
/// segment endpoint list). Used for signed-area + point-in-polygon
/// checks during helix planning. The returned list is the closed
/// polygon's vertex sequence with no duplicate closing vertex.
fn polygon_vertices(segments: &[Segment]) -> Vec<Point2> {
    let mut v: Vec<Point2> = Vec::with_capacity(segments.len() + 1);
    if segments.is_empty() {
        return v;
    }
    v.push(segments[0].start);
    for seg in segments {
        // Push the end of each segment; duplicates with the next
        // segment's start are filtered by the dedupe at the end.
        if matches!(seg.kind, SegmentKind::Point) {
            continue;
        }
        v.push(seg.end);
    }
    // Drop a duplicate trailing vertex (closed path: last == first).
    if v.len() >= 2 && v.first().unwrap().distance(*v.last().unwrap()) < 1e-6 {
        v.pop();
    }
    v
}

/// Shoelace signed area of a polygon given as a vertex list. Positive
/// = CCW, negative = CW. Mirrors `cam::offsets::object_signed_area`
/// but operates on vertices instead of a `VcObject`.
fn polygon_signed_area(verts: &[Point2]) -> f64 {
    let n = verts.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}

/// Even-odd ray-cast point-in-polygon test (horizontal ray to +X).
/// Edges are treated as half-open [lo.y, hi.y) so vertex hits don't
/// double-count. Sufficient for the helix-fit sanity check.
fn point_in_polygon(verts: &[Point2], x: f64, y: f64) -> bool {
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

fn arc_length(seg: &Segment) -> f64 {
    let chord = seg.start.distance(seg.end);
    if seg.bulge.abs() < 1e-12 || chord < 1e-12 {
        return chord;
    }
    let (_, _, _, radius) = math::bulge_to_arc(seg.start, seg.end, seg.bulge);
    let theta = 4.0 * seg.bulge.atan(); // canonical bulge identity
    radius * theta.abs()
}

/// Emit the cut path with tab interruptions. For each LINE segment that
/// crosses a tab's `tab_radius` neighborhood, the cut is split: cut up to
/// the entry, lift Z to `tabs_z`, traverse to the exit, drop back to
/// `cut_z`, continue cutting (Rectangle); or ramp up / flat / ramp down
/// when `ramp_angle_deg` is `Some` (Ramp).
///
/// Arcs through tabs are tab-skipped with a straight Z lift even when
/// Ramp is requested — ramping along a curved path is a v2 follow-up.
fn emit_path_with_tabs<P: PostProcessor>(
    segments: &[Segment],
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: Option<f64>,
    post: &mut P,
) {
    for seg in segments {
        match seg.kind {
            SegmentKind::Line => emit_line_with_tabs(
                seg,
                tabs,
                tabs_z,
                cut_z,
                tab_radius,
                ramp_angle_deg,
                post,
            ),
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), None),
            SegmentKind::Arc | SegmentKind::Circle => {
                // v2: ramp along arcs. For now arcs through tabs always
                // do a straight Z lift, regardless of tab_type=Ramp.
                let crosses = tabs.iter().any(|t| {
                    let mid_x = (seg.start.x + seg.end.x) * 0.5;
                    let mid_y = (seg.start.y + seg.end.y) * 0.5;
                    (mid_x - t.x).hypot(mid_y - t.y) < tab_radius
                });
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if crosses {
                    post.linear(None, None, Some(tabs_z));
                }
                if seg.bulge > 0.0 {
                    post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                } else {
                    post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                }
                if crosses {
                    post.linear(None, None, Some(cut_z));
                }
            }
        }
    }
}

fn emit_line_with_tabs<P: PostProcessor>(
    seg: &Segment,
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: Option<f64>,
    post: &mut P,
) {
    let dx = seg.end.x - seg.start.x;
    let dy = seg.end.y - seg.start.y;
    let len = dx.hypot(dy);
    if len < 1e-9 {
        return;
    }
    // Walk the segment; for every tab whose perpendicular foot is on the
    // segment within `tab_radius`, compute t-entry and t-exit fractions.
    let mut intervals: Vec<(f64, f64)> = Vec::new();
    for tab in tabs {
        let tx = tab.x - seg.start.x;
        let ty = tab.y - seg.start.y;
        let t = (tx * dx + ty * dy) / (len * len);
        // Perpendicular distance.
        let perp_x = tx - t * dx;
        let perp_y = ty - t * dy;
        let perp = (perp_x * perp_x + perp_y * perp_y).sqrt();
        if perp > tab_radius {
            continue;
        }
        let half = (tab_radius * tab_radius - perp * perp).sqrt() / len;
        let t_in = (t - half).max(0.0);
        let t_out = (t + half).min(1.0);
        if t_out > t_in {
            intervals.push((t_in, t_out));
        }
    }
    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Merge overlaps.
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in intervals {
        if let Some(last) = merged.last_mut() {
            if a <= last.1 + 1e-6 {
                last.1 = last.1.max(b);
                continue;
            }
        }
        merged.push((a, b));
    }
    // Ramp horizontal length: flat = (tabs_z - cut_z) / tan(angle). When
    // 2*ramp_length > tab_width we collapse to a triangle (no flat top).
    let dz = (tabs_z - cut_z).abs();
    let ramp_length = ramp_angle_deg.map(|a| {
        if dz < 1e-9 {
            0.0
        } else {
            dz / a.to_radians().tan()
        }
    });
    // Emit: cut up to each interval, lift / ramp, traverse, drop / ramp,
    // repeat.
    let mut cursor = 0.0;
    for (t_in, t_out) in merged {
        if t_in > cursor + 1e-6 {
            let p = lerp(seg, t_in);
            post.linear(Some(p.0), Some(p.1), None);
        }
        match ramp_length {
            Some(rl) if rl > 1e-9 => {
                let tab_world_len = (t_out - t_in) * len;
                if tab_world_len < 2.0 * rl {
                    // Triangle: ramp directly to tabs_z at tab center,
                    // then ramp back down to cut_z at tab exit. Cutter
                    // never reaches a flat top — the tab is too narrow
                    // for the configured angle to fit its full slope.
                    let t_mid = 0.5 * (t_in + t_out);
                    let mid = lerp(seg, t_mid);
                    post.linear(Some(mid.0), Some(mid.1), Some(tabs_z));
                    let exit = lerp(seg, t_out);
                    post.linear(Some(exit.0), Some(exit.1), Some(cut_z));
                } else {
                    // Trapezoid: ramp up over rl, run flat, ramp down
                    // over rl. Translate ramp_length back into t-space
                    // along the segment.
                    let dt_ramp = rl / len;
                    let t_up_end = t_in + dt_ramp;
                    let t_down_start = t_out - dt_ramp;
                    let up_end = lerp(seg, t_up_end);
                    let down_start = lerp(seg, t_down_start);
                    let exit = lerp(seg, t_out);
                    // Ramp up: cut + climb to tabs_z.
                    post.linear(Some(up_end.0), Some(up_end.1), Some(tabs_z));
                    // Flat top across the tab's interior at tabs_z.
                    post.linear(Some(down_start.0), Some(down_start.1), None);
                    // Ramp down: descend back to cut_z by tab exit.
                    post.linear(Some(exit.0), Some(exit.1), Some(cut_z));
                }
            }
            _ => {
                // Rectangle (or zero-height tab): straight Z lift, run
                // across, drop. Original behavior.
                post.linear(None, None, Some(tabs_z));
                let p_out = lerp(seg, t_out);
                post.linear(Some(p_out.0), Some(p_out.1), None);
                post.linear(None, None, Some(cut_z));
            }
        }
        cursor = t_out;
    }
    if cursor < 1.0 - 1e-6 {
        post.linear(Some(seg.end.x), Some(seg.end.y), None);
    }
}

fn lerp(seg: &Segment, t: f64) -> (f64, f64) {
    (
        seg.start.x + t * (seg.end.x - seg.start.x),
        seg.start.y + t * (seg.end.y - seg.start.y),
    )
}

/// Emit segments with optional drag-knife trailing offset. When
/// `dragoff > 0`, every line→line corner is preceded by an arc that swivels
/// the blade around the corner point so the trail aligns with the new
/// direction. Mirrors `viaconstructor.machine_cmd.segment2machine_cmd`.
fn emit_path_with_dragoff<P: PostProcessor>(segments: &[Segment], dragoff: f64, post: &mut P) {
    use std::f64::consts::{FRAC_PI_2, PI};
    let mut last_motion: Option<f64> = None;
    for seg in segments {
        match seg.kind {
            SegmentKind::Line => {
                let new_motion = (seg.end.y - seg.start.y).atan2(seg.end.x - seg.start.x);
                if dragoff > 1e-9 {
                    if let Some(last_m) = last_motion {
                        let last_a = last_m + FRAC_PI_2;
                        let new_a = new_motion + FRAC_PI_2;
                        let off1 = (
                            seg.start.x + dragoff * last_a.sin(),
                            seg.start.y - dragoff * last_a.cos(),
                        );
                        let off2 = (
                            seg.start.x + dragoff * new_a.sin(),
                            seg.start.y - dragoff * new_a.cos(),
                        );
                        post.linear(Some(off1.0), Some(off1.1), None);
                        let mut diff = new_a - last_a;
                        while diff > PI {
                            diff -= 2.0 * PI;
                        }
                        while diff < -PI {
                            diff += 2.0 * PI;
                        }
                        if diff.abs() > 1e-6 {
                            let i = seg.start.x - off1.0;
                            let j = seg.start.y - off1.1;
                            if diff > 0.0 {
                                post.arc_ccw(Some(off2.0), Some(off2.1), None, Some(i), Some(j));
                            } else {
                                post.arc_cw(Some(off2.0), Some(off2.1), None, Some(i), Some(j));
                            }
                        }
                    }
                }
                post.linear(Some(seg.end.x), Some(seg.end.y), None);
                last_motion = Some(new_motion);
            }
            SegmentKind::Point => {
                post.linear(Some(seg.start.x), Some(seg.start.y), None);
                last_motion = None;
            }
            SegmentKind::Arc | SegmentKind::Circle => {
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if seg.bulge > 0.0 {
                    post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                } else {
                    post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                }
                // Tangent at end of arc: rotate radius 90° in the arc's
                // orientation. CCW arc → +90° rotation; CW → -90°.
                let rx = seg.end.x - center.x;
                let ry = seg.end.y - center.y;
                let (tx, ty) = if seg.bulge > 0.0 {
                    (-ry, rx)
                } else {
                    (ry, -rx)
                };
                last_motion = Some(ty.atan2(tx));
            }
        }
    }
}

fn lead_in_point(setup: &Setup, segments: &[Segment]) -> Option<Point2> {
    if setup.leads.r#in == LeadKind::Off || segments.is_empty() {
        return None;
    }
    let first = &segments[0];
    let len = setup.leads.in_lenght.max(0.0);
    if len < 1e-9 {
        return None;
    }
    let theta = (first.end.y - first.start.y).atan2(first.end.x - first.start.x);
    Some(match setup.leads.r#in {
        LeadKind::Straight => Point2::new(
            first.start.x - len * theta.sin(),
            first.start.y + len * theta.cos(),
        ),
        LeadKind::Arc => {
            let radius = len * 2.0 / std::f64::consts::PI;
            let center = Point2::new(
                first.start.x + radius * theta.sin(),
                first.start.y - radius * theta.cos(),
            );
            Point2::new(
                center.x + radius * (theta - std::f64::consts::FRAC_PI_2).sin(),
                center.y - radius * (theta - std::f64::consts::FRAC_PI_2).cos(),
            )
        }
        LeadKind::Off => unreachable!(),
    })
}

fn lead_out_point(setup: &Setup, segments: &[Segment]) -> Option<Point2> {
    if setup.leads.out == LeadKind::Off || segments.is_empty() {
        return None;
    }
    let last = segments.last().unwrap();
    let len = setup.leads.out_lenght.max(0.0);
    if len < 1e-9 {
        return None;
    }
    let theta = (last.end.y - last.start.y).atan2(last.end.x - last.start.x);
    Some(Point2::new(
        last.end.x - len * theta.sin(),
        last.end.y + len * theta.cos(),
    ))
}

/// Internal state shared across post processor implementations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PostState {
    pub last_x: Option<f64>,
    pub last_y: Option<f64>,
    pub last_z: Option<f64>,
    pub last_rate: Option<u32>,
    pub last_speed: Option<u32>,
    pub absolute: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::setup::{LeadKind, ToolOffset};
    use crate::geometry::Segment;

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    fn square_offset() -> PolylineOffset {
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
        }
    }

    #[test]
    fn nearest_neighbor_picks_the_closer_offset_first() {
        use crate::cam::setup::ObjectOrder;
        let mut setup = Setup::default();
        setup.tool.diameter = 1.0;
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;
        setup.mill.objectorder = ObjectOrder::Nearest;

        // Far-from-origin offset first in the input, near-origin second.
        let mut far = square_offset();
        for s in &mut far.segments {
            s.start.x += 100.0;
            s.start.y += 100.0;
            s.end.x += 100.0;
            s.end.y += 100.0;
        }
        far.source_object_idx = 1;
        let offsets = vec![far, square_offset()];

        let order = super::order_offsets(&setup, &offsets, Point2::new(0.0, 0.0));
        assert_eq!(order, vec![1, 0], "near-origin offset should run first");
    }

    #[test]
    fn helix_mode_emits_z_during_arc_or_line_moves() {
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.mill.depth = -2.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.mill.helix_mode = true;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        // After the first pass, subsequent passes should descend Z
        // mid-path (G1 with both XY and Z together).
        let combined_xyz = g
            .lines()
            .filter(|l| l.starts_with("G1"))
            .any(|l| l.contains('X') && l.contains('Z'));
        assert!(
            combined_xyz,
            "helix mode should combine XY moves with Z descent"
        );
    }

    #[test]
    fn tabs_split_a_long_cut_with_z_lifts() {
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_h = 800;
        setup.mill.depth = -2.0;
        setup.mill.step = -2.0;
        setup.mill.fast_move_z = 5.0;
        setup.tabs.active = true;
        setup.tabs.height = 1.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        let mut offset = square_offset();
        // Tab in the middle of the bottom edge.
        offset.tabs = vec![crate::cam::offsets::TabPoint { x: 5.0, y: 0.0 }];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[offset], &mut post);

        // The tab should split the bottom edge: cut → lift to (-2 + 1) = -1
        // → traverse → drop back to -2 → cut to corner.
        assert!(g.contains("Z-1"), "expected lift to tabs_z=-1 in: {g}");
        // Both Z=-2 (cut depth) and Z=-1 (tabs_z) should appear.
        assert!(g.contains("Z-2"), "expected cut at depth -2 in: {g}");
    }

    #[test]
    fn ramped_tab_emits_trapezoid_z_profile() {
        use crate::cam::setup::TabType;
        use crate::gcode::preview::{interpret, MoveKind};
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_h = 800;
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.tabs.active = true;
        setup.tabs.height = 0.5;
        setup.tabs.tab_type = TabType::Ramp;
        setup.tabs.ramp_angle_deg = 30.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        // Single 20mm long line cut along +X with one tab in the middle.
        // tab_radius = max(3.0/2, 0.5) = 1.5 → tab_world_len = 3mm.
        // ramp_length = 0.5 / tan(30°) ≈ 0.866mm. 2*ramp_length ≈ 1.73mm
        // < 3mm tab width → trapezoid (ramp_up / flat / ramp_down).
        let line_offset = PolylineOffset {
            segments: vec![Segment::line(p(0.0, 0.0), p(20.0, 0.0), "0", 7)],
            closed: false,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: vec![crate::cam::offsets::TabPoint { x: 10.0, y: 0.0 }],
        };

        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[line_offset], &mut post);
        let segments = interpret(&g);

        // Only inspect Cut moves along the path (skip Plunge/Retract,
        // which legitimately are pure-Z and bracket the path).
        let cut_z = -1.0_f64;
        let tabs_z = -0.5_f64;
        let mut waypoints: Vec<(f64, f64)> = Vec::new();
        for s in &segments {
            if !matches!(s.kind, MoveKind::Cut) {
                continue;
            }
            if s.from.y.abs() > 1e-3 || s.to.y.abs() > 1e-3 {
                continue;
            }
            if waypoints.is_empty() {
                waypoints.push((s.from.x, s.from.z));
            }
            waypoints.push((s.to.x, s.to.z));
        }

        // Expect a walk that starts and ends at cut_z, climbs to
        // tabs_z mid-path on a sloped ramp, holds tabs_z for the flat,
        // then descends on a sloped ramp.
        assert!(waypoints.len() >= 5, "expected ≥5 waypoints, got {waypoints:?}");

        // Trapezoid signature: a flat-top run at tabs_z (consecutive
        // tabs_z waypoints with ΔX>0).
        let flat_pairs = waypoints
            .windows(2)
            .filter(|w| {
                (w[0].1 - tabs_z).abs() < 1e-6
                    && (w[1].1 - tabs_z).abs() < 1e-6
                    && w[1].0 - w[0].0 > 1e-6
            })
            .count();
        assert!(flat_pairs >= 1, "expected ≥1 flat-top run at tabs_z; waypoints={waypoints:?}");

        // Sloped ramps in and out (Z changes while X advances).
        let has_ramp_up = waypoints.windows(2).any(|w| {
            (w[0].1 - cut_z).abs() < 1e-6
                && (w[1].1 - tabs_z).abs() < 1e-6
                && (w[1].0 - w[0].0).abs() > 1e-3
        });
        let has_ramp_down = waypoints.windows(2).any(|w| {
            (w[0].1 - tabs_z).abs() < 1e-6
                && (w[1].1 - cut_z).abs() < 1e-6
                && (w[1].0 - w[0].0).abs() > 1e-3
        });
        assert!(has_ramp_up, "expected a ramp-up (cut_z→tabs_z with ΔX>0); waypoints={waypoints:?}");
        assert!(has_ramp_down, "expected a ramp-down (tabs_z→cut_z with ΔX>0); waypoints={waypoints:?}");

        // No pure vertical Z step inside the cut path (Rectangle would
        // emit ΔX==0 transitions between cut_z and tabs_z).
        let pure_vertical = waypoints.windows(2).any(|w| {
            (w[0].1 - w[1].1).abs() > 1e-6 && (w[1].0 - w[0].0).abs() < 1e-9
        });
        assert!(!pure_vertical, "ramped tab must not emit pure-Z lifts; waypoints={waypoints:?}");
    }

    #[test]
    fn ramped_tab_with_too_narrow_width_uses_triangle() {
        use crate::cam::setup::TabType;
        use crate::gcode::preview::{interpret, MoveKind};
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_h = 800;
        setup.mill.depth = -2.0;
        setup.mill.step = -2.0;
        setup.mill.fast_move_z = 5.0;
        setup.tabs.active = true;
        setup.tabs.height = 1.5; // tabs_z = -0.5
        setup.tabs.tab_type = TabType::Ramp;
        setup.tabs.ramp_angle_deg = 30.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        // tab_radius = 1.5 → tab_world_len = 3mm.
        // ramp_length = 1.5 / tan(30°) ≈ 2.598mm. 2*ramp_length ≈ 5.2mm
        // > 3mm tab width → triangle (ramp up directly to tabs_z at tab
        // center, then ramp down — no flat top).
        let line_offset = PolylineOffset {
            segments: vec![Segment::line(p(0.0, 0.0), p(20.0, 0.0), "0", 7)],
            closed: false,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: vec![crate::cam::offsets::TabPoint { x: 10.0, y: 0.0 }],
        };

        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[line_offset], &mut post);
        let segments = interpret(&g);

        let cut_z = -2.0_f64;
        let tabs_z = -0.5_f64;
        let mut waypoints: Vec<(f64, f64)> = Vec::new();
        for s in &segments {
            if !matches!(s.kind, MoveKind::Cut) {
                continue;
            }
            if s.from.y.abs() > 1e-3 || s.to.y.abs() > 1e-3 {
                continue;
            }
            if waypoints.is_empty() {
                waypoints.push((s.from.x, s.from.z));
            }
            waypoints.push((s.to.x, s.to.z));
        }

        // Triangle profile: ramp-up directly to tabs_z, then ramp-down
        // back to cut_z, with NO consecutive-tabs_z (flat top) pair.
        let flat_pairs = waypoints
            .windows(2)
            .filter(|w| {
                (w[0].1 - tabs_z).abs() < 1e-6
                    && (w[1].1 - tabs_z).abs() < 1e-6
                    && w[1].0 - w[0].0 > 1e-6
            })
            .count();
        assert_eq!(flat_pairs, 0, "triangle must not have a flat top; waypoints={waypoints:?}");

        // Apex at tabs_z exists.
        assert!(
            waypoints.iter().any(|w| (w.1 - tabs_z).abs() < 1e-6),
            "expected apex at tabs_z; waypoints={waypoints:?}"
        );

        // Both ramp segments are sloped (ΔX>0 + ΔZ != 0).
        let has_ramp_up = waypoints.windows(2).any(|w| {
            (w[0].1 - cut_z).abs() < 1e-6
                && (w[1].1 - tabs_z).abs() < 1e-6
                && (w[1].0 - w[0].0).abs() > 1e-3
        });
        let has_ramp_down = waypoints.windows(2).any(|w| {
            (w[0].1 - tabs_z).abs() < 1e-6
                && (w[1].1 - cut_z).abs() < 1e-6
                && (w[1].0 - w[0].0).abs() > 1e-3
        });
        assert!(has_ramp_up, "expected ramp-up; waypoints={waypoints:?}");
        assert!(has_ramp_down, "expected ramp-down; waypoints={waypoints:?}");
    }

    #[test]
    fn dragoff_inserts_swivel_arcs_at_corners() {
        let mut setup = Setup::default();
        setup.tool.diameter = 0.0; // drag knife: no radius
        setup.tool.speed = 0;
        setup.tool.rate_h = 800;
        setup.tool.dragoff = Some(0.5);
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::On;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        // Each of the 4 corners gets swivel arcs (G2 or G3 with I/J center).
        let arc_count = g
            .lines()
            .filter(|l| (l.starts_with("G2 ") || l.starts_with("G3 ")) && l.contains('I'))
            .count();
        assert!(
            arc_count >= 3,
            "expected at least 3 swivel arcs at square corners; got {arc_count}\n{g}"
        );
    }

    #[test]
    fn linuxcnc_emits_a_recognizable_program() {
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_h = 800;
        setup.mill.depth = -2.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        // Smoke checks: header (G21 mm + G90 absolute), at least one G1 and one G0,
        // and a spindle command.
        assert!(g.contains("G21"), "should set mm units");
        assert!(g.contains("G90"), "should set absolute");
        assert!(g.contains("M3 S12000"), "should start spindle CW at 12000");
        assert!(g.contains("G1 X10"), "should cut to first corner");
        assert!(g.contains("M5"), "should stop spindle at end");
    }
}
