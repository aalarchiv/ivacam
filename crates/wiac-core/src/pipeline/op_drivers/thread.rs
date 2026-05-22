//! Helical thread emitter. For each selected closed circle in the
//! source set, compute the helix radius (bore − `tool_radius` for
//! internal, stud + `tool_radius` for external) and emit helix
//! waypoints between `start_depth` and `depth` at `pitch_mm` per
//! revolution. Reuses V-Carve's `emit_vcarve_block` since both walk a
//! pre-computed XYZ polyline at constant feed.

use crate::cam::setup::Setup;
use crate::cam::VcObject;
use crate::gcode::{emit_vcarve_block, PostProcessor};
use crate::geometry::{Point2, SegmentKind};
use crate::pipeline::{cancelled, op_includes_object, CancelToken, PipelineError, PipelineWarning};
use crate::project::{Op, OpKind, Project};

// Thread driver runs the per-circle helix walker; rather than threading
// state through five helpers, the per-revolution Z table lives inline.
// 55o4 tracks the broader pipeline split.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(in crate::pipeline) fn run_thread_op<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(), PipelineError> {
    let OpKind::Thread {
        pitch_mm,
        internal,
        climb,
    } = op.kind
    else {
        return Ok(());
    };
    let tool = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;
    let tool_radius = tool.diameter * 0.5;
    let top_z = op.params.start_depth;
    let bottom_z = op.params.depth;
    if (bottom_z - top_z).abs() < 1e-9 || pitch_mm <= 0.0 {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "thread_no_depth".into(),
            message: format!(
                "Thread op '{}' has zero Z range or non-positive pitch; nothing emitted.",
                op.name
            ),
        });
        return Ok(());
    }
    let mut polylines: Vec<Vec<(f64, f64, f64)>> = Vec::new();
    let mut emitted = 0usize;
    for (idx, obj) in objects.iter().enumerate() {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        if !op_includes_object(op, obj, idx) {
            continue;
        }
        if !obj.closed {
            continue;
        }
        // Accept any closed loop that is geometrically a circle:
        //   * A single Circle segment (the importer's preferred form).
        //   * A chain of Arc segments that all share the same center —
        //     what `chaining::segments_to_objects` produces for a
        //     DXF/SVG circle split into multiple arcs.
        let Some(first) = obj.segments.first() else {
            continue;
        };
        let (center, bore_radius) = match first.kind {
            SegmentKind::Circle => {
                let Some(c) = first.center else { continue };
                (c, first.start.distance(c))
            }
            SegmentKind::Arc => {
                let Some(c) = first.center else { continue };
                let radius = first.start.distance(c);
                let all_same_center = obj.segments.iter().all(|s| {
                    matches!(s.kind, SegmentKind::Arc | SegmentKind::Circle)
                        && s.center.is_some_and(|sc| {
                            (sc.x - c.x).abs() < 1e-4 && (sc.y - c.y).abs() < 1e-4
                        })
                });
                if !all_same_center {
                    continue;
                }
                (c, radius)
            }
            _ => continue,
        };
        let helix_radius = if internal {
            bore_radius - tool_radius
        } else {
            bore_radius + tool_radius
        };
        if helix_radius <= 0.05 {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "thread_tool_too_large".into(),
                message: format!(
                    "Thread op '{}': bore_radius {:.3} mm with tool_radius {:.3} mm leaves no room for an internal helix (needs bore > tool). Switch to external or pick a smaller cutter.",
                    op.name, bore_radius, tool_radius
                ),
            });
            continue;
        }
        let path = crate::cam::thread::helix_waypoints(
            center,
            helix_radius,
            top_z,
            bottom_z,
            pitch_mm,
            climb,
            internal,
            tool_radius,
        );
        if path.len() >= 2 {
            polylines.push(path);
            emitted += 1;
        }
    }
    if emitted == 0 {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "thread_no_circles".into(),
            message: format!(
                "Thread op '{}' didn't find any closed circles in the selected source.",
                op.name
            ),
        });
        return Ok(());
    }
    emit_vcarve_block(setup, &polylines, post, last_pos);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::cam::setup::MachineConfig;
    use crate::geometry::Point2;
    use crate::pipeline::test_helpers::{closed_circle, closed_square_offset, endmill};
    use crate::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
    use crate::project::{Op, OpKind, OpParams, OpSource, Project};

    /// Thread op (rt1.17): a closed circle source + Thread op emits
    /// a helical descent. The gcode must contain the helix's bottom
    /// Z (rounded to 4 decimals) and a sweep of XY coordinates
    /// around the bore's center.
    #[test]
    fn thread_op_emits_helical_descent_on_a_closed_circle() {
        let center = Point2::new(10.0, 20.0);
        let radius = 5.0;
        let segments = closed_circle(center, radius);
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 1.0)],
            operations: vec![Op {
                id: 1,
                name: "Thread".into(),
                enabled: true,
                kind: OpKind::Thread {
                    pitch_mm: 1.0,
                    internal: true,
                    climb: true,
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
        // Bottom Z = -3 → gcode contains Z-3 somewhere.
        assert!(
            resp.gcode.contains("Z-3"),
            "expected helix bottom Z-3 in gcode:\n{}",
            resp.gcode
        );
        // Internal: helix walks at (bore_radius - tool_radius) = 5 - 0.5 = 4.5 mm
        // around center (10, 20). One waypoint sits at (10 + 4.5, 20) = (14.5, 20).
        assert!(
            resp.gcode.contains("X14.5") || resp.gcode.contains("X14.5000"),
            "expected helix waypoint at X=14.5 (bore - tool_radius):\n{}",
            resp.gcode
        );
    }

    /// Thread op without a closed circle in the source emits a
    /// `thread_no_circles` warning and produces no toolpath.
    #[test]
    fn thread_op_without_circle_warns() {
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 1.0)],
            operations: vec![Op {
                id: 1,
                name: "Thread".into(),
                enabled: true,
                kind: OpKind::Thread {
                    pitch_mm: 1.0,
                    internal: true,
                    climb: true,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
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
        .unwrap();
        assert!(resp.warnings.iter().any(|w| w.kind == "thread_no_circles"));
    }

    /// Thread op with internal + a tool larger than the bore emits a
    /// `thread_tool_too_large` warning rather than producing a
    /// nonsensical helix.
    #[test]
    fn thread_op_internal_with_oversized_tool_warns() {
        let center = Point2::new(0.0, 0.0);
        let radius = 1.0; // 1mm bore
        let segments = closed_circle(center, radius);
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.start_depth = 0.0;
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)], // 3mm tool, bigger than the bore
            operations: vec![Op {
                id: 1,
                name: "Thread".into(),
                enabled: true,
                kind: OpKind::Thread {
                    pitch_mm: 1.0,
                    internal: true,
                    climb: true,
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
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp
            .warnings
            .iter()
            .any(|w| w.kind == "thread_tool_too_large"));
    }
}
