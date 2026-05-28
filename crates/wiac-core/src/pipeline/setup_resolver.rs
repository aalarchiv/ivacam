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

/// jcmx: clamp a resolved feed (mm/min) DOWN to the machine's
/// `max_feed_mm_min` ceiling. `None` (unset) disables the clamp. Used
/// for both cutting and plunge feeds — a single ceiling is the safe
/// limit (plunge is normally well under it, so it rarely fires there).
/// Emits one warning per clamp event, tagged with the pass + axis so
/// the user sees which feed was capped. Mirrors `clamp_spindle_rpm`.
fn clamp_feed(
    feed: u32,
    axis: &str,
    machine: &MachineConfig,
    op: &Op,
    pass: PassKind,
    warnings: &mut Vec<PipelineWarning>,
) -> u32 {
    if let Some(max) = machine.max_feed_mm_min {
        if feed > max {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "feed_clamped_above_max".into(),
                message: format!(
                    "op '{}' ({pass:?} pass): {axis} feed {feed} mm/min exceeds machine max_feed_mm_min {max}; clamped to {max}.",
                    op.name
                ),
            });
            return max;
        }
    }
    feed
}

/// Silent feed clamp (no warnings) — for the inert `header_setup_for`
/// paths, so `ToolConfig` feed fields stay consistent with the emitted
/// per-op path. Mirrors `clamp_rpm_silent`.
fn clamp_feed_silent(feed: u32, machine: &MachineConfig) -> u32 {
    match machine.max_feed_mm_min {
        Some(max) if feed > max => max,
        _ => feed,
    }
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
pub(in crate::pipeline) fn synthesize_op_setup(
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
        // e2mq: thread the project's active WCS into Setup so the
        // post's program_begin can emit the explicit G54..G59 word
        // and GRBL's tool_z_shift maps to the right G10 L20 P<n>.
        wcs: project.work_offset.wcs,
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
        // 0t9o: resolve the drag-knife self-alignment threshold. None ⇒
        // 30° default (real drag knives self-align below this angle).
        // 0° forces the legacy "swivel every corner" behaviour. Negative
        // / non-finite values clamp to 0 so the dot-product compare in
        // walk.rs stays sane.
        drag_self_align_angle_rad: tool
            .drag_knife_self_align_angle_deg
            .filter(|v| v.is_finite())
            .unwrap_or(30.0)
            .max(0.0)
            .to_radians(),
        // Per-op overrides win over the tool library defaults — handy
        // for finishing passes or hard materials without editing the
        // tool entry itself.
        //
        // c0pm: the override is applied to BOTH rough and finish slots.
        // Pre-c0pm Profile ops emitted at rough rates, so the override
        // only needed to win over `rate_h` / `rate_v`. Post-c0pm a
        // single-pass Profile emits at finish rates (it IS the finish
        // pass) and the override has to flow through to the finish
        // slot too — otherwise a user-set feed override would be
        // silently ignored on every Profile op that didn't ALSO set
        // a per-tool finish override. The override wins regardless of
        // whether the user configured separate per-tool finish rates:
        // they explicitly typed the override at the op level, that's
        // the value they want.
        // jcmx: clamp the FINAL feed (after op override) to the machine
        // ceiling so even a fat-fingered override can't emit an
        // out-of-range F-word. Plunge + cut, rough + finish.
        rate_v: clamp_feed(
            op.params.plunge_rate_override.unwrap_or(rough_plunge),
            "plunge",
            &project.machine,
            op,
            main_pass,
            warnings,
        ),
        rate_h: clamp_feed(
            op.params.feed_rate_override.unwrap_or(rough_feed),
            "cut",
            &project.machine,
            op,
            main_pass,
            warnings,
        ),
        speed_finish: finish_speed,
        rate_v_finish: clamp_feed(
            op.params.plunge_rate_override.unwrap_or(finish_plunge),
            "plunge",
            &project.machine,
            op,
            PassKind::Finish,
            warnings,
        ),
        rate_h_finish: clamp_feed(
            op.params.feed_rate_override.unwrap_or(finish_feed),
            "cut",
            &project.machine,
            op,
            PassKind::Finish,
            warnings,
        ),
        pierce_sec,
        wirbeln_radius,
        wirbeln_stepover,
        wirbeln_osc,
        // Spiral rotation direction: matches the op's contour cut
        // direction (Estlcam's Einstellungen.Gleichlauf). Non-contour
        // kinds (Drill et al.) never reach the wirbeln overlay anyway,
        // so the default doesn't matter — pick `true` (climb).
        wirbeln_climb: op.contour_params().map_or(true, |c| {
            matches!(c.cut_direction, crate::project::CutDirection::Climb)
        }),
        default_xy_overlap: tool.default_xy_overlap,
        tip_angle_deg: tool.tip_angle_deg,
        tip_diameter_mm: effective_tip_diameter_mm(tool),
        spindle_direction: tool.spindle_direction,
        // zpuk: plasma pierce / cut heights / pierce delay. 0.0
        // sentinels fall through to plasma defaults at cut time.
        // Resolved unconditionally — the cut emitter gates on
        // `setup.machine.mode == Plasma` before consulting them.
        pierce_height_mm: tool.pierce_height_mm.unwrap_or(0.0).max(0.0),
        cut_height_mm: tool.cut_height_mm.unwrap_or(0.0).max(0.0),
        pierce_delay_sec: tool.pierce_delay_sec.unwrap_or(0.0).max(0.0),
        // ot80: V-Carve lead-in ramp angle. 0.0 sentinel = inherit the
        // legacy 10° at emit time inside `ratchet_emit`. Clamp to the
        // physically meaningful open interval (0°, 90°); anything else
        // means "use the default".
        vcarve_lead_in_angle_deg: resolve_vcarve_lead_in_angle_deg(tool.vcarve_lead_in_angle_deg),
    };
    // 2606: plasma kerf compensation. In Plasma mode the cut width is the
    // torch kerf, not a physical tool diameter — so override the effective
    // cutting diameter to `kerf_mm`. The offset cascade then compensates
    // the cut path by `kerf_mm / 2` (Profile Outside/Inside semantics
    // handled by the same machinery as a milling tool's radius), instead
    // of using the dummy tool diameter. Only when a kerf is configured;
    // otherwise the nominal diameter stands.
    if matches!(setup.machine.mode, MachineMode::Plasma) {
        if let Some(kerf) = tool.kerf_mm.filter(|k| *k > 0.0) {
            setup.tool.diameter = kerf;
        }
    }
    let offset = match &op.kind {
        OpKind::Profile { offset, .. } => *offset,
        OpKind::Pocket { .. } => ToolOffset::None,
        // 3g6u / b7qz: T-slot and dovetail both ride ON the centerline
        // like Engrave — the cutter's own cross-section (not a radius
        // offset) defines the undercut groove width.
        OpKind::Engrave { .. }
        | OpKind::DragKnife { .. }
        | OpKind::TSlot { .. }
        | OpKind::Dovetail { .. } => ToolOffset::On,
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
    // 8xan: if any resolved rate is exactly zero, emit a critical warning.
    // F0 / S0 is silently legal gcode but is never the user's intent —
    // it means the tool library + op overrides combined left the field
    // unset. Don't clamp to a default here (that hides the misconfig);
    // surface it loudly so the 94sf critical-warning gate blocks Generate.
    {
        let feed = setup.tool.rate_h;
        let plunge = setup.tool.rate_v;
        let speed = setup.tool.speed;
        if feed == 0 || plunge == 0 || speed == 0 {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "zero_rate_emitted".into(),
                message: format!(
                    "op '{}': resolved tool rates contain a zero value (feed={} mm/min, plunge={} mm/min, spindle={} RPM). Emitting F0 / S0 / plunge=0 will stall the cut or refuse to start. Set the missing rate on the tool library entry or as a per-op override.",
                    op.name, feed, plunge, speed
                ),
            });
        }
    }
    if let OpKind::Chamfer { width_mm, .. } = op.kind {
        let tip_diameter_mm = tool.tip_diameter.unwrap_or(0.0);
        // 7rt2: surface a `tool_tip_angle_clamped` warning when the user's
        // configured tip angle lies outside [1°, 179°] and the cone math
        // silently clamped it. Mirrors the V-Carve driver — the same
        // chamfer_depth() call clamps internally, but the user never sees
        // it unless we say so here.
        if !(1.0..=179.0).contains(&tool.tip_angle_deg) {
            let clamped = tool.tip_angle_deg.clamp(1.0, 179.0);
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "tool_tip_angle_clamped".into(),
                message: format!(
                    "Chamfer op '{}' tool '{}': configured tip angle {:.2}° is outside the supported [1°, 179°] range and was clamped to {:.2}° for cone-math. Update the tool's tip_angle_deg to silence this warning.",
                    op.name, tool.name, tool.tip_angle_deg, clamped,
                ),
            });
        }
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
    // 3g6u: a T-slot op cuts the undercut in ONE pass at the floor Z. The
    // cutting head sits at a single plane — there is no step-down (and it
    // can't plunge through the narrow stem to reach intermediate levels
    // anyway). Collapse the Z schedule to a single full-range pass so the
    // emitter doesn't cascade the head through -1, -2, … like a Profile
    // would (that head-at-every-depth cascade is exactly the bug this op
    // kind fixes). Unlike Chamfer — which keeps the step-down so a V-bit
    // ramps in gently — the T-slot head MUST arrive at the floor directly.
    // b7qz: a dovetail op is the angled-wall sibling — same single-Z
    // floor pass. The bit arrives at the floor (via the roughing
    // channel) and traverses once; its flanks carve the undercut. No
    // Z cascade for the same reason as T-slot.
    if matches!(op.kind, OpKind::TSlot { .. } | OpKind::Dovetail { .. }) {
        let span = setup.mill.depth - setup.mill.start_depth; // negative = downward
        setup.mill.step = if span.abs() > 1e-9 { span } else { -1e-6 };
        setup.mill.through_depth = 0.0;
        setup.mill.depth_list = Vec::new();
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
        // e2mq: mirror synthesize_op_setup so the program-header path
        // (which routes through program_begin / program_end) sees the
        // same active WCS as the per-op cut blocks.
        wcs: project.work_offset.wcs,
        ..Setup::default()
    };
    // lo7b: pick the first enabled op THAT ACTUALLY CUTS. Pause ops have
    // no tool / source / setup of their own and don't emit any header-
    // relevant tool-setup gcode, so falling back to them produces a
    // header that advertises whatever tool happened to live on the
    // adjacent ToolEntry (often the previous op's tool, or a random one).
    // Skipping Pause ops here makes the header's S<rpm> / F<feed> reflect
    // the first ACTUAL cut.
    if let Some(op) = project
        .operations
        .iter()
        // 8n4k: skip every program-only op so the header's S<rpm> /
        // F<feed> reflect the first ACTUAL cut, not whatever the
        // resolver would invent for a Homing / Probe / CycleMarker
        // op that doesn't carry a tool.
        .find(|o| o.enabled && !o.is_program_only())
    {
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
                // lay8: read the tool's actual spindle-warmup pause
                // rather than hard-coding 1 s. The header_setup_for path
                // is currently a structural inert (header gcode emission
                // routes through the per-op envelopes), but consumers
                // that inspect `ToolConfig.pause` (sim warmup totals,
                // future header dwell emit) need the right value.
                pause: tool.pause,
                mist: matches!(tool.coolant, crate::project::Coolant::Mist),
                flood: matches!(tool.coolant, crate::project::Coolant::Flood),
                dragoff: tool.dragoff,
                // 0t9o: same self-align threshold as the per-op path —
                // header_setup is rarely the active drag-knife setup, but
                // keeping the field consistent across resolution paths
                // prevents a stale 0° leaking through if the header_setup
                // ever drives the walker.
                drag_self_align_angle_rad: tool
                    .drag_knife_self_align_angle_deg
                    .filter(|v| v.is_finite())
                    .unwrap_or(30.0)
                    .max(0.0)
                    .to_radians(),
                // Per-op overrides (9vr) carry through into the program-
                // header feed too — otherwise the header emits the tool
                // default and the user sees an extra `F800` line at the
                // top despite the override. c0pm: the override also
                // applies to the finish slot so single-pass Profile ops
                // (which now emit at finish rates) honour the override.
                rate_v: clamp_feed_silent(
                    op.params.plunge_rate_override.unwrap_or(rp),
                    &project.machine,
                ),
                rate_h: clamp_feed_silent(
                    op.params.feed_rate_override.unwrap_or(rf),
                    &project.machine,
                ),
                speed_finish: fs,
                rate_v_finish: clamp_feed_silent(
                    op.params.plunge_rate_override.unwrap_or(fp),
                    &project.machine,
                ),
                rate_h_finish: clamp_feed_silent(
                    op.params.feed_rate_override.unwrap_or(ff),
                    &project.machine,
                ),
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
                spindle_direction: tool.spindle_direction,
                // zpuk: header_setup_for-path plasma fields — same
                // resolution as the per-op path so any consumer that
                // inspects the header setup sees consistent values.
                pierce_height_mm: tool.pierce_height_mm.unwrap_or(0.0).max(0.0),
                cut_height_mm: tool.cut_height_mm.unwrap_or(0.0).max(0.0),
                pierce_delay_sec: tool.pierce_delay_sec.unwrap_or(0.0).max(0.0),
                vcarve_lead_in_angle_deg: resolve_vcarve_lead_in_angle_deg(
                    tool.vcarve_lead_in_angle_deg,
                ),
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
            // lay8: same as above — read tool.pause rather than 1.
            pause: tool.pause,
            mist: matches!(tool.coolant, crate::project::Coolant::Mist),
            flood: matches!(tool.coolant, crate::project::Coolant::Flood),
            dragoff: tool.dragoff,
            drag_self_align_angle_rad: tool
                .drag_knife_self_align_angle_deg
                .filter(|v| v.is_finite())
                .unwrap_or(30.0)
                .max(0.0)
                .to_radians(),
            rate_v: clamp_feed_silent(rp, &project.machine),
            rate_h: clamp_feed_silent(rf, &project.machine),
            speed_finish: fs,
            rate_v_finish: clamp_feed_silent(fp, &project.machine),
            rate_h_finish: clamp_feed_silent(ff, &project.machine),
            pierce_sec,
            wirbeln_radius: 0.0,
            wirbeln_stepover: 0.0,
            wirbeln_osc: 0.0,
            wirbeln_climb: true,
            default_xy_overlap: tool.default_xy_overlap,
            tip_angle_deg: tool.tip_angle_deg,
            tip_diameter_mm: effective_tip_diameter_mm(tool),
            spindle_direction: tool.spindle_direction,
            pierce_height_mm: tool.pierce_height_mm.unwrap_or(0.0).max(0.0),
            cut_height_mm: tool.cut_height_mm.unwrap_or(0.0).max(0.0),
            pierce_delay_sec: tool.pierce_delay_sec.unwrap_or(0.0).max(0.0),
            vcarve_lead_in_angle_deg: resolve_vcarve_lead_in_angle_deg(
                tool.vcarve_lead_in_angle_deg,
            ),
        };
    }
    setup
}

/// ot80: clamp the tool's optional V-Carve lead-in angle into the
/// physically meaningful open interval (0°, 90°). `None` or
/// non-finite values → 0.0 (the sentinel that
/// [`crate::cam::vcarve_emit::ratchet_emit`] interprets as
/// "inherit the legacy 10° default"). Out-of-range values clamp into
/// (1.0, 89.0) so the ramp neither degenerates to a vertical plunge
/// (≈ 90°) nor stretches out into an infinite-length horizontal walk
/// (≈ 0°). Picked the 1°/89° bounds rather than ε so the resulting
/// `tan(angle)` stays a sane positive number on any platform.
#[must_use]
pub(crate) fn resolve_vcarve_lead_in_angle_deg(opt: Option<f64>) -> f64 {
    match opt {
        Some(v) if v.is_finite() && v > 0.0 && v < 90.0 => v.clamp(1.0, 89.0),
        _ => 0.0,
    }
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

    /// ot80: the lead-in-angle resolver clamps into the physical
    /// (1°, 89°) band, treats unset / out-of-range / non-finite
    /// inputs as the 0.0 sentinel, and passes valid values through.
    #[test]
    fn ot80_resolve_vcarve_lead_in_angle_handles_edge_cases() {
        // None → 0.0 (inherit default at emit time).
        assert_eq!(resolve_vcarve_lead_in_angle_deg(None), 0.0);
        // Valid 5° lands in (1°, 89°) — bumped up to 5° (no clamp).
        assert_eq!(resolve_vcarve_lead_in_angle_deg(Some(5.0)), 5.0);
        // Boundary: 0° rejected → 0.0.
        assert_eq!(resolve_vcarve_lead_in_angle_deg(Some(0.0)), 0.0);
        // Boundary: 90° rejected → 0.0.
        assert_eq!(resolve_vcarve_lead_in_angle_deg(Some(90.0)), 0.0);
        // Negative → 0.0.
        assert_eq!(resolve_vcarve_lead_in_angle_deg(Some(-15.0)), 0.0);
        // NaN → 0.0.
        assert_eq!(resolve_vcarve_lead_in_angle_deg(Some(f64::NAN)), 0.0);
        // Infinite → 0.0.
        assert_eq!(resolve_vcarve_lead_in_angle_deg(Some(f64::INFINITY)), 0.0);
        // 45° passes through unchanged.
        assert_eq!(resolve_vcarve_lead_in_angle_deg(Some(45.0)), 45.0);
    }

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

    /// 2606: in Plasma mode the effective cutting diameter the offset
    /// cascade uses is the torch KERF (cut width), not the nominal /
    /// dummy tool diameter — so a Profile cut is compensated by kerf/2.
    /// No kerf configured ⇒ the nominal diameter stands. Mill mode is
    /// never affected.
    #[test]
    fn plasma_kerf_overrides_effective_cut_diameter() {
        use crate::cam::setup::MachineMode;
        let mut tool = endmill(1, 10.0); // dummy 10 mm "tool" diameter
        tool.kerf_mm = Some(2.0);
        let op = profile_op(1, 1, ToolOffset::Outside);

        // Mill mode: the nominal diameter is used (offset = 5 mm).
        let mut project = project_with(vec![op.clone()], vec![tool.clone()]);
        project.machine.mode = MachineMode::Mill;
        let setup = synthesize_op_setup(&op, &project, &mut Vec::new()).unwrap();
        assert!((setup.tool.diameter - 10.0).abs() < 1e-9);

        // Plasma mode: the kerf wins (offset = kerf/2 = 1 mm).
        project.machine.mode = MachineMode::Plasma;
        let setup_p = synthesize_op_setup(&op, &project, &mut Vec::new()).unwrap();
        assert!(
            (setup_p.tool.diameter - 2.0).abs() < 1e-9,
            "plasma kerf should override effective cut diameter, got {}",
            setup_p.tool.diameter
        );

        // Plasma but no kerf configured: nominal diameter stands.
        let mut tool_nk = endmill(1, 10.0);
        tool_nk.kerf_mm = None;
        let mut project_nk = project_with(vec![op.clone()], vec![tool_nk]);
        project_nk.machine.mode = MachineMode::Plasma;
        let setup_nk = synthesize_op_setup(&op, &project_nk, &mut Vec::new()).unwrap();
        assert!((setup_nk.tool.diameter - 10.0).abs() < 1e-9);
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

    /// 8xan: when the resolved tool rates contain a zero — feed, plunge,
    /// or spindle — the pipeline emits a `zero_rate_emitted` warning so
    /// the 94sf critical gate blocks F0 / S0 from shipping silently.
    #[test]
    fn zero_feed_rate_emits_warning() {
        let mut tool = endmill(1, 3.0);
        tool.feed_rate = 0; // misconfig: user forgot the feed
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = Some(-1.0);
        op.params.depth = -1.0;
        let resp = run_pipeline(
            PipelineRequest {
                project: project_with(vec![op], vec![tool]),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.warnings.iter().any(|w| w.kind == "zero_rate_emitted"),
            "expected zero_rate_emitted warning, got {:?}",
            resp.warnings
        );
    }

    /// 8xan: a zero spindle speed also triggers the warning.
    #[test]
    fn zero_spindle_speed_emits_warning() {
        let mut tool = endmill(1, 3.0);
        tool.speed = 0;
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = Some(-1.0);
        op.params.depth = -1.0;
        let resp = run_pipeline(
            PipelineRequest {
                project: project_with(vec![op], vec![tool]),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.warnings.iter().any(|w| w.kind == "zero_rate_emitted"),
            "expected zero_rate_emitted warning for S0, got {:?}",
            resp.warnings
        );
    }

    /// lo7b: `header_setup_for` skips a leading Pause op so the header's
    /// S<rpm> / F<feed> reflects the first ACTUAL cut. The Pause op
    /// has no tool / source of its own; falling back to it produced a
    /// header that advertised whichever `ToolEntry` happened to share its
    /// id (often the previous op's tool, or a random one).
    #[test]
    fn header_setup_skips_leading_pause_op() {
        // Tool 1 is the small endmill the Pause "carries" (but doesn't
        // actually use); tool 2 is what the real cut uses with a
        // distinct speed/feed so we can assert the header picked it.
        let tool1 = endmill(1, 3.0);
        let mut tool2 = endmill(2, 6.0);
        tool2.feed_rate = 1234;
        tool2.speed = 9876;
        let pause = crate::project::Op {
            id: 1,
            name: "pause".into(),
            enabled: true,
            kind: crate::project::OpKind::Pause {
                message: "swap stock".into(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: crate::project::OpSource::All,
            params: crate::project::OpParams::mill_default(),
        };
        let mut cut = profile_op(2, 2, ToolOffset::Outside);
        cut.params.step = Some(-1.0);
        cut.params.depth = -1.0;
        let project = project_with(vec![pause, cut], vec![tool1, tool2]);
        let header = header_setup_for(&project);
        assert_eq!(
            header.tool.number, 2,
            "header should advertise tool 2, not the pause op's tool 1"
        );
        assert_eq!(header.tool.rate_h, 1234, "header feed should be tool 2's");
        assert_eq!(header.tool.speed, 9876, "header spindle should be tool 2's");
    }

    /// lay8: `header_setup_for` reads the first cutting tool's `pause`
    /// instead of hard-coding 1. A tool with `pause = 7` should round-trip
    /// into `header.tool.pause`.
    #[test]
    fn header_setup_reads_tool_pause() {
        let mut tool = endmill(1, 3.0);
        tool.pause = 7;
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = Some(-1.0);
        op.params.depth = -1.0;
        let project = project_with(vec![op], vec![tool]);
        let header = header_setup_for(&project);
        assert_eq!(
            header.tool.pause, 7,
            "header_setup_for must propagate tool.pause, got {}",
            header.tool.pause
        );
    }

    /// lay8: the no-op fallback branch (no enabled cutting ops) also
    /// reads `tool.pause` from the first tool rather than hard-coding 1.
    #[test]
    fn header_setup_fallback_branch_reads_tool_pause() {
        let mut tool = endmill(1, 3.0);
        tool.pause = 4;
        // No ops at all → fallback branch picks project.tools.first().
        let project = project_with(vec![], vec![tool]);
        let header = header_setup_for(&project);
        assert_eq!(header.tool.pause, 4);
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

    /// jcmx: a cutting feed above the machine's `max_feed_mm_min` ceiling
    /// clamps DOWN at the output boundary and emits a
    /// `feed_clamped_above_max` warning — the raw F-word never reaches
    /// the controller.
    #[test]
    fn feed_above_machine_max_clamps_and_warns() {
        let mut tool = endmill(1, 3.0);
        tool.feed_rate = 5000; // above the machine ceiling
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = Some(-1.0);
        op.params.depth = -1.0;
        let mut project = project_with(vec![op], vec![tool]);
        project.machine.max_feed_mm_min = Some(2000);
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
                .any(|w| w.kind == "feed_clamped_above_max"),
            "expected feed clamp warning, got {:?}",
            resp.warnings
        );
        assert!(
            !resp.gcode.contains("F5000"),
            "raw 5000 mm/min feed leaked into gcode despite clamp:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("F2000"),
            "expected clamped F2000, got:\n{}",
            resp.gcode
        );
    }

    /// jcmx: an op-level feed override above the ceiling is ALSO clamped
    /// — the clamp sits after the override merge, so a fat-fingered
    /// override can't bypass the machine limit.
    #[test]
    fn feed_override_above_machine_max_clamps() {
        let tool = endmill(1, 3.0);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = Some(-1.0);
        op.params.depth = -1.0;
        op.params.feed_rate_override = Some(9999);
        let mut project = project_with(vec![op], vec![tool]);
        project.machine.max_feed_mm_min = Some(1500);
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            !resp.gcode.contains("F9999"),
            "override feed bypassed the machine ceiling:\n{}",
            resp.gcode
        );
        assert!(resp.gcode.contains("F1500"), "expected clamped F1500");
    }
}
