//! Per-op [`Setup`] builders. Each driver run synthesises a one-off
//! Setup from the project + the op via [`synthesize_op_setup`], and
//! the program header / footer uses [`header_setup_for`]'s lighter
//! cousin so its rate words pick up the user's actual feeds rather
//! than struct defaults. Helpers for dual-tool finish radius, peck
//! defaults, and auto-fit helix radius live alongside.

// # CAM/sim pedantic-lint exemptions
// `OpKind → ToolOffset` map enumerates every variant explicitly so
// adding a new kind forces a deliberate choice.
#![allow(clippy::match_same_arms)]

use crate::cam::setup::{MachineConfig, Setup};
use crate::cam::source_combine::combine_source_regions;
use crate::cam::VcObject;
use crate::project::{Op, OpKind, PassKind, PocketStrategy, Project};

use super::{
    effective_step, ordered_selection, source_combine_mode, PipelineError, PipelineWarning,
};

/// 3nnj: clamp a single resolved RPM into the machine's
/// `[spindle_rpm_min, spindle_rpm_max]` window. Either bound may be
/// `None` (unset = no clamp on that side). Emits a warning per
/// clamp event tagged with which side fired + which pass it came
/// from so the user knows finish vs rough vs drill was affected.
fn clamp_spindle_rpm(
    rpm: u32,
    machine: &MachineConfig,
    op: &Op,
    pass: PassKind,
    warnings: &mut Vec<PipelineWarning>,
) -> u32 {
    let mut clamped = rpm;
    if let Some(max) = machine.spindle_rpm_max {
        if rpm > max {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "spindle_speed_clamped_above_max".into(),
                message: format!(
                    "op '{}' ({:?} pass): tool RPM {} exceeds machine spindle_rpm_max {}; clamped to {}.",
                    op.name, pass, rpm, max, max
                ),
            });
            clamped = max;
        }
    }
    if let Some(min) = machine.spindle_rpm_min {
        // Some controllers refuse to start the spindle below their
        // minimum (and you'd be at risk of stalling anyway). Clamp UP
        // and warn so the user reviews the chipload at the new RPM.
        if clamped < min {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "spindle_speed_clamped_below_min".into(),
                message: format!(
                    "op '{}' ({:?} pass): tool RPM {} is below machine spindle_rpm_min {}; clamped to {}.",
                    op.name, pass, clamped, min, min
                ),
            });
            clamped = min;
        }
    }
    clamped
}

pub(super) fn dual_tool_finish_radius(op: &Op, project: &Project) -> Option<f64> {
    if !matches!(op.kind, OpKind::Pocket { .. }) {
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
    op: &Op,
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
// synthesize_op_setup merges machine + tool + op into a per-op Setup —
// the merge is essentially N field-wise picks; splitting into helpers
// would scatter what's really one decision per field.
#[allow(clippy::too_many_lines)]
pub(super) fn synthesize_op_setup(
    op: &Op,
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
    let main_pass = if matches!(op.kind, OpKind::Drill { .. }) {
        crate::project::PassKind::Drill
    } else {
        crate::project::PassKind::Rough
    };
    let (rough_speed_raw, rough_plunge, rough_feed) =
        crate::project::resolve_tool_rates(tool, main_pass);
    let (finish_speed_raw, finish_plunge, finish_feed) = if matches!(op.kind, OpKind::Drill { .. })
    {
        // Drill never emits a finish pass — keep the finish triplet
        // equal to the drill triplet so a caller that reads either side
        // sees consistent values.
        (rough_speed_raw, rough_plunge, rough_feed)
    } else {
        crate::project::resolve_tool_rates(tool, crate::project::PassKind::Finish)
    };
    // 3nnj: clamp each pass's resolved RPM into the machine's
    // [spindle_rpm_min, spindle_rpm_max] window so an emitted S<x>
    // is always physically reachable. Clamp + warn rather than fail
    // hard — the user keeps the program but sees the substitution.
    let rough_speed = clamp_spindle_rpm(rough_speed_raw, &project.machine, op, main_pass, warnings);
    let finish_speed = if matches!(op.kind, OpKind::Drill { .. }) {
        rough_speed
    } else {
        clamp_spindle_rpm(
            finish_speed_raw,
            &project.machine,
            op,
            PassKind::Finish,
            warnings,
        )
    };
    // rt1.29: laser tools get their per-tool pierce-time threaded
    // into ToolConfig so emit_offset can emit a G4 P<sec> dwell
    // before each plunge. Non-laser tools collapse to 0.
    let pierce_sec = if matches!(tool.kind, crate::project::ToolKind::LaserBeam) {
        tool.laser_pierce_sec.unwrap_or(0.0).max(0.0)
    } else {
        0.0
    };
    // 3e5: Wirbeln helical overlay parameters. Off when the tool isn't
    // tagged, or when extra-width is 0 / unset. Stepover defaults to
    // half the spiral radius (one-revolution overlap → smooth motion).
    let (wirbeln_radius, wirbeln_stepover, wirbeln_osc) = if tool.wirbeln {
        let r = tool.wirbeln_extra_width_mm.unwrap_or(0.0) * 0.5;
        if r > 0.0 {
            let stepover = tool.wirbeln_stepover_mm.filter(|v| *v > 0.0).unwrap_or(r);
            let osc = tool.wirbeln_osc_mm.unwrap_or(0.0).max(0.0);
            (r, stepover, osc)
        } else {
            (0.0, 0.0, 0.0)
        }
    } else {
        (0.0, 0.0, 0.0)
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
        wirbeln_radius,
        wirbeln_stepover,
        wirbeln_osc,
        // Spiral rotation direction: matches the op's contour cut
        // direction (Estlcam's Einstellungen.Gleichlauf). Non-contour
        // kinds (Drill et al.) never reach the wirbeln overlay anyway,
        // so the default doesn't matter — pick `true` (climb).
        wirbeln_climb: op
            .contour_params()
            .map_or(true, |c| matches!(c.cut_direction, crate::project::CutDirection::Climb)),
        default_xy_overlap: tool.default_xy_overlap,
        tip_angle_deg: tool.tip_angle_deg,
        tip_diameter_mm: effective_tip_diameter_mm(tool),
    };
    let offset = match &op.kind {
        OpKind::Profile { offset, .. } => *offset,
        OpKind::Pocket { .. } => ToolOffset::None,
        OpKind::Engrave { .. } | OpKind::DragKnife { .. } => ToolOffset::On,
        _ => ToolOffset::None,
    };
    // Trochoidal pockets demand a helical descent. If the user picked
    // Direct/Ramp we override silently here and emit a
    // `plunge_overridden` warning at the build_op_offsets seam.
    let trochoidal = matches!(
        op.kind,
        OpKind::Pocket {
            strategy: PocketStrategy::Trochoidal { .. },
            ..
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
    // kbx5 step 2: read per-kind fields from the embedded variant
    // structs. ContourParams covers Profile/Pocket/Engrave/DragKnife;
    // ProfileParams covers Profile-only fields (overcut, reverse,
    // helix). Non-applicable kinds fall back to defaults — same
    // effective behavior as before since those fields were ignored
    // downstream for non-applicable kinds.
    let contour = op.contour_params();
    let profile = op.profile_params();
    setup.mill = MillConfig {
        active: true,
        depth: op.params.depth,
        start_depth: op.params.start_depth,
        step,
        fast_move_z: op.params.fast_move_z,
        helix_mode: profile.is_some_and(|p| p.helix),
        reverse: profile.is_some_and(|p| p.reverse),
        objectorder: op.params.objectorder,
        offset,
        overcut: profile.is_some_and(|p| p.overcut),
        plunge,
        corner_feed_reduction: contour
            .map_or(0.0, |c| c.corner_feed_reduction)
            .clamp(0.0, 0.95),
        finish_step: op.params.finish_step,
        through_depth: op.params.through_depth.max(0.0),
        depth_list: op.params.depth_list.clone(),
    };
    setup.pockets = match &op.kind {
        OpKind::Pocket {
            strategy, pocket, ..
        } => PocketConfig {
            active: true,
            islands: pocket.pocket_islands,
            zigzag: matches!(strategy, PocketStrategy::Zigzag { .. }),
            insideout: pocket.pocket_insideout,
            nocontour: pocket.pocket_nocontour,
        },
        _ => PocketConfig::default(),
    };
    setup.tabs = contour.map(|c| c.tabs.clone()).unwrap_or_default();
    // C8 (rt1.21 followup): drive `setup.tabs.active` from the single
    // source of truth — `tab_mode != Off`. The legacy `tabs.active`
    // boolean was a separate hand-mirrored flag; honor it (logical OR)
    // so old projects still emit tabs.
    let tab_mode = contour.map_or(crate::project::TabPlacementMode::Off, |c| c.tab_mode);
    setup.tabs.active =
        setup.tabs.active || !matches!(tab_mode, crate::project::TabPlacementMode::Off);
    if trochoidal {
        // Tabs aren't yet supported on trochoidal pockets; force-off so
        // the gcode emitter doesn't see active tabs.
        setup.tabs.active = false;
    }
    setup.leads = contour.map(|c| c.leads.clone()).unwrap_or_default();
    // Laser lead-in (rt1.29 follow-up, kkhf): when the tool is a
    // laser and the op didn't set its own lead-in, fall back to the
    // per-tool `laser_lead_in_mm`. Reduces edge burn at the entry
    // point. Off / `LeadKind::Off` keeps the op's explicit decision.
    if matches!(tool.kind, crate::project::ToolKind::LaserBeam) && setup.leads.in_lenght <= 0.0 {
        if let Some(lead_mm) = tool.laser_lead_in_mm.filter(|v| *v > 0.0) {
            setup.leads.in_lenght = lead_mm;
            if matches!(setup.leads.r#in, crate::cam::setup::LeadKind::Off) {
                setup.leads.r#in = crate::cam::setup::LeadKind::Straight;
            }
        }
    }
    if matches!(op.kind, OpKind::DragKnife { .. }) {
        setup.machine.mode = MachineMode::Drag;
    }
    // Chamfer ops (rt1.18) carve at a final depth computed from the
    // V-bit cone math. The contour is constant-Z (the cutter walks
    // the source path at the pinned final Z), but the descent FROM
    // start_depth TO that final Z must follow the normal stepdown
    // schedule (`setup.mill.step` + `finish_step`) — otherwise the
    // V-bit plunges in one shot into solid stock on deep chamfers
    // and snaps (00ia). depth / start_depth / through_depth / the
    // explicit depth_list get pinned here so a stale user value
    // doesn't sneak through; step + finish_step pass through.
    //
    // The requested chamfer width is also clamped to the V-bit's
    // physical reach (`(diameter - tip_diameter) / 2`). Without the
    // clamp a width > diameter/2 produces a Z that drives the shank
    // into stock — see uo1t and the vcarve driver's tool_reach_r.
    if let OpKind::Chamfer { width_mm, .. } = op.kind {
        let tip_diameter_mm = tool.tip_diameter.unwrap_or(0.0);
        let sol = crate::cam::chamfer::chamfer_depth_capped(
            width_mm,
            tool.tip_angle_deg,
            tool.diameter,
            tip_diameter_mm,
        );
        if sol.clamped_to_reach {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "chamfer_width_clamped_to_reach".into(),
                message: format!(
                    "Chamfer op '{}': requested width {:.3} mm exceeds V-bit '{}' physical reach ({:.3} mm = (diameter {:.3} - tip {:.3}) / 2). Clamped to {:.3} mm so the cone — not the shank — does the cutting.",
                    op.name,
                    width_mm,
                    tool.name,
                    sol.width_cap_mm,
                    tool.diameter,
                    tip_diameter_mm,
                    sol.effective_width_mm,
                ),
            });
        }
        setup.mill.depth = sol.z;
        setup.mill.start_depth = 0.0;
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
    op: &Op,
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
    let best = fit_helix_radius_for_selection(objects, &selected, mode, tool_radius);
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

/// Pure-fit kernel shared by [`resolve_auto_helix_radius`] (per-op
/// pipeline path) and the public [`crate::compute_helix_radius`]
/// preview entry point. Returns the largest inscribed-circle radius
/// across the combined source regions, or `None` when none fit.
///
/// Centralising the math here was an audit follow-up: the two
/// pre-existing call sites had drifted on containment handling — the
/// preview path silently disagreed with the per-op resolution. They
/// now share `combine_source_regions` + the same fit loop.
#[must_use]
pub fn fit_helix_radius_for_selection(
    objects: &[VcObject],
    selected: &[usize],
    combine: crate::project::SourceCombine,
    tool_radius: f64,
) -> Option<f64> {
    let regions = combine_source_regions(objects, selected, combine);
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
    best
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
            let main_pass = if matches!(op.kind, OpKind::Drill { .. }) {
                crate::project::PassKind::Drill
            } else {
                crate::project::PassKind::Rough
            };
            let (rs_raw, rp, rf) = crate::project::resolve_tool_rates(tool, main_pass);
            let (fs_raw, fp, ff) = if matches!(op.kind, OpKind::Drill { .. }) {
                (rs_raw, rp, rf)
            } else {
                crate::project::resolve_tool_rates(tool, crate::project::PassKind::Finish)
            };
            // 3nnj: keep the header S<x> consistent with the per-op
            // clamp. The synth path already pushed the warning when
            // this op ran through synthesize_op_setup; silently clamp
            // here so the header doesn't ship a different (unreachable)
            // RPM in its M3 command.
            let rs = clamp_rpm_silent(rs_raw, &project.machine);
            let fs = if matches!(op.kind, OpKind::Drill { .. }) {
                rs
            } else {
                clamp_rpm_silent(fs_raw, &project.machine)
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
                // Wirbeln (3e5) is a cut-time overlay only — the
                // header_setup_for path is for program header emission
                // and never reaches the cut walker, so the resolved
                // params here are inert defaults.
                wirbeln_radius: 0.0,
                wirbeln_stepover: 0.0,
                wirbeln_osc: 0.0,
                wirbeln_climb: true,
                default_xy_overlap: None,
                tip_angle_deg: tool.tip_angle_deg,
                tip_diameter_mm: effective_tip_diameter_mm(tool),
            };
        }
        setup.mill.fast_move_z = op.params.fast_move_z;
    } else if let Some(tool) = project.tools.first() {
        let (rs_raw, rp, rf) =
            crate::project::resolve_tool_rates(tool, crate::project::PassKind::Rough);
        let (fs_raw, fp, ff) =
            crate::project::resolve_tool_rates(tool, crate::project::PassKind::Finish);
        let rs = clamp_rpm_silent(rs_raw, &project.machine);
        let fs = clamp_rpm_silent(fs_raw, &project.machine);
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
            wirbeln_radius: 0.0,
            wirbeln_stepover: 0.0,
            wirbeln_osc: 0.0,
            wirbeln_climb: true,
            default_xy_overlap: tool.default_xy_overlap,
            tip_angle_deg: tool.tip_angle_deg,
            tip_diameter_mm: effective_tip_diameter_mm(tool),
        };
    }
    setup
}

/// Same as `clamp_spindle_rpm` but silent (no warnings). Used by
/// `header_setup_for` — the equivalent warning has already been
/// emitted at synth time when that op ran through the per-op path,
/// so the header path only needs the value adjustment.
pub(super) fn clamp_rpm_silent(rpm: u32, machine: &MachineConfig) -> u32 {
    let mut clamped = rpm;
    if let Some(max) = machine.spindle_rpm_max {
        if clamped > max {
            clamped = max;
        }
    }
    if let Some(min) = machine.spindle_rpm_min {
        if clamped < min {
            clamped = min;
        }
    }
    clamped
}

/// Resolve the effective `tip_diameter_mm` for a tool. Flat-bottom
/// kinds (endmill, ball-nose, bull-nose, compression, t-slot,
/// form-profile, drag-knife, laser) report their FULL diameter so
/// `ToolConfig.tip_cone_length()` returns 0 — they don't add cone
/// extension to through-cuts. Pointed kinds (drill, V-bit,
/// engraver) report the user-set `tip_diameter` (default 0 for
/// sharp tools).
#[must_use]
fn effective_tip_diameter_mm(tool: &crate::project::ToolEntry) -> f64 {
    use crate::project::ToolKind;
    match tool.kind {
        ToolKind::Drill | ToolKind::VBit | ToolKind::Engraver => {
            tool.tip_diameter.unwrap_or(0.0).max(0.0)
        }
        _ => tool.diameter,
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::cam::setup::ToolOffset;
    use crate::pipeline::test_helpers::{endmill, profile_op, project_with};
    use crate::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
    use crate::project::{resolve_tool_rates, PassKind};

    #[test]
    fn effective_step_op_override_wins() {
        let mut tool = endmill(1, 3.0);
        tool.default_step = Some(-0.5);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = Some(-0.3);
        assert_eq!(effective_step(&op, &tool).unwrap(), -0.3);
    }

    #[test]
    fn effective_step_falls_back_to_tool_default() {
        let mut tool = endmill(1, 3.0);
        tool.default_step = Some(-0.5);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = None;
        assert_eq!(effective_step(&op, &tool).unwrap(), -0.5);
    }

    #[test]
    fn effective_step_warns_when_both_unset() {
        let tool = endmill(1, 3.0);
        let mut op = profile_op(7, 1, ToolOffset::Outside);
        op.params.step = None;
        let w = effective_step(&op, &tool).unwrap_err();
        assert_eq!(w.kind, "step_unspecified");
        assert_eq!(w.op_id, Some(7));
    }

    #[test]
    fn effective_step_rejects_non_negative() {
        let mut tool = endmill(1, 3.0);
        tool.default_step = Some(0.5);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = Some(0.0);
        assert!(effective_step(&op, &tool).is_err());
    }

    #[test]
    fn run_pipeline_emits_step_unspecified_warning() {
        let tool = endmill(1, 3.0);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = None;
        let resp = run_pipeline(
            PipelineRequest {
                project: project_with(vec![op], vec![tool]),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.warnings.iter().any(|w| w.kind == "step_unspecified"),
            "expected step_unspecified warning, got {:?}",
            resp.warnings
        );
    }

    /// `resolve_tool_rates`: unset finish/drill variants fall back to the
    /// general triplet (rt1.27).
    #[test]
    fn resolve_tool_rates_falls_back_when_unset() {
        let t = endmill(1, 3.0);
        assert_eq!(resolve_tool_rates(&t, PassKind::Rough), (18_000, 100, 800));
        assert_eq!(resolve_tool_rates(&t, PassKind::Finish), (18_000, 100, 800));
        assert_eq!(resolve_tool_rates(&t, PassKind::Drill), (18_000, 100, 800));
    }

    /// `resolve_tool_rates`: each variant honors its own override when set.
    #[test]
    fn resolve_tool_rates_honors_per_pass_overrides() {
        let mut t = endmill(1, 3.0);
        t.speed_finish = Some(12_000);
        t.feed_rate_finish = Some(400);
        t.speed_drill = Some(8_000);
        t.feed_rate_drill = Some(200);
        t.plunge_rate_drill = Some(50);
        assert_eq!(resolve_tool_rates(&t, PassKind::Rough), (18_000, 100, 800));
        assert_eq!(resolve_tool_rates(&t, PassKind::Finish), (12_000, 100, 400));
        assert_eq!(resolve_tool_rates(&t, PassKind::Drill), (8_000, 50, 200));
    }

    /// 3nnj: tool RPM above the machine spindle ceiling clamps to the
    /// ceiling and emits a `spindle_speed_clamped_above_max` warning.
    /// The emitted `M3 S<n>` reflects the clamped value, not the raw
    /// tool speed.
    #[test]
    fn rpm_above_machine_max_clamps_and_warns() {
        let mut tool = endmill(1, 3.0);
        tool.speed = 24_000; // way above hobby spindle range
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = Some(-1.0);
        op.params.depth = -1.0;
        let mut project = project_with(vec![op], vec![tool]);
        project.machine.spindle_rpm_max = Some(12_000);
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
                .any(|w| w.kind == "spindle_speed_clamped_above_max"),
            "expected clamp warning, got {:?}",
            resp.warnings
        );
        // The emitted gcode shouldn't ever say S24000 — the clamp must
        // bite at the output boundary, not just in the warning.
        assert!(
            !resp.gcode.contains("S24000"),
            "raw 24000 RPM leaked into gcode despite clamp"
        );
        assert!(
            resp.gcode.contains("S12000"),
            "expected clamped S12000, got {}",
            resp.gcode
        );
    }
}
