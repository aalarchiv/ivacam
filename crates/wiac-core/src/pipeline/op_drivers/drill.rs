//! Drill op driver: canned drill cycle + optional Stufenfase rim
//! chamfer.
//!
//! Run from [`super::run_standard_op`] when the op kind matches
//! `OpKind::Drill { cycle }`. Emits the canned cycle via
//! `emit_drill_block`, then — when `chamfer_after_width_mm` is set
//! (rt1.20 Stufenfase) — walks a single revolution at each hole's
//! rim at the V-bit chamfer depth.

// CAM/sim pedantic-lint exemption: lead-ramp sample counts are
// bounded by a tiny constant; the f64 cast for trig math is fine.
#![allow(clippy::cast_precision_loss)]

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::Setup;
use crate::cam::VcObject;
use crate::gcode::{emit_drill_block, emit_stufenfase_rim_block, PostProcessor, StufenfaseHole};
use crate::geometry::{Point2, SegmentKind};
use crate::pipeline::setup_resolver::{resolve_peck_step, synthesize_op_setup};
use crate::pipeline::{
    emit_toolchange_envelope, op_includes_object, synthesize_finish_setup, PipelineError,
    PipelineWarning,
};
use crate::project::{DrillCycle, Op, OpKind, Project, SpotConfig};

/// Returns `true` when the driver actually emitted an internal
/// drill→chamfer toolchange envelope (nguf). Used by `run_per_op` to
/// decide whether to bias `prev_tool_id` to `finish_tool_id` for the
/// next op's M6 decision.
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
) -> Result<bool, PipelineError> {
    // r2af: optional spot/centerdrill pre-pass BEFORE the main drill
    // block. Twist drills walk on hard / polished stock — the spot
    // dimple locks the chisel edge so the main bit drops on-nominal.
    // The pre-pass runs at every hole center the offset list contains
    // (Simple G81 cycle, depth = spot_depth_mm).
    if let OpKind::Drill {
        spot_first: Some(spot),
        ..
    } = &op.kind
    {
        emit_spot_pre_pass(op, project, setup, offsets, *spot, post, last_pos, warnings)?;
    }
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
            return emit_stufenfase(op, project, objects, setup, w, post, last_pos, warnings);
        }
    }
    Ok(false)
}

/// r2af: emit the spot/centerdrill pre-pass at every hole the main
/// drill op targets. Runs BEFORE the main drill block. Uses a
/// Simple G81 cycle (no peck — at 0.3-1.0 mm depths the peck would
/// retract above stock between every micro-peck, pointless). When
/// `spot.spot_tool_id` differs from `op.tool_id` the function emits
/// the standard safety envelope around both swaps (main → spot at
/// entry, spot → main on exit) so the operator hand-changes both
/// times. Positive / zero / non-finite `spot_depth_mm` collapses
/// the spot to a no-op (early return) — the field gate is
/// `Option<SpotConfig>` but we still defend against bogus values.
#[allow(clippy::too_many_arguments)]
fn emit_spot_pre_pass<P: PostProcessor>(
    op: &Op,
    project: &Project,
    main_setup: &Setup,
    offsets: &[PolylineOffset],
    spot: SpotConfig,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<(), PipelineError> {
    if !spot.spot_depth_mm.is_finite() || spot.spot_depth_mm >= 0.0 {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "drill_spot_depth_non_negative".into(),
            message: format!(
                "Drill op '{}' has spot_first.spot_depth_mm = {:.4} (must be negative to dimple stock); skipping the spot pre-pass.",
                op.name, spot.spot_depth_mm
            ),
        });
        return Ok(());
    }
    if offsets.is_empty() {
        return Ok(());
    }
    // Resolve the spot tool. If it doesn't exist, warn and skip the
    // pre-pass (don't fail the whole op — the main drill still runs).
    let Some(spot_tool) = project.tools.iter().find(|t| t.id == spot.spot_tool_id) else {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "drill_spot_tool_missing".into(),
            message: format!(
                "Drill op '{}': spot_first.spot_tool_id={} is not in the project's tool library; skipping the spot pre-pass.",
                op.name, spot.spot_tool_id
            ),
        });
        return Ok(());
    };
    // Synthesize a tiny synthetic op pointing at the spot tool so the
    // tool's feeds / RPMs / pierce settings flow through the regular
    // setup path. Override the depth to spot_depth_mm so emit_drill_block
    // doesn't accidentally drill to the main op's depth.
    let mut spot_op = op.clone();
    spot_op.tool_id = spot.spot_tool_id;
    spot_op.finish_tool_id = None;
    // Force a Simple drill cycle and clear the chamfer flag — only
    // the spot pre-pass runs here. spot_first is cleared so the
    // synthesized op doesn't infinite-loop.
    spot_op.kind = OpKind::Drill {
        cycle: DrillCycle::Simple { dwell_sec: 0.0 },
        chamfer_after_width_mm: None,
        pattern: None,
        spot_first: None,
    };
    spot_op.params.depth = spot.spot_depth_mm;
    let mut spot_setup = synthesize_op_setup(&spot_op, project, warnings)?;
    // Spot block shares the main op's start_depth / fast_move_z so
    // the rapid retract plane stays consistent across the
    // spot→main→chamfer sequence.
    spot_setup.mill.start_depth = main_setup.mill.start_depth;
    spot_setup.mill.fast_move_z = main_setup.mill.fast_move_z;

    // If the spot tool differs from the main drill tool, emit a
    // toolchange envelope BEFORE the spot block AND a return swap
    // AFTER it. The post's spindle state is reset between blocks so
    // the M5+dwell pre-swap fires unconditionally on the post-spot
    // return — matches the dual-tool / stufenfase pattern.
    let needs_swap = spot.spot_tool_id != op.tool_id;
    if needs_swap {
        post.raw(&format!(
            "; spot: toolchange to T{} for spot pre-pass",
            spot_tool.id
        ));
        emit_toolchange_envelope(
            post,
            &project.machine,
            main_setup,
            Some(spot_tool),
            spot_tool.id,
            false,
            // Spot pre-pass runs at the spot tool's library speed — no
            // finish-pass override here (liyy).
            None,
        );
    }
    post.raw(&format!("; OP {} spot", op.id));
    emit_drill_block(
        &spot_setup,
        offsets,
        DrillCycle::Simple { dwell_sec: 0.0 },
        post,
        last_pos,
    );
    if needs_swap {
        // Swap back to the main drill tool before the main block runs.
        let main_tool = project
            .tools
            .iter()
            .find(|t| t.id == op.tool_id)
            .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;
        post.raw(&format!(
            "; spot: toolchange back to T{} for main drill",
            main_tool.id
        ));
        emit_toolchange_envelope(
            post,
            &project.machine,
            main_setup,
            Some(main_tool),
            main_tool.id,
            false,
            // Returning to the main drill tool's own speed (liyy).
            None,
        );
    }
    Ok(())
}

/// Single full-revolution rim chamfer emitted after the drill block.
/// V-bit depth comes from the cutter's tip angle and the user-set
/// chamfer width. Honors `op.finish_tool_id` for dual-tool
/// drill+chamfer setups. Returns `true` when an actual drill→chamfer
/// toolchange envelope was emitted (nguf).
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
) -> Result<bool, PipelineError> {
    // sq8z: rim chamfer is now emitted as a single G2/G3 full-circle
    // (via `emit_stufenfase_rim_block`) plus a short angled lead-in
    // ramp. The legacy 64-chord-G1 polyline path is gone.
    let cutter_id = op.finish_tool_id.unwrap_or(op.tool_id);
    let cutter = project
        .tools
        .iter()
        .find(|t| t.id == cutter_id)
        .ok_or(PipelineError::UnknownTool(op.id, cutter_id))?;
    // e63q: pass tip_diameter so the cone math accounts for the
    // bit's nose-flat (engraver-style V-bits have a small flat that
    // shifts the cone's z=0 width).
    let chamfer_z = crate::cam::chamfer::chamfer_depth(
        width_mm,
        cutter.tip_angle_deg,
        cutter.tip_diameter.unwrap_or(0.0),
    );
    if chamfer_z.abs() < 1e-9 {
        return Ok(false);
    }
    let mut holes: Vec<StufenfaseHole> = Vec::new();
    let mut found = 0usize;
    let mut non_circle_skipped = 0usize;
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
            // s43q: stufenfase only chamfers true Circle objects today.
            // Closed contours that are arcs / lines / polygons get
            // dropped without complaint, leaving the user wondering why
            // their square holes got no rim chamfer. Warn explicitly so
            // the silent skip becomes visible.
            non_circle_skipped += 1;
            continue;
        }
        let Some(center) = first.center else {
            continue;
        };
        let r = first.start.distance(center);
        if r < 0.05 {
            continue;
        }
        // x412: insert a spiral lead-in BEFORE the flat revolution so
        // the V-bit doesn't plunge vertically at the rim. Match the pmpk
        // pattern: ramp Z from 0 down to chamfer_z at LEAD_IN_ANGLE_DEG
        // (10°) from horizontal along the rim, then walk one full
        // revolution at chamfer_z. If the rim's circumference is too
        // short for the 10° ramp, the lead-in occupies the full
        // circumference and the slope steepens (depth still reaches
        // chamfer_z — still better than a vertical plunge).
        //
        // sq8z: emit the flat revolution as a single G2/G3 full-circle
        // (handled by `emit_stufenfase_rim_block`) rather than the
        // pre-fix 64-chord G1 polyline. Only the ramp's varying-Z piece
        // still needs polyline samples — at LEAD_RAMP_STEPS the chord
        // error stays well under 1 % of `r` for any realistic chamfer.
        const LEAD_IN_ANGLE_DEG: f64 = 10.0;
        const LEAD_RAMP_STEPS: usize = 12;
        let circumference = std::f64::consts::TAU * r;
        let needed_arc = chamfer_z.abs() / LEAD_IN_ANGLE_DEG.to_radians().tan();
        let lead_arc = needed_arc.min(circumference);
        let lead_angle = lead_arc / r; // radians swept by the ramp
                                       // Lead-in resolution: enough samples to chord-tessellate the
                                       // arc to ~1% of `r`. Compute from the angular sweep so short
                                       // ramps don't over-sample.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let lead_steps = ((lead_angle / std::f64::consts::TAU * (LEAD_RAMP_STEPS as f64)).ceil()
            as usize)
            .max(4);
        let mut ramp: Vec<(f64, f64, f64)> = Vec::with_capacity(lead_steps + 1);
        for i in 0..=lead_steps {
            let t = (i as f64) / (lead_steps as f64);
            let a = -lead_angle + t * lead_angle;
            let z = chamfer_z * t;
            ramp.push((center.x + r * a.cos(), center.y + r * a.sin(), z));
        }
        // The ramp ends at angle 0 by construction (a = 0 when t = 1).
        // The full-circle revolution starts AND ends at that same point;
        // the post emits one G2/G3 with target XY == start XY and I/J
        // pointing back to the rim center.
        holes.push(StufenfaseHole {
            center,
            radius: r,
            // Match the legacy polyline orientation (lead-in walked
            // from negative angle toward 0 CCW, flat revolution walked
            // 0 → TAU CCW). CCW ⇒ G3.
            ccw: true,
            flat_z: chamfer_z,
            ramp,
        });
        found += 1;
    }
    if non_circle_skipped > 0 {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "stufenfase_non_circle_skipped".into(),
            message: format!(
                "drill op '{}': stufenfase rim chamfer only fires on closed Circle objects; {non_circle_skipped} closed contour(s) (arc-chains, polygons, etc.) were skipped without a chamfer.",
                op.name
            ),
        });
    }
    if found == 0 {
        return Ok(false);
    }
    let mut chamfer_setup = drill_setup.clone();
    let mut swapped = false;
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
                // liyy: the rim-chamfer block emits at the resolved finish
                // RPM — spin up to it directly to avoid a transient M3 at
                // the rough speed.
                Some(finish_setup.tool.speed),
            );
            chamfer_setup = finish_setup;
            swapped = true;
        }
    }
    emit_stufenfase_rim_block(&chamfer_setup, &holes, post, last_pos);
    Ok(swapped)
}

#[cfg(test)]
mod tests {
    use crate::cam::setup::MachineConfig;
    use crate::cam::setup::ToolOffset;
    use crate::geometry::Point2;
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
            stock: None,
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
            stock: None,
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
            stock: None,
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
            stock: None,
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
            stock: None,
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
                    spot_first: None,
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
            stock: None,
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
            stock: None,
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
            stock: None,
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
                    spot_first: None,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
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
        // e63q: with the vbit's 0.1mm tip flat, chamfer revolution Z
        // = -(1 - 0.05) / tan(45°) = -0.95 (not -1; the pre-e63q
        // formula ignored the tip flat).
        assert!(
            resp.gcode.contains("Z-0.95"),
            "expected chamfer revolution at Z-0.95 (90° tip + 1mm width, e63q tip-flat correction):\n{}",
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
                    spot_first: None,
                },
                tool_id: 1,
                finish_tool_id: Some(2),
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
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
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
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
            .next_back()
            .map_or_else(
                || panic!("expected a drill cycle line in:\n{}", resp.gcode),
                |(i, _)| i,
            );
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
            .map_or_else(
                || panic!("expected a G0 after the drill block in:\n{}", resp.gcode),
                |(i, _)| i,
            );
        assert!(
            g80_idx < next_g0_after_drill,
            "G80 (idx {g80_idx}) must precede the next G0 (idx {next_g0_after_drill}):\n{}",
            resp.gcode
        );
    }

    /// x412: a stufenfase chamfer must not begin with a vertical
    /// G1 plunge at the rim XY. Pre-fix `emit_stufenfase` built a
    /// flat 64-point polyline at `chamfer_z` directly, and
    /// `emit_vcarve_block` then drove the cutter G1 down from
    /// `start_depth=0` to `chamfer_z` at the SAME XY as the first
    /// rim point — a vertical plunge that snaps sharp V-bit tips on
    /// hardwood / aluminum. The fix prepends a spiral lead-in
    /// (`LEAD_IN_ANGLE_DEG=10`°) so the first G1 with a Z change also
    /// moves in XY.
    #[test]
    fn stufenfase_first_g1_is_not_vertical_plunge_at_rim() {
        let mut vbit_drill = vbit();
        vbit_drill.kind = ToolKind::Drill;
        vbit_drill.diameter = 3.0;
        vbit_drill.tip_angle_deg = 90.0;
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
                    spot_first: None,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // Find the chamfer block — it's after the drill cycle's G80
        // cancel-canned-cycle marker.
        let g80_idx = resp.gcode.find("\nG80").unwrap_or_else(|| {
            panic!("expected G80 between drill and chamfer in:\n{}", resp.gcode)
        });
        let chamfer = &resp.gcode[g80_idx..];
        // Locate the first G1 with a Z token AFTER any rapids /
        // straight-Z plunge to start_depth=0. That is the first
        // *cutting* descent toward chamfer_z. It MUST also include an
        // X or Y delta (i.e. not a pure vertical plunge at the rim).
        // Walk past the G0 X/Y rapid, the G1 Z0 plunge, and find the
        // first subsequent G1 that has a non-zero Z move below 0.
        let mut saw_g1_to_zero = false;
        let mut first_descent: Option<String> = None;
        for raw in chamfer.lines() {
            let l = raw.trim_start();
            if l.is_empty() || l.starts_with(';') {
                continue;
            }
            if !l.starts_with("G1 ") {
                continue;
            }
            // Lead-in plunge to Z0 ("Z0" or "Z-0.0" etc.) — skip it,
            // it happens above stock.
            if l.contains('Z') && !l.contains('X') && !l.contains('Y') {
                saw_g1_to_zero = true;
                continue;
            }
            // First G1 with a Z token that ALSO has X or Y — the lead
            // descent. If we instead see a pure G1 Z<negative> as the
            // first descending move, that's the bug.
            if l.contains('Z') {
                first_descent = Some(l.to_string());
                break;
            }
        }
        assert!(
            saw_g1_to_zero,
            "x412: expected a G1 plunge to z=0 before the chamfer descent:\n{chamfer}"
        );
        let first = first_descent
            .unwrap_or_else(|| panic!("x412: expected a chamfer-descent G1 in:\n{chamfer}"));
        assert!(
            first.contains('X') || first.contains('Y'),
            "x412: first chamfer descent must include XY motion (spiral lead-in), \
             got pure vertical plunge: `{first}`\nchamfer block:\n{chamfer}"
        );
    }

    /// sq8z: the rim revolution emits as a single G2/G3 full-circle
    /// (which the linuxcnc / grbl posts may split into two half-arcs
    /// for full-circle handling) rather than the legacy 64-chord G1
    /// polyline. The total G2+G3 lines for the rim should be a small
    /// number (1 or 2 from full-circle splitting); the chamfer block
    /// should contain at most a handful of G1 moves (the lead-in
    /// ramp).
    #[test]
    fn sq8z_stufenfase_rim_emits_g2_full_circle_not_64_g1() {
        let mut vbit_drill = vbit();
        vbit_drill.kind = ToolKind::Drill;
        vbit_drill.diameter = 3.0;
        vbit_drill.tip_angle_deg = 90.0;
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
                    spot_first: None,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // The chamfer block follows the drill's G80. Count G1 / G2 / G3
        // tokens AFTER G80 so the canned-cycle output (which doesn't
        // emit cut G1s) doesn't confuse the totals.
        let g80_idx = resp.gcode.find("\nG80").unwrap_or_else(|| {
            panic!("expected G80 between drill and chamfer in:\n{}", resp.gcode)
        });
        let chamfer = &resp.gcode[g80_idx..];
        let g2_count = chamfer
            .lines()
            .filter(|l| l.trim_start().starts_with("G2 "))
            .count();
        let g3_count = chamfer
            .lines()
            .filter(|l| l.trim_start().starts_with("G3 "))
            .count();
        let g1_count = chamfer
            .lines()
            .filter(|l| l.trim_start().starts_with("G1 "))
            .count();
        // Pre-fix: 64 chord-G1s for the rim plus a handful of lead-in
        // G1s — total > 60. Post-fix: short ramp (a dozen at most) plus
        // 1 or 2 arc moves for the full circle.
        assert!(
            g2_count + g3_count >= 1,
            "sq8z: expected at least one G2/G3 arc for the rim revolution; got g2={g2_count} g3={g3_count} in:\n{chamfer}",
        );
        assert!(
            g1_count < 30,
            "sq8z: expected fewer than 30 G1 moves (lead-ramp only); got {g1_count} in:\n{chamfer}",
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
            stock: None,
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

    /// r2af: a Drill op with `spot_first` set emits a spot pre-pass
    /// at every hole center BEFORE the main drill block. The spot
    /// block uses a Simple G81 cycle (no peck) and runs at the
    /// configured `spot_depth_mm`. When the spot tool differs from
    /// the main drill tool, two toolchanges fire (main → spot, then
    /// spot → main).
    #[test]
    fn drill_with_spot_first_emits_spot_pre_pass_block() {
        let center = Point2::new(5.0, 7.0);
        let mut spot_tool = endmill(2, 2.0);
        spot_tool.id = 2;
        let mut main_drill = endmill(1, 6.0);
        main_drill.id = 1;
        let machine = MachineConfig {
            supports_toolchange: true,
            ..MachineConfig::default()
        };
        let mut params = OpParams::mill_default();
        params.depth = -5.0;
        params.start_depth = 0.0;
        params.fast_move_z = 5.0;
        let project = Project {
            segments: closed_circle(center, 0.5),
            machine,
            tools: vec![main_drill, spot_tool],
            operations: vec![Op {
                id: 1,
                name: "Drill+spot".into(),
                enabled: true,
                kind: OpKind::Drill {
                    cycle: crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
                    chamfer_after_width_mm: None,
                    pattern: None,
                    spot_first: Some(crate::project::SpotConfig {
                        spot_depth_mm: -0.5,
                        spot_tool_id: 2,
                    }),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // Spot block exists — labeled with `; OP 1 spot` comment.
        let lines: Vec<&str> = resp.gcode.lines().collect();
        let spot_marker = lines.iter().position(|l| l.contains("OP 1 spot"));
        assert!(
            spot_marker.is_some(),
            "r2af: expected `; OP 1 spot` marker in gcode:\n{}",
            resp.gcode
        );
        // Spot block has its own G81 drill cycle at depth -0.5.
        let spot_g81 = lines
            .iter()
            .skip(spot_marker.unwrap())
            .find(|l| l.starts_with("G81 "));
        assert!(
            spot_g81.is_some_and(|l| l.contains("Z-0.5")),
            "r2af: expected spot G81 at Z-0.5 in spot block; got:\n{}",
            resp.gcode
        );
        // The main G81 still fires at the main depth -5.
        let main_g81_at_main_depth = lines
            .iter()
            .any(|l| l.starts_with("G81 ") && l.contains("Z-5"));
        assert!(
            main_g81_at_main_depth,
            "r2af: expected main G81 at Z-5; got:\n{}",
            resp.gcode
        );
        // T2 M6 fires once (main → spot) and T1 M6 fires twice
        // (program entry + spot → main).
        let t2_m6 = resp.gcode.lines().filter(|l| l.contains("T2 M6")).count();
        assert!(
            t2_m6 >= 1,
            "r2af: expected at least one T2 M6 for spot toolchange:\n{}",
            resp.gcode
        );
    }

    /// r2af: a Drill op WITHOUT `spot_first` set must NOT emit any
    /// spot block — the legacy path is exactly preserved.
    #[test]
    fn drill_without_spot_first_emits_no_spot_block() {
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
            stock: None,
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
            !resp.gcode.contains("OP 1 spot"),
            "drill without spot_first should not emit a spot block:\n{}",
            resp.gcode
        );
    }
}
