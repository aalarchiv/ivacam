//! Gcode generation — port of viaConstructor's `machine_cmd.py` and the
//! three output plugins (`gcode_grbl`, `gcode_linuxcnc`, hpgl).
//!
//! `PostProcessor` is the trait every dialect implements; `emit_polylines`
//! is the dialect-agnostic orchestrator that walks offsets and writes
//! gcode through the trait.

// # CAM/sim pedantic-lint exemptions
// CAM emission walks index arithmetic over offset/segment lists where indices
// are bounded by chain length (≪ 2^52). Short names (`x`, `y`, `z`, `cx`,
// `cy`, `bd`) follow the gcode-coordinate convention.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

use serde::{Deserialize, Serialize};

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::{MachineMode, Setup, ToolOffset, UnitSystem};
use crate::geometry::{Point2, Segment, SegmentKind};

pub mod arc_fit;
mod entry;
pub mod grbl;
pub mod hpgl;
pub(crate) mod leads;
pub mod linuxcnc;
mod order;
pub mod post_profile;
pub mod preview;
mod tabs;
mod walk;
pub mod wirbeln;
mod z_schedule;

use entry::{
    emit_helix_entry, emit_helix_pass, emit_ramp_pass, is_closed_path, plan_helix_entry, HelixEntry,
};
use leads::{lead_in_geometry, lead_out_geometry, LeadGeometry};
use order::{end_pos, order_offsets};
use tabs::emit_path_with_tabs;
use walk::{emit_cut_path, fit_line_runs};
use z_schedule::{arc_length, build_z_schedule};

/// Generic post-processor trait. Stateful — implementations track the last
/// emitted XYZ/feedrate/spindle so they can delta-encode output.
pub trait PostProcessor {
    fn separation(&mut self) {}
    fn raw(&mut self, _cmd: &str) {}
    fn comment(&mut self, _text: &str) {}

    fn unit(&mut self, _unit: UnitSystem);
    fn absolute(&mut self, _active: bool) {}
    fn feedrate(&mut self, rate: u32);

    fn program_start(&mut self) {}
    fn program_end(&mut self) {}

    fn tool(&mut self, _number: u32) {}
    fn tool_offsets(&mut self, _offset: ToolOffset) {}
    fn machine_offsets(&mut self, _offsets: (f64, f64, f64), _soft: bool) {}

    fn coolant_mist(&mut self) {}
    fn coolant_flood(&mut self) {}
    fn coolant_off(&mut self) {}

    fn spindle_off(&mut self) {}
    fn spindle_cw(&mut self, speed: u32, pause_seconds: u32);
    fn spindle_ccw(&mut self, speed: u32, pause_seconds: u32);

    fn move_to(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>);
    fn linear(&mut self, x: Option<f64>, y: Option<f64>, z: Option<f64>);
    fn arc_cw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    );
    fn arc_ccw(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        z: Option<f64>,
        i: Option<f64>,
        j: Option<f64>,
    );

    /// G81 simple drill: rapid to (x, y, r), feed plunge to z, dwell, retract to r.
    /// Default: manual G0/G1 expansion for posts that don't support canned cycles.
    fn drill_simple(&mut self, x: f64, y: f64, z: f64, r: f64, dwell_sec: f64) {
        self.move_to(Some(x), Some(y), Some(r));
        self.linear(None, None, Some(z));
        if dwell_sec > 0.0 {
            self.raw(&format!("G4 P{}", fmt_dwell(dwell_sec)));
        }
        self.linear(None, None, Some(r));
    }

    /// G83 peck: as G81 but pecks `q` mm at a time, fully retracting to r each peck.
    /// Default: manual G0/G1 expansion for posts that don't support canned cycles.
    fn drill_peck(&mut self, x: f64, y: f64, z: f64, r: f64, q: f64, dwell_sec: f64) {
        let q = q.abs();
        if q < 1e-9 {
            self.drill_simple(x, y, z, r, dwell_sec);
            return;
        }
        self.move_to(Some(x), Some(y), Some(r));
        // Drill bottom is below the retract plane (z < r). Each peck
        // descends by q from the *previous* depth (not from r) so we don't
        // re-cut already-cleared material; full retract to r is by rapid.
        let mut current_z = r;
        loop {
            // Next target: q deeper than current_z, but not past the bottom.
            let next_z = (current_z - q).max(z);
            self.linear(None, None, Some(next_z));
            if dwell_sec > 0.0 {
                self.raw(&format!("G4 P{}", fmt_dwell(dwell_sec)));
            }
            // Full retract to clearance plane.
            self.move_to(None, None, Some(r));
            current_z = next_z;
            if current_z <= z + 1e-9 {
                break;
            }
            // Re-enter to just above the previous peck depth at rapid, then
            // continue feeding. We approximate that with a rapid back to
            // current_z (the just-cut depth) — a real machine would step
            // off a hair to avoid rubbing, but the manual fallback's job is
            // just to be functionally equivalent.
            self.move_to(None, None, Some(current_z));
        }
    }

    /// G73 chip-break: as G83 but only retracts a small amount between pecks.
    /// Default: manual G0/G1 expansion for posts that don't support canned cycles.
    fn drill_chip_break(&mut self, x: f64, y: f64, z: f64, r: f64, q: f64, dwell_sec: f64) {
        const CHIP_BREAK_RETRACT: f64 = 0.5;
        let q = q.abs();
        if q < 1e-9 {
            self.drill_simple(x, y, z, r, dwell_sec);
            return;
        }
        self.move_to(Some(x), Some(y), Some(r));
        let mut current_z = r;
        loop {
            let next_z = (current_z - q).max(z);
            self.linear(None, None, Some(next_z));
            if dwell_sec > 0.0 {
                self.raw(&format!("G4 P{}", fmt_dwell(dwell_sec)));
            }
            current_z = next_z;
            if current_z <= z + 1e-9 {
                break;
            }
            // Small partial retract to break the chip, then continue.
            self.linear(None, None, Some(current_z + CHIP_BREAK_RETRACT));
        }
        // Final retract to clearance plane.
        self.move_to(None, None, Some(r));
    }

    fn finish(&self) -> String;

    /// Number of buffered output lines so far. Used by the per-op
    /// pipeline cache to slice each operation's contribution.
    fn out_lines_count(&self) -> usize {
        0
    }

    /// Clone the buffered output lines starting at `start` (inclusive).
    /// Returns an empty Vec when `start >= out_lines_count()`.
    fn out_lines_clone_from(&self, _start: usize) -> Vec<String> {
        Vec::new()
    }

    /// Append a pre-rendered batch of output lines verbatim. Used on
    /// cache hits — the lines were captured from a prior run and are
    /// already absolute-coordinate (see [`reset_state`]), so they're
    /// safe to splice in regardless of the current delta-encoding
    /// state.
    fn out_extend_lines(&mut self, _lines: &[String]) {}

    /// Reset the delta-encoding state so the next emitted move writes
    /// every coordinate explicitly (no `last_x`-based suppression).
    /// Used at op boundaries by the per-op pipeline cache so each op's
    /// captured output is self-contained and reusable across runs.
    fn reset_state(&mut self) {}

    /// Capture the current delta-encoding state (`last_x/y/z` + rates).
    /// Paired with [`PostProcessor::restore_state`] so a cache hit can
    /// resume from the same state a fresh run would have left the post
    /// in. Default returns zeroed/None fields — posts that delta-encode
    /// override this.
    fn capture_state(&self) -> CapturedPostState {
        CapturedPostState::default()
    }

    /// Restore a previously-captured delta-encoding state. Used on
    /// cache hits to splice cached gcode lines and resume as if those
    /// lines had been emitted live.
    fn restore_state(&mut self, _state: &CapturedPostState) {}

    /// Configure the program-wide number formatter (rt1.36): decimal
    /// separator and optional N-line-numbering start. Called once at
    /// `program_begin` from `MachineConfig`. Default impl is a no-op —
    /// posts that emit numeric coordinates (linuxcnc, grbl) override
    /// it; HPGL / pen plotters ignore it.
    fn configure(&mut self, _decimal_separator: char, _line_number_start: Option<u32>) {}

    /// rt1.15: attach a user-configurable post-processor profile.
    /// Called once at `program_begin` from `MachineConfig`. Default
    /// impl is a no-op; linuxcnc / grbl posts override to store the
    /// profile in their `PostState` and consult it for
    /// `program_start` / _end / tool / coolant.
    fn set_post_profile(&mut self, _profile: Option<&post_profile::PostProfile>) {}

    /// rt1.15: refresh the token-substitution context. Called at
    /// `program_begin` and at every op boundary so per-op tokens
    /// (`<op>`, `<t>`, `<n>`, `<f>`, `<s>`) reflect the active
    /// state. Default impl is a no-op.
    fn set_token_ctx(&mut self, _ctx: &post_profile::TokenCtx) {}

    /// Apply a per-tool Z work-coordinate offset (rt1.30). Called
    /// at `program_begin` for the first op's tool and right after each
    /// emitted toolchange. `LinuxCNC` / GRBL emit `G92 Z<shift>`;
    /// HPGL ignores. Skip when `shift_mm == 0`.
    fn tool_z_shift(&mut self, _shift_mm: f64) {}

    /// Emit a dwell of `seconds` (rt1.29 — used for laser pierce
    /// time). `LinuxCNC` / GRBL emit `G4 P<seconds>`; HPGL ignores.
    /// Skip when `seconds <= 0`.
    fn dwell(&mut self, _seconds: f64) {}
}

/// Public projection of [`PostState`] used by the per-op cache. Mirrors
/// the fields that affect delta-encoded output emission; the cache stores
/// one of these per op and restores it on a hit.
#[derive(Debug, Clone, Default)]
pub struct CapturedPostState {
    pub last_x: Option<f64>,
    pub last_y: Option<f64>,
    pub last_z: Option<f64>,
    pub last_rate: Option<u32>,
    pub last_speed: Option<u32>,
}

/// Format a dwell value for `G4 P` — strip trailing zeros so the line
/// stays readable. Mirrors the `LinuxCNC` post's number formatting.
fn fmt_dwell(v: f64) -> String {
    let s = format!("{v:.4}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".into()
    } else {
        trimmed.to_string()
    }
}

/// Top-level orchestrator. Walks `offsets` and emits gcode through `post`.
/// Replaces `polylines2machine_cmd` from `machine_cmd.py`.
pub fn emit_polylines<P: PostProcessor>(
    setup: &Setup,
    offsets: &[PolylineOffset],
    post: &mut P,
) -> String {
    program_begin(setup, post);
    let mut last_pos = Point2::new(0.0, 0.0);
    emit_polylines_block(setup, offsets, post, &mut last_pos);
    program_end(setup, post);
    post.finish()
}

/// Header-only emit. Per-op pipeline drivers call this once at the start
/// of the program, then loop through each op calling
/// [`emit_polylines_block`], then close with [`emit_program_end`].
pub fn emit_program_begin<P: PostProcessor>(setup: &Setup, post: &mut P) {
    program_begin(setup, post);
}

/// Footer-only emit. Counterpart to [`emit_program_begin`].
pub fn emit_program_end<P: PostProcessor>(setup: &Setup, post: &mut P) {
    program_end(setup, post);
}

/// Cut-block emit — the per-offset loop without program-begin / -end. The
/// per-op driver calls this once per operation; the `setup` passed is the
/// op's *synthesized* setup (its tool + params), and `last_pos` is shared
/// across calls so the next op continues from where the previous one
/// finished.
pub fn emit_polylines_block<P: PostProcessor>(
    setup: &Setup,
    offsets: &[PolylineOffset],
    post: &mut P,
    last_pos: &mut Point2,
) {
    let order = order_offsets(setup, offsets, *last_pos);
    for &idx in &order {
        emit_offset(setup, &offsets[idx], post, last_pos);
    }
}

/// V-Carve emit. Walks a list of XYZ polylines (each one already
/// ratchet-deepened by [`crate::cam::vcarve_emit::ratchet_emit`]) and
/// emits them as G1 cuts, with G0 lifts to safe Z between polylines.
/// `start_depth` is honored as the plunge entry plane; per-point Z is
/// already absolute.
pub fn emit_vcarve_block<P: PostProcessor>(
    setup: &Setup,
    polylines: &[Vec<(f64, f64, f64)>],
    post: &mut P,
    last_pos: &mut Point2,
) {
    if polylines.is_empty() {
        return;
    }
    let fast_z = setup.mill.fast_move_z;
    if setup.machine.mode == MachineMode::Mill {
        post.spindle_cw(setup.tool.speed, setup.tool.pause);
    }
    if setup.tool.flood {
        post.coolant_flood();
    }
    if setup.tool.mist {
        post.coolant_mist();
    }
    for poly in polylines {
        if poly.len() < 2 {
            continue;
        }
        let (sx, sy, _) = poly[0];
        // Travel: lift to safe Z, fly to the start XY, drop to start_depth.
        post.move_to(None, None, Some(fast_z));
        post.move_to(Some(sx), Some(sy), None);
        post.feedrate(setup.tool.rate_v);
        post.linear(None, None, Some(setup.mill.start_depth));
        post.feedrate(setup.tool.rate_h);
        for &(x, y, z) in poly {
            post.linear(Some(x), Some(y), Some(z));
        }
        let (lx, ly, _) = *poly.last().unwrap();
        *last_pos = Point2::new(lx, ly);
    }
    post.move_to(None, None, Some(fast_z));
}

/// Drill-cycle emit. Walks `offsets` whose single segment is a Point and
/// dispatches to the [`PostProcessor`] drill_* method matching `cycle`.
/// Used by the pipeline's per-op driver when `OpKind::Drill`.
///
/// `setup.mill.depth`        → drill bottom Z (typically negative).
/// `setup.mill.start_depth`  → R (clearance plane just above the workpiece).
/// `setup.mill.fast_move_z`  → safe Z for rapid moves between drill sites.
pub fn emit_drill_block<P: PostProcessor>(
    setup: &Setup,
    offsets: &[PolylineOffset],
    cycle: crate::project::DrillCycle,
    post: &mut P,
    last_pos: &mut Point2,
) {
    let order = order_offsets(setup, offsets, *last_pos);
    // Drill final Z. `setup.mill.depth` is the nominal bore floor;
    // `through_depth` extends it deeper to clear minor stock-
    // thickness variation. For conical tool tips (twist drills,
    // V-bits, engravers), `tool.tip_cone_length()` is the extra
    // depth needed for the FULL bore diameter to reach the bottom,
    // so we add it automatically — clarifying the user's
    // through-cut intent matches the actual geometry. The user's
    // explicit `through_depth` stacks on top so manual extension
    // still works.
    let cone_extra = setup.tool.tip_cone_length();
    let z = setup.mill.depth - setup.mill.through_depth.max(0.0) - cone_extra;
    let r = setup.mill.start_depth;
    let fast_z = setup.mill.fast_move_z;
    if setup.machine.mode == MachineMode::Mill {
        post.spindle_cw(setup.tool.speed, setup.tool.pause);
    }
    if setup.tool.flood {
        post.coolant_flood();
    }
    if setup.tool.mist {
        post.coolant_mist();
    }
    post.feedrate(setup.tool.rate_v);
    for &idx in &order {
        let offset = &offsets[idx];
        if offset.segments.is_empty() {
            continue;
        }
        let pt = offset.segments[0].start;
        if setup.machine.comments {
            post.separation();
            post.comment(&format!(
                "drill object={} x={:.4} y={:.4} z={:.4}",
                offset.source_object_idx, pt.x, pt.y, z
            ));
        }
        // Rapid up to a safe Z above the workpiece before traversing,
        // mirroring what emit_offset does for normal cuts.
        post.move_to(None, None, Some(fast_z));
        match cycle {
            crate::project::DrillCycle::Simple { dwell_sec } => {
                post.drill_simple(pt.x, pt.y, z, r, dwell_sec);
            }
            crate::project::DrillCycle::Peck {
                peck_step_mm,
                dwell_sec,
            } => {
                post.drill_peck(pt.x, pt.y, z, r, peck_step_mm, dwell_sec);
            }
            crate::project::DrillCycle::ChipBreak {
                peck_step_mm,
                dwell_sec,
            } => {
                post.drill_chip_break(pt.x, pt.y, z, r, peck_step_mm, dwell_sec);
            }
        }
        *last_pos = pt;
    }
    // Lift back to safe Z so subsequent ops start clean.
    post.move_to(None, None, Some(fast_z));
}

fn program_begin<P: PostProcessor>(setup: &Setup, post: &mut P) {
    // rt1.36: thread the decimal separator + N-numbering knobs into
    // the post state BEFORE any output flows so every emitted line
    // honors the project's MachineConfig.
    post.configure(
        setup.machine.decimal_separator,
        setup.machine.line_number_start,
    );
    // rt1.15: thread the user-configurable post profile + initial
    // token-substitution context. Profile templates can reference
    // tool / feed / spindle / unit etc. that we know from `setup`
    // even before any op runs.
    post.set_post_profile(setup.machine.post_profile.as_ref());
    let mut ctx = post_profile::TokenCtx::with_wiac_version();
    ctx.tool_number = setup.tool.number;
    ctx.tool_name.clone_from(&setup.tool.name);
    ctx.tool_diameter = setup.tool.diameter;
    ctx.feed = setup.tool.rate_h;
    ctx.spindle = setup.tool.speed;
    ctx.unit = setup.machine.unit;
    post.set_token_ctx(&ctx);
    post.program_start();
    post.unit(setup.machine.unit);
    post.absolute(true);
    post.feedrate(setup.tool.rate_h);
    post.move_to(None, None, Some(setup.mill.fast_move_z));
}

fn program_end<P: PostProcessor>(setup: &Setup, post: &mut P) {
    post.move_to(None, None, Some(setup.mill.fast_move_z));
    post.spindle_off();
    if setup.tool.flood || setup.tool.mist {
        post.coolant_off();
    }
    post.program_end();
    let _ = setup;
}

/// Emit a single polyline offset (one cut pass per multi-pass step).
// emit_offset is the per-offset emission: rapid-to-start → ramp/helix
// plunge → cut → retract. Each phase reads top-to-bottom and shares
// state with the next.
#[allow(clippy::too_many_lines)]
fn emit_offset<P: PostProcessor>(
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
    // wall-defining ring of a Pocket op (rt1.27), rough-set everywhere
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
    if setup.machine.mode == MachineMode::Mill {
        post.spindle_cw(use_speed, setup.tool.pause);
    }
    if setup.tool.flood {
        post.coolant_flood();
    }
    if setup.tool.mist {
        post.coolant_mist();
    }
    // Surface the chosen cut feedrate before the cut; the plunge feed
    // gets set explicitly at each Z-down move inside multi_pass.
    post.feedrate(use_rate_h);
    let _ = use_rate_v;
    let start = offset.segments[0].start;
    // Lead-in (straight, arc, or off) before the first cut. The arc
    // lead is a tangent roll-on at z=0 that lands the cutter on the
    // contour with motion already aligned to the first segment's
    // tangent — no dwell at the start point. multi_pass then plunges
    // from z=0 to the first pass depth at segments[0].start.
    let lead_in = lead_in_geometry(setup, &offset.segments);
    // rt1.29: laser pierce — once we've rapid'd to the entry point at
    // safe Z, dwell with the laser ON so it burns through stock
    // before any cutting motion starts.
    let pierce_sec = setup.tool.pierce_sec;
    match lead_in {
        LeadGeometry::Straight { from } => {
            post.move_to(Some(from.x), Some(from.y), Some(setup.mill.fast_move_z));
            if pierce_sec > 0.0 {
                post.dwell(pierce_sec);
            }
            post.linear(None, None, Some(0.0));
        }
        LeadGeometry::Arc {
            entry_or_exit: from,
            center,
            ccw,
        } => {
            post.move_to(Some(from.x), Some(from.y), Some(setup.mill.fast_move_z));
            if pierce_sec > 0.0 {
                post.dwell(pierce_sec);
            }
            post.linear(None, None, Some(0.0));
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
            if pierce_sec > 0.0 {
                post.dwell(pierce_sec);
            }
            post.linear(None, None, Some(0.0));
        }
    }

    multi_pass(
        setup,
        &offset.segments,
        &offset.tabs,
        offset.is_finish,
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
    post.linear(None, None, Some(setup.mill.fast_move_z));

    *last_pos = offset.segments.last().map_or(start, |s| s.end);
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
    post: &mut P,
) {
    use crate::cam::setup::{PlungeStrategy, TabType};
    // Finish-set rates (rt1.27): swap in the tool's _finish overrides
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

    // Plot-mode (rt1.35): emit ONE pass at the op's cut depth,
    // skipping the multi-step schedule + helix / ramp / finish_step /
    // through_depth / depth_list machinery. Laser / plasma / pen
    // plotter / 3D-printer / drag-knife controllers expect binary
    // pen-up / pen-down Z values; all the descent stages are noise.
    if setup.machine.plot_mode_z {
        let cut_z = setup.mill.depth.min(0.0);
        post.feedrate(rate_v);
        post.linear(None, None, Some(cut_z));
        post.feedrate(rate_h);
        let dragoff = setup.tool.dragoff.unwrap_or(0.0);
        let fitted = fit_line_runs(segments, setup);
        emit_cut_path(
            &fitted,
            setup,
            cut_z,
            dragoff,
            rate_h,
            setup.mill.corner_feed_reduction,
            post,
        );
        let _ = tabs; // tabs are meaningless in plot mode
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
    let z_schedule = build_z_schedule(
        setup.mill.start_depth,
        total_depth,
        step_raw,
        setup.mill.finish_step,
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
    // Walk the depth schedule. When empty (degenerate) bail.
    if z_schedule.is_empty() {
        return;
    }
    for &z in &z_schedule {
        let pass_uses_tabs = setup.tabs.active && !tabs.is_empty() && z < tabs_z;
        if let (true, Some(pz)) = (helix, prev_z) {
            // Spiral from prev_z down to z while tracing the segments.
            post.feedrate(rate_h);
            emit_helix_pass(segments, pz, z, post);
        } else if let Some(plan) = helix_entry.as_ref().filter(|_| !pass_uses_tabs) {
            // Start-of-cut helical entry: spiral down on a small
            // circle inside the pocket boundary, then walk to the
            // path start and continue normally. Only the descent
            // portion is helix-driven; the rest of the pass uses the
            // ordinary path emit at constant z.
            let pz = ramp_from;
            post.feedrate(rate_h);
            emit_helix_entry(plan, pz, z, post);
            // Cut from helix landing point to the path's actual start.
            let start = segments.first().map_or(plan.center, |s| s.start);
            post.linear(Some(start.x), Some(start.y), Some(z));
            let dragoff = setup.tool.dragoff.unwrap_or(0.0);
            let fitted = fit_line_runs(segments, setup);
            emit_cut_path(
                &fitted,
                setup,
                z,
                dragoff,
                rate_h,
                setup.mill.corner_feed_reduction,
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
                emit_ramp_pass(segments, pz, z, ramp_length, post);
            } else {
                // Path too short for the ramp → fall back to straight
                // plunge so the user still gets a valid program.
                post.feedrate(rate_v);
                post.linear(None, None, Some(z));
                post.feedrate(rate_h);
                let dragoff = setup.tool.dragoff.unwrap_or(0.0);
                let fitted = fit_line_runs(segments, setup);
                emit_cut_path(
                    &fitted,
                    setup,
                    z,
                    dragoff,
                    rate_h,
                    setup.mill.corner_feed_reduction,
                    post,
                );
            }
        } else {
            post.feedrate(rate_v);
            post.linear(None, None, Some(z));
            post.feedrate(rate_h);
            if pass_uses_tabs {
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
                let fitted = fit_line_runs(segments, setup);
                emit_cut_path(
                    &fitted,
                    setup,
                    z,
                    dragoff,
                    rate_h,
                    setup.mill.corner_feed_reduction,
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
        let fitted = fit_line_runs(segments, setup);
        emit_cut_path(
            &fitted,
            setup,
            total_depth,
            dragoff,
            rate_h,
            setup.mill.corner_feed_reduction,
            post,
        );
    }
}

/// Internal state shared across post processor implementations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostState {
    pub last_x: Option<f64>,
    pub last_y: Option<f64>,
    pub last_z: Option<f64>,
    pub last_rate: Option<u32>,
    pub last_speed: Option<u32>,
    pub absolute: bool,
    /// Decimal separator used by the number formatter — `.` (default)
    /// or `,` for European-locale Siemens / Heidenhain controllers
    /// (rt1.36). Configured once at program start from
    /// `MachineConfig::decimal_separator`.
    #[serde(default = "default_decimal_separator")]
    pub decimal_separator: char,
    /// When `Some(next)`, every emitted line gets a `N<next> ` prefix
    /// and `next` increments by 10 (rt1.36 / FANUC / vintage
    /// controllers). `None` = no numbering. Configured once at
    /// program start from `MachineConfig::line_number_start`.
    #[serde(default)]
    pub line_counter: Option<u32>,
    /// rt1.15: user-configurable post-processor profile attached to
    /// `MachineConfig`. When `Some`, the built-in posts consult its
    /// template strings instead of their hard-coded headers /
    /// footers / toolchange / coolant lines. `None` = use the
    /// post's built-in defaults.
    #[serde(default, skip)]
    pub profile: Option<crate::gcode::post_profile::PostProfile>,
    /// Current token substitution context for `profile` templates.
    /// Refreshed at `program_begin` and at each op boundary.
    #[serde(default, skip)]
    pub token_ctx: crate::gcode::post_profile::TokenCtx,
}

fn default_decimal_separator() -> char {
    '.'
}

impl Default for PostState {
    fn default() -> Self {
        Self {
            last_x: None,
            last_y: None,
            last_z: None,
            last_rate: None,
            last_speed: None,
            absolute: false,
            decimal_separator: '.',
            line_counter: None,
            profile: None,
            token_ctx: crate::gcode::post_profile::TokenCtx::default(),
        }
    }
}

/// Apply the post-processor numbering / separator settings derived
/// from `MachineConfig` (rt1.36). Drains down into `PostState` so the
/// per-post `write` / `fmt` helpers consult them on every line.
pub fn configure_post_state(
    state: &mut PostState,
    decimal_separator: char,
    line_number_start: Option<u32>,
) {
    // Only '.' and ',' are supported; anything else silently falls
    // back to '.' so the gcode stays parseable.
    state.decimal_separator = match decimal_separator {
        '.' | ',' => decimal_separator,
        _ => '.',
    };
    state.line_counter = line_number_start;
}

/// Format a floating-point number using the post-state's decimal
/// separator. Matches the upstream's formatting otherwise: 4 decimal
/// places, strip trailing zeros, never end with `.`.
#[must_use]
pub fn fmt_num(v: f64, sep: char) -> String {
    let s = format!("{v:.4}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    let base = if trimmed.is_empty() {
        "0".into()
    } else {
        trimmed.to_string()
    };
    if sep == ',' {
        base.replace('.', ",")
    } else {
        base
    }
}

/// Build the `N<n> ` line-number prefix and advance the counter when
/// active. When the counter is `None`, returns empty and leaves
/// state untouched.
pub fn line_number_prefix(state: &mut PostState) -> String {
    if let Some(n) = state.line_counter {
        let s = format!("N{n} ");
        state.line_counter = Some(n + 10);
        s
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::setup::{LeadKind, ToolOffset};
    use crate::geometry::Segment;

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    fn square_offset() -> PolylineOffset {
        PolylineOffset {
            segments: vec![
                Segment::line(p(0.0, 0.0), p(10.0, 0.0), "0", 7),
                Segment::line(p(10.0, 0.0), p(10.0, 10.0), "0", 7),
                Segment::line(p(10.0, 10.0), p(0.0, 10.0), "0", 7),
                Segment::line(p(0.0, 10.0), p(0.0, 0.0), "0", 7),
            ],
            closed: true,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        }
    }

    #[test]
    fn nearest_neighbor_picks_the_closer_offset_first() {
        use crate::cam::setup::ObjectOrder;
        let mut setup = Setup::default();
        setup.tool.diameter = 1.0;
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;
        setup.mill.objectorder = ObjectOrder::Nearest;

        // Far-from-origin offset first in the input, near-origin second.
        let mut far = square_offset();
        for s in &mut far.segments {
            s.start.x += 100.0;
            s.start.y += 100.0;
            s.end.x += 100.0;
            s.end.y += 100.0;
        }
        far.source_object_idx = 1;
        let offsets = vec![far, square_offset()];

        let order = super::order_offsets(&setup, &offsets, Point2::new(0.0, 0.0));
        assert_eq!(order, vec![1, 0], "near-origin offset should run first");
    }

    #[test]
    fn helix_mode_emits_z_during_arc_or_line_moves() {
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.mill.depth = -2.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.mill.helix_mode = true;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        // After the first pass, subsequent passes should descend Z
        // mid-path (G1 with both XY and Z together).
        let combined_xyz = g
            .lines()
            .filter(|l| l.starts_with("G1"))
            .any(|l| l.contains('X') && l.contains('Z'));
        assert!(
            combined_xyz,
            "helix mode should combine XY moves with Z descent"
        );
    }

    #[test]
    fn tabs_split_a_long_cut_with_z_lifts() {
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_h = 800;
        setup.mill.depth = -2.0;
        setup.mill.step = -2.0;
        setup.mill.fast_move_z = 5.0;
        setup.tabs.active = true;
        setup.tabs.height = 1.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        let mut offset = square_offset();
        // Tab in the middle of the bottom edge.
        offset.tabs = vec![crate::cam::offsets::TabPoint {
            x: 5.0,
            y: 0.0,
            width_override_mm: None,
            height_override_mm: None,
        }];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[offset], &mut post);

        // The tab should split the bottom edge: cut → lift to (-2 + 1) = -1
        // → traverse → drop back to -2 → cut to corner.
        assert!(g.contains("Z-1"), "expected lift to tabs_z=-1 in: {g}");
        // Both Z=-2 (cut depth) and Z=-1 (tabs_z) should appear.
        assert!(g.contains("Z-2"), "expected cut at depth -2 in: {g}");
    }

    #[test]
    fn ramped_tab_emits_trapezoid_z_profile() {
        use crate::cam::setup::TabType;
        use crate::gcode::preview::{interpret, MoveKind};
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_h = 800;
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.tabs.active = true;
        setup.tabs.height = 0.5;
        setup.tabs.tab_type = TabType::Ramp;
        setup.tabs.ramp_angle_deg = 30.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        // Single 20mm long line cut along +X with one tab in the middle.
        // tab_radius = max(3.0/2, 0.5) = 1.5 → tab_world_len = 3mm.
        // ramp_length = 0.5 / tan(30°) ≈ 0.866mm. 2*ramp_length ≈ 1.73mm
        // < 3mm tab width → trapezoid (ramp_up / flat / ramp_down).
        let line_offset = PolylineOffset {
            segments: vec![Segment::line(p(0.0, 0.0), p(20.0, 0.0), "0", 7)],
            closed: false,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: vec![crate::cam::offsets::TabPoint {
                x: 10.0,
                y: 0.0,
                width_override_mm: None,
                height_override_mm: None,
            }],
            is_finish: false,
        };

        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[line_offset], &mut post);
        let segments = interpret(&g);

        // Only inspect Cut moves along the path (skip Plunge/Retract,
        // which legitimately are pure-Z and bracket the path).
        let cut_z = -1.0_f64;
        let tabs_z = -0.5_f64;
        let mut waypoints: Vec<(f64, f64)> = Vec::new();
        for s in &segments {
            if !matches!(s.kind, MoveKind::Cut) {
                continue;
            }
            if s.from.y.abs() > 1e-3 || s.to.y.abs() > 1e-3 {
                continue;
            }
            if waypoints.is_empty() {
                waypoints.push((s.from.x, s.from.z));
            }
            waypoints.push((s.to.x, s.to.z));
        }

        // Expect a walk that starts and ends at cut_z, climbs to
        // tabs_z mid-path on a sloped ramp, holds tabs_z for the flat,
        // then descends on a sloped ramp.
        assert!(
            waypoints.len() >= 5,
            "expected ≥5 waypoints, got {waypoints:?}"
        );

        // Trapezoid signature: a flat-top run at tabs_z (consecutive
        // tabs_z waypoints with ΔX>0).
        let flat_pairs = waypoints
            .windows(2)
            .filter(|w| {
                (w[0].1 - tabs_z).abs() < 1e-6
                    && (w[1].1 - tabs_z).abs() < 1e-6
                    && w[1].0 - w[0].0 > 1e-6
            })
            .count();
        assert!(
            flat_pairs >= 1,
            "expected ≥1 flat-top run at tabs_z; waypoints={waypoints:?}"
        );

        // Sloped ramps in and out (Z changes while X advances).
        let has_ramp_up = waypoints.windows(2).any(|w| {
            (w[0].1 - cut_z).abs() < 1e-6
                && (w[1].1 - tabs_z).abs() < 1e-6
                && (w[1].0 - w[0].0).abs() > 1e-3
        });
        let has_ramp_down = waypoints.windows(2).any(|w| {
            (w[0].1 - tabs_z).abs() < 1e-6
                && (w[1].1 - cut_z).abs() < 1e-6
                && (w[1].0 - w[0].0).abs() > 1e-3
        });
        assert!(
            has_ramp_up,
            "expected a ramp-up (cut_z→tabs_z with ΔX>0); waypoints={waypoints:?}"
        );
        assert!(
            has_ramp_down,
            "expected a ramp-down (tabs_z→cut_z with ΔX>0); waypoints={waypoints:?}"
        );

        // No pure vertical Z step inside the cut path (Rectangle would
        // emit ΔX==0 transitions between cut_z and tabs_z).
        let pure_vertical = waypoints
            .windows(2)
            .any(|w| (w[0].1 - w[1].1).abs() > 1e-6 && (w[1].0 - w[0].0).abs() < 1e-9);
        assert!(
            !pure_vertical,
            "ramped tab must not emit pure-Z lifts; waypoints={waypoints:?}"
        );
    }

    #[test]
    fn ramped_tab_with_too_narrow_width_uses_triangle() {
        use crate::cam::setup::TabType;
        use crate::gcode::preview::{interpret, MoveKind};
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_h = 800;
        setup.mill.depth = -2.0;
        setup.mill.step = -2.0;
        setup.mill.fast_move_z = 5.0;
        setup.tabs.active = true;
        setup.tabs.height = 1.5; // tabs_z = -0.5
        setup.tabs.tab_type = TabType::Ramp;
        setup.tabs.ramp_angle_deg = 30.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        // tab_radius = 1.5 → tab_world_len = 3mm.
        // ramp_length = 1.5 / tan(30°) ≈ 2.598mm. 2*ramp_length ≈ 5.2mm
        // > 3mm tab width → triangle (ramp up directly to tabs_z at tab
        // center, then ramp down — no flat top).
        let line_offset = PolylineOffset {
            segments: vec![Segment::line(p(0.0, 0.0), p(20.0, 0.0), "0", 7)],
            closed: false,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: vec![crate::cam::offsets::TabPoint {
                x: 10.0,
                y: 0.0,
                width_override_mm: None,
                height_override_mm: None,
            }],
            is_finish: false,
        };

        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[line_offset], &mut post);
        let segments = interpret(&g);

        let cut_z = -2.0_f64;
        let tabs_z = -0.5_f64;
        let mut waypoints: Vec<(f64, f64)> = Vec::new();
        for s in &segments {
            if !matches!(s.kind, MoveKind::Cut) {
                continue;
            }
            if s.from.y.abs() > 1e-3 || s.to.y.abs() > 1e-3 {
                continue;
            }
            if waypoints.is_empty() {
                waypoints.push((s.from.x, s.from.z));
            }
            waypoints.push((s.to.x, s.to.z));
        }

        // Triangle profile: ramp-up directly to tabs_z, then ramp-down
        // back to cut_z, with NO consecutive-tabs_z (flat top) pair.
        let flat_pairs = waypoints
            .windows(2)
            .filter(|w| {
                (w[0].1 - tabs_z).abs() < 1e-6
                    && (w[1].1 - tabs_z).abs() < 1e-6
                    && w[1].0 - w[0].0 > 1e-6
            })
            .count();
        assert_eq!(
            flat_pairs, 0,
            "triangle must not have a flat top; waypoints={waypoints:?}"
        );

        // Apex at tabs_z exists.
        assert!(
            waypoints.iter().any(|w| (w.1 - tabs_z).abs() < 1e-6),
            "expected apex at tabs_z; waypoints={waypoints:?}"
        );

        // Both ramp segments are sloped (ΔX>0 + ΔZ != 0).
        let has_ramp_up = waypoints.windows(2).any(|w| {
            (w[0].1 - cut_z).abs() < 1e-6
                && (w[1].1 - tabs_z).abs() < 1e-6
                && (w[1].0 - w[0].0).abs() > 1e-3
        });
        let has_ramp_down = waypoints.windows(2).any(|w| {
            (w[0].1 - tabs_z).abs() < 1e-6
                && (w[1].1 - cut_z).abs() < 1e-6
                && (w[1].0 - w[0].0).abs() > 1e-3
        });
        assert!(has_ramp_up, "expected ramp-up; waypoints={waypoints:?}");
        assert!(has_ramp_down, "expected ramp-down; waypoints={waypoints:?}");
    }

    #[test]
    fn dragoff_inserts_swivel_arcs_at_corners() {
        let mut setup = Setup::default();
        setup.tool.diameter = 0.0; // drag knife: no radius
        setup.tool.speed = 0;
        setup.tool.rate_h = 800;
        setup.tool.dragoff = Some(0.5);
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::On;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        // Each of the 4 corners gets swivel arcs (G2 or G3 with I/J center).
        let arc_count = g
            .lines()
            .filter(|l| (l.starts_with("G2 ") || l.starts_with("G3 ")) && l.contains('I'))
            .count();
        assert!(
            arc_count >= 3,
            "expected at least 3 swivel arcs at square corners; got {arc_count}\n{g}"
        );
    }

    #[test]
    fn profile_circle_gcode_smaller_with_arc_fitting() {
        // Closed circle, tessellated into 128 chord segments at radius 10.
        // Sagitta of each chord ≈ r·(1 − cos(π/n)) ≈ 0.003 mm at n=128 —
        // well within the default 0.01 mm fit tolerance. With arc
        // fitting OFF the post emits 128 G1 lines per pass; with it ON
        // the polyline collapses to a small number of G2/G3 arcs. The
        // fitted program must contain at least one G2 or G3 token and
        // be < 1/5 the size of the unfitted program.
        let mut segs: Vec<Segment> = Vec::new();
        let n = 128usize;
        let r = 10.0_f64;
        let mut prev = p(r, 0.0);
        for i in 1..=n {
            let t = (i as f64) * std::f64::consts::TAU / (n as f64);
            let next = p(r * t.cos(), r * t.sin());
            segs.push(Segment::line(prev, next, "0", 7));
            prev = next;
        }
        let offset = PolylineOffset {
            segments: segs,
            closed: true,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        };

        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_h = 800;
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::On;

        setup.machine.arcs = true;
        let mut post = linuxcnc::Post::new();
        let g_arcs = emit_polylines(&setup, &[offset.clone()], &mut post);

        setup.machine.arcs = false;
        let mut post2 = linuxcnc::Post::new();
        let g_lines = emit_polylines(&setup, &[offset], &mut post2);

        let has_arc = g_arcs
            .lines()
            .any(|l| l.starts_with("G2 ") || l.starts_with("G3 "));
        assert!(
            has_arc,
            "fitted program must contain G2 or G3; got:\n{g_arcs}"
        );
        assert!(
            g_arcs.len() * 5 <= g_lines.len(),
            "arc-fitted program ({} bytes) should be ≥5x smaller than unfitted ({} bytes)",
            g_arcs.len(),
            g_lines.len(),
        );
    }

    #[test]
    fn linuxcnc_emits_a_recognizable_program() {
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_h = 800;
        setup.mill.depth = -2.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        // Smoke checks: header (G21 mm + G90 absolute), at least one G1 and one G0,
        // and a spindle command.
        assert!(g.contains("G21"), "should set mm units");
        assert!(g.contains("G90"), "should set absolute");
        assert!(g.contains("M3 S12000"), "should start spindle CW at 12000");
        assert!(g.contains("G1 X10"), "should cut to first corner");
        assert!(g.contains("M5"), "should stop spindle at end");
    }
}
