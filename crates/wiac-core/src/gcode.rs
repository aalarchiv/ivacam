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
fn order_offsets(
    setup: &Setup,
    offsets: &[PolylineOffset],
    start: Point2,
) -> Vec<usize> {
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
                let g = *group_of
                    .entry(o.source_object_idx)
                    .or_insert_with(|| {
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

    // Helix mode replaces the straight Z plunge between passes with a
    // spiral down the contour — gentler on small-diameter tools and
    // produces cleaner closed-contour entries. Only meaningful for
    // closed paths; for open paths we silently fall back to straight.
    let closed_path = is_closed_path(segments);
    let helix = setup.mill.helix_mode && closed_path;

    let mut prev_z: Option<f64> = None;
    let mut z = (setup.mill.start_depth + step).max(total_depth);
    loop {
        if let (true, Some(pz)) = (helix, prev_z) {
            // Spiral from prev_z down to z while tracing the segments.
            post.feedrate(setup.tool.rate_h);
            emit_helix_pass(segments, pz, z, post);
        } else {
            post.feedrate(setup.tool.rate_v);
            post.linear(None, None, Some(z));
            post.feedrate(setup.tool.rate_h);
            let pass_uses_tabs = setup.tabs.active && !tabs.is_empty() && z < tabs_z;
            if pass_uses_tabs {
                emit_path_with_tabs(segments, tabs, tabs_z, z, tab_radius, post);
            } else {
                let dragoff = setup.tool.dragoff.unwrap_or(0.0);
                emit_path_with_dragoff(segments, dragoff, post);
            }
        }
        prev_z = Some(z);
        if z <= total_depth + 1e-9 {
            break;
        }
        z = (z + step).max(total_depth);
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
fn emit_helix_pass<P: PostProcessor>(
    segments: &[Segment],
    from_z: f64,
    to_z: f64,
    post: &mut P,
) {
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
/// `cut_z`, continue cutting. Arcs through tabs are tab-skipped wholesale
/// for now (rectangle-tab on arc is a follow-up).
fn emit_path_with_tabs<P: PostProcessor>(
    segments: &[Segment],
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    post: &mut P,
) {
    for seg in segments {
        match seg.kind {
            SegmentKind::Line => emit_line_with_tabs(seg, tabs, tabs_z, cut_z, tab_radius, post),
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), None),
            SegmentKind::Arc | SegmentKind::Circle => {
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
    // Emit: cut up to each interval, lift, traverse, drop, repeat.
    let mut cursor = 0.0;
    for (t_in, t_out) in merged {
        if t_in > cursor + 1e-6 {
            let p = lerp(seg, t_in);
            post.linear(Some(p.0), Some(p.1), None);
        }
        // Lift over the tab.
        post.linear(None, None, Some(tabs_z));
        let p_out = lerp(seg, t_out);
        post.linear(Some(p_out.0), Some(p_out.1), None);
        post.linear(None, None, Some(cut_z));
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
fn emit_path_with_dragoff<P: PostProcessor>(
    segments: &[Segment],
    dragoff: f64,
    post: &mut P,
) {
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
        assert!(combined_xyz, "helix mode should combine XY moves with Z descent");
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
