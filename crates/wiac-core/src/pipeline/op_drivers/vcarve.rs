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
// planned per-stage extraction. Length budget waived for the same
// reason — the function is the per-stage split's main entry point.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
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
    let tip_radius_mm = tool.tip_diameter.unwrap_or(0.0).max(0.0) * 0.5;
    // Physical reach of the V-bit. Past `diameter / 2` the cutter has
    // run out of cone — engaging deeper would scrape the shank into
    // the stock. Folded into the r_cap below.
    let tool_reach_r = tool.diameter * 0.5;

    let selected = ordered_selection(op, objects);
    let combine = source_combine_mode(op);
    let regions = combine_source_regions(objects, &selected, combine);
    // Guard (rt1.7 / user report): combine_source_regions returns empty
    // when the selection has no closable contours — e.g. the user pointed
    // a V-Carve op at a single-line text layer or at open polylines from
    // an SVG <line>. Silently no-op'ing left the user wondering why
    // Generate produced no toolpath. Surface it instead.
    if regions.is_empty() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "vcarve_no_closed_region".into(),
            message: format!(
                "V-Carve op '{}' has no closed source regions. V-Carve operates on the medial axis of a closed shape — pick objects whose contours close (DXF LWPOLYLINE/POLYLINE/CIRCLE/etc.). Single-line text or open polylines need an Engrave op.",
                op.name,
            ),
        });
        return Ok(());
    }

    // kbx5 step 2: V-Carve cap lives on VCarveParams.
    // Effective r cap = min(user carve_max_width_mm, tool reach radius).
    // The tool-reach clamp prevents the medial-axis-driven depth from
    // running deeper than the cone can physically reach, which would
    // produce gcode that scrapes the shank into the workpiece.
    let user_cap = op.vcarve_params().and_then(|v| v.carve_max_width_mm);
    let effective_r_cap = match user_cap {
        Some(c) => Some(c.min(tool_reach_r)),
        None => Some(tool_reach_r),
    };
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

    // r8ut: full_medial_axis defaults to false → Estlcam-style perimeter
    // pass. The medial-axis path is opt-in for the rare "carve a depth
    // gradient across the entire interior" workflow (think Aspire-style
    // relief). The two paths share tip-angle / tip-radius / reach-cap
    // / depth-cap math; only the traversal shape differs.
    let full_medial = op
        .vcarve_params()
        .is_some_and(|v| v.full_medial_axis);
    // rt1.7: optional pre-offset for the source region. Inlay plug side
    // sets this to the desired gap so the plug ends up `gap` mm smaller
    // per side than the pocket. None / 0 = identity (the common case).
    let source_inset = op
        .vcarve_params()
        .and_then(|v| v.source_inset_mm)
        .unwrap_or(0.0)
        .max(0.0);

    for region in &regions {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        if region.boundary.len() < 3 {
            continue;
        }
        // rt1.7: when the user requested a pre-inset (inlay plug side),
        // shrink the boundary and any holes BEFORE the V-carve pass.
        // If the inset collapses the region we silently skip it — the
        // plug is geometrically too small to exist (e.g. a 0.1 mm-wide
        // sliver with 0.1 mm gap).
        let (inset_outer, inset_holes_storage);
        let (boundary, holes): (&[Point2], &[Vec<Point2>]) = if source_inset > 1e-9 {
            let rings = crate::cam::offsets::boundary_offset_inward(
                &region.boundary,
                &region.holes,
                source_inset,
            );
            if rings.is_empty() {
                continue;
            }
            // clipper2 with EndType::Polygon returns the inset boundary
            // first, then any inset hole rings. Inscribed area > 0
            // means at least one outer ring; treat rings[0] as the new
            // outer and rings[1..] as the new holes.
            inset_outer = rings[0].clone();
            inset_holes_storage = rings.into_iter().skip(1).collect::<Vec<_>>();
            (inset_outer.as_slice(), inset_holes_storage.as_slice())
        } else {
            (region.boundary.as_slice(), region.holes.as_slice())
        };
        if boundary.len() < 3 {
            continue;
        }
        if full_medial {
            // Medial-axis traversal (pre-r8ut behaviour). Plunges along
            // every interior medial-axis chain; depth follows local
            // inscribed radius up to effective_r_cap.
            let vc_region = crate::cam::vcarve::VcRegion {
                outer: boundary.to_vec(),
                holes: holes.to_vec(),
            };
            let axes = crate::cam::geometry_cache::medial_axis_cached(&vc_region, cancel);
            if cancelled(cancel) {
                return Err(PipelineError::Cancelled);
            }
            for axis in &axes {
                let (z_axis, depth_limited) = crate::cam::vcarve::polyline_to_z(
                    axis,
                    tip_angle_rad,
                    tip_radius_mm,
                    effective_r_cap,
                    z_cap,
                );
                if depth_limited {
                    any_depth_limited = true;
                }
                let path = crate::cam::vcarve_emit::ratchet_emit(&z_axis, dpp);
                if path.len() >= 2 {
                    polylines.push(path);
                }
            }
        } else {
            // Default Estlcam-style perimeter pass: inset the boundary
            // (and holes) by R = effective_r_cap; emit each resulting
            // ring at constant z = -(R - tip_r) / tan(angle / 2).
            // Centre plateau and any deep interior are left untouched.
            let r_offset = effective_r_cap.unwrap_or(tool_reach_r);
            if r_offset <= tip_radius_mm + 1e-9 {
                // The cap is below the bit's flat tip — perimeter offset
                // would lie at z=0, indistinguishable from an engrave.
                // Bail with a warning so the user knows nothing got cut.
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "vcarve_below_tip_radius".into(),
                    message: format!(
                        "V-Carve op '{}' effective carve width ({:.3} mm) is at or below the V-bit's flat tip ({:.3} mm); the bit's nose rides the surface and no material would be removed. Pick a sharper bit or raise carve_max_width_mm.",
                        op.name, r_offset, tip_radius_mm,
                    ),
                });
                continue;
            }
            // Compute target z. polyline_to_z's r-cap logic isn't needed
            // here — we already know we're cutting at the cap.
            let tan_half = (tip_angle_rad * 0.5).tan().max(1e-9);
            let mut z_target = -(r_offset - tip_radius_mm) / tan_half;
            if let Some(c) = z_cap {
                let limit = c.abs();
                if z_target < -limit {
                    z_target = -limit;
                    any_depth_limited = true;
                }
            }
            let rings = crate::cam::offsets::boundary_offset_inward(
                boundary,
                holes,
                r_offset,
            );
            for ring in rings {
                if ring.len() < 2 {
                    continue;
                }
                // Close the ring so the cutter returns to its start —
                // perimeter passes are closed loops, not open polylines.
                let mut path: Vec<(f64, f64, f64)> = ring
                    .iter()
                    .map(|p| (p.x, p.y, z_target))
                    .collect();
                let first = path[0];
                let last = *path.last().expect("len >= 2");
                if (first.0 - last.0).hypot(first.1 - last.1) > 1e-9 {
                    path.push(first);
                }
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

    /// User report: V-Carve op pointed at an open polyline (e.g. a
    /// single-line text layer) silently produced no toolpath because
    /// `combine_source_regions` returns empty. Now warns instead.
    #[test]
    fn vcarve_op_warns_when_no_closed_region() {
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
                depth: -3.0,
                start_depth: 0.0,
                step: Some(-1.0),
                fast_move_z: 5.0,
                ..OpParams::default()
            },
        };
        // A single LINE segment doesn't form a closed contour. No
        // region → expect the warning.
        let project = Project {
            segments: vec![Segment::line(
                Point2::new(0.0, 0.0),
                Point2::new(50.0, 0.0),
                "0",
                7,
            )],
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
        assert!(
            resp.warnings
                .iter()
                .any(|w| w.kind == "vcarve_no_closed_region"),
            "expected vcarve_no_closed_region warning; got {:?}",
            resp.warnings
                .iter()
                .map(|w| &w.kind)
                .collect::<Vec<_>>(),
        );
    }

    /// Tool-reach clamp (rbl follow-up): a 6mm V-bit physically can't
    /// engage past r = 3mm. For a 30x30 square (incircle radius 15mm)
    /// the medial axis hits r = 15 — without the clamp the depth math
    /// would dive to z = -15 / tan(30°) ≈ -26mm regardless of the bit's
    /// 3mm reach. The clamp keeps z above ≈ -5.2mm (3 / tan(30°)).
    /// Exercises `full_medial_axis` = true so the medial-axis path runs.
    #[test]
    fn vcarve_op_respects_tool_reach() {
        let op = Op {
            id: 7,
            name: "Carve".into(),
            enabled: true,
            kind: OpKind::VCarve {
                carve: crate::project::VCarveParams {
                    full_medial_axis: true,
                    ..Default::default()
                },
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams {
                depth: -50.0, // very deep so the tool-reach cap is the limiting factor
                start_depth: 0.0,
                step: Some(-1.0),
                fast_move_z: 5.0,
                ..OpParams::default()
            },
        };
        // 30x30 closed square — incircle radius 15mm.
        let project = Project {
            segments: vec![
                Segment::line(Point2::new(0.0, 0.0), Point2::new(30.0, 0.0), "0", 7),
                Segment::line(Point2::new(30.0, 0.0), Point2::new(30.0, 30.0), "0", 7),
                Segment::line(Point2::new(30.0, 30.0), Point2::new(0.0, 30.0), "0", 7),
                Segment::line(Point2::new(0.0, 30.0), Point2::new(0.0, 0.0), "0", 7),
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
        let z_min = resp
            .toolpath
            .iter()
            .map(|s| s.to.z)
            .fold(0.0_f64, f64::min);
        // vbit() default is diameter 6.35mm, tip 60° → tool_reach_r = 3.175,
        // tan(30°) ≈ 0.5774, z_min_expected ≈ -5.50mm. The cone-floor
        // depth could only go that deep with the clamp; without it, the
        // medial-axis radius of ~15mm produces z ≈ -26mm.
        assert!(
            z_min > -10.0,
            "with tool-reach clamp, z_min should be > -10mm; got {z_min}",
        );
        assert!(
            resp.warnings
                .iter()
                .any(|w| w.kind == "vcarve_depth_limited"),
            "tool-reach cap should mark depth_limited",
        );
    }

    /// r8ut: default V-Carve (`full_medial_axis` = false) traces ONLY a
    /// perimeter loop on a 30×30 square — no spine cuts through the
    /// interior. Compare with `vcarve_op_respects_tool_reach` above
    /// which exercises the medial-axis branch on the same geometry.
    #[test]
    fn vcarve_op_default_is_perimeter_only() {
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
        };
        // 30x30 closed square — incircle radius 15mm. Boundary inset by
        // R = 3.175 mm (vbit tool reach) → expect a ~23.65×23.65 square
        // loop centred on (15, 15).
        let project = Project {
            segments: vec![
                Segment::line(Point2::new(0.0, 0.0), Point2::new(30.0, 0.0), "0", 7),
                Segment::line(Point2::new(30.0, 0.0), Point2::new(30.0, 30.0), "0", 7),
                Segment::line(Point2::new(30.0, 30.0), Point2::new(0.0, 30.0), "0", 7),
                Segment::line(Point2::new(0.0, 30.0), Point2::new(0.0, 0.0), "0", 7),
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
        // All cut points should be in a thin annular band near the
        // boundary — no point should be deep inside the square (which
        // is where the medial-axis spine would land). The inset is
        // ≤ 5 mm so points beyond (5, 25) on both axes signal a spine
        // cut that shouldn't exist in perimeter mode.
        let cut_pts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| !matches!(s.kind, crate::gcode::preview::MoveKind::Rapid))
            .map(|s| (s.to.x, s.to.y, s.to.z))
            .collect();
        assert!(!cut_pts.is_empty(), "perimeter pass should emit cut moves");
        for (x, y, _z) in &cut_pts {
            // Distance from boundary <= ~5 mm tolerance. The boundary
            // is the [0..30] square; the closest edge gives the
            // distance.
            let d = x.min(30.0 - x).min(*y).min(30.0 - y);
            assert!(
                d < 5.0,
                "perimeter mode emitted a cut at ({x:.2}, {y:.2}) which is {d:.2} mm from any edge — looks like a spine cut",
            );
        }
        // The perimeter should sit at the expected z depth (≈ -5.5 mm).
        let z_min = cut_pts.iter().map(|(_, _, z)| *z).fold(0.0_f64, f64::min);
        assert!(
            z_min < -3.0 && z_min > -8.0,
            "expected perimeter depth around -5.5 mm; got z_min = {z_min}",
        );
    }

    /// rt1.7: a Plug-side V-Carve with `source_inset_mm = 1.0` emits a
    /// perimeter loop further from the original boundary than the
    /// Pocket-side default. We pair them on the same 30×30 square and
    /// verify the plug's outer extent is ≈ 1 mm narrower than the
    /// pocket's — that's the geometric clearance an inlay needs.
    #[test]
    fn vcarve_inlay_plug_offsets_by_source_inset() {
        let make_op = |source_inset: Option<f64>| Op {
            id: 7,
            name: "Carve".into(),
            enabled: true,
            kind: OpKind::VCarve {
                carve: crate::project::VCarveParams {
                    source_inset_mm: source_inset,
                    ..Default::default()
                },
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams {
                depth: -3.0,
                start_depth: 0.0,
                step: Some(-1.0),
                fast_move_z: 5.0,
                ..OpParams::default()
            },
        };
        let square = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(30.0, 0.0), "0", 7),
            Segment::line(Point2::new(30.0, 0.0), Point2::new(30.0, 30.0), "0", 7),
            Segment::line(Point2::new(30.0, 30.0), Point2::new(0.0, 30.0), "0", 7),
            Segment::line(Point2::new(0.0, 30.0), Point2::new(0.0, 0.0), "0", 7),
        ];
        let extent_x = |resp: &crate::pipeline::PipelineResponse| -> (f64, f64) {
            let xs: Vec<f64> = resp
                .toolpath
                .iter()
                .filter(|s| !matches!(s.kind, crate::gcode::preview::MoveKind::Rapid))
                .map(|s| s.to.x)
                .collect();
            (
                xs.iter().fold(f64::INFINITY, |a, &b| a.min(b)),
                xs.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b)),
            )
        };
        let run = |op: Op| -> crate::pipeline::PipelineResponse {
            let project = Project {
                segments: square.clone(),
                machine: MachineConfig::default(),
                tools: vec![vbit()],
                operations: vec![op],
                fixtures: Vec::default(),
                text_layers: Vec::default(),
            };
            run_pipeline(
                PipelineRequest {
                    project,
                    post_processor: None,
                },
                |_, _, _| {},
            )
            .expect("pipeline ran")
        };
        let pocket = run(make_op(None));
        let plug = run(make_op(Some(1.0)));
        let (pmin, pmax) = extent_x(&pocket);
        let (qmin, qmax) = extent_x(&plug);
        // Plug perimeter is offset inward by 1mm MORE than pocket.
        assert!(
            (qmin - pmin - 1.0).abs() < 0.2,
            "plug x_min {qmin:.3} should be ~1mm inside pocket x_min {pmin:.3}",
        );
        assert!(
            (pmax - qmax - 1.0).abs() < 0.2,
            "plug x_max {qmax:.3} should be ~1mm inside pocket x_max {pmax:.3}",
        );
    }

    #[test]
    fn vcarve_op_round_trips_through_serde_json() {
        let op = Op {
            id: 11,
            name: "Sign carve".into(),
            enabled: true,
            kind: OpKind::VCarve {
                carve: crate::project::VCarveParams {
                    carve_max_width_mm: Some(4.0),
                    multi_pass_refine: true,
                    full_medial_axis: false,
                    source_inset_mm: None,
                },
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams {
                depth: -8.0,
                start_depth: 0.0,
                step: Some(-0.8),
                fast_move_z: 6.0,
                ..OpParams::default()
            },
        };
        let json = serde_json::to_string(&op).expect("serialize");
        let back: Op = serde_json::from_str(&json).expect("deserialize");
        let OpKind::VCarve { carve } = &back.kind else {
            panic!("expected VCarve kind, got {:?}", back.kind);
        };
        assert_eq!(carve.carve_max_width_mm, Some(4.0));
        assert!(carve.multi_pass_refine);
        assert_eq!(back.params.depth, -8.0);
    }
}
