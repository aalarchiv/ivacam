//! Halfpipe pocket driver. Reuses V-Carve's medial-axis sweep but
//! derives the per-axis Z from the configured half-pipe profile
//! (`CircularArc { R }` ⇒ `z = -(R - sqrt(R² - r²))` capped at `-R`;
//! `VBottom { θ }` ⇒ `z = -r / tan(θ/2)`). Both clip to the op's
//! nominal `depth`.

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
use crate::project::{Op, OpKind, PocketStrategy, Project};

// Halfpipe driver (Pocket strategy with cross-section profile) walks
// densified pocket regions per pass — see 55o4 for the planned split.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(in crate::pipeline) fn run_halfpipe_op<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(), PipelineError> {
    let OpKind::Pocket {
        strategy: PocketStrategy::Halfpipe { profile: strategy },
        ..
    } = op.kind
    else {
        return Ok(());
    };
    push_tool_fit_kind_warnings(op, project, setup, warnings);
    let tool = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;
    // Profile-specific tool-kind hint. CircularArc wants a ball-nose
    // whose radius matches the configured R; VBottom wants a V-bit.
    match strategy {
        crate::project::HalfpipeProfile::CircularArc { radius_mm } => {
            if !matches!(tool.kind, crate::project::ToolKind::BallNose) {
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "tool_kind_mismatch".into(),
                    message: format!(
                        "Halfpipe (CircularArc) op '{}' uses tool '{}' which is not a ball-nose. The cut floor profile assumes a ball-bottom cutter; flat / V-bit will not produce a true half-pipe.",
                        op.name, tool.name
                    ),
                });
            }
            let tool_r = tool.diameter * 0.5;
            if (tool_r - radius_mm).abs() > 0.5 * radius_mm.max(1.0) {
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "halfpipe_radius_mismatch".into(),
                    message: format!(
                        "Halfpipe op '{}': tool radius {:.3} mm doesn't match the configured profile radius {:.3} mm — the cut won't trace the desired pipe.",
                        op.name, tool_r, radius_mm
                    ),
                });
            }
        }
        crate::project::HalfpipeProfile::VBottom { .. } => {
            if !matches!(tool.kind, crate::project::ToolKind::VBit) {
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "tool_kind_mismatch".into(),
                    message: format!(
                        "Halfpipe (VBottom) op '{}' uses tool '{}' which is not a V-bit; the depth math assumes a cone.",
                        op.name, tool.name
                    ),
                });
            }
        }
    }

    let selected = ordered_selection(op, objects);
    let combine = source_combine_mode(op);
    let regions = combine_source_regions(objects, &selected, combine);
    if regions.is_empty() {
        return Ok(());
    }

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
                crate::cam::halfpipe::polyline_to_z(axis, strategy, z_cap);
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
            kind: "halfpipe_depth_limited".into(),
            message: format!(
                "Halfpipe op '{}' was depth-limited: the slot is wider than the configured profile cap (or the op's `depth` clipped it) at some medial-axis points.",
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
mod tests {
    use crate::cam::setup::MachineConfig;
    use crate::geometry::{Point2, Segment};
    use crate::pipeline::test_helpers::{closed_square_offset, endmill};
    use crate::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
    use crate::project::{Op, OpKind, OpParams, OpSource, Project, ToolEntry, ToolKind};

    /// Wirbeln (3e5): when a Pocket op uses a Wirbeln-tagged tool
    /// with a non-zero extra-width, the gcode body contains many more
    /// G1 moves than the same op without Wirbeln — the helical-spiral
    /// overlay subdivides every cut move at the spiral stride. The
    /// cascade-ring count stays the same (3e5 removed the v1
    /// `xy_step` clamp); the extra moves come from the overlay's
    /// stride stamping at gcode-emit time.
    #[test]
    fn wirbeln_tool_inflates_gcode_g1_count() {
        let tool_a = endmill(1, 6.0);
        let mut tool_b = endmill(1, 6.0);
        tool_b.wirbeln = true;
        tool_b.wirbeln_extra_width_mm = Some(2.0); // 1 mm spiral radius
        tool_b.wirbeln_stepover_mm = Some(2.0); // 2 mm stride per rev
        let params = OpParams::mill_default();
        let pocket = crate::project::PocketParams {
            xy_overlap: 0.5,
            ..crate::project::PocketParams::default()
        };
        let project_with_tool = |tool: ToolEntry| Project {
            segments: closed_square_offset(80.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: pocket.clone(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: params.clone(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp_a = run_pipeline(
            PipelineRequest {
                project: project_with_tool(tool_a),
                post_processor: Some(crate::pipeline::PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let resp_b = run_pipeline(
            PipelineRequest {
                project: project_with_tool(tool_b),
                post_processor: Some(crate::pipeline::PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let g1_a = resp_a.gcode.lines().filter(|l| l.starts_with("G1")).count();
        let g1_b = resp_b.gcode.lines().filter(|l| l.starts_with("G1")).count();
        assert!(
            g1_b > g1_a * 3,
            "Wirbeln overlay should multiply G1 count substantially: on={g1_b} vs off={g1_a}",
        );
        // Cascade ring count stays the same — the overlay doesn't add rings.
        assert_eq!(
            resp_a.stats.offset_count, resp_b.stats.offset_count,
            "3e5 removed the xy_step clamp; ring count should match",
        );
    }

    /// Wirbeln serde round-trip on `ToolEntry` (rt1.25). Default = false
    /// (skipped on serialize); when on with an override, both round-trip.
    #[test]
    fn wirbeln_serde_round_trip() {
        let mut tool = endmill(1, 6.0);
        let json_default = serde_json::to_string(&tool).unwrap();
        assert!(!json_default.contains("wirbeln"));
        tool.wirbeln = true;
        tool.wirbeln_stepover_mm = Some(0.75);
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"wirbeln\":true"));
        assert!(json.contains("wirbeln_stepover_mm"));
        let back: ToolEntry = serde_json::from_str(&json).unwrap();
        assert!(back.wirbeln);
        assert_eq!(back.wirbeln_stepover_mm, Some(0.75));
    }

    /// Halfpipe op (rt1.19): a closed region + Halfpipe `CircularArc`
    /// emits cutting moves whose Z dips to within tolerance of the
    /// configured profile radius along the centerline.
    #[test]
    fn halfpipe_circular_arc_emits_curved_floor() {
        // 40×8 mm narrow slot. Inscribed circle along the centerline
        // is ~4 mm radius (half-width). With profile R=5: at the
        // widest medial-axis point z = -(5 - sqrt(25 - 16)) = -2.
        let mut segments_8w: Vec<Segment> = Vec::new();
        let p = |x: f64, y: f64| Point2::new(x, y);
        segments_8w.push(Segment::line(p(0.0, 0.0), p(40.0, 0.0), "0", 7));
        segments_8w.push(Segment::line(p(40.0, 0.0), p(40.0, 8.0), "0", 7));
        segments_8w.push(Segment::line(p(40.0, 8.0), p(0.0, 8.0), "0", 7));
        segments_8w.push(Segment::line(p(0.0, 8.0), p(0.0, 0.0), "0", 7));

        let mut ball = endmill(1, 10.0);
        ball.kind = ToolKind::BallNose;
        let mut params = OpParams::mill_default();
        params.depth = -10.0; // permissive cap so the profile drives Z
        params.start_depth = 0.0;
        params.step = Some(-2.0);
        let project = Project {
            segments: segments_8w,
            machine: MachineConfig::default(),
            tools: vec![ball],
            operations: vec![Op {
                id: 1,
                name: "Halfpipe".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Halfpipe {
                        profile: crate::project::HalfpipeProfile::CircularArc { radius_mm: 5.0 },
                    },
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
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
        let any_deep_cut = resp.gcode.lines().any(|l| {
            if !l.starts_with("G1 ") {
                return false;
            }
            for tok in l.split_whitespace() {
                if let Some(rest) = tok.strip_prefix('Z') {
                    if let Ok(z) = rest.parse::<f64>() {
                        if z < -1.0 {
                            return true;
                        }
                    }
                }
            }
            false
        });
        assert!(
            any_deep_cut,
            "expected at least one G1 line with Z below -1 mm:\n{}",
            resp.gcode
        );
    }

    /// `PocketStrategy::Halfpipe` serde round-trip (rt1.19) covers both
    /// `CircularArc` and `VBottom` profiles.
    #[test]
    fn halfpipe_serde_round_trip() {
        let cases = [
            crate::project::PocketStrategy::Halfpipe {
                profile: crate::project::HalfpipeProfile::CircularArc { radius_mm: 5.0 },
            },
            crate::project::PocketStrategy::Halfpipe {
                profile: crate::project::HalfpipeProfile::VBottom {
                    included_angle_deg: 60.0,
                },
            },
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            assert!(json.contains("halfpipe"));
            let back: crate::project::PocketStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(back, case);
        }
    }
}
