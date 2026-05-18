//! V-Carve op driver. Builds the medial axis of the source region(s)
//! and emits a per-axis ratchet sweep with depth varying from
//! `start_depth` to the geometric V-bit depth at each point.

// CAM/sim pedantic-lint exemption: STEPS-style sample counts cast to
// f64 for trig are tiny constants.
#![allow(clippy::cast_precision_loss)]

use crate::cam::setup::Setup;
use crate::cam::source_combine::combine_source_regions;
use crate::cam::VcObject;
use crate::gcode::{emit_vcarve_block, PostProcessor};
use crate::geometry::Point2;
use crate::pipeline::warnings::push_tool_fit_kind_warnings;
use crate::pipeline::{
    cancelled, effective_step, ordered_selection, source_combine_mode, CancelToken, PipelineError,
    PipelineWarning,
};
use crate::project::{Op, Project};

// V-Carve driver couples medial-axis sampling, multi-pass cascade, and
// optional finish-pass into a single state machine — see 55o4 for the
// planned per-stage extraction.
#[allow(clippy::too_many_arguments)]
pub(in crate::pipeline) fn run_vcarve_op<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(), PipelineError> {
    push_tool_fit_kind_warnings(op, project, setup, warnings);
    let tool = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;
    if !matches!(tool.kind, crate::project::ToolKind::VBit) {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tool_kind_mismatch".into(),
            message: format!(
                "V-Carve op '{}' uses tool '{}' which is not a V-bit. The carve depth is computed from the V-bit cone angle; engraver / endmill geometry won't produce a true V-groove.",
                op.name, tool.name
            ),
        });
    }
    let tip_angle_deg = tool.tip_angle_deg.clamp(1.0, 179.0);
    let tip_angle_rad = tip_angle_deg.to_radians();

    let selected = ordered_selection(op, objects);
    let combine = source_combine_mode(op);
    let regions = combine_source_regions(objects, &selected, combine);
    if regions.is_empty() {
        return Ok(());
    }

    // kbx5 step 2: V-Carve cap lives on VCarveParams.
    let r_cap = op.vcarve_params().and_then(|v| v.carve_max_width_mm);
    let z_cap = if op.params.depth.abs() > 1e-9 {
        Some(op.params.depth)
    } else {
        None
    };
    let dpp = effective_step(op, tool)
        .map(f64::abs)
        .unwrap_or(1.0)
        .max(0.05);

    let mut polylines: Vec<Vec<(f64, f64, f64)>> = Vec::new();
    let mut any_depth_limited = false;

    for region in &regions {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        if region.boundary.len() < 3 {
            continue;
        }
        let vc_region = crate::cam::vcarve::VcRegion {
            outer: region.boundary.clone(),
            holes: region.holes.clone(),
        };
        let axes = crate::cam::vcarve::medial_axis_cancellable(&vc_region, cancel);
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        for axis in &axes {
            let (z_axis, depth_limited) =
                crate::cam::vcarve::polyline_to_z(axis, tip_angle_rad, r_cap, z_cap);
            if depth_limited {
                any_depth_limited = true;
            }
            let path = crate::cam::vcarve_emit::ratchet_emit(&z_axis, dpp);
            if path.len() >= 2 {
                polylines.push(path);
            }
        }
    }

    if any_depth_limited {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "vcarve_depth_limited".into(),
            message: format!(
                "V-Carve op '{}' was depth-limited: the V-bit can't reach the geometric corner because depth and/or carve_max_width caps clipped the inscribed-circle radius.",
                op.name
            ),
        });
    }

    if polylines.is_empty() {
        return Ok(());
    }

    emit_vcarve_block(setup, &polylines, post, last_pos);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use crate::cam::setup::MachineConfig;
    use crate::geometry::{Point2, Segment};
    use crate::pipeline::test_helpers::vbit;
    use crate::pipeline::{run_pipeline, PipelineRequest};
    use crate::project::{Op, OpKind, OpParams, OpSource, Project};

    /// `VCarve` op produces a non-empty toolpath whose deepest cutting
    /// move sits well below `start_depth - 0.1` — proves the medial
    /// axis ratchet actually plunges into the slot rather than just
    /// tracing the boundary at z=0.
    #[test]
    fn vcarve_op_emits_cutting_moves_below_start_depth() {
        let op = Op {
            id: 7,
            name: "Carve".into(),
            enabled: true,
            kind: OpKind::VCarve {
                carve: crate::project::VCarveParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams {
                depth: -10.0,
                start_depth: 0.0,
                step: Some(-1.0),
                fast_move_z: 5.0,
                ..OpParams::default()
            },
            pattern: None,
        };
        let project = Project {
            segments: vec![
                Segment::line(Point2::new(0.0, 0.0), Point2::new(20.0, 0.0), "0", 7),
                Segment::line(
                    Point2::new(20.0, 0.0),
                    Point2::new(10.0, 17.320_508),
                    "0",
                    7,
                ),
                Segment::line(Point2::new(10.0, 17.320_508), Point2::new(0.0, 0.0), "0", 7),
            ],
            machine: MachineConfig::default(),
            tools: vec![vbit()],
            operations: vec![op],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .expect("pipeline ran");
        assert!(!resp.gcode.is_empty(), "gcode should not be empty");
        let any_deep = resp
            .toolpath
            .iter()
            .any(|s| s.to.z < -0.1 && !matches!(s.kind, crate::gcode::preview::MoveKind::Rapid));
        assert!(
            any_deep,
            "expected at least one cutting move below start_depth - 0.1; got {} toolpath segs",
            resp.toolpath.len()
        );
    }

    #[test]
    fn vcarve_op_round_trips_through_serde_json() {
        let op = Op {
            id: 11,
            name: "Sign carve".into(),
            enabled: true,
            kind: OpKind::VCarve {
                carve: crate::project::VCarveParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams {
                depth: -8.0,
                start_depth: 0.0,
                step: Some(-0.8),
                fast_move_z: 6.0,
                carve_max_width_mm: Some(4.0),
                multi_pass_refine: true,
                ..OpParams::default()
            },
            pattern: None,
        };
        let json = serde_json::to_string(&op).expect("serialize");
        let back: Op = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back.kind, OpKind::VCarve { .. }));
        assert_eq!(back.params.carve_max_width_mm, Some(4.0));
        assert!(back.params.multi_pass_refine);
        assert_eq!(back.params.depth, -8.0);
    }
}
