//! Per-op [`Setup`] builders. Each driver run synthesises a one-off
//! Setup from the project + the op via [`synthesize_op_setup`], and
//! the program header / footer uses [`header_setup_for`]'s lighter
//! cousin so its rate words pick up the user's actual feeds rather
//! than struct defaults. Helpers for dual-tool finish radius, peck
//! defaults, and auto-fit helix radius live alongside.

use crate::cam::setup::Setup;
use crate::cam::source_combine::combine_source_regions;
use crate::cam::VcObject;
use crate::project::{Operation, OperationKind, PocketStrategy, Project};

use super::{
    effective_step, ordered_selection, source_combine_mode, PipelineError, PipelineWarning,
};

pub(super) fn dual_tool_finish_radius(op: &Operation, project: &Project) -> Option<f64> {
    if !matches!(op.kind, OperationKind::Pocket { .. }) {
        return None;
    }
    let finish_id = op.finish_tool_id?;
    if finish_id == op.tool_id {
        return None;
    }
    let t = project.tools.iter().find(|t| t.id == finish_id)?;
    Some(t.diameter * 0.5)
}

/// Apply the tool library's `default_peck_step_mm` to peck-style drill
/// cycles when the op leaves `peck_step_mm` at 0. Unrecognized values
/// pass through untouched.
pub(super) fn resolve_peck_step(
    cycle: crate::project::DrillCycle,
    project: &Project,
    op: &Operation,
) -> crate::project::DrillCycle {
    use crate::project::DrillCycle;
    let tool_default = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .and_then(|t| t.default_peck_step_mm);
    let resolved = |op_step: f64| -> f64 {
        if op_step.abs() > 1e-9 {
            op_step
        } else {
            tool_default.unwrap_or(0.0)
        }
    };
    match cycle {
        DrillCycle::Simple { .. } => cycle,
        DrillCycle::Peck {
            peck_step_mm,
            dwell_sec,
        } => DrillCycle::Peck {
            peck_step_mm: resolved(peck_step_mm),
            dwell_sec,
        },
        DrillCycle::ChipBreak {
            peck_step_mm,
            dwell_sec,
        } => DrillCycle::ChipBreak {
            peck_step_mm: resolved(peck_step_mm),
            dwell_sec,
        },
    }
}

/// Build a [`Setup`] that represents this single op — copy in its
/// tool from `project.tools` and its params.kind-driven mill /
/// pockets / tabs / leads.
pub(super) fn synthesize_op_setup(
    op: &Operation,
    project: &Project,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<Setup, PipelineError> {
    use crate::cam::setup::{MachineMode, MillConfig, PocketConfig, ToolConfig, ToolOffset};

    let tool = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;
    let step = match effective_step(op, tool) {
        Ok(v) => v,
        Err(w) => {
            warnings.push(w);
            0.0
        }
    };

    let mut setup = Setup {
        machine: project.machine.clone(),
        ..Setup::default()
    };
    // Pick the per-tool rate variant. Drill ops consume the dedicated
    // _drill set throughout; everything else uses Rough (general) for
    // the main passes and Finish for the level=0 wall-defining ring
    // (selected per-offset at emit time).
    let main_pass = if matches!(op.kind, OperationKind::Drill { .. }) {
        crate::project::PassKind::Drill
    } else {
        crate::project::PassKind::Rough
    };
    let (rough_speed, rough_plunge, rough_feed) =
        crate::project::resolve_tool_rates(tool, main_pass);
    let (finish_speed, finish_plunge, finish_feed) =
        if matches!(op.kind, OperationKind::Drill { .. }) {
            // Drill never emits a finish pass — keep the finish triplet
            // equal to the drill triplet so a caller that reads either side
            // sees consistent values.
            (rough_speed, rough_plunge, rough_feed)
        } else {
            crate::project::resolve_tool_rates(tool, crate::project::PassKind::Finish)
        };
    // rt1.29: laser tools get their per-tool pierce-time threaded
    // into ToolConfig so emit_offset can emit a G4 P<sec> dwell
    // before each plunge. Non-laser tools collapse to 0.
    let pierce_sec = if matches!(tool.kind, crate::project::ToolKind::LaserBeam) {
        tool.laser_pierce_sec.unwrap_or(0.0).max(0.0)
    } else {
        0.0
    };
    setup.tool = ToolConfig {
        number: tool.id,
        name: tool.name.clone(),
        diameter: tool.diameter,
        speed: rough_speed,
        // Per-tool spindle-warmup pause (seconds) flows into the
        // post's G4 P<n> dwell after each M3 / M4.
        pause: tool.pause,
        mist: matches!(tool.coolant, crate::project::Coolant::Mist),
        flood: matches!(tool.coolant, crate::project::Coolant::Flood),
        dragoff: tool.dragoff,
        // Per-op overrides win over the tool library defaults — handy
        // for finishing passes or hard materials without editing the
        // tool entry itself. They apply to the ROUGH side only; the
        // finish-set is the user's explicit per-tool finish override,
        // so a per-op feed override doesn't bulldoze it.
        rate_v: op.params.plunge_rate_override.unwrap_or(rough_plunge),
        rate_h: op.params.feed_rate_override.unwrap_or(rough_feed),
        speed_finish: finish_speed,
        rate_v_finish: finish_plunge,
        rate_h_finish: finish_feed,
        pierce_sec,
    };
    let offset = match op.kind {
        OperationKind::Profile { offset } => offset,
        OperationKind::Pocket { .. } => ToolOffset::None,
        OperationKind::Engrave | OperationKind::DragKnife => ToolOffset::On,
        _ => ToolOffset::None,
    };
    // Trochoidal pockets demand a helical descent. If the user picked
    // Direct/Ramp we override silently here and emit a
    // `plunge_overridden` warning at the build_op_offsets seam.
    let trochoidal = matches!(
        op.kind,
        OperationKind::Pocket {
            strategy: PocketStrategy::Trochoidal { .. }
        }
    );
    let plunge = if trochoidal
        && !matches!(
            op.params.plunge,
            crate::cam::setup::PlungeStrategy::Helix { .. }
        ) {
        crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(tool.diameter * 0.75),
        }
    } else {
        op.params.plunge
    };
    setup.mill = MillConfig {
        active: true,
        depth: op.params.depth,
        start_depth: op.params.start_depth,
        step,
        fast_move_z: op.params.fast_move_z,
        helix_mode: op.params.helix,
        reverse: op.params.reverse,
        objectorder: op.params.objectorder,
        offset,
        overcut: op.params.overcut,
        plunge,
        corner_feed_reduction: op.params.corner_feed_reduction.clamp(0.0, 0.95),
        finish_step: op.params.finish_step,
        through_depth: op.params.through_depth.max(0.0),
        depth_list: op.params.depth_list.clone(),
    };
    setup.pockets = match op.kind {
        OperationKind::Pocket { strategy } => PocketConfig {
            active: true,
            islands: op.params.pocket_islands,
            zigzag: matches!(strategy, PocketStrategy::Zigzag),
            insideout: op.params.pocket_insideout,
            nocontour: op.params.pocket_nocontour,
        },
        _ => PocketConfig::default(),
    };
    setup.tabs = op.params.tabs.clone();
    // C8 (rt1.21 followup): drive `setup.tabs.active` from the
    // single source of truth — `tab_mode != Off`. The legacy
    // `op.params.tabs.active` boolean was a separate hand-mirrored
    // flag; the FE no longer maintains it perfectly, and a non-Off
    // tab_mode with `tabs.active=false` would silently emit no tabs
    // despite the user seeing markers on the canvas. Honor the
    // legacy flag too (logical OR) so projects saved before this
    // change keep working.
    setup.tabs.active =
        setup.tabs.active || !matches!(op.params.tab_mode, crate::project::TabPlacementMode::Off);
    if trochoidal {
        // Tabs aren't yet supported on trochoidal pockets; force-off so
        // the gcode emitter doesn't see active tabs.
        setup.tabs.active = false;
    }
    setup.leads = op.params.leads.clone();
    // Laser lead-in (rt1.29 follow-up, kkhf): when the tool is a
    // laser and the op didn't set its own lead-in, fall back to the
    // per-tool `laser_lead_in_mm`. Reduces edge burn at the entry
    // point. Off / `LeadKind::Off` keeps the op's explicit decision.
    if matches!(tool.kind, crate::project::ToolKind::LaserBeam)
        && setup.leads.in_lenght <= 0.0 {
            if let Some(lead_mm) = tool.laser_lead_in_mm.filter(|v| *v > 0.0) {
                setup.leads.in_lenght = lead_mm;
                if matches!(setup.leads.r#in, crate::cam::setup::LeadKind::Off) {
                    setup.leads.r#in = crate::cam::setup::LeadKind::Straight;
                }
            }
        }
    if matches!(op.kind, OperationKind::DragKnife) {
        setup.machine.mode = MachineMode::Drag;
    }
    // Chamfer ops (rt1.18) carve at a single depth computed from the
    // V-bit cone math — override the schedule fields after the main
    // synth so no user-set depth / step / finish_step / through_depth
    // sneaks through. The chamfer is a constant-Z pass; the cone-tip
    // sits at `-width / tan(tip_angle / 2)` while the centerline rides
    // the source path. Tabs / leads / objectorder still honor the op.
    if let OperationKind::Chamfer { width_mm, .. } = op.kind {
        let z = crate::cam::chamfer::chamfer_depth(width_mm, tool.tip_angle_deg);
        setup.mill.depth = z;
        setup.mill.start_depth = 0.0;
        // step == depth -> build_z_schedule emits a single pass.
        setup.mill.step = z;
        setup.mill.finish_step = None;
        setup.mill.through_depth = 0.0;
        setup.mill.depth_list = Vec::new();
        if !matches!(tool.kind, crate::project::ToolKind::VBit) {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "chamfer_non_vbit".into(),
                message: format!(
                    "Chamfer op '{}' uses tool '{}' which is not a V-bit. The cone math assumes a conical cutter; flat / ball tools will not produce a true bevel.",
                    op.name, tool.name
                ),
            });
        }
    }
    Ok(setup)
}

/// Resolve `PlungeStrategy::Helix { radius_mm: None }` (auto-fit) into
/// a concrete radius by picking the largest inscribed circle across the
/// op's source regions. When no fit is possible we leave `radius_mm` as
/// None so the gcode emitter falls through to Ramp, and emit a
/// `helix_radius_unfittable` info warning so the user understands why
/// the helix didn't apply.
pub(super) fn resolve_auto_helix_radius(
    op: &Operation,
    objects: &[VcObject],
    setup: &mut Setup,
    warnings: &mut Vec<PipelineWarning>,
) {
    use crate::cam::setup::PlungeStrategy;
    let PlungeStrategy::Helix {
        angle_deg,
        radius_mm: None,
    } = setup.mill.plunge
    else {
        return;
    };
    let tool_radius = setup.tool.diameter * 0.5;
    let selected = ordered_selection(op, objects);
    let mode = source_combine_mode(op);
    let regions = combine_source_regions(objects, &selected, mode);
    let mut best: Option<f64> = None;
    for region in &regions {
        if region.boundary.len() < 3 {
            continue;
        }
        let vc_region = crate::cam::vcarve::VcRegion {
            outer: region.boundary.clone(),
            holes: region.holes.clone(),
        };
        if let Some((_, _, r)) = crate::cam::inscribed::inscribed_circle(&vc_region, tool_radius) {
            best = Some(best.map_or(r, |prev| prev.max(r)));
        }
    }
    if let Some(r) = best {
        setup.mill.plunge = PlungeStrategy::Helix {
            angle_deg,
            radius_mm: Some(r),
        };
    } else {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "helix_radius_unfittable".into(),
            message: format!(
                "op '{}': auto helix radius could not be fit (pocket too small for tool); falling back to Ramp.",
                op.name
            ),
        });
    }
}

/// Header / footer [`Setup`] for the program. Synthesised from the
/// first enabled op so `machine.unit`, `mill.fast_move_z`,
/// `tool.rate_h` pick up the user's actual values rather than struct
/// defaults.
pub(super) fn header_setup_for(project: &Project) -> Setup {
    let mut setup = Setup {
        machine: project.machine.clone(),
        ..Setup::default()
    };
    if let Some(op) = project.operations.iter().find(|o| o.enabled) {
        if let Some(tool) = project.tools.iter().find(|t| t.id == op.tool_id) {
            let main_pass = if matches!(op.kind, OperationKind::Drill { .. }) {
                crate::project::PassKind::Drill
            } else {
                crate::project::PassKind::Rough
            };
            let (rs, rp, rf) = crate::project::resolve_tool_rates(tool, main_pass);
            let (fs, fp, ff) = if matches!(op.kind, OperationKind::Drill { .. }) {
                (rs, rp, rf)
            } else {
                crate::project::resolve_tool_rates(tool, crate::project::PassKind::Finish)
            };
            let pierce_sec = if matches!(tool.kind, crate::project::ToolKind::LaserBeam) {
                tool.laser_pierce_sec.unwrap_or(0.0).max(0.0)
            } else {
                0.0
            };
            setup.tool = crate::cam::setup::ToolConfig {
                number: tool.id,
                name: tool.name.clone(),
                diameter: tool.diameter,
                speed: rs,
                pause: 1,
                mist: matches!(tool.coolant, crate::project::Coolant::Mist),
                flood: matches!(tool.coolant, crate::project::Coolant::Flood),
                dragoff: tool.dragoff,
                // Per-op overrides (9vr) carry through into the program-
                // header feed too — otherwise the header emits the tool
                // default and the user sees an extra `F800` line at the
                // top despite the override.
                rate_v: op.params.plunge_rate_override.unwrap_or(rp),
                rate_h: op.params.feed_rate_override.unwrap_or(rf),
                speed_finish: fs,
                rate_v_finish: fp,
                rate_h_finish: ff,
                pierce_sec,
            };
        }
        setup.mill.fast_move_z = op.params.fast_move_z;
    } else if let Some(tool) = project.tools.first() {
        let (rs, rp, rf) =
            crate::project::resolve_tool_rates(tool, crate::project::PassKind::Rough);
        let (fs, fp, ff) =
            crate::project::resolve_tool_rates(tool, crate::project::PassKind::Finish);
        let pierce_sec = if matches!(tool.kind, crate::project::ToolKind::LaserBeam) {
            tool.laser_pierce_sec.unwrap_or(0.0).max(0.0)
        } else {
            0.0
        };
        setup.tool = crate::cam::setup::ToolConfig {
            number: tool.id,
            name: tool.name.clone(),
            diameter: tool.diameter,
            speed: rs,
            pause: 1,
            mist: matches!(tool.coolant, crate::project::Coolant::Mist),
            flood: matches!(tool.coolant, crate::project::Coolant::Flood),
            dragoff: tool.dragoff,
            rate_v: rp,
            rate_h: rf,
            speed_finish: fs,
            rate_v_finish: fp,
            rate_h_finish: ff,
            pierce_sec,
        };
    }
    setup
}
