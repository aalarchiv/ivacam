//! Drill op driver: canned drill cycle + optional Stufenfase rim
//! chamfer.
//!
//! Run from [`super::run_standard_op`] when the op kind matches
//! `OpKind::Drill { cycle }`. Emits the canned cycle via
//! `emit_drill_block`, then — when `chamfer_after_width_mm` is set
//! (rt1.20 Stufenfase) — walks a single revolution at each hole's
//! rim at the V-bit chamfer depth.

// CAM/sim pedantic-lint exemption: STEPS is a tiny constant, cast
// to f64 for trig is fine.
#![allow(clippy::cast_precision_loss)]

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::Setup;
use crate::cam::VcObject;
use crate::gcode::{emit_drill_block, emit_vcarve_block, PostProcessor};
use crate::geometry::{Point2, SegmentKind};
use crate::pipeline::setup_resolver::resolve_peck_step;
use crate::pipeline::{
    emit_toolchange_envelope, op_includes_object, synthesize_finish_setup, PipelineError,
    PipelineWarning,
};
use crate::project::{DrillCycle, Op, Project};

#[allow(clippy::too_many_arguments)]
pub(super) fn run_drill<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    offsets: &[PolylineOffset],
    cycle: DrillCycle,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<(), PipelineError> {
    // Peck cycles fall back to the tool's `default_peck_step_mm`
    // when the op's own peck_step_mm is unset (== 0).
    let resolved_cycle = resolve_peck_step(cycle, project, op);
    emit_drill_block(setup, offsets, resolved_cycle, post, last_pos);
    // rt1.20 (Stufenfase): when the drill op carries a
    // chamfer-after width, walk a single revolution at each hole's
    // rim at the V-bit chamfer depth.
    // kbx5 step 2: Stufenfase width is now on the OpKind::Drill variant.
    if let Some(w) = op.drill_chamfer_after_width_mm() {
        if w > 0.0 {
            emit_stufenfase(op, project, objects, setup, w, post, last_pos, warnings)?;
        }
    }
    Ok(())
}

/// Single full-revolution rim chamfer emitted after the drill block.
/// V-bit depth comes from the cutter's tip angle and the user-set
/// chamfer width. Honors `op.finish_tool_id` for dual-tool
/// drill+chamfer setups.
#[allow(clippy::too_many_arguments)]
fn emit_stufenfase<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    drill_setup: &Setup,
    width_mm: f64,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<(), PipelineError> {
    // Single full revolution at constant Z. 64 waypoints + closing
    // point so arc-fit produces clean one-or-two arcs.
    const STEPS: usize = 64;
    let cutter_id = op.finish_tool_id.unwrap_or(op.tool_id);
    let cutter = project
        .tools
        .iter()
        .find(|t| t.id == cutter_id)
        .ok_or(PipelineError::UnknownTool(op.id, cutter_id))?;
    let chamfer_z = crate::cam::chamfer::chamfer_depth(width_mm, cutter.tip_angle_deg);
    if chamfer_z.abs() < 1e-9 {
        return Ok(());
    }
    let mut polylines: Vec<Vec<(f64, f64, f64)>> = Vec::new();
    let mut found = 0usize;
    for (idx, obj) in objects.iter().enumerate() {
        if !op_includes_object(op, obj, idx) {
            continue;
        }
        if !obj.closed {
            continue;
        }
        let Some(first) = obj.segments.first() else {
            continue;
        };
        if !matches!(first.kind, SegmentKind::Circle) {
            continue;
        }
        let Some(center) = first.center else {
            continue;
        };
        let r = first.start.distance(center);
        if r < 0.05 {
            continue;
        }
        let mut flat: Vec<(f64, f64, f64)> = (0..=STEPS)
            .map(|i| {
                let a = (i as f64) * std::f64::consts::TAU / (STEPS as f64);
                (center.x + r * a.cos(), center.y + r * a.sin(), chamfer_z)
            })
            .collect();
        if let Some(&first) = flat.first() {
            if let Some(last) = flat.last_mut() {
                *last = first;
            }
        }
        polylines.push(flat);
        found += 1;
    }
    if found == 0 {
        return Ok(());
    }
    let mut chamfer_setup = drill_setup.clone();
    if op.finish_tool_id.is_some() && op.finish_tool_id != Some(op.tool_id) {
        if !project.machine.supports_toolchange {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "stufenfase_no_toolchange".into(),
                message: format!(
                    "drill op '{}' has chamfer_after_width_mm + a distinct finish_tool_id but the machine doesn't support toolchange; gcode will assume manual change.",
                    op.name
                ),
            });
        }
        if let Some(finish_setup) = synthesize_finish_setup(op, project, warnings)? {
            post.raw(&format!(
                "; stufenfase: toolchange to {} for hole-rim chamfer",
                finish_setup.tool.number
            ));
            // bd rwv8: wrap the drill→chamfer M6 in the safety envelope
            // (safe-Z → M5+dwell → M6 → z-shift → M3+dwell). The
            // previous code emitted `T<n> M6` immediately after the
            // drill block ended, with the spindle still spinning and
            // the cutter potentially still in the hole.
            emit_toolchange_envelope(
                post,
                &project.machine,
                drill_setup,
                Some(cutter),
                finish_setup.tool.number,
                false,
            );
            chamfer_setup = finish_setup;
        }
    }
    emit_vcarve_block(&chamfer_setup, &polylines, post, last_pos);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::cam::setup::MachineConfig;
    use crate::geometry::Point2;
    use crate::cam::setup::ToolOffset;
    use crate::pipeline::test_helpers::{
        closed_circle, closed_square_offset, drill_op, endmill, profile_op, vbit,
    };
    use crate::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
    use crate::project::{Op, OpKind, OpParams, OpSource, Project, SourceCombine, ToolKind};

    /// A 0.5mm-radius closed circle with a 3mm endmill running an
    /// `OpKind::Drill` { Simple } op should emit a recognizable
    /// `LinuxCNC` G81 (or G82 for dwell) drill at the circle's center.
    #[test]
    fn drill_op_emits_gcode_for_circle_smaller_than_tool() {
        let project = Project {
            segments: closed_circle(Point2::new(5.0, 7.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("G81"),
            "expected G81 in linuxcnc drill output:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("X5") && resp.gcode.contains("Y7"),
            "expected drill at (5, 7):\n{}",
            resp.gcode
        );
    }

    /// Drill onto a closed rectangle should drill at the rectangle's
    /// bbox center. Regression for the user-reported "drilling op is
    /// not correct" — the rectangle case used to be silently skipped,
    /// leaving the drill op with no output and no warning.
    #[test]
    fn drill_op_targets_bbox_center_of_closed_rectangle() {
        // 20mm × 20mm rectangle offset to (10, 5) ⇒ corners at
        // (10,5)-(30,25). Bbox center = (20, 15).
        let segments = closed_square_offset(20.0, 10.0, 5.0);
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.stats.offset_count >= 1,
            "drill op produced no offsets — bbox-center fallback is missing",
        );
        assert!(
            resp.gcode.contains("G81"),
            "expected G81 in linuxcnc drill output:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("X20") && resp.gcode.contains("Y15"),
            "expected drill at bbox center (20, 15):\n{}",
            resp.gcode
        );
    }

    /// Drill cycle Peck with a non-zero step should map to G83 in
    /// `LinuxCNC`, with the per-peck Q operand carrying the step.
    #[test]
    fn drill_peck_emits_g83() {
        let project = Project {
            segments: closed_circle(Point2::new(0.0, 0.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Peck {
                    peck_step_mm: 1.0,
                    dwell_sec: 0.0,
                },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("G83"),
            "expected G83 in linuxcnc peck output:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("Q1"),
            "expected Q1 peck step:\n{}",
            resp.gcode
        );
    }

    /// Drill cycle `ChipBreak` should map to G73 in `LinuxCNC`.
    #[test]
    fn drill_chip_break_emits_g73() {
        let project = Project {
            segments: closed_circle(Point2::new(0.0, 0.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::ChipBreak {
                    peck_step_mm: 1.0,
                    dwell_sec: 0.0,
                },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("G73"),
            "expected G73 in linuxcnc chip-break output:\n{}",
            resp.gcode
        );
    }

    /// GRBL doesn't support canned drill cycles. The post should fall
    /// back to the trait's default G0/G1 expansion.
    #[test]
    fn drill_grbl_falls_back_to_g0g1_sequence() {
        let project = Project {
            segments: closed_circle(Point2::new(0.0, 0.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Peck {
                    peck_step_mm: 1.0,
                    dwell_sec: 0.0,
                },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Grbl),
            },
            |_, _, _| {},
        )
        .unwrap();
        for code in ["G81", "G82", "G83", "G73"] {
            assert!(
                !resp.gcode.contains(code),
                "{code} should not appear in GRBL fallback output:\n{}",
                resp.gcode
            );
        }
        let drill_block = resp
            .gcode
            .lines()
            .skip_while(|l| !l.contains("OP 1"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            drill_block.contains("G0"),
            "expected at least one G0 (rapid) in the drill block:\n{drill_block}"
        );
        assert!(
            drill_block.contains("G1"),
            "expected at least one G1 (feed plunge) in the drill block:\n{drill_block}"
        );
    }

    /// A Drill op with `OpSource::Objects` selecting only one of
    /// several drill candidates must emit gcode for *just* that one.
    #[test]
    fn drill_op_respects_object_selection() {
        let mut segments = closed_circle(Point2::new(0.0, 0.0), 0.5);
        segments.extend(closed_circle(Point2::new(20.0, 0.0), 0.5));
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Drill".into(),
                enabled: true,
                kind: OpKind::Drill {
                    cycle: crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
                    chamfer_after_width_mm: None,
                    pattern: None,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![2],
                    combine: SourceCombine::Auto,
                },
                params: {
                    let mut p = OpParams::mill_default();
                    p.depth = -5.0;
                    p.start_depth = 0.0;
                    p.fast_move_z = 5.0;
                    p
                },
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("G81"),
            "expected G81 drill, got:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("X20"),
            "expected drill at the second circle (x=20):\n{}",
            resp.gcode
        );
        let g81_count = resp.gcode.matches("G81").count();
        assert_eq!(
            g81_count, 1,
            "expected exactly one drill cycle in selection-restricted output:\n{}",
            resp.gcode
        );
    }

    /// Drill op picks the per-tool _drill speed/feed/plunge variants
    /// (rt1.27).
    #[test]
    fn drill_op_uses_drill_set() {
        let mut tool = endmill(1, 3.0);
        tool.kind = ToolKind::Drill;
        tool.speed = 20_000;
        tool.plunge_rate = 100;
        tool.feed_rate = 1500;
        tool.speed_drill = Some(3_000);
        tool.plunge_rate_drill = Some(30);

        let project = Project {
            segments: closed_circle(Point2::new(5.0, 7.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("S3000"),
            "expected drill spindle 3000 in gcode:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("F30"),
            "expected drill plunge 30 in gcode:\n{}",
            resp.gcode
        );
    }

    /// Drill op with peck cycle and `peck_step_mm=0` falls back to the
    /// tool's `default_peck_step_mm` (rt1.27).
    #[test]
    fn drill_peck_uses_tool_default_when_op_step_zero() {
        let mut tool = endmill(1, 3.0);
        tool.kind = ToolKind::Drill;
        tool.default_peck_step_mm = Some(1.25);
        let project = Project {
            segments: closed_circle(Point2::new(5.0, 7.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Peck {
                    peck_step_mm: 0.0,
                    dwell_sec: 0.0,
                },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("Q1.25"),
            "expected resolved peck step Q1.25 in gcode:\n{}",
            resp.gcode
        );
    }

    /// Stufenfase (rt1.20): a drill op with `chamfer_after_width_mm`
    /// follows the drill cycle with a constant-Z revolution at each
    /// hole's rim, computed from the cutter's tip angle.
    #[test]
    fn drill_with_chamfer_after_emits_constant_z_revolution() {
        let mut vbit_drill = vbit();
        vbit_drill.kind = ToolKind::Drill;
        vbit_drill.diameter = 3.0;
        vbit_drill.tip_angle_deg = 90.0; // z = -width when tan(45°) = 1
        let center = Point2::new(5.0, 7.0);
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        params.fast_move_z = 5.0;
        let project = Project {
            segments: closed_circle(center, 0.5),
            machine: MachineConfig::default(),
            tools: vec![vbit_drill],
            operations: vec![Op {
                id: 1,
                name: "Drill+chamfer".into(),
                enabled: true,
                kind: OpKind::Drill {
                    cycle: crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
                    chamfer_after_width_mm: Some(1.0),
                    pattern: None,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode
                .lines()
                .any(|l| l.starts_with("G81") || l.starts_with("G82")),
            "expected drill cycle (G81/G82):\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("Z-1"),
            "expected chamfer revolution at Z-1 (90° tip + 1mm width):\n{}",
            resp.gcode
        );
    }

    /// Drill with `chamfer_after` AND a distinct `finish_tool_id` emits
    /// the toolchange between the drill cycle and the chamfer
    /// revolution (rt1.20 × rt1.33).
    #[test]
    fn drill_chamfer_after_with_tool_override_emits_m6() {
        let mut drill = vbit();
        drill.kind = ToolKind::Drill;
        drill.diameter = 3.0;
        drill.id = 1;
        let mut vbit_finish = vbit();
        vbit_finish.id = 2;
        vbit_finish.diameter = 6.35;
        vbit_finish.tip_angle_deg = 90.0;
        let machine = MachineConfig {
            supports_toolchange: true,
            ..MachineConfig::default()
        };
        let center = Point2::new(5.0, 7.0);
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        let project = Project {
            segments: closed_circle(center, 0.5),
            machine,
            tools: vec![drill, vbit_finish],
            operations: vec![Op {
                id: 1,
                name: "Drill".into(),
                enabled: true,
                kind: OpKind::Drill {
                    cycle: crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
                    chamfer_after_width_mm: Some(0.5),
                    pattern: None,
                },
                tool_id: 1,
                finish_tool_id: Some(2),
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("T2 M6"),
            "expected toolchange T2 M6 for chamfer cutter:\n{}",
            resp.gcode
        );
    }

    /// olpn: a drill op followed by a profile op must emit G80 (cancel
    /// canned cycle) inside the drill block before the next op's first
    /// G0. Otherwise FANUC / Mach3 reinterpret that G0 as another
    /// invocation of the same drill cycle at the modal Z / R.
    #[test]
    fn drill_op_emits_g80_before_next_op() {
        let project = Project {
            segments: {
                // One closed circle (the drill target) plus a closed
                // square (the profile target) so build_op_offsets gets
                // both an drillable point and a profile cut.
                let mut s = closed_circle(Point2::new(5.0, 7.0), 0.5);
                s.extend(closed_square_offset(20.0, 30.0, 30.0));
                s
            },
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![
                drill_op(1, 1, crate::project::DrillCycle::Simple { dwell_sec: 0.0 }),
                profile_op(2, 1, ToolOffset::Outside),
            ],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let lines: Vec<&str> = resp.gcode.lines().collect();
        let g80_idx = lines
            .iter()
            .position(|l| l == &"G80" || l.starts_with("G80 "))
            .unwrap_or_else(|| panic!("expected G80 in:\n{}", resp.gcode));
        // Find the FIRST G0 line strictly after the drill cycle. The
        // drill cycle is identified by the G81 (or G82 / G83 / G73)
        // line just before the G80; verify G80 sits between the last
        // canned-cycle line and any subsequent G0.
        let last_drill_idx = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                l.starts_with("G81 ")
                    || l.starts_with("G82 ")
                    || l.starts_with("G83 ")
                    || l.starts_with("G73 ")
            })
            .last()
            .map(|(i, _)| i)
            .unwrap_or_else(|| panic!("expected a drill cycle line in:\n{}", resp.gcode));
        assert!(
            g80_idx > last_drill_idx,
            "G80 (idx {g80_idx}) must come AFTER the last drill cycle (idx {last_drill_idx}):\n{}",
            resp.gcode
        );
        let next_g0_after_drill = lines
            .iter()
            .enumerate()
            .skip(last_drill_idx + 1)
            .find(|(_, l)| l.starts_with("G0 "))
            .map(|(i, _)| i)
            .unwrap_or_else(|| panic!("expected a G0 after the drill block in:\n{}", resp.gcode));
        assert!(
            g80_idx < next_g0_after_drill,
            "G80 (idx {g80_idx}) must precede the next G0 (idx {next_g0_after_drill}):\n{}",
            resp.gcode
        );
    }

    /// Drill without `chamfer_after_width_mm` emits no rim revolution.
    #[test]
    fn drill_without_chamfer_after_emits_no_revolution() {
        let project = Project {
            segments: closed_circle(Point2::new(5.0, 7.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let any_chamfer_g1 = resp
            .gcode
            .lines()
            .any(|l| l.starts_with("G1 ") && l.contains("Z-"));
        assert!(
            !any_chamfer_g1,
            "expected no chamfer revolution gcode in drill-only op:\n{}",
            resp.gcode
        );
    }
}
