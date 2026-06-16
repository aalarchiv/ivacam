//! Per-offset gcode emission machinery, split out of `gcode.rs`.
//!
//! `emit_offset` is the per-offset shell — rapid-to-start, ramp/helix
//! plunge, cut, retract — and `multi_pass` walks the Z-pass schedule with
//! per-pass tab / helix / ramp handling. Both stay private to the gcode
//! module tree; the public emit shells in `gcode.rs` (`emit_polylines_block`
//! et al.) call `emit_offset`. Kept as a child module so it shares
//! `super`'s `PostProcessor` trait + cut helpers + the `entry` / `tabs`
//! submodules, while leaving gcode.rs's trait + block shells legible.

use super::face_mill_overlay::WhirlState;
use super::{
    arc_length, build_z_schedule, cut_tool_off, cut_tool_on, cut_tool_pierce, emit_cut_path,
    emit_helix_entry, emit_helix_pass, emit_path_with_tabs, emit_ramp_pass, end_pos, fit_line_runs,
    is_closed_path, lead_in_geometry, lead_out_geometry, plan_helix_entry, reverse_chain,
    HelixEntry, LeadGeometry, PostProcessor,
};
use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::Setup;
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::project::MachineMode;

/// Emit a single polyline offset (one cut pass per multi-pass step).
// emit_offset is the per-offset emission: rapid-to-start → ramp/helix
// plunge → cut → retract. Each phase reads top-to-bottom and shares
// state with the next.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_offset<P: PostProcessor>(
    setup: &Setup,
    offset: &PolylineOffset,
    post: &mut P,
    last_pos: &mut Point2,
) {
    if offset.segments.is_empty() {
        return;
    }
    if setup.machine.comments {
        post.separation();
        post.comment(&format!(
            "object={} level={} pocket={} segments={}{}",
            offset.source_object_idx,
            offset.level,
            offset.is_pocket,
            offset.segments.len(),
            if offset.is_finish { " finish" } else { "" }
        ));
    }
    // Pick the per-tool feed / speed / plunge set: finish-set for the
    // wall-defining ring of a Pocket op, rough-set everywhere
    // else. Posts delta-encode so emitting the same values back-to-back
    // is free.
    let (use_speed, use_rate_v, use_rate_h) = if offset.is_finish {
        (
            setup.tool.speed_finish,
            setup.tool.rate_v_finish,
            setup.tool.rate_h_finish,
        )
    } else {
        (setup.tool.speed, setup.tool.rate_v, setup.tool.rate_h)
    };
    // Dispatch by machine mode. Mill: spin the spindle. Laser:
    // fire M3 S<power>. Drag: no-op. The previous code gated
    // `spindle_on` behind `mode == Mill` only, so laser cuts never
    // turned the beam on — the program emitted G0/G1 moves with the
    // laser silently off and produced no engraving.
    cut_tool_on(post, setup, use_speed);
    if setup.tool.flood {
        post.coolant_flood();
    }
    if setup.tool.mist {
        post.coolant_mist();
    }
    // Surface the chosen cut feedrate before the cut; the plunge feed
    // gets set explicitly at each Z-down move inside multi_pass and at
    // the lead-in entry plunge below.
    post.feedrate(use_rate_h);
    let start = offset.segments[0].start;
    // Lead-in (straight, arc, or off) before the first cut. The arc
    // lead is a tangent roll-on at z=0 that lands the cutter on the
    // contour with motion already aligned to the first segment's
    // tangent — no dwell at the start point. multi_pass then plunges
    // from z=0 to the first pass depth at segments[0].start.
    let lead_in = lead_in_geometry(setup, &offset.segments);
    // Laser pierce — rapid XY at safe Z (no Z change
    // away from fast_move_z), plunge to cut Z, THEN dwell at the cut
    // height so the beam burns through focused stock before motion
    // begins. Dwelling at fast_move_z (the old order) left the head
    // defocused, never pierced, and the first cut yanked unmelted
    // material. Order matches Lightburn / T2Laser laser convention.
    let pierce_sec = setup.tool.pierce_sec;
    // Plasma entry — when machine.mode == Plasma the lead-in
    // emits a two-step Z descent instead of a single plunge:
    //   1. Rapid XY at fast_z, torch OFF (cut_tool_on is a plasma
    //      no-op — a lit positioning rapid scars the sheet and burns
    //      pilot-arc duty cycle).
    //   2. Rapid (G0) to pierce_height_mm above stock.
    //   3. Fire the torch (M3 S<power> via cut_tool_pierce) at the
    //      pierce point — standard plasma convention.
    //   4. Dwell pierce_delay_sec while the arc transfers + pierces.
    //   5. G1 down to cut_height_mm at the plunge rate.
    //   6. Walk the contour (multi_pass collapses to one pass).
    // Falls back to safe defaults (3.8 / 1.5 / 0.5) when the
    // resolved values are 0. We carry the booleans + heights into
    // a small struct so the three lead branches below stay readable.
    let plasma_entry = if setup.machine.mode == MachineMode::Plasma {
        let pierce_h = if setup.tool.pierce_height_mm > 0.0 {
            setup.tool.pierce_height_mm
        } else {
            3.8
        };
        let cut_h = if setup.tool.cut_height_mm > 0.0 {
            setup.tool.cut_height_mm
        } else {
            1.5
        };
        let delay = if setup.tool.pierce_delay_sec > 0.0 {
            setup.tool.pierce_delay_sec
        } else {
            0.5
        };
        Some((pierce_h, cut_h, delay))
    } else {
        None
    };
    // The lead-in plunge must drop to `start_depth` (the entry
    // plane just above the workpiece), NOT to a literal Z=0. Stock
    // proud of Z=0 (start_depth < 0) would crash the cutter at the
    // approach; recesses (start_depth > 0) would have the cutter
    // cutting air. `multi_pass` then descends from `start_depth` to
    // the first pass depth via plunge / ramp / helix.
    let entry_z = setup.mill.start_depth;
    // The lead-in plunge from fast_move_z to entry_z is a G1
    // Z-drop — emit it at the plunge feed (rate_v), not the cut feed
    // (rate_h). Modal F was set to `use_rate_h` above (so the
    // first cut motion has a known F nearby), so we switch to rate_v
    // here, plunge, then restore rate_h before the lateral cut. Without
    // this, the cutter dives from safe Z to start_depth at the (often
    // 8x faster) cut feed — snaps non-center-cutting endmill tips and
    // is the canonical proud-stock crash. Posts dedupe identical-rate
    // F-emits so the restore is free when rate_v == rate_h.
    // A straight lead leaves the head at the off-contour hop point;
    // the pass machinery must emit the lead-in cut (hop → contour
    // start) at the pass depth before walking. Without it the walk's
    // first motion ran hop → segments[0].END, cutting a chord that
    // skipped the first segment entirely — a permanently wrong kerf on
    // every single-pass cut (plasma / laser / drag / single-pass
    // mill); multi-pass mill only self-healed on pass ≥ 2 because
    // each later pass starts exactly at the contour start.
    let straight_lead = matches!(lead_in, LeadGeometry::Straight { .. });
    match lead_in {
        LeadGeometry::Straight { from } => {
            post.move_to(Some(from.x), Some(from.y), Some(setup.mill.fast_move_z));
            if let Some((pierce_h, cut_h, delay)) = plasma_entry {
                // Plasma two-step Z. Rapid to pierce height, FIRE
                // the torch at the pierce point, dwell while the arc
                // transfers + pierces, G1 to cut height. cut_height is
                // the cut plane that multi_pass walks at (it
                // short-circuits to one pass because mode == Plasma —
                // see the plasma branch in multi_pass).
                post.move_to(None, None, Some(pierce_h));
                cut_tool_pierce(post, setup, use_speed);
                if delay > 0.0 {
                    post.dwell(delay);
                }
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(cut_h));
            } else {
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(entry_z));
                // Laser-mode ramps from armed (S0) to full power
                // here, between the plunge and the pierce dwell. Mill /
                // drag / plasma are no-ops in this helper.
                cut_tool_pierce(post, setup, use_speed);
                if pierce_sec > 0.0 {
                    post.dwell(pierce_sec);
                }
            }
            post.feedrate(use_rate_h);
        }
        LeadGeometry::Arc {
            entry_or_exit: from,
            center,
            ccw,
        } => {
            post.move_to(Some(from.x), Some(from.y), Some(setup.mill.fast_move_z));
            if let Some((pierce_h, cut_h, delay)) = plasma_entry {
                post.move_to(None, None, Some(pierce_h));
                cut_tool_pierce(post, setup, use_speed);
                if delay > 0.0 {
                    post.dwell(delay);
                }
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(cut_h));
            } else {
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(entry_z));
                // Laser-mode ramps from armed (S0) to full power
                // here, between the plunge and the pierce dwell. Mill /
                // drag are no-ops in this helper.
                cut_tool_pierce(post, setup, use_speed);
                if pierce_sec > 0.0 {
                    post.dwell(pierce_sec);
                }
            }
            // Re-emit the cutting feedrate immediately before
            // the arc lead-in. The roll-on is the first ACTUAL cut
            // motion in the program (G2/G3 honors F); relying on a
            // modal set further upstream means the listing's first
            // arc has no F line nearby — defensive on FANUC / vintage
            // controllers that re-evaluate F at each motion-mode
            // change. Posts dedupe identical-rate emits so this is
            // free when the modal already matches. This is also the
            // post-plunge feedrate restore (rate_v → rate_h).
            post.feedrate(use_rate_h);
            // I/J are the offset from the arc's start (current XY) to
            // its center — same convention as ezdxf / ngc / linuxcnc.
            let i = center.x - from.x;
            let j = center.y - from.y;
            if ccw {
                post.arc_ccw(Some(start.x), Some(start.y), None, Some(i), Some(j));
            } else {
                post.arc_cw(Some(start.x), Some(start.y), None, Some(i), Some(j));
            }
        }
        LeadGeometry::None => {
            post.move_to(Some(start.x), Some(start.y), Some(setup.mill.fast_move_z));
            if let Some((pierce_h, cut_h, delay)) = plasma_entry {
                post.move_to(None, None, Some(pierce_h));
                cut_tool_pierce(post, setup, use_speed);
                if delay > 0.0 {
                    post.dwell(delay);
                }
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(cut_h));
            } else {
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(entry_z));
                // Laser-mode ramps from armed (S0) to full power
                // here, between the plunge and the pierce dwell. Mill /
                // drag are no-ops in this helper.
                cut_tool_pierce(post, setup, use_speed);
                if pierce_sec > 0.0 {
                    post.dwell(pierce_sec);
                }
            }
            post.feedrate(use_rate_h);
        }
    }

    multi_pass(
        setup,
        &offset.segments,
        &offset.tabs,
        offset.is_finish,
        straight_lead,
        post,
    );

    // Lead-out happens at the FINAL pass depth — it's a real cutting
    // motion that rolls the cutter off the contour into free space.
    let lead_out = lead_out_geometry(setup, &offset.segments);
    match lead_out {
        LeadGeometry::Straight { from: to } => {
            post.linear(Some(to.x), Some(to.y), None);
        }
        LeadGeometry::Arc {
            entry_or_exit: to,
            center,
            ccw,
        } => {
            // Arc starts at the cutter's current XY (= end_pos) and
            // ends at `to`. I/J = center - end_pos.
            let end_pt = end_pos(offset);
            let i = center.x - end_pt.x;
            let j = center.y - end_pt.y;
            if ccw {
                post.arc_ccw(Some(to.x), Some(to.y), None, Some(i), Some(j));
            } else {
                post.arc_cw(Some(to.x), Some(to.y), None, Some(i), Some(j));
            }
        }
        LeadGeometry::None => {}
    }
    // Drop the laser BEFORE the safe-Z retract so the rapid
    // traverse to the next offset / op doesn't burn a stripe through
    // the part. Mill keeps the spindle running between cuts (the
    // post's delta-encoded `last_speed` dedupes the next cut's M3
    // re-arm so no extra lines emit); Drag is a no-op.
    cut_tool_off(post, setup);
    // Final retract after lead-out is a rapid (G0), not a cut
    // motion (G1). The lead-out already rolled the cutter off the
    // contour into free space; lifting to fast_move_z at cut feed
    // multiplies cycle time across hundreds of contours with zero
    // safety benefit. Use `move_to` (G0) to retract at the controller's
    // rapid feed.
    post.move_to(None, None, Some(setup.mill.fast_move_z));

    *last_pos = offset.segments.last().map_or(start, |s| s.end);
}

/// Single-pass emit for the plot-mode / drag-knife / plasma modes that
/// collapse the multi-pass schedule to ONE pass at the cut depth.
///
/// Laser / plasma / pen-plotter / 3D-printer / drag-knife
/// controllers expect binary pen-up / pen-down Z — the multi-step
/// descent + helix / ramp / finish_step / through_depth / depth_list
/// machinery is noise. `MachineMode::Drag` collapses here even when
/// the global `plot_mode_z` is off (`setup_resolver` pins `mode = Drag`
/// per-op for `DragKnife`); the knife is locked at one depth, so extra
/// passes only wear the Z axis. Plasma holds the torch at
/// `cut_height_mm` for the whole cut.
fn emit_single_pass<P: PostProcessor>(
    setup: &Setup,
    segments: &[Segment],
    rate_v: u32,
    rate_h: u32,
    straight_lead: bool,
    post: &mut P,
) {
    // Plasma cuts at `cut_height_mm` above stock (positive Z), NOT
    // at `mill.depth` (the milling-style depth below stock). For
    // plot_mode_z / Drag the cut Z is still mill.depth.min(0).
    let cut_z = if setup.machine.mode == MachineMode::Plasma {
        // Default 1.5 mm if the resolved value is 0 (legacy projects
        // without plasma fields set).
        if setup.tool.cut_height_mm > 0.0 {
            setup.tool.cut_height_mm
        } else {
            1.5
        }
    } else {
        setup.mill.depth.min(0.0)
    };
    // For plot_mode_z / Drag we still need to dive to cut_z (the lead-in
    // plunges to mill.start_depth, which may not equal the cut depth). For
    // Plasma the lead-in already dropped to cut_height — the post
    // delta-encodes Z so re-emitting is a no-op, but skip it for clarity.
    if setup.machine.mode != MachineMode::Plasma {
        post.feedrate(rate_v);
        post.linear(None, None, Some(cut_z));
    }
    post.feedrate(rate_h);
    // Straight lead-in cut: the head sits at the off-contour hop
    // point (plasma pierced there; drag / plot dove there) — cut onto
    // the contour start at the cut plane so the walk traces the first
    // segment from its true start instead of chording to its end.
    if straight_lead {
        if let Some(first) = segments.first() {
            post.linear(Some(first.start.x), Some(first.start.y), None);
        }
    }
    let dragoff = setup.tool.dragoff.unwrap_or(0.0);
    let fitted = fit_line_runs(segments, setup);
    // Single-pass; a fresh whirl state is fine.
    let mut whirl_state = WhirlState::default();
    emit_cut_path(
        &fitted,
        setup,
        cut_z,
        dragoff,
        rate_h,
        setup.mill.corner_feed_reduction,
        &mut whirl_state,
        post,
    );
}

// multi_pass walks the Z schedule with per-pass tab handling, helix
// state, and ramp planning. Splitting would scatter the per-pass state
// (helix-entry plan, ramp-length tracking) across multiple helpers.
#[allow(clippy::too_many_lines)]
fn multi_pass<P: PostProcessor>(
    setup: &Setup,
    segments: &[Segment],
    tabs: &[crate::cam::offsets::TabPoint],
    is_finish: bool,
    straight_lead: bool,
    post: &mut P,
) {
    use crate::project::{PlungeStrategy, TabType};
    // Finish-set rates: swap in the tool's _finish overrides
    // when this offset is the wall-defining ring of a Pocket. Falls
    // back to rough rates everywhere else.
    let rate_v = if is_finish {
        setup.tool.rate_v_finish
    } else {
        setup.tool.rate_v
    };
    let rate_h = if is_finish {
        setup.tool.rate_h_finish
    } else {
        setup.tool.rate_h
    };

    // Plot-mode / drag-knife / plasma collapse the whole Z schedule to
    // ONE pass at the cut depth (no descent / helix / ramp / tabs) — see
    // [`emit_single_pass`]. `tabs` is meaningless on these single-pass
    // paths; it's consumed by the multi-pass schedule walk below.
    if setup.machine.plot_mode_z
        || setup.machine.mode == MachineMode::Drag
        || setup.machine.mode == MachineMode::Plasma
    {
        emit_single_pass(setup, segments, rate_v, rate_h, straight_lead, post);
        return;
    }
    // Build the Z schedule. depth_list (when non-empty) wins as an
    // explicit list; otherwise use step + finish_step + through_depth
    // to derive a step-down sequence ending at depth - through_depth.
    let nominal_depth = setup.mill.depth;
    let total_depth = nominal_depth - setup.mill.through_depth.max(0.0);
    let step_raw = if setup.mill.step.abs() < 1e-9 {
        total_depth
    } else if setup.mill.step > 0.0 {
        -setup.mill.step
    } else {
        setup.mill.step
    };
    // Normalize finish_step at the call boundary — reject
    // negative / zero / non-finite values so z_schedule sees a clean
    // positive magnitude. The schedule builder also abs()-then-filters
    // internally, but normalizing here makes the contract explicit and
    // lets the next reader spot a sign-bug without reading three
    // files.
    let finish_step = setup.mill.finish_step.and_then(|f| {
        if f.is_finite() && f.abs() > 1e-9 {
            Some(f.abs())
        } else {
            None
        }
    });
    let z_schedule = build_z_schedule(
        setup.mill.start_depth,
        total_depth,
        step_raw,
        finish_step,
        &setup.mill.depth_list,
    );
    let tabs_z = total_depth + setup.tabs.height.abs();
    let tab_radius = (setup.tool.diameter * 0.5).max(0.5);
    // Ramp profile only applies when tab_type=Ramp. ramp_length is the
    // horizontal distance over which Z transitions between cut_z and
    // tabs_z at the configured angle. Computed once per pass below.
    let tab_ramp_angle_deg = match setup.tabs.tab_type {
        TabType::Ramp => Some(setup.tabs.ramp_angle_deg.clamp(0.5, 89.0)),
        TabType::Rectangle => None,
    };

    // Helix mode replaces the straight Z plunge between passes with a
    // spiral down the contour — gentler on small-diameter tools and
    // produces cleaner closed-contour entries. Only meaningful for
    // closed paths; for open paths we silently fall back to straight.
    let closed_path = is_closed_path(segments);
    let helix = setup.mill.helix_mode && closed_path;
    // Ramp plunge: descend Z while walking the first `ramp_length` of
    // the path, then continue at depth. Computed once per pass from
    // `step / tan(angle)`. Disabled when helix is active (the helix
    // already provides a ramped descent over the full path).
    //
    // Helix-entry plunge: a start-of-cut spiral descent on a small
    // circle inside the closed pocket boundary, distinct from the
    // path-wide `helix_mode` above. Only meaningful for closed paths
    // when the helix circle (radius ≥ tool_radius) fits inside the
    // boundary polygon — otherwise we fall back to Ramp / Direct.
    let helix_entry: Option<HelixEntry> = match setup.mill.plunge {
        PlungeStrategy::Helix {
            angle_deg,
            radius_mm: Some(radius_mm),
        } if closed_path => {
            let tool_radius = setup.tool.diameter * 0.5;
            plan_helix_entry(segments, radius_mm, tool_radius, angle_deg)
        }
        _ => None,
    };
    let ramp_angle_deg = match setup.mill.plunge {
        PlungeStrategy::Ramp { angle_deg } => Some(angle_deg.clamp(0.5, 45.0)),
        PlungeStrategy::Helix { angle_deg, .. } if helix_entry.is_none() => {
            // Helix didn't fit (radius too small or circle outside
            // boundary) — fall back to Ramp at the same angle so the
            // user still gets a non-vertical entry.
            Some(angle_deg.clamp(0.5, 45.0))
        }
        _ => None,
    };
    let total_path_len: f64 = segments
        .iter()
        .map(|s| match s.kind {
            SegmentKind::Line | SegmentKind::Point => s.start.distance(s.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(s),
        })
        .sum();

    // For the helix-vs-direct decision we treat the first pass as
    // having no prev_z (no spiral from somewhere), but the ramp plunge
    // wants to descend from start_depth on the first pass too — that's
    // when it matters most. We track them with separate state.
    let mut prev_z: Option<f64> = None;
    let mut ramp_from: f64 = setup.mill.start_depth;
    // One shared whirl state for the entire multi-pass cut so
    // the spiral phase accumulates continuously across pass boundaries
    // — same continuity principle as cross-chord extended to
    // cross-pass. Previously, every pass instantiated fresh state at
    // `angle = 0`, leaving a visible flat spot on the wall at every
    // pass boundary.
    let mut whirl_state = WhirlState::default();
    // Walk the depth schedule. When empty (degenerate) bail.
    if z_schedule.is_empty() {
        return;
    }
    // Arc-fit the contour ONCE up front and reuse across every Z pass —
    // the fit depends only on the (Z-independent) XY geometry, so re-running
    // it per pass multiplied O(N) arc-fit work by the pass count P (deep
    // pockets reach 20–80 passes). Open-path cascades alternate walk
    // direction, so the reversed orientation is fit once too (closed paths
    // never reverse). The reversed fit is computed from `reverse_chain(
    // segments)` exactly as the per-pass code used to, so output is
    // byte-identical — this is a pure speedup.
    let fitted_forward = fit_line_runs(segments, setup);
    let fitted_reversed: Option<Vec<Segment>> = if closed_path {
        None
    } else {
        Some(fit_line_runs(&reverse_chain(segments), setup))
    };
    // The fit matching this pass's walk direction (forward / reversed).
    let fitted_for = |reversed: bool| -> &[Segment] {
        if reversed {
            fitted_reversed.as_deref().unwrap_or(&fitted_forward)
        } else {
            &fitted_forward
        }
    };
    for (pass_idx, &z) in z_schedule.iter().enumerate() {
        let pass_uses_tabs = setup.tabs.active && !tabs.is_empty() && z < tabs_z;
        // For an OPEN polyline, the cascade alternates walk
        // direction so consecutive passes pick up where the
        // previous one ended — pass 0 forward (start→end), pass 1
        // reversed (end→start), pass 2 forward, ... Closed paths
        // naturally land at their starting point each pass, so the
        // alternation only fires for `!closed_path`. The helix
        // branches below are unreachable for open paths
        // (`helix = helix_mode && closed_path`,
        // `helix_entry = plan_helix_entry(...closed paths only...)`),
        // so they always see the original `segments`.
        let reverse_this_pass = !closed_path && pass_idx % 2 == 1;
        let pass_segments_owned: Vec<Segment>;
        let pass_segments: &[Segment] = if reverse_this_pass {
            pass_segments_owned = reverse_chain(segments);
            &pass_segments_owned
        } else {
            segments
        };
        if let (true, Some(pz)) = (helix, prev_z) {
            // Spiral from prev_z down to z while tracing the segments.
            post.feedrate(rate_h);
            emit_helix_pass(segments, pz, z, post);
        } else if let Some(plan) = helix_entry.as_ref().filter(|_| !pass_uses_tabs) {
            // Start-of-cut helical entry: spiral down on a small
            // circle inside the pocket boundary, then move to the
            // path start and continue normally. Only the descent
            // portion is helix-driven; the rest of the pass uses the
            // ordinary path emit at constant z.
            let pz = ramp_from;
            post.feedrate(rate_h);
            emit_helix_entry(plan, pz, z, post);
            // Previously this walked from the helix landing
            // point STRAIGHT to the contour start with a G1 at rate_h
            // at the new cut depth — cutting through unmilled stock
            // at full DOC, which defeats the safety helix entry was
            // supposed to provide. Instead: lift to fast_move_z, rapid
            // XY to the contour start, then plunge at the tool's
            // plunge rate (rate_v). This costs one extra retract per
            // pass but the helix-entry plunge strategy is selected
            // specifically because the tool CAN'T plunge straight at
            // full depth — the lift+rapid+plunge below uses rate_v
            // (typically 100 mm/min) for that small final plunge step.
            let start = segments.first().map_or(plan.center, |s| s.start);
            // This lift to fast_move_z must be a G0 rapid, not a
            // G1 cut-feed move. The helix-entry landing already cleared
            // the helix radius worth of stock; the lift just retracts
            // through air on the way to the contour-start rapid. The
            // prior G1 added cycle time across every helix pass with
            // zero safety benefit (the controller's rapid feed isn't
            // any less safe in air than the cut feed).
            post.move_to(None, None, Some(setup.mill.fast_move_z));
            post.move_to(Some(start.x), Some(start.y), Some(setup.mill.fast_move_z));
            post.feedrate(rate_v);
            post.linear(None, None, Some(z));
            post.feedrate(rate_h);
            let dragoff = setup.tool.dragoff.unwrap_or(0.0);
            // Helix branches are closed-path only, so always the forward fit.
            emit_cut_path(
                fitted_for(false),
                setup,
                z,
                dragoff,
                rate_h,
                setup.mill.corner_feed_reduction,
                &mut whirl_state,
                post,
            );
        } else if let Some(angle) = ramp_angle_deg.filter(|_| !pass_uses_tabs) {
            // Ramp plunge: descend from pz to z over the first
            // ramp_length of arc length, then continue at z for the
            // remainder. emit_ramp_pass walks ALL segments — the ramp
            // IS the full pass — so we don't follow it with another
            // path emit. Tabs-needed passes fall through to the direct
            // branch below to keep the tabs walker authoritative.
            let pz = ramp_from;
            let dz = (pz - z).abs();
            let ramp_length = if dz < 1e-9 {
                0.0
            } else {
                dz / angle.to_radians().tan()
            };
            if ramp_length > 1e-6 && total_path_len >= ramp_length {
                post.feedrate(rate_h);
                // Straight lead-in cut (pass 0 only): the head sits at
                // the off-contour hop point where the lead plunged — cut
                // onto the contour start so the walk traces the first
                // segment from its true start instead of chording to its
                // end. Later passes already start at the contour start.
                if straight_lead && pass_idx == 0 {
                    if let Some(first) = pass_segments.first() {
                        post.linear(Some(first.start.x), Some(first.start.y), None);
                    }
                }
                emit_ramp_pass(pass_segments, pz, z, ramp_length, post);
            } else {
                // Path too short for the ramp → fall back to straight
                // plunge so the user still gets a valid program.
                post.feedrate(rate_v);
                post.linear(None, None, Some(z));
                post.feedrate(rate_h);
                // Straight lead-in cut (pass 0 only): the head sits at
                // the off-contour hop point where the lead plunged — cut
                // onto the contour start so the walk traces the first
                // segment from its true start instead of chording to its
                // end. Later passes already start at the contour start.
                if straight_lead && pass_idx == 0 {
                    if let Some(first) = pass_segments.first() {
                        post.linear(Some(first.start.x), Some(first.start.y), None);
                    }
                }
                let dragoff = setup.tool.dragoff.unwrap_or(0.0);
                emit_cut_path(
                    fitted_for(reverse_this_pass),
                    setup,
                    z,
                    dragoff,
                    rate_h,
                    setup.mill.corner_feed_reduction,
                    &mut whirl_state,
                    post,
                );
            }
        } else {
            post.feedrate(rate_v);
            post.linear(None, None, Some(z));
            post.feedrate(rate_h);
            // Straight lead-in cut (pass 0 only): the head sits at
            // the off-contour hop point where the lead plunged — cut
            // onto the contour start so the walk traces the first
            // segment from its true start instead of chording to its
            // end. Later passes already start at the contour start.
            if straight_lead && pass_idx == 0 {
                if let Some(first) = pass_segments.first() {
                    post.linear(Some(first.start.x), Some(first.start.y), None);
                }
            }
            if pass_uses_tabs {
                // Tabs are a closed-path feature; open paths don't
                // generate tab points so this branch isn't reached on
                // the open-cascade fix path. Forward `segments` to
                // keep the tabs walker's positioning math intact.
                emit_path_with_tabs(
                    segments,
                    tabs,
                    tabs_z,
                    z,
                    tab_radius,
                    tab_ramp_angle_deg,
                    rate_v,
                    rate_h,
                    post,
                );
            } else {
                let dragoff = setup.tool.dragoff.unwrap_or(0.0);
                emit_cut_path(
                    fitted_for(reverse_this_pass),
                    setup,
                    z,
                    dragoff,
                    rate_h,
                    setup.mill.corner_feed_reduction,
                    &mut whirl_state,
                    post,
                );
            }
        }
        prev_z = Some(z);
        ramp_from = z;
    }
    // Ramp plunge leaves a sloped section at the start of every pass —
    // the cells under the ramp sit at progressively descending Z, NOT
    // at the pass's final depth. Earlier passes' slopes are re-cut by
    // later passes (which start at the previous z and ramp deeper),
    // but the LAST pass's slope persists as material left in the
    // pocket. Add a constant-depth cleanup walk at total_depth to
    // sweep that slope flat. Skipped on tabs-active paths because the
    // tabs walker already lifts/lowers Z based on its own logic and a
    // bonus pass would double-cut.
    let needs_ramp_cleanup = ramp_angle_deg.is_some()
        && (!setup.tabs.active || tabs.is_empty())
        && total_path_len > 1e-6;
    if needs_ramp_cleanup {
        post.feedrate(rate_h);
        let dragoff = setup.tool.dragoff.unwrap_or(0.0);
        // For the open-polyline alternating cascade, the cleanup
        // walks from wherever the LAST pass ended so the controller
        // doesn't backtrack at cut feedrate across the entire path. With
        // pass 0 forward / pass 1 reversed / …, pass index N-1 is
        // reversed when N is even — tool at original start, cleanup
        // walks forward. When N is odd the last pass was forward — tool
        // at end, cleanup walks reversed. Closed paths land at their
        // start every pass and the cleanup direction is irrelevant.
        // Cleanup walks reversed when the last pass ended away from the
        // contour start (open path, odd pass count) — reuse the matching
        // precomputed fit instead of re-fitting.
        let cleanup_reversed = !closed_path && z_schedule.len() % 2 == 1;
        emit_cut_path(
            fitted_for(cleanup_reversed),
            setup,
            total_depth,
            dragoff,
            rate_h,
            setup.mill.corner_feed_reduction,
            &mut whirl_state,
            post,
        );
    }
}
