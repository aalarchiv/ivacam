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

    multi_pass(setup, &offset.segments, post);

    if let Some(out) = lead_out_point(setup, &offset.segments) {
        post.linear(Some(out.x), Some(out.y), None);
    }
    post.linear(None, None, Some(setup.mill.fast_move_z));

    *last_pos = offset.segments.last().map(|s| s.end).unwrap_or(start);
}

fn multi_pass<P: PostProcessor>(setup: &Setup, segments: &[Segment], post: &mut P) {
    let total_depth = setup.mill.depth;
    let step = if setup.mill.step.abs() < 1e-9 {
        total_depth
    } else {
        // Always negative-going for milling.
        if setup.mill.step > 0.0 {
            -setup.mill.step
        } else {
            setup.mill.step
        }
    };
    let mut z = (setup.mill.start_depth + step).max(total_depth);
    loop {
        // Plunge to z, then cut around.
        post.feedrate(setup.tool.rate_v);
        post.linear(None, None, Some(z));
        post.feedrate(setup.tool.rate_h);
        emit_path(segments, post);
        if z <= total_depth + 1e-9 {
            break;
        }
        z = (z + step).max(total_depth);
    }
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
        }
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
