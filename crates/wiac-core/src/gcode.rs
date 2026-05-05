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
    for offset in offsets {
        emit_offset(setup, offset, post, &mut last_pos);
    }
    program_end(setup, post);
    post.finish()
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
    // Tabs interrupt the cut from `total_depth + tab.height` upward — when
    // the pass is shallower than that, tabs are irrelevant for that pass.
    let tabs_z = total_depth + setup.tabs.height.abs();
    let tab_radius = (setup.tool.diameter * 0.5).max(0.5);

    let mut z = (setup.mill.start_depth + step).max(total_depth);
    loop {
        post.feedrate(setup.tool.rate_v);
        post.linear(None, None, Some(z));
        post.feedrate(setup.tool.rate_h);
        let pass_uses_tabs = setup.tabs.active && !tabs.is_empty() && z < tabs_z;
        if pass_uses_tabs {
            emit_path_with_tabs(segments, tabs, tabs_z, z, tab_radius, post);
        } else {
            emit_path(segments, post);
        }
        if z <= total_depth + 1e-9 {
            break;
        }
        z = (z + step).max(total_depth);
    }
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

fn emit_path<P: PostProcessor>(segments: &[Segment], post: &mut P) {
    for seg in segments {
        match seg.kind {
            SegmentKind::Line => post.linear(Some(seg.end.x), Some(seg.end.y), None),
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), None),
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
