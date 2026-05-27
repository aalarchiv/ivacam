//! Shared fixtures for pipeline integration tests. Visibility is
//! `pub(in crate::pipeline)` so submodules of `pipeline` can import
//! and the helpers stay invisible to the rest of the crate.
//!
//! Helpers were previously inlined in `pipeline.rs`'s 6 000-line
//! test mod; extracting them lets per-module test blocks
//! (`pipeline/op_drivers/*.rs`) share the same fixtures without
//! duplicating them.

// `cfg(test)` is applied at the `mod test_helpers` declaration in
// pipeline.rs — repeating it here would trip the duplicated_attributes
// lint. Test-only float-eq is intentional in the fixtures.
#![allow(clippy::float_cmp)]

use crate::cam::setup::{LeadKind, LeadsConfig, MachineConfig, ToolOffset};
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::project::{
    Coolant, DrillCycle, Op, OpKind, OpParams, OpSource, PatternConfig, Project, ToolEntry,
    ToolKind,
};

pub(in crate::pipeline) fn closed_square(side: f64) -> Vec<Segment> {
    vec![
        Segment::line(Point2::new(0.0, 0.0), Point2::new(side, 0.0), "0", 7),
        Segment::line(Point2::new(side, 0.0), Point2::new(side, side), "0", 7),
        Segment::line(Point2::new(side, side), Point2::new(0.0, side), "0", 7),
        Segment::line(Point2::new(0.0, side), Point2::new(0.0, 0.0), "0", 7),
    ]
}

pub(in crate::pipeline) fn closed_square_offset(side: f64, ox: f64, oy: f64) -> Vec<Segment> {
    vec![
        Segment::line(Point2::new(ox, oy), Point2::new(ox + side, oy), "0", 7),
        Segment::line(
            Point2::new(ox + side, oy),
            Point2::new(ox + side, oy + side),
            "0",
            7,
        ),
        Segment::line(
            Point2::new(ox + side, oy + side),
            Point2::new(ox, oy + side),
            "0",
            7,
        ),
        Segment::line(Point2::new(ox, oy + side), Point2::new(ox, oy), "0", 7),
    ]
}

pub(in crate::pipeline) fn closed_circle(center: Point2, radius: f64) -> Vec<Segment> {
    let p_right = Point2::new(center.x + radius, center.y);
    let p_left = Point2::new(center.x - radius, center.y);
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
    ]
}

pub(in crate::pipeline) fn endmill(id: u32, diameter: f64) -> ToolEntry {
    ToolEntry {
        id,
        name: format!("{diameter:.1}mm endmill"),
        kind: ToolKind::Endmill,
        diameter,
        tip_diameter: None,
        tip_angle_deg: 60.0,
        dragoff: None,
        flutes: 2,
        speed: 18_000,
        plunge_rate: 100,
        feed_rate: 800,
        coolant: Coolant::Off,
        speed_finish: None,
        plunge_rate_finish: None,
        feed_rate_finish: None,
        speed_drill: None,
        plunge_rate_drill: None,
        feed_rate_drill: None,
        default_peck_step_mm: None,
        default_step: None,
        default_xy_overlap: None,
        comment: None,
        z_shift_mm: None,
        laser_pierce_sec: None,
        laser_lead_in_mm: None,
        kerf_mm: None,
        corner_radius_mm: None,
        form_profile_mm: Vec::new(),
        wirbeln: false,
        wirbeln_stepover_mm: None,
        wirbeln_extra_width_mm: None,
        wirbeln_osc_mm: None,
        pause: 1,
        flute_length_mm: None,
        length_mm: None,
        compression_transition_mm: None,
        thread_pitch_mm: None,
        shank_diameter_mm: None,
        stickout_length_mm: None,
        holder: None,
        spindle_direction: crate::project::SpindleDirection::default(),
        drag_knife_self_align_angle_deg: None,
        pierce_height_mm: None,
        cut_height_mm: None,
        pierce_delay_sec: None,
        vcarve_lead_in_angle_deg: None,
    }
}

pub(in crate::pipeline) fn vbit() -> ToolEntry {
    ToolEntry {
        id: 1,
        name: "60° V".into(),
        kind: ToolKind::VBit,
        diameter: 6.35,
        tip_diameter: Some(0.1),
        tip_angle_deg: 60.0,
        dragoff: None,
        flutes: 2,
        speed: 18_000,
        plunge_rate: 200,
        feed_rate: 1200,
        coolant: Coolant::Off,
        speed_finish: None,
        plunge_rate_finish: None,
        feed_rate_finish: None,
        speed_drill: None,
        plunge_rate_drill: None,
        feed_rate_drill: None,
        default_peck_step_mm: None,
        default_step: None,
        default_xy_overlap: None,
        comment: None,
        z_shift_mm: None,
        laser_pierce_sec: None,
        laser_lead_in_mm: None,
        kerf_mm: None,
        corner_radius_mm: None,
        form_profile_mm: Vec::new(),
        wirbeln: false,
        wirbeln_stepover_mm: None,
        wirbeln_extra_width_mm: None,
        wirbeln_osc_mm: None,
        pause: 1,
        flute_length_mm: None,
        length_mm: None,
        compression_transition_mm: None,
        thread_pitch_mm: None,
        shank_diameter_mm: None,
        stickout_length_mm: None,
        holder: None,
        spindle_direction: crate::project::SpindleDirection::default(),
        drag_knife_self_align_angle_deg: None,
        pierce_height_mm: None,
        cut_height_mm: None,
        pierce_delay_sec: None,
        vcarve_lead_in_angle_deg: None,
    }
}

pub(in crate::pipeline) fn profile_op(id: u32, tool_id: u32, offset: ToolOffset) -> Op {
    Op {
        id,
        name: format!("Profile {id}"),
        enabled: true,
        kind: OpKind::Profile {
            offset,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
    }
}

/// kbx5: patterns only attach to `OpKind::Drill` now. Tests that need
/// to exercise pattern expansion build a drill op with the pattern
/// embedded directly.
pub(in crate::pipeline) fn drill_op_with_pattern(pattern: PatternConfig) -> Op {
    let mut op = drill_op(1, 1, crate::project::DrillCycle::Simple { dwell_sec: 0.0 });
    if let crate::project::OpKind::Drill { pattern: slot, .. } = &mut op.kind {
        *slot = Some(pattern);
    }
    op
}

pub(in crate::pipeline) fn profile_leads_op(
    offset: ToolOffset,
    kind_in: LeadKind,
    len_in: f64,
) -> Op {
    let mut params = OpParams::mill_default();
    params.depth = -1.0;
    params.step = Some(-1.0);
    params.fast_move_z = 5.0;
    let leads_for_op = LeadsConfig {
        r#in: kind_in,
        out: LeadKind::Off,
        in_lenght: len_in,
        out_lenght: 0.0,
    };
    let contour = crate::project::ContourParams {
        leads: leads_for_op,
        ..crate::project::ContourParams::default()
    };
    Op {
        id: 1,
        name: "Profile".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset,
            contour,
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params,
    }
}

pub(in crate::pipeline) fn pocket_op(id: u32, tool_id: u32, source: OpSource) -> Op {
    Op {
        id,
        name: format!("Pocket {id}"),
        enabled: true,
        kind: OpKind::Pocket {
            strategy: crate::project::PocketStrategy::Cascade,
            contour: crate::project::ContourParams::default(),
            pocket: crate::project::PocketParams::default(),
        },
        tool_id,
        finish_tool_id: None,
        source,
        params: OpParams::mill_default(),
    }
}

pub(in crate::pipeline) fn drill_op(id: u32, tool_id: u32, cycle: DrillCycle) -> Op {
    let mut params = OpParams::mill_default();
    params.depth = -5.0;
    params.start_depth = 0.0;
    params.fast_move_z = 5.0;
    Op {
        id,
        name: format!("Drill {id}"),
        enabled: true,
        kind: OpKind::Drill {
            cycle,
            chamfer_after_width_mm: None,
            pattern: None,
            spot_first: None,
        },
        tool_id,
        finish_tool_id: None,
        source: OpSource::All,
        params,
    }
}

pub(in crate::pipeline) fn project_with(ops: Vec<Op>, tools: Vec<ToolEntry>) -> Project {
    project_with_segments(closed_square(20.0), ops, tools)
}

pub(in crate::pipeline) fn project_with_segments(
    segments: Vec<Segment>,
    ops: Vec<Op>,
    tools: Vec<ToolEntry>,
) -> Project {
    Project {
        segments,
        machine: MachineConfig::default(),
        tools,
        operations: ops,
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
    }
}

pub(in crate::pipeline) fn dejavu_font_bytes() -> Vec<u8> {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fonts/DejaVuSans.ttf");
    std::fs::read(p).expect("DejaVuSans.ttf fixture")
}

/// Scan `gcode` for the X coordinate of the first cut move in each
/// `; OP` block — useful for verifying that pattern instances landed
/// at the expected offsets.
pub(in crate::pipeline) fn cut_x_values(gcode: &str) -> Vec<f64> {
    let mut xs = Vec::new();
    for line in gcode.lines() {
        // G0 / G1 cover the standard travel + cut moves; G81 / G82 / G83 /
        // G73 cover the canned drill cycles whose X coordinates encode
        // drill-hole positions (kbx5: pattern tests now use Drill ops).
        if !(line.starts_with("G1")
            || line.starts_with("G0")
            || line.starts_with("G8")
            || line.starts_with("G73"))
        {
            continue;
        }
        if let Some(idx) = line.find('X') {
            let rest = &line[idx + 1..];
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                .unwrap_or(rest.len());
            if let Ok(x) = rest[..end].parse::<f64>() {
                xs.push(x);
            }
        }
    }
    xs
}

/// Walk the emitted gcode and split it into (rapid-target,
/// lead-moves-at-z0, plunge-target-z, first-cut-move).
pub(in crate::pipeline) fn first_lead_phase(
    gcode: &str,
) -> (Option<(f64, f64)>, Vec<String>, Option<String>) {
    let mut state = 0u8;
    let mut rapid_xy: Option<(f64, f64)> = None;
    let mut between: Vec<String> = Vec::new();
    let mut first_cut: Option<String> = None;
    for raw in gcode.lines() {
        let l = raw.trim_start();
        if l.is_empty() || l.starts_with(';') || l.starts_with('(') {
            continue;
        }
        match state {
            0 => {
                if l.starts_with("G0 ") && (l.contains('X') || l.contains('Y')) {
                    rapid_xy = parse_xy(l);
                    state = 1;
                }
            }
            1 => {
                if l.starts_with("G1 ") && l.contains('Z') && !l.contains('X') && !l.contains('Y') {
                    state = 2;
                }
            }
            2 => {
                if l.starts_with("G1 ") && l.contains('Z') && !l.contains('X') && !l.contains('Y') {
                    state = 3;
                    continue;
                }
                between.push(l.to_string());
            }
            3 => {
                if l.starts_with("G0 ")
                    || l.starts_with("G1 ")
                    || l.starts_with("G2 ")
                    || l.starts_with("G3 ")
                {
                    first_cut = Some(l.to_string());
                    break;
                }
            }
            _ => break,
        }
    }
    (rapid_xy, between, first_cut)
}

pub(in crate::pipeline) fn parse_xy(line: &str) -> Option<(f64, f64)> {
    let mut x: Option<f64> = None;
    let mut y: Option<f64> = None;
    for tok in line.split_whitespace() {
        if let Some(rest) = tok.strip_prefix('X') {
            x = rest.parse().ok();
        } else if let Some(rest) = tok.strip_prefix('Y') {
            y = rest.parse().ok();
        }
    }
    match (x, y) {
        (Some(xv), Some(yv)) => Some((xv, yv)),
        _ => None,
    }
}
