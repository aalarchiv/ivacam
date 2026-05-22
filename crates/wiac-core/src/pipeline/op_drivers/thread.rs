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
        radial_passes,
        start_angle_rad,
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
    // sqnh: schedule multiple roughing passes when the user opts in
    // (`radial_passes > 1`). Each pass cuts at a fraction of the
    // final radial engagement, ramping linearly from
    // THREAD_START_RADIUS_FRAC of the final helix offset to the
    // full offset. radial_passes = 1 keeps the legacy single-helix
    // behaviour (full engagement in one revolution).
    const THREAD_START_RADIUS_FRAC: f64 = 0.75;
    let n_passes = radial_passes.max(1);
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
        // al30: guard against zero / near-zero bore radius. The
        // pre-al30 code only checked `helix_radius <= 0.05` which
        // caught internal-tool-too-large but missed corrupt source
        // data (zero-radius circle from a CAD import) on the EXTERNAL
        // branch — there the helix_radius came out to `tool_radius`
        // and the emitter happily wrote a tiny helical scratch around
        // the source XY where the user expected a real thread. Warn
        // + skip whenever the source circle is degenerate, regardless
        // of internal/external.
        const MIN_BORE_RADIUS_MM: f64 = 0.1;
        if bore_radius < MIN_BORE_RADIUS_MM {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "thread_zero_bore".into(),
                message: format!(
                    "Thread op '{}': source circle has radius {:.4} mm (< {MIN_BORE_RADIUS_MM:.2} mm) — looks like corrupt CAD import. Skipping; the helix would otherwise emit a scratch at the source XY.",
                    op.name, bore_radius
                ),
            });
            continue;
        }
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
        // sqnh: emit `n_passes` helices ramping from
        // THREAD_START_RADIUS_FRAC × helix_radius up to the full
        // helix_radius. For internal threads the cutter is INSIDE the
        // bore so a smaller radius means a LESS deeply engaged
        // thread; for external threads a smaller radius means MORE
        // standoff (no chip yet on the stud). The geometry below
        // computes per-pass effective radius accordingly.
        for pass in 0..n_passes {
            let frac = if n_passes == 1 {
                1.0
            } else {
                THREAD_START_RADIUS_FRAC
                    + (1.0 - THREAD_START_RADIUS_FRAC)
                        * (f64::from(pass) / f64::from(n_passes - 1))
            };
            let pass_radius = if internal {
                // Internal: helix at bore_radius - tool_radius for
                // full engagement. Reduced radius means cutter sits
                // CLOSER to bore center → less radial engagement.
                helix_radius * frac
            } else {
                // External: helix at stud_radius + tool_radius for
                // full engagement. A larger radius means MORE standoff
                // → smaller chip. Ramp from outer (no engagement) to
                // helix_radius (full engagement) inversely.
                let max_standoff = bore_radius + 2.0 * tool_radius;
                max_standoff + (helix_radius - max_standoff) * frac
            };
            if pass_radius <= 0.05 {
                continue;
            }
            let path = crate::cam::thread::helix_waypoints(
                center,
                pass_radius,
                top_z,
                bottom_z,
                pitch_mm,
                climb,
                internal,
                tool_radius,
                start_angle_rad,
            );
            if path.len() >= 2 {
                polylines.push(path);
                emitted += 1;
            }
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
    // zajd: feed compensation. When a small cutter walks a helix of
    // radius `helix_r`, the outer cutting edge at radius `helix_r +
    // tool_r` travels at F * (helix_r + tool_r) / helix_r. Tight
    // bores (small helix_r) amplify this — on an M6 bore (helix_r =
    // 3 - 1.5 = 1.5 mm with a 3 mm cutter) the outer edge moves at
    // 2× the commanded feed, doubling chipload on the tooth. The fix
    // is to REDUCE the commanded feed by helix_r / (helix_r + tool_r)
    // so the outer edge moves at the user's requested rate. The
    // compensation factor uses the deepest (= full-engagement) helix
    // radius across passes since that's the tightest case. For
    // external threads the convention flips: the cutter is on the
    // OUTSIDE of the stud, so the outer edge (on the air side) doesn't
    // engage stock, but the INSIDE edge does — same math, since the
    // inside edge sits at helix_r - tool_r. We still divide by
    // (helix_r + tool_r) (outermost cutting radius) for both cases —
    // the conservative bound.
    //
    // Find the smallest helix radius we emitted; that's the tightest
    // case across passes. Skip compensation when not internal (the
    // outer-edge speedup on an external thread is the other way
    // around — see thread.rs module docs — and not a chipload risk).
    if internal {
        let mut tightest: Option<f64> = None;
        // Re-derive tightest helix radius without re-walking objects.
        // For internal threads, the tightest engagement is at the
        // FINAL pass (frac = 1.0) → bore_r - tool_r.
        for (_idx, obj) in objects.iter().enumerate() {
            if !obj.closed {
                continue;
            }
            let Some(first) = obj.segments.first() else {
                continue;
            };
            let bore_r = match first.kind {
                crate::geometry::SegmentKind::Circle | crate::geometry::SegmentKind::Arc => {
                    first.center.map(|c| first.start.distance(c))
                }
                _ => None,
            };
            if let Some(br) = bore_r {
                let r = br - tool_radius;
                if r > 0.05 {
                    tightest = Some(tightest.map_or(r, |t| t.min(r)));
                }
            }
        }
        if let Some(helix_r) = tightest {
            let outer = helix_r + tool_radius;
            if outer > 1e-9 {
                let factor = helix_r / outer;
                let mut compensated = setup.clone();
                let compensated_rate =
                    ((f64::from(compensated.tool.rate_h) * factor).round()).max(1.0) as u32;
                compensated.tool.rate_h = compensated_rate;
                // Same compensation on the finish rate; the helix is
                // a single feed across the cut.
                let compensated_rate_finish =
                    ((f64::from(compensated.tool.rate_h_finish) * factor).round()).max(1.0) as u32;
                compensated.tool.rate_h_finish = compensated_rate_finish;
                emit_vcarve_block(&compensated, &polylines, post, last_pos);
                return Ok(());
            }
        }
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
                    radial_passes: 1,
                    start_angle_rad: 0.0,
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
                    radial_passes: 1,
                    start_angle_rad: 0.0,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
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
                    radial_passes: 1,
                    start_angle_rad: 0.0,
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

    /// sqnh: three radial passes on a single closed circle must
    /// produce three helices at scaled helix radii (75 %, 87.5 %,
    /// 100 %). Detect by counting how many distinct helical descents
    /// the gcode contains — each helix ends with a Z dive to bottom
    /// followed by a G0 lift to fast_z.
    #[test]
    fn thread_op_emits_one_helix_per_radial_pass() {
        let center = Point2::new(0.0, 0.0);
        let radius = 5.0;
        let segments = closed_circle(center, radius);
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 1.0)], // 1mm cutter → 0.5mm radius
            operations: vec![Op {
                id: 1,
                name: "Thread".into(),
                enabled: true,
                kind: OpKind::Thread {
                    pitch_mm: 1.0,
                    internal: true,
                    climb: true,
                    radial_passes: 3,
                    start_angle_rad: 0.0,
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
        // Each helix terminates with a G0 lift to fast Z (handled by
        // `emit_vcarve_block`); 3 passes ⇒ at least 3 separate lift
        // sequences. Match the post's lift token "G0 Z" — and count
        // distinct helix-end Z drops.
        let lift_count = resp
            .gcode
            .lines()
            .filter(|l| l.trim_start().starts_with("G0") && l.contains('Z'))
            .count();
        assert!(
            lift_count >= 3,
            "expected at least 3 G0-Z lifts for 3 passes; got {lift_count}\n{}",
            resp.gcode
        );
    }

    /// zajd: feed compensation for outer-edge speed. M6 internal
    /// thread with a 3mm cutter (full-engagement helix_r = 3 - 1.5
    /// = 1.5) and rate_h=300 — outer edge at (helix_r + tool_r) = 3
    /// walks at F * 3 / 1.5 = 2F. Compensated feed = 300 * 1.5/3 =
    /// 150 mm/min. The emitted F-line should be 150 (not 300).
    #[test]
    fn thread_op_compensates_feed_for_outer_edge_speed() {
        let center = Point2::new(0.0, 0.0);
        let bore_radius = 3.0; // M6-ish bore (radius)
        let segments = closed_circle(center, bore_radius);
        let mut tool = endmill(1, 3.0);
        tool.feed_rate = 300; // ToolEntry → setup.tool.rate_h
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![Op {
                id: 1,
                name: "Thread".into(),
                enabled: true,
                kind: OpKind::Thread {
                    pitch_mm: 1.0,
                    internal: true,
                    climb: true,
                    radial_passes: 1,
                    start_angle_rad: 0.0,
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
        // Expected compensated feedrate: 300 * 1.5 / 3.0 = 150.
        // Confirm F150 appears AFTER the OP marker (the program
        // prologue may also emit an uncompensated F300 as the header
        // feed — that's a pre-cut warm-up, not the cut feed).
        let mut after_op_marker = false;
        let mut found_compensated = false;
        for line in resp.gcode.lines() {
            if line.trim_start().starts_with("; OP ") {
                after_op_marker = true;
                continue;
            }
            if after_op_marker && line.trim() == "F150" {
                found_compensated = true;
                break;
            }
        }
        assert!(
            found_compensated,
            "expected compensated F150 inside the thread block (300 * 1.5/3 = 150); got:\n{}",
            resp.gcode,
        );
    }
}
