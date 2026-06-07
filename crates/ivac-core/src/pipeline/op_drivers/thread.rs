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
use crate::pipeline::warnings::push_tool_fit_kind_warnings;
use crate::pipeline::{cancelled, op_includes_object, CancelToken, PipelineError, PipelineWarning};
use crate::project::{Op, OpKind, Project};

/// o3od: cheap pre-check used by `run_per_op` to decide whether the
/// toolchange envelope (M5+dwell → M6 → z-shift → M3+dwell) needs to
/// fire BEFORE this op. Mirrors the driver's own "no closed circles"
/// short-circuit (`emitted == 0` → `thread_no_circles` warning).
/// Returns `true` only when at least one selected closed object is a
/// circle (single Circle segment, or an Arc chain with all the same
/// center — the same shapes the driver accepts). Returning `false`
/// skips the envelope; the driver still runs so it emits the
/// `thread_no_circles` warning.
#[must_use]
pub(in crate::pipeline) fn thread_would_emit(op: &Op, objects: &[VcObject]) -> bool {
    if !matches!(op.kind, OpKind::Thread { .. }) {
        return false;
    }
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
        match first.kind {
            SegmentKind::Circle => {
                if first.center.is_some() {
                    return true;
                }
            }
            SegmentKind::Arc => {
                let Some(c) = first.center else { continue };
                let all_same_center = obj.segments.iter().all(|s| {
                    matches!(s.kind, SegmentKind::Arc | SegmentKind::Circle)
                        && s.center.is_some_and(|sc| {
                            (sc.x - c.x).abs() < 1e-4 && (sc.y - c.y).abs() < 1e-4
                        })
                });
                if all_same_center {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

// Thread driver runs the per-circle helix walker; rather than threading
// state through five helpers, the per-revolution Z table lives inline.
// 55o4 tracks the broader pipeline split.
// juvx: `THREAD_START_RADIUS_FRAC` and `MIN_BORE_RADIUS_MM` consts
// live near their use sites so each carries its rationale comment
// inline.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::items_after_statements
)]
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
    // lo4j: surface tool-kind mismatches (e.g. user pointed a Drill or
    // LaserBeam at a Thread op) the same way the V-Carve / Halfpipe /
    // standard drivers do. Pre-fix the thread driver silently emitted
    // a helix using whatever cutter happened to be configured —
    // including non-rotating tools the user almost certainly did not
    // mean to thread with.
    push_tool_fit_kind_warnings(op, project, setup, warnings);
    let OpKind::Thread {
        pitch_mm,
        internal,
        climb,
        radial_passes,
        start_angle_rad,
        thread_depth_mm,
    } = op.kind
    else {
        return Ok(());
    };
    // mniu: thread depth = radial bite past the source-circle wall.
    // For an ISO metric 60° thread the canonical single-flank depth is
    // `0.6495 × pitch` (H × 5/8 where H = pitch × √3/2). The
    // pre-mniu code skipped this entirely — the cutter walked a helix
    // tangent to the bore/stud wall with ZERO engagement (literally
    // kissed it) and emitted a perfectly clean program that cut no
    // thread at all. The fix is to OFFSET the cutter past the wall by
    // `thread_depth` so the cutting edge actually engages the material.
    let thread_depth = thread_depth_mm
        .filter(|d| d.is_finite() && *d > 0.0)
        .unwrap_or(0.6495 * pitch_mm);
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
    // ttoa: when the requested Z range is smaller than one full pitch
    // (e.g. a shallow chase / finishing pass), the helix emitter clamps
    // to a minimum of one full revolution at the configured pitch so
    // the cutter doesn't degenerate to a single G1 diagonal across the
    // bore. Surface a `thread_dz_less_than_pitch` warning so the user
    // knows the Z descent will be steeper than `pitch` over the helix.
    if (bottom_z - top_z).abs() < pitch_mm {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "thread_dz_less_than_pitch".into(),
            message: format!(
                "Thread op '{}' has |Z range| ({:.4} mm) smaller than pitch ({:.4} mm). Emitting one full helical turn at the configured pitch so the cutter doesn't shortcut across the bore; the helix descent will be faster than the configured pitch.",
                op.name,
                (bottom_z - top_z).abs(),
                pitch_mm,
            ),
        });
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
        // mniu: helix radius places the cutter so its working edge
        // sits `thread_depth` PAST the source-circle wall.
        //   internal: cutter outer edge at `bore_radius + thread_depth`
        //     → helix (cutter centerline) at
        //     `bore_radius + thread_depth - tool_radius`. The helix
        //     therefore SHRINKS by `thread_depth` relative to the
        //     pre-mniu "tangent" radius (bore - tool), so the cutter's
        //     OUTER edge (helix + tool) reaches into the wall by exactly
        //     `thread_depth`.
        //   external: cutter inner edge at `stud_radius - thread_depth`
        //     → helix at `stud_radius - thread_depth + tool_radius`.
        //     The helix GROWS by `tool_radius` past the stud wall and
        //     shrinks by `thread_depth`, so the cutter's INNER edge
        //     bites the stud's flank by `thread_depth`.
        let helix_radius = if internal {
            bore_radius - tool_radius + thread_depth
        } else {
            bore_radius + tool_radius - thread_depth
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
        // THREAD_START_RADIUS_FRAC of the final engagement up to the
        // full engagement. mniu: ramp anchors are the zero-engagement
        // radius (cutter just kisses the wall) and the full-engagement
        // helix_radius (cutter bites by `thread_depth`). Linear lerp
        // by `frac` between the two — the per-pass radial bite is
        // therefore `frac × thread_depth`.
        //
        // Per-side directions:
        //   internal: zero = bore - tool (kiss), full = bore - tool +
        //     thread_depth. Radius GROWS toward the wall with frac.
        //   external: zero = bore + tool (kiss), full = bore + tool -
        //     thread_depth. Radius SHRINKS toward the wall with frac.
        let kiss_radius = if internal {
            bore_radius - tool_radius
        } else {
            bore_radius + tool_radius
        };
        for pass in 0..n_passes {
            let frac = if n_passes == 1 {
                1.0
            } else {
                THREAD_START_RADIUS_FRAC
                    + (1.0 - THREAD_START_RADIUS_FRAC) * (f64::from(pass) / f64::from(n_passes - 1))
            };
            // Lerp from zero-engagement (kiss) to full-engagement
            // (helix_radius). Works for both internal (helix > kiss)
            // and external (helix < kiss) without a sign branch.
            let pass_radius = kiss_radius + (helix_radius - kiss_radius) * frac;
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
                setup.tool.spindle_direction,
            );
            if path.len() >= 2 {
                // 4p8c: prepend an axial lead-in arc segment so the
                // cutter doesn't engage the full thread tooth at the
                // first G1 of revolution 0. The lead-in sits at the
                // kiss_radius (the cutter just touches the wall, zero
                // engagement) at top_z, and ramps in over a
                // quarter-turn to the pass_radius. The full helix
                // body follows unchanged.
                //
                // We approximate the lead-in as a chord polyline at the
                // same chord density as the helix (steps_per_rev=64,
                // default DEFAULT_STEPS_PER_REV in cam::thread). Over a
                // quarter-turn (16 chord steps) the radial interpolation
                // is from kiss_radius → pass_radius and Z stays at
                // top_z. This gives the cutter a tangential ramp into
                // material before the helical descent begins.
                let leadin = thread_lead_in(
                    center,
                    kiss_radius,
                    pass_radius,
                    top_z,
                    start_angle_rad,
                    climb,
                    internal,
                    setup.tool.spindle_direction,
                );
                let mut combined = leadin;
                combined.extend(path);
                polylines.push(combined);
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
        // FINAL pass (frac = 1.0) → bore_r - tool_r + thread_depth
        // (mniu: includes the radial bite past the bore wall).
        for obj in objects {
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
                let r = br - tool_radius + thread_depth;
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

/// 4p8c: axial lead-in for the thread helix. Returns waypoints that
/// take the cutter from the kiss radius (no engagement) to the helix
/// pass radius over approximately a quarter revolution, all at the
/// helix top Z. Prepended to the helix waypoints so the cutter eases
/// into the thread tooth instead of slamming the full engagement on
/// revolution 0.
///
/// The geometry mirrors `cam::thread::helix_waypoints`: the lead-in
/// winds in the same direction (climb XOR !internal, XOR spindle) as
/// the helix so the lead-in tangentially flows into the first helix
/// waypoint. Radius is linearly interpolated from `kiss_radius` to
/// `final_radius` across `LEAD_IN_STEPS` chord steps over
/// `LEAD_IN_SWEEP_RAD` radians.
///
/// `start_angle_rad` matches the helix entry angle; the lead-in ENDS
/// at `start_angle_rad` (so the last lead-in waypoint coincides with
/// the first helix waypoint) and STARTS a quarter-turn back along the
/// helix winding direction.
#[allow(clippy::too_many_arguments)]
fn thread_lead_in(
    center: crate::geometry::Point2,
    kiss_radius: f64,
    final_radius: f64,
    top_z: f64,
    start_angle_rad: f64,
    climb: bool,
    internal: bool,
    spindle: crate::project::tool::SpindleDirection,
) -> Vec<(f64, f64, f64)> {
    use crate::project::tool::SpindleDirection;
    // 4p8c: quarter-turn lead-in over 16 chord steps (matching the
    // helix's 64 steps/rev default density). A quarter-turn at typical
    // M6 helix radii covers ≈ 2-3 mm of arc length — well over one
    // tool diameter, so the cutter ramps engagement instead of
    // engaging the full thread tooth on the first chip.
    const LEAD_IN_STEPS: usize = 16;
    const LEAD_IN_SWEEP_RAD: f64 = std::f64::consts::FRAC_PI_2; // quarter turn
    if !final_radius.is_finite() || final_radius <= 0.0 {
        return Vec::new();
    }
    // Match the helix winding direction (see helix_waypoints).
    let ccw_rh = climb ^ !internal;
    let ccw = match spindle {
        SpindleDirection::Cw => ccw_rh,
        SpindleDirection::Ccw => !ccw_rh,
    };
    let dir: f64 = if ccw { 1.0 } else { -1.0 };
    // Lead-in starts a quarter-turn back from `start_angle_rad` so the
    // last lead-in waypoint lands exactly at `start_angle_rad`. The
    // helix's first waypoint is at that same angle / `top_z`, so the
    // join is smooth.
    let start_theta = start_angle_rad - dir * LEAD_IN_SWEEP_RAD;
    let mut out = Vec::with_capacity(LEAD_IN_STEPS + 1);
    for i in 0..LEAD_IN_STEPS {
        // i ranges 0..LEAD_IN_STEPS; skip the final endpoint so the
        // helix's first waypoint isn't duplicated.
        #[allow(clippy::cast_precision_loss)]
        let t = (i as f64) / (LEAD_IN_STEPS as f64);
        let theta = start_theta + dir * t * LEAD_IN_SWEEP_RAD;
        let r = kiss_radius + (final_radius - kiss_radius) * t;
        let x = center.x + r * theta.cos();
        let y = center.y + r * theta.sin();
        out.push((x, y, top_z));
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::geometry::Point2;
    use crate::pipeline::test_helpers::{closed_circle, closed_square_offset, endmill};
    use crate::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
    use crate::project::{MachineConfig, ToolChangeStrategy};
    use crate::project::{Op, OpKind, OpParams, OpSource, Project};

    /// Thread op (rt1.17): a closed circle source + Thread op emits
    /// a helical descent. The gcode must contain the helix's bottom
    /// Z (rounded to 4 decimals) and a sweep of XY coordinates
    /// around the bore's center.
    ///
    /// mniu: helix radius is now
    /// `bore_radius - tool_radius + thread_depth` so the cutter's
    /// outer edge actually engages the wall by `thread_depth`. Test
    /// pins `thread_depth_mm = Some(0.5)` for a clean integer
    /// waypoint at X = 10 + 5 - 0.5 + 0.5 = 15.
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
                    thread_depth_mm: Some(0.5),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                group: None,
                pin_order: false,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
            group_ops_by_tool: false,
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
        // Internal: helix walks at bore - tool + thread_depth
        // = 5 - 0.5 + 0.5 = 5.0 mm around center (10, 20). One
        // waypoint sits at (10 + 5, 20) = (15, 20).
        assert!(
            resp.gcode.contains("X15 ")
                || resp.gcode.contains("X15.0")
                || resp.gcode.contains("X15\n"),
            "expected helix waypoint at X=15 (bore - tool + thread_depth):\n{}",
            resp.gcode
        );
    }

    /// mniu: external thread engages the stud — the cutter inner
    /// edge bites by `thread_depth` past the stud wall. Before
    /// mniu the helix sat tangent to the stud (zero engagement, no
    /// chip). Verify the helix radius shrinks below
    /// `stud_radius + tool_radius` by the configured depth.
    #[test]
    fn thread_op_external_helix_engages_stud_by_thread_depth() {
        let center = Point2::new(0.0, 0.0);
        let stud_radius = 5.0;
        let segments = closed_circle(center, stud_radius);
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        // 1mm cutter, thread_depth = 0.5 mm.
        // Helix radius = stud + tool_r - depth = 5 + 0.5 - 0.5 = 5.0.
        // Waypoint at (center.x + helix_r, center.y) = (5, 0).
        // (Pre-mniu the helix would have walked at 5 + 0.5 = 5.5 —
        // tangent to the stud, zero cut.)
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
                    internal: false,
                    climb: false,
                    radial_passes: 1,
                    start_angle_rad: 0.0,
                    thread_depth_mm: Some(0.5),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                group: None,
                pin_order: false,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
            group_ops_by_tool: false,
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // External helix should NOT contain a waypoint at X=5.5 (the
        // pre-mniu tangent radius — zero engagement). It SHOULD
        // contain one at X=5 (stud + tool - depth = 5 + 0.5 - 0.5).
        assert!(
            !resp.gcode.contains("X5.5"),
            "external helix must not sit tangent to the stud (pre-mniu bug):\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("X5 ")
                || resp.gcode.contains("X5.0")
                || resp.gcode.contains("X5\n"),
            "expected external helix waypoint at X=5 (stud + tool - depth):\n{}",
            resp.gcode
        );
    }

    /// mniu: `thread_depth` defaults to the ISO 60° formula
    /// `0.6495 × pitch_mm` when the field is `None`. Verify the
    /// driver picks up the default instead of treating None as 0
    /// (which would reproduce the pre-mniu zero-engagement bug).
    #[test]
    fn thread_op_uses_iso_default_when_depth_unset() {
        let center = Point2::new(0.0, 0.0);
        let bore_radius = 5.0;
        let segments = closed_circle(center, bore_radius);
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        // 1mm pitch ⇒ ISO depth = 0.6495 mm.
        // Internal helix = 5 - 0.5 + 0.6495 = 5.1495 mm.
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
                    thread_depth_mm: None,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                group: None,
                pin_order: false,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
            group_ops_by_tool: false,
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // Expect a waypoint at X=5.1495 (helix at bore-tool+ISO_depth).
        // The post emits to 4 decimals.
        assert!(
            resp.gcode.contains("X5.1495"),
            "expected helix at X=5.1495 (ISO default depth):\n{}",
            resp.gcode
        );
        // And the helix MUST NOT be tangent (pre-mniu X=4.5).
        assert!(
            !resp.gcode.contains("X4.5 ")
                && !resp.gcode.contains("X4.5\n")
                && !resp.gcode.contains("X4.5000"),
            "ISO default helix must not match pre-mniu tangent radius 4.5:\n{}",
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
                    thread_depth_mm: None,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                group: None,
                pin_order: false,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
            group_ops_by_tool: false,
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
            // mniu: post-mniu helix_r = bore - tool + thread_depth.
            // With pitch=1 (depth≈0.65), a 3 mm tool gave helix_r =
            // 1 - 1.5 + 0.65 ≈ 0.15 which still slipped past the
            // > 0.05 guard. Use a 5 mm tool so helix_r ≈ -0.85 and
            // the guard fires.
            tools: vec![endmill(1, 5.0)],
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
                    thread_depth_mm: None,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                group: None,
                pin_order: false,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
            group_ops_by_tool: false,
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
    /// followed by a G0 lift to `fast_z`.
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
                    thread_depth_mm: None,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                group: None,
                pin_order: false,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
            group_ops_by_tool: false,
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
    /// thread, 3mm cutter (`tool_r` = 1.5). mniu: pinning
    /// `thread_depth_mm = Some(1.5)` makes full-engagement
    /// `helix_r` = bore - tool + depth = 3 - 1.5 + 1.5 = 3.0. Outer
    /// edge at `helix_r` + `tool_r` = 4.5, so factor = 3.0 / 4.5 =
    /// 2/3 and compensated feed = 300 × 2/3 = 200 mm/min. The
    /// emitted F-line should be 200.
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
                    // mniu: pin to a round value so the
                    // compensation ratio simplifies to 2/3.
                    thread_depth_mm: Some(1.5),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                group: None,
                pin_order: false,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
            group_ops_by_tool: false,
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // Expected compensated feedrate: 300 × 3.0 / 4.5 = 200.
        let mut after_op_marker = false;
        let mut found_compensated = false;
        for line in resp.gcode.lines() {
            if line.trim_start().starts_with("; OP ") {
                after_op_marker = true;
                continue;
            }
            if after_op_marker && line.trim() == "F200" {
                found_compensated = true;
                break;
            }
        }
        assert!(
            found_compensated,
            "expected compensated F200 inside the thread block (300 × 3/4.5 = 200); got:\n{}",
            resp.gcode,
        );
    }

    /// o3od: a Thread op whose source contains no closed circles
    /// (the typical "user pointed Thread at a square" misconfig)
    /// must NOT emit a toolchange envelope. Before the fix the
    /// driver returned with `thread_no_circles` warning AFTER the
    /// envelope (M6 + dwells) had already been written — the
    /// operator hand-swapped to the thread mill, the spindle warmed
    /// up, then the program emitted ZERO cut moves and the next op
    /// would M6 right back to the previous tool.
    ///
    /// With the o3od fix the envelope is gated on
    /// `thread_would_emit`; a Thread op against a closed-square
    /// source returns false and the M6 line is suppressed entirely.
    #[test]
    fn thread_op_skips_toolchange_envelope_when_no_circles() {
        // Two ops: a Profile against the square (T1) followed by a
        // Thread also targeting the square (T2). Without the fix
        // the Thread op would emit `T2 M6` between the Profile and
        // the Thread block; with the fix the M6 is suppressed because
        // Thread has nothing to emit.
        let mut machine = MachineConfig::default();
        machine.tool_change = ToolChangeStrategy::Atc;
        let mut t1 = endmill(1, 3.0);
        t1.id = 1;
        let mut t2 = endmill(2, 1.0);
        t2.id = 2;
        let params_profile = OpParams::mill_default();
        let mut params_thread = OpParams::mill_default();
        params_thread.depth = -3.0;
        params_thread.start_depth = 0.0;
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine,
            tools: vec![t1, t2],
            operations: vec![
                Op {
                    id: 1,
                    name: "Profile".into(),
                    enabled: true,
                    kind: OpKind::Profile {
                        offset: crate::project::ToolOffset::Outside,
                        contour: crate::project::ContourParams::default(),
                        profile: crate::project::ProfileParams::default(),
                    },
                    tool_id: 1,
                    finish_tool_id: None,
                    source: OpSource::All,
                    params: params_profile,
                    group: None,
                    pin_order: false,
                },
                Op {
                    id: 2,
                    name: "Thread (no circles)".into(),
                    enabled: true,
                    kind: OpKind::Thread {
                        pitch_mm: 1.0,
                        internal: true,
                        climb: true,
                        radial_passes: 1,
                        start_angle_rad: 0.0,
                        thread_depth_mm: None,
                    },
                    tool_id: 2,
                    finish_tool_id: None,
                    source: OpSource::All,
                    params: params_thread,
                    group: None,
                    pin_order: false,
                },
            ],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
            group_ops_by_tool: false,
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // The thread driver still surfaces its "no circles" warning.
        assert!(
            resp.warnings.iter().any(|w| w.kind == "thread_no_circles"),
            "thread_no_circles warning should fire even when envelope is suppressed; got: {:?}",
            resp.warnings.iter().map(|w| &w.kind).collect::<Vec<_>>(),
        );
        // The only T<n> M6 line should be T1 — for the Profile op.
        // T2 M6 used to fire just before the empty Thread block.
        let t2_m6_count = resp
            .gcode
            .lines()
            .filter(|l| l.contains("T2 M6") || l.trim() == "T2" || l.trim() == "M6")
            .filter(|l| l.contains("T2"))
            .count();
        assert_eq!(
            t2_m6_count, 0,
            "o3od: T2 M6 must not appear when the Thread op produces no output; got gcode:\n{}",
            resp.gcode
        );
    }

    /// lo4j: a Thread op assigned a wrong-kind tool (Drill / `DragKnife`
    /// / `LaserBeam`) must surface a `tool_kind_mismatch` warning. The
    /// thread driver routes tool-fit sanity through the shared
    /// `push_tool_fit_kind_warnings` helper at op entry; pre-fix the
    /// driver silently emitted a helix with whatever cutter the user
    /// configured, including ones that can't physically cut a thread.
    #[test]
    fn lo4j_thread_op_with_drill_tool_emits_kind_mismatch_warning() {
        let center = Point2::new(0.0, 0.0);
        let radius = 5.0;
        let segments = closed_circle(center, radius);
        // Configure a Drill tool — the driver should still attempt to
        // emit (it doesn't reject the op) but must raise the warning.
        let mut tool = endmill(1, 1.0);
        tool.kind = crate::project::ToolKind::Drill;
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![Op {
                id: 1,
                name: "Thread+Drill".into(),
                enabled: true,
                kind: OpKind::Thread {
                    pitch_mm: 1.0,
                    internal: true,
                    climb: true,
                    radial_passes: 1,
                    start_angle_rad: 0.0,
                    thread_depth_mm: Some(0.5),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                group: None,
                pin_order: false,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
            stock: None,
            relief_sources: Vec::new(),
            group_ops_by_tool: false,
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
            resp.warnings.iter().any(|w| w.kind == "tool_kind_mismatch"),
            "lo4j: expected tool_kind_mismatch warning for Thread + Drill tool; got: {:?}",
            resp.warnings.iter().map(|w| &w.kind).collect::<Vec<_>>(),
        );
    }
}
