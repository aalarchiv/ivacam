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

/// Cheap pre-check used by `run_per_op` to decide whether the
/// toolchange envelope needs to fire before this op. Mirrors the
/// driver's own `regions.is_empty()` short-circuit at the top of
/// `run_halfpipe_op` — an op selecting open polylines / no closed
/// contours produces no medial-axis sweep at all and the M6 + dwells
/// were firing for a no-output op. Returning `false` skips the
/// envelope; the driver still runs (silently no-ops, matching the
/// existing behaviour for empty regions — there's no halfpipe warning
/// to surface today).
#[must_use]
pub(in crate::pipeline) fn halfpipe_would_emit(op: &Op, objects: &[VcObject]) -> bool {
    if !matches!(
        op.kind,
        OpKind::Pocket {
            strategy: PocketStrategy::Halfpipe { .. },
            ..
        }
    ) {
        return false;
    }
    let selected = ordered_selection(op, objects);
    let combine = source_combine_mode(op);
    !combine_source_regions(objects, &selected, combine).is_empty()
}

// Halfpipe driver (Pocket strategy with cross-section profile) walks
// densified pocket regions per pass.
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
            let tool_r = tool.effective_diameter() * 0.5;
            // The previous threshold was 50 % of the profile R,
            // which let large mismatches through silently (a 6 mm tool
            // on a 5 mm profile is a 20 % mismatch and used to pass —
            // the resulting floor isn't even close to the requested
            // half-pipe shape). The user-facing acceptance criterion is
            // a 10 % tolerance: anything looser produces visibly wrong
            // gcode. Made effectively constant here; if a future op
            // needs to override, add a per-op tolerance knob.
            let tolerance_factor = 0.10_f64;
            let allowed = tolerance_factor * radius_mm.max(1e-9);
            if (tool_r - radius_mm).abs() > allowed {
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "halfpipe_radius_mismatch".into(),
                    message: format!(
                        "Halfpipe op '{}': tool radius {:.3} mm doesn't match the configured profile radius {:.3} mm (tolerance ±{:.1} % ≈ ±{:.3} mm). The cut won't trace the desired pipe — pick a ball-nose tool whose diameter equals 2 × the profile radius.",
                        op.name, tool_r, radius_mm, tolerance_factor * 100.0, allowed,
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
    // Cap Z to the cutting flute length so the ball-nose shank
    // never engages stock. Fallback to tool radius when flute_length
    // isn't recorded — past that depth the shank starts to drag even
    // on a pointed bit if it's a long-thin cutter.
    let reach_z = tool
        .flute_length_mm
        .filter(|v| *v > 0.0)
        .unwrap_or(tool.effective_diameter() * 0.5);
    let tool_reach_z = Some(reach_z);

    let mut polylines: Vec<Vec<(f64, f64, f64)>> = Vec::new();
    let mut any_depth_limited = false;
    let mut any_tool_reach_limited = false;

    let tool_r_for_prune = tool.effective_diameter() * 0.5;
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
        let axes_raw = crate::cam::geometry_cache::medial_axis_cached(&vc_region, cancel);
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        // Prune medial-axis spurs (same hairy-boundary problem
        // as V-carve). For halfpipe the tip-radius is 0 (the cutter's
        // tip is the deepest point of the ball / V apex, not a flat
        // plateau), so only the short-branch rule fires.
        let axes = crate::cam::vcarve::prune_medial_axis(axes_raw, tool_r_for_prune, 0.0);
        // Build the flat boundary segment list so polyline_to_z
        // can detect re-entrant corners. The outer ring plus any
        // hole rings — same convention as the V-Carve medial-axis
        // builder.
        let mut boundary_segs: Vec<(Point2, Point2)> = Vec::new();
        let push_ring = |ring: &[Point2], out: &mut Vec<(Point2, Point2)>| {
            if ring.len() < 2 {
                return;
            }
            for i in 0..ring.len() {
                let a = ring[i];
                let b = ring[(i + 1) % ring.len()];
                if a.distance(b) > 1e-12 {
                    out.push((a, b));
                }
            }
        };
        push_ring(&region.boundary, &mut boundary_segs);
        for h in &region.holes {
            push_ring(h, &mut boundary_segs);
        }
        for axis in &axes {
            let (z_axis, depth_limited, tool_reach_limited) = crate::cam::halfpipe::polyline_to_z(
                axis,
                strategy,
                z_cap,
                tool_reach_z,
                Some(&boundary_segs),
            );
            if depth_limited {
                any_depth_limited = true;
            }
            if tool_reach_limited {
                any_tool_reach_limited = true;
            }
            // ratchet_emit returns sub-polylines split at any
            // above-surface gaps. Push each one as a separate cut
            // block so the caller G0-rapids between them at safe Z.
            // Per-tool lead-in angle override (0.0 ⇒ default 10°).
            for p in crate::cam::vcarve_emit::ratchet_emit_with_lead_in(
                &z_axis,
                dpp,
                setup.tool.vcarve_lead_in_angle_deg,
            ) {
                if p.len() >= 2 {
                    polylines.push(p);
                }
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
    if any_tool_reach_limited {
        let reach = reach_z;
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "halfpipe_tool_reach_exceeded".into(),
            message: format!(
                "Halfpipe op '{}': cut depth clipped to tool reach {:.3} mm (ball-nose '{}' flute length) at some medial-axis points. The profile is deeper than the cutter can reach without engaging the shank — pick a longer-flute tool or reduce the profile radius.",
                op.name, reach, tool.name,
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
    use crate::geometry::{Point2, Segment};
    use crate::pipeline::test_helpers::{closed_square_offset, endmill};
    use crate::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
    use crate::project::MachineConfig;
    use crate::project::{Op, OpKind, OpParams, OpSource, Project, ToolEntry, ToolKind};

    /// When a Pocket op uses a Whirl-tagged tool
    /// with a non-zero extra-width, the gcode body contains many more
    /// G1 moves than the same op without Whirl — the helical-spiral
    /// overlay subdivides every cut move at the spiral stride. The
    /// cascade-ring count stays the same (the `xy_step` clamp was
    /// removed); the extra moves come from the overlay's
    /// stride stamping at gcode-emit time.
    #[test]
    fn whirl_tool_inflates_gcode_g1_count() {
        let tool_a = endmill(1, 6.0);
        let mut tool_b = endmill(1, 6.0);
        tool_b.whirl = true;
        tool_b.whirl_extra_width_mm = Some(2.0); // 1 mm spiral radius
        tool_b.whirl_stepover_mm = Some(2.0); // 2 mm stride per rev
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
            "Whirl overlay should multiply G1 count substantially: on={g1_b} vs off={g1_a}",
        );
        // Cascade ring count stays the same — the overlay doesn't add rings.
        assert_eq!(
            resp_a.stats.offset_count, resp_b.stats.offset_count,
            "xy_step clamp was removed; ring count should match",
        );
    }

    /// Whirl serde round-trip on `ToolEntry`. Default = false
    /// (skipped on serialize); when on with an override, both round-trip.
    #[test]
    fn whirl_serde_round_trip() {
        let mut tool = endmill(1, 6.0);
        let json_default = serde_json::to_string(&tool).unwrap();
        assert!(!json_default.contains("whirl"));
        tool.whirl = true;
        tool.whirl_stepover_mm = Some(0.75);
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"whirl\":true"));
        assert!(json.contains("whirl_stepover_mm"));
        let back: ToolEntry = serde_json::from_str(&json).unwrap();
        assert!(back.whirl);
        assert_eq!(back.whirl_stepover_mm, Some(0.75));
    }

    /// A closed region + Halfpipe `CircularArc`
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

    /// A 20 % tool/profile-R mismatch — 6 mm tool on a 5 mm
    /// profile — used to pass silently under the loose 50 % threshold.
    /// The tightened 10 % gate now fires `halfpipe_radius_mismatch`.
    #[test]
    fn halfpipe_warns_when_tool_radius_mismatch_exceeds_10pct() {
        let mut segments_8w: Vec<Segment> = Vec::new();
        let p = |x: f64, y: f64| Point2::new(x, y);
        segments_8w.push(Segment::line(p(0.0, 0.0), p(40.0, 0.0), "0", 7));
        segments_8w.push(Segment::line(p(40.0, 0.0), p(40.0, 8.0), "0", 7));
        segments_8w.push(Segment::line(p(40.0, 8.0), p(0.0, 8.0), "0", 7));
        segments_8w.push(Segment::line(p(0.0, 8.0), p(0.0, 0.0), "0", 7));

        // 12-mm diameter ball ⇒ tool radius 6.0 mm. Profile R = 5.0 mm.
        // Mismatch = 1.0 mm = 20 % of R. Old threshold (50 %) ignored
        // this; new threshold (10 %) catches it.
        let mut ball = endmill(1, 12.0);
        ball.kind = ToolKind::BallNose;
        let mut params = OpParams::mill_default();
        params.depth = -10.0;
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
            resp.warnings
                .iter()
                .any(|w| w.kind == "halfpipe_radius_mismatch"),
            "expected halfpipe_radius_mismatch warning; got {:?}",
            resp.warnings.iter().map(|w| &w.kind).collect::<Vec<_>>(),
        );
    }

    /// Halfpipe with profile R larger than the cutter's flute
    /// length must clamp Z to -`flute_length` and emit a
    /// `halfpipe_tool_reach_exceeded` warning.
    #[test]
    fn halfpipe_tool_reach_clamps_deep_profile() {
        // 40mm-wide rectangle slot (matches the existing harness).
        let mut segs: Vec<Segment> = Vec::new();
        let p = |x: f64, y: f64| Point2::new(x, y);
        // 80×40 — middle inscribed circle radius = 20.
        segs.push(Segment::line(p(0.0, 0.0), p(80.0, 0.0), "0", 7));
        segs.push(Segment::line(p(80.0, 0.0), p(80.0, 40.0), "0", 7));
        segs.push(Segment::line(p(80.0, 40.0), p(0.0, 40.0), "0", 7));
        segs.push(Segment::line(p(0.0, 40.0), p(0.0, 0.0), "0", 7));

        // 40mm-diameter ball-nose (radius 20mm matches profile R=20)
        // but ONLY 5mm flute length — the bit can only engage stock
        // 5mm deep before the shank starts dragging.
        let mut ball = endmill(1, 40.0);
        ball.kind = ToolKind::BallNose;
        ball.flute_length_mm = Some(5.0);
        let mut params = OpParams::mill_default();
        params.depth = -50.0; // permissive; the tool cap should drive
        params.start_depth = 0.0;
        params.step = Some(-2.0);
        let project = Project {
            segments: segs,
            machine: MachineConfig::default(),
            tools: vec![ball],
            operations: vec![Op {
                id: 1,
                name: "Halfpipe".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Halfpipe {
                        profile: crate::project::HalfpipeProfile::CircularArc { radius_mm: 20.0 },
                    },
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
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
            resp.warnings
                .iter()
                .any(|w| w.kind == "halfpipe_tool_reach_exceeded"),
            "expected halfpipe_tool_reach_exceeded warning; got {:?}",
            resp.warnings.iter().map(|w| &w.kind).collect::<Vec<_>>(),
        );
        // Every G1 line must have Z >= -5 (no deeper than flute reach).
        for l in resp.gcode.lines().filter(|l| l.starts_with("G1 ")) {
            for tok in l.split_whitespace() {
                if let Some(rest) = tok.strip_prefix('Z') {
                    if let Ok(z) = rest.parse::<f64>() {
                        assert!(
                            z >= -5.0 - 1e-6,
                            "Z {z} exceeds tool flute reach (-5.0 mm) on line: {l}",
                        );
                    }
                }
            }
        }
    }

    /// `PocketStrategy::Halfpipe` serde round-trip covers both
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
