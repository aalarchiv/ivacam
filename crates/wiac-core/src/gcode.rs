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
use crate::project::tool::SpindleDirection;

/// z1y0: route the post's spindle-direction call based on the tool's
/// `spindle_direction`. Centralized so every cut-emission site
/// (`emit_offset`, `emit_drill_block`, `emit_vcarve_block`) and the
/// toolchange envelope (`pipeline::emit_toolchange_envelope`) pick
/// the same path without each caller open-coding the match.
pub(crate) fn spindle_on<P: PostProcessor>(
    post: &mut P,
    dir: SpindleDirection,
    speed: u32,
    pause: u32,
) {
    match dir {
        SpindleDirection::Cw => post.spindle_cw(speed, pause),
        SpindleDirection::Ccw => post.spindle_ccw(speed, pause),
    }
}

/// 20y5: dispatch the cut-entry "tool-on" call based on the machine
/// mode. Mill spins the spindle (M3 / M4). Laser fires the beam at the
/// configured power (M3 S<power>) via the post's `laser_on` hook. Drag
/// (knives, pen plotters) has no spindle / beam — no-op.
///
/// Centralized so the three emission sites (`emit_offset`,
/// `emit_drill_block`, `emit_vcarve_block`) don't each re-derive the
/// mode dispatch. The previous code gated `spindle_on` behind
/// `mode == Mill` only, which left laser cuts with NO `M3 S<power>` at
/// all — the program ran the moves but the beam stayed off.
fn cut_tool_on<P: PostProcessor>(post: &mut P, setup: &Setup, power_or_speed: u32) {
    match setup.machine.mode {
        MachineMode::Mill => {
            spindle_on(
                post,
                setup.tool.spindle_direction,
                power_or_speed,
                setup.tool.pause,
            );
        }
        MachineMode::Laser => {
            // xkvv: arm the laser at S0 BEFORE the rapid to the entry
            // point. The previous `laser_on(power)` here fired the beam
            // at full power before the head had moved — every rapid
            // traverse scorched a line across the workpiece. The
            // matching `cut_tool_pierce` below ramps to cut power just
            // before the pierce dwell.
            post.laser_arm();
        }
        MachineMode::Drag => {
            // Drag knife / pen plotter — no spindle, no beam.
        }
        MachineMode::Plasma => {
            // zpuk: plasma torch — most controllers accept M3 S<power>
            // for "fire arc at <power>" the same way GRBL / LinuxCNC
            // accept it for a laser. Unlike laser, the plasma torch
            // must be LIT during the rapid + Z-drop to pierce height
            // (the arc needs to strike before the head reaches the
            // workpiece), so we keep the full-power fire here rather
            // than routing through `laser_arm` / `cut_tool_pierce`.
            post.laser_on(power_or_speed);
        }
    }
}

/// xkvv: ramp the laser from armed (S0) to cut power right before the
/// pierce dwell. No-op for every mode except Laser; the matching arm
/// happened at `cut_tool_on` so the rapid traverse runs cold.
fn cut_tool_pierce<P: PostProcessor>(post: &mut P, setup: &Setup, power: u32) {
    if matches!(setup.machine.mode, MachineMode::Laser) {
        post.laser_on(power);
    }
}

/// 20y5: dispatch the cut-exit "tool-off" call. Laser MUST drop the
/// beam (M5 or S0) before any rapid traverse, otherwise the rapid
/// burns a stripe through the workpiece. Mill leaves the spindle
/// running between cuts (the post's delta-encoded spindle state
/// dedupes the re-arm); Drag is a no-op.
fn cut_tool_off<P: PostProcessor>(post: &mut P, setup: &Setup) {
    if matches!(setup.machine.mode, MachineMode::Laser | MachineMode::Plasma) {
        // zpuk: plasma torch-off mirrors laser — drop the arc between
        // cuts so the rapid traverse doesn't leave a melt trail.
        post.laser_off();
    }
}

pub mod arc_fit;
mod entry;
pub mod face_mill_overlay;
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

    /// 20y5: laser-on at the configured power. Called by `cut_tool_pierce`
    /// AFTER the rapid + Z-drop has landed the head at the pierce point;
    /// the beam ramps up to cut power just before the pierce dwell so
    /// the rapid itself runs at zero power (see `laser_arm`).
    /// `LinuxCNC` / GRBL override to emit `M3 S<power>` (dynamic-laser
    /// mode `M4` on GRBL is also acceptable, but `M3` matches what
    /// Lightburn / `T2Laser` / Estlcam laser emit by default). HPGL
    /// ignores. Default no-op so non-laser-aware posts keep working.
    fn laser_on(&mut self, _power: u32) {}

    /// xkvv: laser-arm — emit `M3 S0` to bring the controller into
    /// laser-on / spindle-clockwise modal state at ZERO power BEFORE
    /// the rapid traverse to the entry point. Without this the prior
    /// `cut_tool_on` fired `M3 S<power>` and the rapid burnt a stripe
    /// through the workpiece. Called by `cut_tool_on` for laser mode;
    /// followed by `laser_on(power)` at pierce time via
    /// `cut_tool_pierce`. Default no-op so non-laser-aware posts keep
    /// working.
    fn laser_arm(&mut self) {}

    /// 20y5: laser-off — drop the beam between cuts so the rapid
    /// traverse doesn't burn a stripe through the part. Called by
    /// `cut_tool_off` at the end of every cut block in Laser mode.
    /// `LinuxCNC` / GRBL override to emit `M5` (which is `S0` modally
    /// for GRBL's laser mode). HPGL ignores. Default no-op.
    fn laser_off(&mut self) {}

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

    /// pxyt: format a dwell `seconds` value for the post's `G4 P<...>`
    /// word. Default returns seconds (`LinuxCNC` / Smoothieware reading).
    /// Posts that own a [`post_profile::PostProfile`] override this to
    /// honour `dwell_unit` — Mach3 / Mach4 / Centroid / most Fanuc
    /// derivatives read `P` in milliseconds, so a profile that opts in
    /// emits `seconds * 1000` here. Trait-default `drill_simple` /
    /// `drill_peck` / `drill_chip_break` route through this method
    /// (instead of the seconds-only free `fmt_dwell` helper) so a GRBL
    /// post inheriting the defaults but carrying a Mach3-ms profile
    /// stops producing 1000x dwell mismatches.
    fn fmt_dwell_post(&self, seconds: f64) -> String {
        fmt_dwell(seconds)
    }

    /// G81 simple drill: rapid to (x, y, r), feed plunge to z, dwell, retract to r.
    /// `rate_v` is the plunge feed in mm/min; the default impl emits an
    /// F<`rate_v`> before each G1 plunge so the move lands at a known
    /// feed regardless of what the modal F was when the caller invoked
    /// us (o01e — non-canned posts must self-anchor their feed).
    fn drill_simple(&mut self, x: f64, y: f64, z: f64, r: f64, rate_v: u32, dwell_sec: f64) {
        self.move_to(Some(x), Some(y), Some(r));
        // o01e: anchor plunge feed inside the cycle so we don't inherit
        // whatever F a prior op left modal (often rate_h or 0).
        self.feedrate(rate_v);
        self.linear(None, None, Some(z));
        if dwell_sec > 0.0 {
            self.raw(&format!("G4 P{}", self.fmt_dwell_post(dwell_sec)));
        }
        self.linear(None, None, Some(r));
    }

    /// G83 peck: as G81 but pecks `q` mm at a time, fully retracting to r each peck.
    /// `rate_v` is the plunge feed (see [`PostProcessor::drill_simple`]).
    /// Default: manual G0/G1 expansion for posts that don't support canned cycles.
    fn drill_peck(&mut self, x: f64, y: f64, z: f64, r: f64, q: f64, rate_v: u32, dwell_sec: f64) {
        let q = q.abs();
        if q < 1e-9 {
            self.drill_simple(x, y, z, r, rate_v, dwell_sec);
            return;
        }
        self.move_to(Some(x), Some(y), Some(r));
        // o01e: anchor the plunge feed at entry. Without this, the
        // first G1 plunge would inherit whatever F was last set — for
        // GRBL (which uses this default impl) that could be rate_h
        // from the prior cut block, which slams the bit into the work
        // at 8x the safe plunge feed.
        self.feedrate(rate_v);
        // Drill bottom is below the retract plane (z < r). Each peck
        // descends by q from the *previous* depth (not from r) so we don't
        // re-cut already-cleared material; full retract to r is by rapid.
        let mut current_z = r;
        loop {
            // Next target: q deeper than current_z, but not past the bottom.
            let next_z = (current_z - q).max(z);
            self.linear(None, None, Some(next_z));
            if dwell_sec > 0.0 {
                self.raw(&format!("G4 P{}", self.fmt_dwell_post(dwell_sec)));
            }
            // Full retract to clearance plane.
            self.move_to(None, None, Some(r));
            current_z = next_z;
            if current_z <= z + 1e-9 {
                break;
            }
            // co8b: re-enter to a small clearance ABOVE the previous
            // peck depth at rapid, then feed the last 0.5 mm down at
            // plunge feed. Rapidding all the way down to the just-cut
            // depth lets the cutter slam straight into chip-clogged
            // material — fine on a slow Z servo, but it chips tips on
            // a fast Z.
            const RE_ENTRY_CLEARANCE_MM: f64 = 0.5;
            let re_entry_z = current_z + RE_ENTRY_CLEARANCE_MM;
            self.move_to(None, None, Some(re_entry_z));
            // o01e: re-anchor the plunge feed after every rapid retract.
            // G0 doesn't consume F, but it does NOT roll back any prior
            // modal change, so a controller that re-evaluates F at each
            // motion-mode change (FANUC, vintage Mach3) sees the right
            // value at the G1 boundary. Posts dedupe identical-rate
            // emits so the repeat is free.
            self.feedrate(rate_v);
            self.linear(None, None, Some(current_z));
        }
    }

    /// G73 chip-break: as G83 but only retracts a small amount between pecks.
    /// `rate_v` is the plunge feed (see [`PostProcessor::drill_simple`]).
    /// Default: manual G0/G1 expansion for posts that don't support canned cycles.
    fn drill_chip_break(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        r: f64,
        q: f64,
        rate_v: u32,
        dwell_sec: f64,
    ) {
        const CHIP_BREAK_RETRACT: f64 = 0.5;
        let q = q.abs();
        if q < 1e-9 {
            self.drill_simple(x, y, z, r, rate_v, dwell_sec);
            return;
        }
        self.move_to(Some(x), Some(y), Some(r));
        // o01e: anchor plunge feed at entry (see drill_peck).
        self.feedrate(rate_v);
        let mut current_z = r;
        loop {
            let next_z = (current_z - q).max(z);
            self.linear(None, None, Some(next_z));
            if dwell_sec > 0.0 {
                self.raw(&format!("G4 P{}", self.fmt_dwell_post(dwell_sec)));
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
    /// separator and optional N-line-numbering start, plus the project
    /// unit (w9hd) so the emit-time mm→inch scale applies to every
    /// X/Y/Z/I/J/R/F number. Called once at `program_begin` from
    /// `MachineConfig`. Default impl is a no-op — posts that emit
    /// numeric coordinates (linuxcnc, grbl) override it; HPGL / pen
    /// plotters use their own integer plotter units and ignore.
    fn configure(
        &mut self,
        _decimal_separator: char,
        _line_number_start: Option<u32>,
        _unit: UnitSystem,
    ) {
    }

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

    /// sbtg: select the XY plane for arc interpretation. `LinuxCNC` /
    /// GRBL emit `G17`; HPGL ignores. A controller booted in G18 / G19
    /// would otherwise reinterpret our G2/G3 arcs in XZ / YZ. Called
    /// once per program in [`program_begin`] before any motion.
    fn plane_xy(&mut self) {}

    /// sbtg: cancel any active cutter-radius compensation. `LinuxCNC`
    /// / GRBL emit `G40`; HPGL ignores. Defends against G41 / G42
    /// left modal by a prior program.
    fn cutter_comp_off(&mut self) {}

    /// sbtg: select feed-per-minute mode. `LinuxCNC` / GRBL emit
    /// `G94`; HPGL ignores. Defends against G95 (units-per-revolution)
    /// left modal by a prior turning program.
    fn feed_per_minute(&mut self) {}

    /// olpn: cancel any active canned drill cycle. `LinuxCNC` emits
    /// `G80`; GRBL has no canned cycles so the default no-op is fine
    /// (its drill block was already G0/G1 expanded). Called at the end
    /// of [`emit_drill_block`] so a following op's G0 / G1 is not
    /// reinterpreted by FANUC / Mach3 as another drill at the canned
    /// cycle's modal Z / R.
    fn cancel_canned_cycle(&mut self) {}

    /// e2mq: select the program's active work coordinate system. Called
    /// once from [`program_begin`] with `Setup.wcs`. `LinuxCNC` / GRBL
    /// write the explicit `G54..G59` word AND pin the same `Wcs` value
    /// into `PostState.wcs` so `tool_z_shift` can emit a
    /// `G10 L20 P<n>` against the active table (P1=G54, P2=G55, …).
    /// HPGL ignores. Default no-op — posts that don't model a WCS just
    /// drop the call.
    fn select_wcs(&mut self, _wcs: crate::project::Wcs) {}
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
    /// sulg: last commanded coolant state. Without this, an op that
    /// turned coolant off would have its M9 line cached, but the next
    /// op's `coolant_flood` would see the live `PostState`'s stale
    /// `last_coolant` and skip re-emitting the M8 — leaving the next
    /// cut dry. `Unknown` keeps pre-sulg cache entries (which deserialize
    /// via `Default`) compatible with the live default initial state.
    pub last_coolant: CoolantState,
    /// sulg: last commanded spindle direction. Op N may flip a tool
    /// to `Ccw` (M4); when op N+1 is cached and its body was authored
    /// against `Cw`, the replay would otherwise leave the spindle in
    /// the wrong direction. None = no spindle direction commanded yet
    /// (post-program-begin, pre-first-tool).
    pub last_spindle_dir: Option<SpindleDirection>,
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
    // 20y5: spin up the spindle / arm the laser ONCE at block entry.
    // For Mill, the spindle stays on and the loop's re-arms dedupe via
    // `last_speed`. For Laser, the loop bounces M3 S<power> / M5 around
    // each rapid traverse so the beam is off during travel — see the
    // cut_tool_off / cut_tool_on pair around the inter-poly rapid below.
    cut_tool_on(post, setup, setup.tool.speed);
    if setup.tool.flood {
        post.coolant_flood();
    }
    if setup.tool.mist {
        post.coolant_mist();
    }
    for (i, poly) in polylines.iter().enumerate() {
        if poly.len() < 2 {
            continue;
        }
        let (sx, sy, _) = poly[0];
        // Travel: lift to safe Z, fly to the start XY, drop to start_depth.
        // 20y5: drop the laser BEFORE the inter-poly rapid traverse;
        // re-arm at the new start XY. Mill's spindle_off is NOT called
        // here — only laser_off, which is a no-op for non-laser modes.
        // The first iteration skips the off/on bounce because the
        // outer `cut_tool_on` already armed the tool.
        if i > 0 {
            cut_tool_off(post, setup);
        }
        post.move_to(None, None, Some(fast_z));
        post.move_to(Some(sx), Some(sy), None);
        if i > 0 {
            cut_tool_on(post, setup, setup.tool.speed);
        }
        post.feedrate(setup.tool.rate_v);
        post.linear(None, None, Some(setup.mill.start_depth));
        // md0m: laser-mode V-carve needs a pierce dwell at the cut
        // plane so the beam burns through stock before lateral motion
        // begins. gd2x added this to emit_offset; emit_vcarve_block
        // had the same plunge-then-immediately-cut shape and dragged
        // the first few mm through unmelted material. Mirror the
        // gd2x ordering: F<rate_v> → G1 Z<start> → dwell → F<rate_h>.
        if setup.tool.pierce_sec > 0.0 {
            post.dwell(setup.tool.pierce_sec);
        }
        post.feedrate(setup.tool.rate_h);
        for &(x, y, z) in poly {
            post.linear(Some(x), Some(y), Some(z));
        }
        let (lx, ly, _) = *poly.last().unwrap();
        *last_pos = Point2::new(lx, ly);
        // The polyline emitter (e.g. thread::helix_waypoints) is
        // responsible for ending on a safe XY before the G0 lift below
        // (7388 — thread helices end with a radial retract so the lift
        // doesn't scrape the just-cut crest).
    }
    // 20y5: drop the laser before the final lift so a subsequent op's
    // rapid (or program_end's park traverse) doesn't burn.
    cut_tool_off(post, setup);
    post.move_to(None, None, Some(fast_z));
}

/// Stufenfase rim chamfer geometry: lead-in ramp polyline (Z descending
/// along the rim's arc) followed by a single full-revolution G2/G3 at
/// `flat_z`. The ramp polyline starts above stock and ends tangent to
/// the flat revolution's start XY at `flat_z`.
///
/// sq8z: prior to this struct the rim revolution was a 64-point flat
/// polyline emitted via `emit_vcarve_block`, which walks point-by-point
/// at G1. Output was bloated, jerky, and chord-approximated a true
/// circle. By passing the rim's actual `(center, radius, ccw)` we can
/// emit ONE G2/G3 full-circle and let the post-processor split it for
/// controllers that need the half-circle pair (3p7v in linuxcnc.rs and
/// grbl.rs).
#[derive(Debug, Clone)]
pub struct StufenfaseHole {
    pub center: Point2,
    pub radius: f64,
    /// CCW = true ⇒ G3 (positive bulge convention); false ⇒ G2.
    pub ccw: bool,
    /// Final cut depth at the rim. Negative.
    pub flat_z: f64,
    /// Lead-in ramp waypoints: XYZ along the rim, Z descending from 0
    /// to `flat_z`. The last point's XY MUST be the rim's revolution
    /// start (the same point the full-circle G2/G3 starts AND ends on).
    pub ramp: Vec<(f64, f64, f64)>,
}

/// Emit a sequence of Stufenfase rim chamfers. Each hole gets:
///   * G0 lift to safe Z, then G0 XY to the ramp's start;
///   * G1 plunge to `start_depth`;
///   * G1 walk of the lead-in ramp at cut feed;
///   * single G2/G3 full revolution at `flat_z` (the post splits
///     full-circles for controllers that need it — see 3p7v).
///
/// sq8z: replaces the previous "build a 64-point polyline and feed it
/// to `emit_vcarve_block`" path; rim revolutions now emit as a single
/// arc move rather than 64 chord G1s.
pub fn emit_stufenfase_rim_block<P: PostProcessor>(
    setup: &Setup,
    holes: &[StufenfaseHole],
    post: &mut P,
    last_pos: &mut Point2,
) {
    if holes.is_empty() {
        return;
    }
    let fast_z = setup.mill.fast_move_z;
    cut_tool_on(post, setup, setup.tool.speed);
    if setup.tool.flood {
        post.coolant_flood();
    }
    if setup.tool.mist {
        post.coolant_mist();
    }
    for (i, hole) in holes.iter().enumerate() {
        if hole.ramp.len() < 2 {
            continue;
        }
        let (sx, sy, _) = hole.ramp[0];
        // Inter-hole travel: lift to safe Z, fly to the ramp's start XY,
        // drop to the op's start_depth before the ramp walk.
        if i > 0 {
            cut_tool_off(post, setup);
        }
        post.move_to(None, None, Some(fast_z));
        post.move_to(Some(sx), Some(sy), None);
        if i > 0 {
            cut_tool_on(post, setup, setup.tool.speed);
        }
        post.feedrate(setup.tool.rate_v);
        post.linear(None, None, Some(setup.mill.start_depth));
        post.feedrate(setup.tool.rate_h);
        // Walk the ramp's XYZ waypoints at cut feed.
        for &(x, y, z) in &hole.ramp {
            post.linear(Some(x), Some(y), Some(z));
        }
        // Full revolution at constant Z. After the ramp, the cutter
        // sits at the rim's revolution start point. Emit the full
        // circle as a single arc — same XY target as the start, with
        // I/J pointing from the cutter to the rim's center.
        let (lx, ly, _) = *hole.ramp.last().unwrap();
        let i_off = hole.center.x - lx;
        let j_off = hole.center.y - ly;
        if hole.ccw {
            post.arc_ccw(Some(lx), Some(ly), Some(hole.flat_z), Some(i_off), Some(j_off));
        } else {
            post.arc_cw(Some(lx), Some(ly), Some(hole.flat_z), Some(i_off), Some(j_off));
        }
        *last_pos = Point2::new(lx, ly);
    }
    cut_tool_off(post, setup);
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
    // 3kqo: separate the canned-cycle retract plane R from the user's
    // `start_depth` (the entry / clearance plane configured per op).
    // R is the plane to which G83/G73 RAPID retract after every peck —
    // it MUST be above the stock surface, otherwise the bit retracts
    // INSIDE the chip-clogged hole and breaks. Treat Z=0 as the stock
    // top (project convention) and add a small clearance so chips
    // clear; never let R drop below start_depth (recessed work where
    // start_depth > 0 — there the user explicitly said "stock surface
    // is at start_depth"). Match the co8b re-entry clearance value
    // (0.5 mm) so the canned-cycle path uses the same air-gap budget
    // as the trait-default manual peck loop.
    const DRILL_R_CLEARANCE_MM: f64 = 0.5;
    let stock_top_z = 0.0_f64;
    let r = setup
        .mill
        .start_depth
        .max(stock_top_z + DRILL_R_CLEARANCE_MM);
    let fast_z = setup.mill.fast_move_z;
    // 20y5: laser-aware tool-on. Drilling under a laser is an unusual
    // workflow (you'd be ablating spots) but it should at least fire
    // the beam — better than the previous "mode != Mill" gate that
    // emitted moves with the laser silently off.
    cut_tool_on(post, setup, setup.tool.speed);
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
                post.drill_simple(pt.x, pt.y, z, r, setup.tool.rate_v, dwell_sec);
            }
            crate::project::DrillCycle::Peck {
                peck_step_mm,
                dwell_sec,
            } => {
                post.drill_peck(pt.x, pt.y, z, r, peck_step_mm, setup.tool.rate_v, dwell_sec);
            }
            crate::project::DrillCycle::ChipBreak {
                peck_step_mm,
                dwell_sec,
            } => {
                post.drill_chip_break(
                    pt.x,
                    pt.y,
                    z,
                    r,
                    peck_step_mm,
                    setup.tool.rate_v,
                    dwell_sec,
                );
            }
        }
        *last_pos = pt;
    }
    // olpn: cancel the canned drill cycle before any subsequent G0 /
    // G1 from the next op. Otherwise FANUC / Mach3 (and LinuxCNC in
    // strict modes) reinterpret the next G0 as another invocation of
    // the same drill cycle at the modal Z / R, with disastrous
    // results. Emit BEFORE the safe-Z lift so the G80 lands inside
    // the drill block, not adjacent to the next op's spindle line.
    post.cancel_canned_cycle();
    // 20y5: drop the laser before the final lift so a subsequent op's
    // rapid traverse (or program_end's park move) doesn't burn.
    cut_tool_off(post, setup);
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
        setup.machine.unit,
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
    // e2mq: pin the active WCS so the controller can't be left on a
    // stale `G55`/`G56` from a prior program — and so per-tool
    // `tool_z_shift` writes its `G10 L20 P<n>` against the right
    // table. Emitted after the unit/absolute pragmas (the G54..G59
    // word is itself a modal pragma; its position in the preamble
    // is conventional) and before any motion. HPGL ignores.
    post.select_wcs(setup.wcs);
    // sbtg: emit a known modal preamble before any motion so a
    // controller booted in a non-default state doesn't reinterpret our
    // arcs in XZ (G18), leave cutter-comp on from a prior program
    // (G42/G41), or feed in units-per-revolution (G95) instead of
    // units-per-minute (G94). `raw` is a no-op on HPGL so plotter
    // output is unaffected.
    post.plane_xy();
    post.cutter_comp_off();
    post.feed_per_minute();
    // l3o6: don't emit F<rate> here — the next motion is a G0 rapid
    // (move_to fast_move_z) and G0 ignores the modal feedrate. The
    // first G1/G2/G3 that actually needs F is in the per-offset block
    // below (`post.feedrate(use_rate_h)` before any cut), so deferring
    // here means F appears on the line that actually consumes it,
    // matching what controllers expect (and what most CAM systems
    // emit). The cost is zero: posts dedupe identical-rate F-emits.
    post.move_to(None, None, Some(setup.mill.fast_move_z));
}

fn program_end<P: PostProcessor>(setup: &Setup, post: &mut P) {
    // syol: lift to fast_move_z FIRST so any park-XY move happens
    // safely above the workpiece (the previous code emitted only the
    // Z lift before M5/M30, leaving the head parked over the part).
    post.move_to(None, None, Some(setup.mill.fast_move_z));
    // syol: emit a safe XY parking move BEFORE the spindle stops.
    //   1. explicit `park_xy` in the work coordinate system, or
    //   2. machine-home via G53 G0 X0 Y0 when `park_at_home == true`
    //      (most hobby + pro controllers accept G53 since LinuxCNC
    //      2.x / Mach3 / GRBL 1.1), or
    //   3. work-zero (G0 X0 Y0) as the defensible default — both
    //      sim and operator know where the head is at end-of-program.
    if let Some((px, py)) = setup.machine.park_xy {
        post.move_to(Some(px), Some(py), None);
    } else if setup.machine.park_at_home {
        // G53 modifies the next motion line to be in machine
        // coordinates regardless of the active WCS. Emit raw so
        // posts that don't model G53 (HPGL) silently drop it.
        post.raw("G53 G0 X0 Y0");
    } else {
        post.move_to(Some(0.0), Some(0.0), None);
    }
    post.spindle_off();
    if setup.tool.flood || setup.tool.mist {
        post.coolant_off();
    }
    post.program_end();
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
    // 20y5: dispatch by machine mode. Mill: spin the spindle. Laser:
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
    // the lead-in entry plunge below (vfpa).
    post.feedrate(use_rate_h);
    let start = offset.segments[0].start;
    // Lead-in (straight, arc, or off) before the first cut. The arc
    // lead is a tangent roll-on at z=0 that lands the cutter on the
    // contour with motion already aligned to the first segment's
    // tangent — no dwell at the start point. multi_pass then plunges
    // from z=0 to the first pass depth at segments[0].start.
    let lead_in = lead_in_geometry(setup, &offset.segments);
    // rt1.29 / gd2x: laser pierce — rapid XY at safe Z (no Z change
    // away from fast_move_z), plunge to cut Z, THEN dwell at the cut
    // height so the beam burns through focused stock before motion
    // begins. Dwelling at fast_move_z (the old order) left the head
    // defocused, never pierced, and the first cut yanked unmelted
    // material. Order matches Lightburn / T2Laser / Estlcam laser.
    let pierce_sec = setup.tool.pierce_sec;
    // zpuk: plasma entry — when machine.mode == Plasma the lead-in
    // emits a two-step Z descent instead of a single plunge:
    //   1. Torch already on (cut_tool_on above) — rapid XY at fast_z.
    //   2. Rapid (G0) to pierce_height_mm above stock.
    //   3. Dwell pierce_delay_sec while the arc pierces.
    //   4. G1 down to cut_height_mm at the plunge rate.
    //   5. Walk the contour (multi_pass collapses to one pass).
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
    // lyq6: the lead-in plunge must drop to `start_depth` (the entry
    // plane just above the workpiece), NOT to a literal Z=0. Stock
    // proud of Z=0 (start_depth < 0) would crash the cutter at the
    // approach; recesses (start_depth > 0) would have the cutter
    // cutting air. `multi_pass` then descends from `start_depth` to
    // the first pass depth via plunge / ramp / helix.
    let entry_z = setup.mill.start_depth;
    // vfpa: the lead-in plunge from fast_move_z to entry_z is a G1
    // Z-drop — emit it at the plunge feed (rate_v), not the cut feed
    // (rate_h). Modal F was set to `use_rate_h` at line 715 (so the
    // first cut motion has a known F nearby), so we switch to rate_v
    // here, plunge, then restore rate_h before the lateral cut. Without
    // this, the cutter dives from safe Z to start_depth at the (often
    // 8x faster) cut feed — snaps non-center-cutting endmill tips and
    // is the canonical proud-stock crash. Posts dedupe identical-rate
    // F-emits so the restore is free when rate_v == rate_h.
    match lead_in {
        LeadGeometry::Straight { from } => {
            post.move_to(Some(from.x), Some(from.y), Some(setup.mill.fast_move_z));
            if let Some((pierce_h, cut_h, delay)) = plasma_entry {
                // zpuk: plasma two-step Z. Rapid to pierce, dwell, G1
                // to cut height. cut_height is the cut plane that
                // multi_pass walks at (it short-circuits to one pass
                // because mode == Plasma — see the plasma branch in
                // multi_pass).
                post.move_to(None, None, Some(pierce_h));
                if delay > 0.0 {
                    post.dwell(delay);
                }
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(cut_h));
            } else {
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(entry_z));
                // xkvv: laser-mode ramps from armed (S0) to full power
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
                if delay > 0.0 {
                    post.dwell(delay);
                }
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(cut_h));
            } else {
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(entry_z));
                // xkvv: laser-mode ramps from armed (S0) to full power
                // here, between the plunge and the pierce dwell. Mill /
                // drag / plasma are no-ops in this helper.
                cut_tool_pierce(post, setup, use_speed);
                if pierce_sec > 0.0 {
                    post.dwell(pierce_sec);
                }
            }
            // 3o3n: re-emit the cutting feedrate immediately before
            // the arc lead-in. The roll-on is the first ACTUAL cut
            // motion in the program (G2/G3 honors F); relying on a
            // modal set further upstream means the listing's first
            // arc has no F line nearby — defensive on FANUC / vintage
            // controllers that re-evaluate F at each motion-mode
            // change. Posts dedupe identical-rate emits so this is
            // free when the modal already matches. vfpa also makes
            // this the post-plunge feedrate restore (rate_v → rate_h).
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
                if delay > 0.0 {
                    post.dwell(delay);
                }
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(cut_h));
            } else {
                post.feedrate(use_rate_v);
                post.linear(None, None, Some(entry_z));
                // xkvv: laser-mode ramps from armed (S0) to full power
                // here, between the plunge and the pierce dwell. Mill /
                // drag / plasma are no-ops in this helper.
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
    // 20y5: drop the laser BEFORE the safe-Z retract so the rapid
    // traverse to the next offset / op doesn't burn a stripe through
    // the part. Mill keeps the spindle running between cuts (the
    // post's delta-encoded `last_speed` dedupes the next cut's M3
    // re-arm so no extra lines emit); Drag is a no-op.
    cut_tool_off(post, setup);
    // o1g3: final retract after lead-out is a rapid (G0), not a cut
    // motion (G1). The lead-out already rolled the cutter off the
    // contour into free space; lifting to fast_move_z at cut feed
    // multiplies cycle time across hundreds of contours with zero
    // safety benefit. Use `move_to` (G0) to retract at the controller's
    // rapid feed.
    post.move_to(None, None, Some(setup.mill.fast_move_z));

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
    //
    // 6yhs: MachineMode::Drag also collapses to a single pass even
    // when the global `plot_mode_z` is off. setup_resolver pins
    // setup.machine.mode = Drag per-op for OpKind::DragKnife
    // (see setup_resolver.rs:349-351); without this branch, a Drag op
    // on a Mill-default machine (hobby Shapeoko with a knife taped to
    // the spindle) walked the same path N times at incrementally more
    // negative Z values. The knife is physically locked at one
    // depth — extra passes do nothing useful except wear the Z axis
    // and waste cycle time. Mirror the plot_mode_z branch: one pass
    // at the configured `depth`.
    //
    // zpuk: Plasma is also a single-pass mode — the torch height stays
    // at `cut_height_mm` for the whole cut, never stepping down. The
    // pre-cut pierce + height drop is emitted up in emit_offset BEFORE
    // multi_pass runs, so by the time we reach here Z is already at
    // cut_height. Skipping the schedule keeps the walk at constant Z.
    if setup.machine.plot_mode_z
        || setup.machine.mode == MachineMode::Drag
        || setup.machine.mode == MachineMode::Plasma
    {
        // zpuk: plasma cuts at `cut_height_mm` above stock (positive
        // Z), NOT at `mill.depth` (which is the milling-style depth
        // below stock). For plot_mode_z / Drag the cut Z is still
        // mill.depth.min(0). The lead-in in emit_offset already
        // positioned Z at cut_height for plasma; we only need to
        // re-emit the linear-to-cut-Z guard for plot_mode_z / Drag.
        let cut_z = if setup.machine.mode == MachineMode::Plasma {
            // Default 1.5 mm if the resolved value is 0 (legacy
            // projects without plasma fields set).
            if setup.tool.cut_height_mm > 0.0 {
                setup.tool.cut_height_mm
            } else {
                1.5
            }
        } else {
            setup.mill.depth.min(0.0)
        };
        // For plot_mode_z / Drag we still need to dive to cut_z (the
        // lead-in plunges to mill.start_depth which may not equal
        // the cut depth). For Plasma the lead-in already dropped to
        // cut_height — the post delta-encodes Z so re-emitting the
        // same Z is a no-op, but skip it anyway for clarity.
        if setup.machine.mode != MachineMode::Plasma {
            post.feedrate(rate_v);
            post.linear(None, None, Some(cut_z));
        }
        post.feedrate(rate_h);
        let dragoff = setup.tool.dragoff.unwrap_or(0.0);
        let fitted = fit_line_runs(segments, setup);
        // Plot mode is single-pass; a fresh wirbeln state is fine.
        let mut wirbeln_state = face_mill_overlay::WirbelnState::default();
        emit_cut_path(
            &fitted,
            setup,
            cut_z,
            dragoff,
            rate_h,
            setup.mill.corner_feed_reduction,
            &mut wirbeln_state,
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
    // 580k: normalize finish_step at the call boundary — reject
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
    // qm9x: ONE shared wirbeln state for the entire multi-pass cut so
    // the spiral phase accumulates continuously across pass boundaries
    // — same continuity principle as 89n5 (cross-chord) extended to
    // cross-pass. Pre-qm9x, every pass instantiated fresh state at
    // `winkel = 0`, leaving a visible flat spot on the wall at every
    // pass boundary.
    let mut wirbeln_state = face_mill_overlay::WirbelnState::default();
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
            // circle inside the pocket boundary, then move to the
            // path start and continue normally. Only the descent
            // portion is helix-driven; the rest of the pass uses the
            // ordinary path emit at constant z.
            let pz = ramp_from;
            post.feedrate(rate_h);
            emit_helix_entry(plan, pz, z, post);
            // lja0: previously this walked from the helix landing
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
            // i6c2: this lift to fast_move_z must be a G0 rapid, not a
            // G1 cut-feed move. The helix-entry landing already cleared
            // the helix radius worth of stock; the lift just retracts
            // through air on the way to the contour-start rapid. The
            // prior G1 added cycle time across every helix pass with
            // zero safety benefit (the controller's rapid feed isn't
            // any less safe in air than the cut feed). Pairs with the
            // o1g3 fix at line 896 (final-retract G0).
            post.move_to(None, None, Some(setup.mill.fast_move_z));
            post.move_to(Some(start.x), Some(start.y), Some(setup.mill.fast_move_z));
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
                &mut wirbeln_state,
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
                    &mut wirbeln_state,
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
                    &mut wirbeln_state,
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
            &mut wirbeln_state,
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
    /// w9hd: emit-time length scale from project units (mm) to gcode
    /// units. 1.0 for `UnitSystem::Mm`, 1/25.4 for `UnitSystem::Inch`.
    /// Multiplied into every X/Y/Z/I/J/R/Q coordinate AND into the
    /// feedrate F word at emission time. The pipeline math keeps
    /// running in mm; only the rendered numbers convert at the gcode
    /// boundary so G20 + 100mm authored emits ~3.937, not 100. Set
    /// by `configure_post_state` from `MachineConfig::unit`.
    #[serde(default = "default_unit_scale")]
    pub unit_scale: f64,
    /// f78z: last commanded coolant state. Dedupe target so M7 / M8 /
    /// M9 lines only emit on state changes — the old code re-emitted
    /// the SAME M7 / M8 on every offset because the cut-block helpers
    /// unconditionally call `coolant_mist` / `coolant_flood` before
    /// each cut. `Unknown` is the initial state; `OffEmitted` is the
    /// state right after `coolant_off` so a re-enable after off still
    /// gets through.
    #[serde(default, skip)]
    pub last_coolant: CoolantState,
    /// sulg: last commanded spindle direction. Tracked so the cache
    /// can capture and restore it across cached-op boundaries — without
    /// this, op N flipping to `Ccw` (M4) would leave a cached op N+1
    /// re-emitting against a stale "we're in Cw" assumption. None =
    /// no spindle direction commanded yet.
    #[serde(default, skip)]
    pub last_spindle_dir: Option<SpindleDirection>,
    /// e2mq: the active work coordinate system the gcode program runs
    /// under. Threaded in from `Project.work_offset.wcs` via
    /// `configure_post_state` and emitted as an explicit `G54..G59`
    /// in `program_begin`. Without this, GRBL's `tool_z_shift` had to
    /// hardcode `G10 L20 P1` (= G54) even when the user had picked
    /// G55 — writing the per-tool z-shift into the wrong WCS.
    #[serde(default, skip)]
    pub wcs: crate::project::Wcs,
}

/// f78z: tracked coolant state for dedup. Mirrors the M-code we last
/// commanded (or `Unknown` at program start, before any M7 / M8 / M9
/// has been emitted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoolantState {
    #[default]
    Unknown,
    Mist,
    Flood,
    Off,
}

fn default_unit_scale() -> f64 {
    1.0
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
            unit_scale: 1.0,
            last_coolant: CoolantState::Unknown,
            last_spindle_dir: None,
            wcs: crate::project::Wcs::G54,
        }
    }
}

impl PostState {
    /// tsay: number of decimal places to use when formatting numbers
    /// for emission. mm projects stay at 4 (0.0001 mm = 0.1 µm, finer
    /// than any realistic CNC repeats). Inch projects consult the
    /// post profile's `decimal_places_inch` override and fall back to
    /// 4 — that's the historical default; shops doing sub-mil work
    /// can opt up to 5 or 6 via the profile.
    #[must_use]
    pub fn decimals(&self) -> u8 {
        // 1.0 / 25.4 is the only inch scale we ever set, but the test
        // helpers occasionally hand-set unit_scale; use the !=1.0 check
        // as the "we're not in mm" gate.
        let is_inch = (self.unit_scale - 1.0).abs() > 1e-12;
        if is_inch {
            self.profile
                .as_ref()
                .and_then(|p| p.decimal_places_inch)
                .unwrap_or(4)
        } else {
            4
        }
    }
}

/// Apply the post-processor numbering / separator settings derived
/// from `MachineConfig` (rt1.36) and the program-wide unit scale
/// (w9hd). Drains down into `PostState` so the per-post `write` /
/// `fmt` helpers consult them on every line.
pub fn configure_post_state(
    state: &mut PostState,
    decimal_separator: char,
    line_number_start: Option<u32>,
    unit: UnitSystem,
) {
    // Only '.' and ',' are supported; anything else silently falls
    // back to '.' so the gcode stays parseable.
    state.decimal_separator = match decimal_separator {
        '.' | ',' => decimal_separator,
        _ => '.',
    };
    state.line_counter = line_number_start;
    // w9hd: pipeline math runs in mm. When the machine is unit=Inch
    // the G20 pragma flips and every emitted X/Y/Z/I/J/R + F must be
    // divided by 25.4 to convert mm -> inches AT THE OUTPUT BOUNDARY.
    // Without this the controller mis-scales by 25.4× (catastrophic).
    state.unit_scale = match unit {
        UnitSystem::Mm => 1.0,
        UnitSystem::Inch => 1.0 / 25.4,
    };
}

/// Format a floating-point number using the post-state's decimal
/// separator. Default precision is 4 decimal places, strip trailing
/// zeros, never end with `.`. The decimal count is the maximum width;
/// shorter renderings (e.g. round numbers) trim down identically.
///
/// e0hq: snap values whose magnitude is below half-an-ULP of the
/// emitted precision to a positive literal `0` so we never render
/// `-0.000` / `-0` — some controllers (Heidenhain, vintage FANUC)
/// reject a leading minus on a zero coordinate, and operators reading
/// the listing rightly find `Z-0` confusing.
#[must_use]
pub fn fmt_num(v: f64, sep: char) -> String {
    fmt_num_dp(v, sep, 4)
}

/// tsay: same as [`fmt_num`] but with caller-chosen decimal places.
/// Inch mode (0.0001 in = 0.00254 mm) is borderline for sub-mil work,
/// so the post can opt into 5 or 6 decimals via
/// `PostProfile::decimal_places_inch`. mm-mode defaults remain at 4.
#[must_use]
pub fn fmt_num_dp(v: f64, sep: char, decimals: u8) -> String {
    // Suppress signed-zero: any value with magnitude < 0.5 * 10^-N
    // (half-ULP of the N-decimal output) would round to "0" anyway —
    // including `-0.000049…` at 4 dp, which used to render as `-0`.
    // Snap those to a clean positive zero before formatting so the
    // leading `-` never appears.
    let zero_eps = 0.5 * 10f64.powi(-i32::from(decimals));
    let v = if v.abs() < zero_eps { 0.0 } else { v };
    let dp = usize::from(decimals);
    let s = format!("{v:.dp$}");
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

    #[test]
    fn f78z_coolant_emits_once_across_multiple_offsets() {
        // Two offsets with flood-on tooling: the M8 line must appear
        // exactly once. The old code re-emitted M8 before every cut,
        // which a few controllers (older Mach3) interpret as a
        // request to re-prime the pump — at best a noisy listing, at
        // worst a relay-life issue.
        let mut setup = Setup::default();
        setup.tool.diameter = 1.0;
        setup.tool.flood = true;
        setup.tool.speed = 12000;
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;
        let mut sq2 = square_offset();
        for s in &mut sq2.segments {
            s.start.x += 50.0;
            s.end.x += 50.0;
        }
        sq2.source_object_idx = 1;
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[square_offset(), sq2], &mut post);
        let m8_count = g.lines().filter(|l| l.trim() == "M8").count();
        assert_eq!(
            m8_count, 1,
            "expected exactly one M8 across two offsets, got {m8_count}:\n{g}",
        );
    }

    #[test]
    fn z1y0_spindle_ccw_routes_through_post_spindle_ccw() {
        // A left-hand cutter (SpindleDirection::Ccw) must emit M4
        // instead of M3 — left-hand tools chip-load in the reverse
        // direction, so commanding M3 spins them backwards and the
        // cutter tries to climb up the workpiece. Routing through
        // `spindle_ccw` (M4) is the difference between "works" and
        // "cuts the operator's hand".
        let mut setup = Setup::default();
        setup.tool.diameter = 1.0;
        setup.tool.speed = 12000;
        setup.tool.spindle_direction = SpindleDirection::Ccw;
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[square_offset()], &mut post);
        assert!(
            g.contains("M4 S12000") || g.contains("M4S12000"),
            "expected M4 (CCW) for left-hand cutter, got: {g}",
        );
        // Exclude "M30" (program end) from the M3 prohibition — the
        // substring match would otherwise alias on it. We only forbid
        // M3 followed by a space or S (the spindle-on word).
        assert!(
            !g.lines().any(|l| l.starts_with("M3 ") || l == "M3" || l.starts_with("M3S")),
            "must not emit M3 (spindle CW) when spindle_direction = Ccw: {g}",
        );
    }

    #[test]
    fn z1y0_default_spindle_direction_still_cw() {
        // Default behavior (Cw) must keep emitting M3 — z1y0 only
        // adds the CCW path; existing projects round-trip unchanged.
        let mut setup = Setup::default();
        setup.tool.diameter = 1.0;
        setup.tool.speed = 12000;
        // spindle_direction defaults to Cw via Default impl.
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[square_offset()], &mut post);
        assert!(g.contains("M3"), "default direction (Cw) must emit M3: {g}");
        assert!(!g.contains("M4"), "default must not emit M4: {g}");
    }

    #[test]
    fn l3o6_first_f_emits_after_initial_g0() {
        // Before this fix, program_begin emitted F<rate> BEFORE the
        // initial rapid lift to fast_move_z — and G0 doesn't consume
        // the feedrate modal. The visible effect is a stray F line
        // that confuses linenumber-driven dry-run tracing on FANUC /
        // Mach3; the real cost is that the F applied to the rapid is
        // misinterpreted by some sims as a slow cutting move.
        let mut setup = Setup::default();
        setup.tool.diameter = 1.0;
        setup.tool.rate_h = 800;
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[square_offset()], &mut post);
        // Locate the first G0 (initial rapid) and the first F<rate>;
        // F must appear AFTER the G0, not before.
        let first_g0 = g.lines().position(|l| l.trim_start().starts_with("G0"));
        let first_f = g.lines().position(|l| l.trim_start().starts_with('F'));
        let (g0, f) = (first_g0.expect("expected a G0"), first_f.expect("expected an F"));
        assert!(
            f > g0,
            "expected F to appear AFTER initial G0 (G0 ignores F); got F at line {f}, G0 at line {g0}\n{g}",
        );
    }

    #[test]
    fn e0hq_fmt_num_suppresses_negative_zero() {
        // -0.0 must never round-trip as "-0"; same for any value
        // whose magnitude is below half an ULP of the 4-decimal
        // output (those would all render as "0" anyway and the
        // leading minus is pure noise — and breaks Heidenhain /
        // vintage FANUC controllers that reject `-0`).
        assert_eq!(fmt_num(-0.0, '.'), "0");
        assert_eq!(fmt_num(-0.000_001, '.'), "0");
        assert_eq!(fmt_num(-4.9e-5, '.'), "0");
        // Just above the snap threshold still renders signed.
        assert_eq!(fmt_num(-0.0001, '.'), "-0.0001");
        // Sanity: positive zero is unchanged.
        assert_eq!(fmt_num(0.0, '.'), "0");
        // Comma locale: same suppression rule.
        assert_eq!(fmt_num(-0.0, ','), "0");
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
    fn gcode_helix_walk_to_start_uses_safe_feed() {
        // lja0: After emit_helix_entry lands the cutter on a small
        // circle inside the pocket boundary, the post must NOT walk
        // from there to the contour start with a G1 at rate_h at the
        // new cut depth — that's a full-immersion straight-line cut
        // through unmilled stock, defeating the safety the helix
        // entry was supposed to provide. Instead, the post lifts to
        // fast_move_z, rapids to the contour start, and plunges at
        // rate_v.
        use crate::cam::setup::PlungeStrategy;
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.rate_h = 800;
        setup.tool.rate_v = 100;
        setup.mill.depth = -2.0;
        setup.mill.step = -2.0;
        setup.mill.fast_move_z = 5.0;
        setup.mill.plunge = PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(2.0),
        };
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Inside;

        // A 30 mm square is big enough for a 2 mm-radius helix circle
        // with the 3 mm tool to fit inside (clearance > radius +
        // tool_radius = 2 + 1.5 = 3.5).
        let mut sq = PolylineOffset {
            segments: vec![
                Segment::line(p(2.0, 2.0), p(28.0, 2.0), "0", 7),
                Segment::line(p(28.0, 2.0), p(28.0, 28.0), "0", 7),
                Segment::line(p(28.0, 28.0), p(2.0, 28.0), "0", 7),
                Segment::line(p(2.0, 28.0), p(2.0, 2.0), "0", 7),
            ],
            closed: true,
            level: 0,
            is_pocket: 1,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        };
        sq.is_finish = false;

        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[sq], &mut post);
        // The emitted gcode must contain a retract to fast_move_z (Z5)
        // after the helix and before reaching the contour start. The
        // old code went straight from helix-end to contour-start at
        // cut depth (Z-2).
        let has_retract = g.contains("Z5") || g.contains("Z 5") || g.contains("Z5.");
        assert!(
            has_retract,
            "post-helix must retract to fast_move_z (Z=5); gcode was:\n{g}",
        );
        // Verify it uses G0 at some point after the helix arcs (one of
        // the lift / rapid pair must be a G0).
        let has_g0_post_helix = g
            .lines()
            .skip_while(|l| !(l.contains("G3") || l.contains("G2")))
            .any(|l| l.trim_start().starts_with("G0"));
        assert!(
            has_g0_post_helix,
            "post-helix lift/rapid must use G0 in gcode:\n{g}"
        );
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

    /// sbtg: the prologue must contain G17 (XY plane), G40 (cutter-comp
    /// off), and G94 (feed-per-minute) BEFORE the first motion line so
    /// a controller booted in G18 / G42 / G95 doesn't reinterpret the
    /// first G0 / G1.
    #[test]
    fn program_begin_emits_g17_g40_g94() {
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
        setup.mill.offset = ToolOffset::Outside;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);

        let lines: Vec<&str> = g.lines().collect();
        let first_motion = lines
            .iter()
            .position(|l| l.starts_with("G0 ") || l.starts_with("G1 "))
            .expect("expected at least one motion line");
        let head: Vec<&str> = lines.iter().take(first_motion).copied().collect();
        for code in ["G17", "G40", "G94"] {
            assert!(
                head.iter().any(|l| l == &code || l.starts_with(&format!("{code} "))),
                "expected {code} in prologue (before first G0/G1) — got head:\n{}",
                head.join("\n")
            );
        }
        assert!(
            first_motion < 30,
            "expected prologue within 30 lines; first motion at {first_motion}"
        );
    }

    /// 3p7v: a full-circle arc (start == end, with a non-trivial I/J
    /// vector to the center) must split into two G2 / G3 commands so
    /// GRBL doesn't reject the program with error:33.
    #[test]
    fn full_circle_arc_splits_into_two_g2() {
        // Drive the post directly: rapid to (5, 0), then "arc back to
        // (5, 0)" with center (0, 0) — a full circle of radius 5.
        let mut post = linuxcnc::Post::new();
        post.absolute(true);
        post.move_to(Some(5.0), Some(0.0), None);
        // I = center.x - start.x = -5; J = center.y - start.y = 0.
        post.arc_cw(Some(5.0), Some(0.0), None, Some(-5.0), Some(0.0));
        let g = post.finish();
        let g2_lines: Vec<&str> = g.lines().filter(|l| l.starts_with("G2 ")).collect();
        assert_eq!(
            g2_lines.len(),
            2,
            "expected full circle to split into two G2 commands; got:\n{g}"
        );
        // Each half must carry an I or J center vector.
        for l in &g2_lines {
            assert!(
                l.contains('I') || l.contains('J'),
                "G2 line should keep its I/J center vector: {l}"
            );
        }
        // The two halves' endpoints must differ (start ≠ first endpoint
        // ≠ second endpoint = start). The first G2 goes to the
        // diametrically-opposite point (-5, 0); the second returns.
        assert!(
            g2_lines[0].contains("X-5"),
            "first half should end at X-5 (diametrically opposite the start): {}",
            g2_lines[0]
        );
        assert!(
            g2_lines[1].contains("X5"),
            "second half should end back at X5 (the original start): {}",
            g2_lines[1]
        );
    }

    /// lyq6: the lead-in plunge must drop to `setup.mill.start_depth`,
    /// not a literal Z=0. Verifies the proud-stock case
    /// (`start_depth` < 0) where Z=0 would crash the cutter.
    #[test]
    fn lead_in_plunge_uses_mill_start_depth() {
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_h = 800;
        setup.mill.depth = -5.0;
        setup.mill.start_depth = -2.0; // proud stock; cutter must drop to Z-2 first
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);

        // Find the FIRST G1 line — it's the lead-in plunge. With the
        // bug it would carry Z0; with the fix it must carry Z-2.
        let first_g1 = g
            .lines()
            .find(|l| l.starts_with("G1 "))
            .expect("expected at least one G1 line");
        assert!(
            first_g1.contains("Z-2"),
            "lead-in must plunge to start_depth=-2; first G1: {first_g1}\nfull:\n{g}"
        );
        assert!(
            !first_g1.contains("Z0") || first_g1.contains("Z-2"),
            "lead-in must NOT plunge to literal Z0; first G1: {first_g1}"
        );
    }

    #[test]
    fn syol_program_end_parks_at_work_zero_by_default() {
        // syol: program_end must lift Z to fast_move_z, traverse to a
        // safe XY, THEN shut off the spindle. Default (no park config)
        // = G0 X0 Y0 in WCS — the operator's reference zero, away
        // from the part for most setups.
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
        // The tail of the program must lift to Z5, traverse to (0,0),
        // THEN turn the spindle off.
        let lines: Vec<&str> = g.lines().collect();
        let m5_idx = lines.iter().position(|l| l.contains("M5")).expect("M5 expected");
        // At least one of the lines before M5 must contain X0 Y0 (the work zero).
        let parks_before_m5 = lines[..m5_idx]
            .iter()
            .any(|l| l.contains("X0") && l.contains("Y0"));
        assert!(
            parks_before_m5,
            "expected an X0 Y0 park before M5; gcode:\n{g}",
        );
    }

    #[test]
    fn syol_program_end_uses_g53_when_park_at_home() {
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
        setup.machine.park_at_home = true;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        assert!(
            g.contains("G53 G0 X0 Y0"),
            "park_at_home should emit G53 G0 X0 Y0; got:\n{g}",
        );
    }

    #[test]
    fn syol_program_end_explicit_park_xy() {
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
        setup.machine.park_xy = Some((150.0, 200.0));

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        assert!(
            g.contains("X150") && g.contains("Y200"),
            "explicit park_xy should drive the parking move; got:\n{g}",
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

    /// 20y5 / xkvv: in Laser mode, every cut block must arm the beam at
    /// S0 BEFORE the rapid traverse (so the rapid doesn't burn), ramp to
    /// `M3 S<power>` AFTER the plunge to cut Z, and OFF (M5) before the
    /// safe-Z retract / rapid out — otherwise the rapid burns a stripe
    /// through the workpiece (xkvv) and / or the program runs with the
    /// laser silently off (20y5's original bug from the `mode == Mill`-
    /// only gate).
    #[test]
    fn laser_mode_emits_m3_at_cut_entry_and_m5_before_retract() {
        let mut setup = Setup::default();
        setup.machine.mode = MachineMode::Laser;
        setup.machine.plot_mode_z = true; // typical laser config
        setup.tool.diameter = 0.0;
        setup.tool.speed = 750; // laser power
        setup.tool.rate_h = 1200;
        setup.tool.rate_v = 1200;
        setup.mill.depth = 0.0;
        setup.mill.step = 0.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::On;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);

        assert!(
            g.contains("M3 S750"),
            "laser mode must fire the beam with `M3 S<power>` at pierce time; got:\n{g}",
        );
        let lines: Vec<&str> = g.lines().collect();
        let full_power = lines
            .iter()
            .position(|l| l.contains("M3 S750"))
            .expect("M3 S750 missing");
        // First LATERAL cut: a `G1 X…` or `G1 Y…`, not the `G1 Z…` plunge.
        let first_lateral_cut = lines
            .iter()
            .position(|l| (l.starts_with("G1 X") || l.starts_with("G1 Y")))
            .expect("no G1 X/Y cut motion");
        assert!(
            full_power < first_lateral_cut,
            "`M3 S<power>` must come BEFORE the first lateral cut; power at {full_power}, cut at {first_lateral_cut}\n{g}",
        );
        // M5 must appear AFTER the last G1 cut and BEFORE program_end's park.
        // Find the last G1 cut and verify some M5 sits between it and the
        // tail of the program — that's the cut_tool_off drop.
        let m5_positions: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim() == "M5")
            .map(|(i, _)| i)
            .collect();
        assert!(
            m5_positions.len() >= 2,
            "expected at least two M5 lines (one between cuts at end-of-block, one at program_end); got {}:\n{g}",
            m5_positions.len(),
        );
    }

    /// 20y5: in Laser mode with multiple offsets, M5 must be emitted
    /// between every pair of cut blocks so the rapid traverse doesn't
    /// burn. Each subsequent cut re-arms the beam with M3 S<power>.
    #[test]
    fn laser_mode_drops_beam_between_offsets() {
        let mut setup = Setup::default();
        setup.machine.mode = MachineMode::Laser;
        setup.machine.plot_mode_z = true;
        setup.tool.diameter = 0.0;
        setup.tool.speed = 500;
        setup.tool.rate_h = 1200;
        setup.tool.rate_v = 1200;
        setup.mill.depth = 0.0;
        setup.mill.step = 0.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::On;

        let sq1 = square_offset();
        let mut sq2 = square_offset();
        for s in &mut sq2.segments {
            s.start.x += 50.0;
            s.end.x += 50.0;
        }
        sq2.source_object_idx = 1;

        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[sq1, sq2], &mut post);
        // M3 S500 must appear AT LEAST TWICE — once per cut block —
        // because each cut_tool_off clears `last_speed`, forcing the
        // next cut_tool_on to re-emit the M3 word.
        let m3_count = g
            .lines()
            .filter(|l| l.contains("M3 S500"))
            .count();
        assert!(
            m3_count >= 2,
            "expected ≥2 `M3 S500` lines (one per cut block); got {m3_count}\n{g}",
        );
        // M5 between cuts: at least one M5 in the interior of the
        // program (not just the program_end M5).
        let m5_count = g.lines().filter(|l| l.trim() == "M5").count();
        assert!(
            m5_count >= 2,
            "expected ≥2 `M5` lines (one per inter-cut transition + program_end); got {m5_count}\n{g}",
        );
    }

    /// xkvv: in Laser mode the BEAM must be at S0 during the rapid
    /// traverse to the entry point. Sequence: `M3 S0` → G0 rapid → G1
    /// plunge → `M3 S<power>` → optional pierce dwell → cut motion.
    /// Pre-fix the M3 S<power> appeared BEFORE the rapid, scorching a
    /// line across the workpiece on every cut block.
    #[test]
    fn laser_op_does_not_scorch_during_rapid() {
        let mut setup = Setup::default();
        setup.machine.mode = MachineMode::Laser;
        setup.machine.plot_mode_z = true;
        setup.tool.diameter = 0.0;
        setup.tool.speed = 800; // laser power (PWM duty)
        setup.tool.rate_h = 1200;
        setup.tool.rate_v = 1200;
        setup.tool.pierce_sec = 0.5; // arm the pierce dwell
        setup.mill.depth = 0.0;
        setup.mill.step = 0.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::On;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        let lines: Vec<&str> = g.lines().collect();

        let pos = |needle: &str| -> Option<usize> {
            lines.iter().position(|l| l.contains(needle))
        };
        let arm = pos("M3 S0")
            .unwrap_or_else(|| panic!("`M3 S0` must arm the laser BEFORE the rapid\n{g}"));
        // The G0 rapid TRAVERSE to the entry XY (not the safe-Z lift
        // that program_begin already emitted). Match on `G0 X` so we
        // don't grab the leading `G0 Z5`.
        let g0_xy = lines
            .iter()
            .position(|l| l.starts_with("G0 X"))
            .unwrap_or_else(|| panic!("missing G0 X rapid to entry\n{g}"));
        let full_power = pos("M3 S800")
            .unwrap_or_else(|| panic!("`M3 S800` must ramp up before pierce\n{g}"));
        // First LATERAL cut — skip the G1 Z plunge that follows the rapid.
        let first_lateral = lines
            .iter()
            .position(|l| l.starts_with("G1 X") || l.starts_with("G1 Y"))
            .unwrap_or_else(|| panic!("missing G1 X/Y cut motion\n{g}"));

        assert!(
            arm < g0_xy,
            "`M3 S0` (arm) must come BEFORE the rapid traverse; arm at {arm}, G0 X at {g0_xy}\n{g}",
        );
        assert!(
            full_power > g0_xy,
            "`M3 S<power>` must come AFTER the rapid traverse (S0 during travel);\
             power at {full_power}, G0 X at {g0_xy}\n{g}",
        );
        assert!(
            full_power < first_lateral,
            "`M3 S<power>` must come BEFORE the first lateral cut (pierce time);\
             power at {full_power}, lateral G1 at {first_lateral}\n{g}",
        );

        // The pierce dwell (`G4 P0.5`) must sit between the power ramp
        // and the first cut motion. Otherwise the cut starts before the
        // beam has burned through focused stock.
        let dwell = pos("G4 P0.5")
            .unwrap_or_else(|| panic!("expected pierce dwell `G4 P0.5` after power ramp\n{g}"));
        assert!(
            full_power < dwell && dwell < first_lateral,
            "pierce dwell must sit between power ramp ({full_power}) and first lateral cut ({first_lateral}); dwell at {dwell}\n{g}",
        );
    }

    /// 20y5: Drag knife / pen plotter mode must NOT emit M3 or M5 —
    /// there's no spindle or beam to control. The default (Mill)
    /// path keeps emitting M3 / M5; this test pins the Drag exclusion.
    #[test]
    fn drag_mode_emits_no_spindle_or_laser_commands() {
        let mut setup = Setup::default();
        setup.machine.mode = MachineMode::Drag;
        setup.machine.plot_mode_z = true;
        setup.tool.diameter = 0.0;
        setup.tool.speed = 0;
        setup.tool.rate_h = 800;
        setup.tool.rate_v = 800;
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
        // No M3 / M4 in the body. (program_end's spindle_off may
        // still emit M5 unconditionally — that's harmless, but we
        // want to verify no laser power was ever switched on.)
        for line in g.lines() {
            let trimmed = line.trim();
            assert!(
                !trimmed.starts_with("M3 ") && trimmed != "M3" && !trimmed.starts_with("M3S"),
                "drag mode must not emit M3 (no spindle/beam): {line}\nfull:\n{g}",
            );
            assert!(
                !trimmed.starts_with("M4 ") && trimmed != "M4" && !trimmed.starts_with("M4S"),
                "drag mode must not emit M4: {line}\nfull:\n{g}",
            );
        }
    }

    /// 3kqo: G83 / G73 R-word must be above the stock surface, NOT at
    /// `start_depth` when `start_depth` sits below the stock top. If R
    /// is below the stock surface, the canned cycle's rapid retract
    /// between pecks pulls the bit back into the chip-clogged hole
    /// instead of clearing the chips.
    #[test]
    fn drill_peck_r_word_above_stock_top_when_start_depth_negative() {
        use crate::cam::offsets::PolylineOffset;
        use crate::geometry::Segment;
        use crate::project::DrillCycle;

        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_v = 200;
        setup.tool.tip_diameter_mm = 3.0; // flat-bottom: no cone_extra
        // Proud stock or recessed-feature edge: start_depth dips BELOW
        // the stock surface (Z=0). The old code used this as R; the
        // fix clamps R to stock_top + 0.5 mm.
        setup.mill.start_depth = -1.0;
        setup.mill.depth = -5.0;
        setup.mill.fast_move_z = 10.0;
        setup.machine.comments = false;

        let pt = Point2::new(2.5, 4.5);
        let offsets = vec![PolylineOffset {
            segments: vec![Segment::point(pt, "0", 7)],
            closed: false,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        }];

        let cycle = DrillCycle::Peck {
            peck_step_mm: 1.0,
            dwell_sec: 0.0,
        };
        let mut post = linuxcnc::Post::new();
        let mut last = Point2::new(0.0, 0.0);
        // Header / footer not needed for the R-word check.
        emit_drill_block(&setup, &offsets, cycle, &mut post, &mut last);
        let g = post.finish();

        // Find the G83 line and parse its R value.
        let g83_line = g
            .lines()
            .find(|l| l.starts_with("G83 "))
            .expect("expected a G83 line");
        // R must be POSITIVE (above stock surface) — the old code
        // would have emitted `R-1` (start_depth) which retracts INTO
        // the hole.
        assert!(
            !g83_line.contains("R-"),
            "G83 R-word must be above stock top (positive Z); got: {g83_line}\nfull:\n{g}",
        );
        assert!(
            g83_line.contains("R0.5"),
            "expected R=0.5 (stock_top + 0.5 mm clearance) when start_depth = -1; got: {g83_line}",
        );
    }

    /// 3kqo: when `start_depth` sits ABOVE the stock surface (recessed
    /// work where the user explicitly raised the entry plane), R
    /// follows `start_depth` — it would be wasteful to drop R to the
    /// `stock_top` clearance because every peck rapid then has to
    /// travel further down through air to get back to the previous
    /// peck depth.
    #[test]
    fn drill_peck_r_word_follows_start_depth_when_above_stock() {
        use crate::cam::offsets::PolylineOffset;
        use crate::geometry::Segment;
        use crate::project::DrillCycle;

        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_v = 200;
        setup.tool.tip_diameter_mm = 3.0;
        setup.mill.start_depth = 2.0; // 2 mm above the stock surface
        setup.mill.depth = -5.0;
        setup.mill.fast_move_z = 10.0;
        setup.machine.comments = false;

        let pt = Point2::new(0.0, 0.0);
        let offsets = vec![PolylineOffset {
            segments: vec![Segment::point(pt, "0", 7)],
            closed: false,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        }];
        let cycle = DrillCycle::Peck {
            peck_step_mm: 1.0,
            dwell_sec: 0.0,
        };
        let mut post = linuxcnc::Post::new();
        let mut last = Point2::new(0.0, 0.0);
        emit_drill_block(&setup, &offsets, cycle, &mut post, &mut last);
        let g = post.finish();

        let g83_line = g
            .lines()
            .find(|l| l.starts_with("G83 "))
            .expect("expected a G83 line");
        assert!(
            g83_line.contains("R2"),
            "R should follow start_depth (=2) when it's above stock; got: {g83_line}",
        );
    }

    /// vfpa: lead-in plunge (G1 Z-drop from `fast_move_z` to `start_depth`)
    /// must execute at the plunge feed (`rate_v`), not the cut feed
    /// (`rate_h`). Asserts the F-word sequence at the contour entry:
    /// F<`rate_v`> → G1 Z<entry> → F<`rate_h`> → G1 X/Y (first cut).
    /// Without the fix the cutter plunges at `rate_h` (8x faster) and
    /// snaps non-center-cutting endmill tips.
    #[test]
    fn vfpa_lead_in_plunge_uses_plunge_feed_not_cut_feed() {
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_v = 100; // plunge feed
        setup.tool.rate_h = 800; // cut feed
        setup.mill.depth = -2.0;
        setup.mill.start_depth = 0.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off; // Straight lead-in
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        let lines: Vec<&str> = g.lines().collect();

        // Find: first F-line carrying rate_v (100) followed by a G1 Z move,
        // then an F-line carrying rate_h (800), then a G1 X/Y move.
        let f100_idx = lines
            .iter()
            .position(|l| l.trim() == "F100")
            .unwrap_or_else(|| panic!("expected F100 (rate_v) before lead-in plunge:\n{g}"));
        let g1_z_after_f100 = lines[f100_idx + 1..]
            .iter()
            .position(|l| l.starts_with("G1 ") && l.contains('Z') && !l.contains('X') && !l.contains('Y'))
            .unwrap_or_else(|| panic!("expected `G1 Z<entry>` right after F100:\n{g}"));
        let f800_after_plunge = lines[f100_idx + 1 + g1_z_after_f100..]
            .iter()
            .position(|l| l.trim() == "F800")
            .unwrap_or_else(|| panic!("expected F800 (rate_h restore) after plunge:\n{g}"));
        let g1_xy_after_f800 = lines[f100_idx + 1 + g1_z_after_f100 + f800_after_plunge + 1..]
            .iter()
            .position(|l| l.starts_with("G1 ") && (l.contains('X') || l.contains('Y')))
            .unwrap_or_else(|| panic!("expected `G1 X/Y` (first cut) after F800:\n{g}"));
        // Sanity: the chain succeeded; just touch g1_xy_after_f800 so the
        // compiler doesn't warn.
        let _ = g1_xy_after_f800;
    }

    /// irg7: with the vfpa lead-plunge-feed fix in place, EVERY lead
    /// arm (Arc / Straight / None) must restore F<`rate_h`> between
    /// the plunge Z-drop and the first cut motion. The Arc arm got
    /// this via the 3o3n defensive re-emit historically; the Straight
    /// and None arms relied on the modal F set further upstream
    /// matching `rate_h` — which after vfpa it no longer does (modal
    /// is `rate_v` at that point). Regression: an op with each lead
    /// kind must emit `F<rate_h>` between plunge Z and the first
    /// cutting motion. Uses a separately-closed square per arm to
    /// avoid Arc lead-fit fallback to Straight on tight geometry.
    #[test]
    fn irg7_feedrate_restored_on_all_three_lead_arms() {
        use crate::cam::offsets::PolylineOffset;
        use crate::geometry::Segment;

        // Closed 30mm square — large enough for Arc lead-in to "fit"
        // (62pd arc_lead_fits check).
        fn big_closed_square() -> PolylineOffset {
            PolylineOffset {
                segments: vec![
                    Segment::line(p(0.0, 0.0), p(30.0, 0.0), "0", 7),
                    Segment::line(p(30.0, 0.0), p(30.0, 30.0), "0", 7),
                    Segment::line(p(30.0, 30.0), p(0.0, 30.0), "0", 7),
                    Segment::line(p(0.0, 30.0), p(0.0, 0.0), "0", 7),
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

        fn check_arm(lead_kind: LeadKind, in_len: f64, arm_label: &str) {
            let mut setup = Setup::default();
            setup.tool.diameter = 3.0;
            setup.tool.speed = 12000;
            setup.tool.rate_v = 100; // distinctive plunge feed
            setup.tool.rate_h = 800; // distinctive cut feed
            setup.mill.depth = -2.0;
            setup.mill.start_depth = 0.0;
            setup.mill.step = -1.0;
            setup.mill.fast_move_z = 5.0;
            setup.leads.r#in = lead_kind;
            setup.leads.in_lenght = in_len;
            setup.leads.out = LeadKind::Off;
            setup.machine.comments = false;
            setup.mill.offset = ToolOffset::Outside;

            let mut post = linuxcnc::Post::new();
            let g = emit_polylines(&setup, &[big_closed_square()], &mut post);
            let lines: Vec<&str> = g.lines().collect();

            // For Off → LeadGeometry::None; for Arc / Straight → the
            // emitter does F100 (plunge) → G1 Z → F800 (restore) →
            // cut motion. Verify F800 appears AFTER the first F100 +
            // G1 Z descent so the lateral cut runs at rate_h.
            let f100_idx = lines
                .iter()
                .position(|l| l.trim() == "F100")
                .unwrap_or_else(|| {
                    panic!("[{arm_label}] expected F100 (rate_v) before plunge:\n{g}")
                });
            let g1_z_after_f100 = lines[f100_idx + 1..]
                .iter()
                .position(|l| l.starts_with("G1 ") && l.contains('Z'))
                .unwrap_or_else(|| panic!("[{arm_label}] expected G1 Z after F100:\n{g}"));
            let _f800_after_plunge = lines[f100_idx + 1 + g1_z_after_f100..]
                .iter()
                .position(|l| l.trim() == "F800")
                .unwrap_or_else(|| {
                    panic!(
                        "[{arm_label}] expected F800 (rate_h restore) between plunge Z and first cut:\n{g}"
                    )
                });
        }

        // All three lead arms covered. Arc: large in_lenght on a closed
        // 30mm square so arc_lead_fits succeeds. Straight: any positive
        // in_lenght. Off: in_lenght irrelevant but pass a value so the
        // setup is realistic.
        check_arm(LeadKind::Arc, 3.0, "Arc");
        check_arm(LeadKind::Straight, 3.0, "Straight");
        check_arm(LeadKind::Off, 0.0, "None");
    }

    /// o1g3: final retract after lead-out (to `fast_move_z`) must be a
    /// rapid (G0), not a cut motion (G1). The lead-out already rolled
    /// the cutter into free space; retracting at cut feed multiplies
    /// cycle time across hundreds of contours with zero safety benefit.
    #[test]
    fn o1g3_final_retract_after_leadout_is_g0_not_g1() {
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_v = 100;
        setup.tool.rate_h = 800;
        setup.mill.depth = -1.0;
        setup.mill.start_depth = 0.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 7.5;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Outside;

        let offsets = vec![square_offset()];
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        let lines: Vec<&str> = g.lines().collect();

        // Find every line that retracts to fast_move_z (Z7.5). The last
        // one is program_end's lift (G0 by convention). The earlier one
        // — emitted at the END of the cut block — must also be G0 with
        // the fix; before the fix it would be `G1 Z7.5`.
        let retracts: Vec<(usize, &&str)> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains("Z7.5") && (l.starts_with("G0 ") || l.starts_with("G1 ")))
            .collect();
        assert!(
            retracts.len() >= 2,
            "expected at least 2 retract-to-fast_move_z lines (post-cut + program_end); got: {retracts:?}\n{g}",
        );
        // EVERY retract to fast_move_z must be a G0 — no G1.
        for (i, l) in &retracts {
            assert!(
                l.starts_with("G0 "),
                "retract to fast_move_z must be G0 (rapid), not G1 (cut feed); line {i}: {l}\n{g}",
            );
        }
    }

    /// o01e: GRBL has no canned-cycle support, so a Peck drill uses the
    /// trait-default G0/G1 expansion. That default must self-anchor the
    /// plunge feed (F<`rate_v`>) at entry AND after each rapid retract
    /// so the G1 plunges land at the safe plunge feed regardless of
    /// what modal F a prior op left set.
    #[test]
    fn o01e_grbl_peck_anchors_plunge_feed_before_each_g1_plunge() {
        use crate::cam::offsets::PolylineOffset;
        use crate::geometry::Segment;
        use crate::project::DrillCycle;

        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.speed = 12000;
        setup.tool.rate_v = 75; // distinctive plunge feed
        setup.tool.rate_h = 1200; // distinctive cut feed
        setup.tool.tip_diameter_mm = 3.0;
        setup.mill.start_depth = 0.0;
        setup.mill.depth = -3.0;
        setup.mill.fast_move_z = 10.0;
        setup.machine.comments = false;

        // Three pecks: 1mm, 1mm, 1mm. With peck_step=1.0 and depth=-3,
        // we expect three G1 plunges (each preceded by an F75 anchor).
        let pt = Point2::new(0.0, 0.0);
        let offsets = vec![PolylineOffset {
            segments: vec![Segment::point(pt, "0", 7)],
            closed: false,
            level: 0,
            is_pocket: 0,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        }];
        let cycle = DrillCycle::Peck {
            peck_step_mm: 1.0,
            dwell_sec: 0.0,
        };
        let mut post = grbl::Post::new();
        let mut last = Point2::new(0.0, 0.0);
        emit_drill_block(&setup, &offsets, cycle, &mut post, &mut last);
        let g = post.finish();
        let lines: Vec<&str> = g.lines().collect();

        // Every G1 line in the GRBL drill block must be preceded by an
        // F<rate_v> within the few lines above it — never an F<rate_h>.
        let g1_indices: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with("G1 ") && l.contains('Z'))
            .map(|(i, _)| i)
            .collect();
        assert!(
            !g1_indices.is_empty(),
            "expected at least one G1 plunge in GRBL peck output:\n{g}",
        );
        for &g1_idx in &g1_indices {
            // Walk backwards from g1_idx until we hit an F-line or the
            // top of the block. That F must be F<rate_v>.
            let mut found_f: Option<&str> = None;
            for i in (0..g1_idx).rev() {
                let trimmed = lines[i].trim();
                if trimmed.starts_with('F') && trimmed[1..].chars().all(|c| c.is_ascii_digit() || c == '.') {
                    found_f = Some(trimmed);
                    break;
                }
            }
            let f = found_f.unwrap_or_else(|| {
                panic!("no F-line found before G1 plunge at line {g1_idx}:\n{g}")
            });
            assert_eq!(
                f, "F75",
                "G1 plunge at line {g1_idx} ({}) must be preceded by F75 (rate_v), not {f}\n{g}",
                lines[g1_idx],
            );
        }
    }

    /// g30a: drag-knife Line→Arc transitions must emit a swivel arc
    /// BEFORE the cut arc — otherwise the trailing blade enters the
    /// arc still aligned with the prior line direction, bending the
    /// blade and tearing material at every line→arc seam. Build a
    /// closed shape with a Line→Arc→Line→Line sequence and assert the
    /// gcode contains a swivel arc just before the cut arc.
    #[test]
    fn g30a_dragoff_emits_swivel_before_arc_on_line_to_arc_transition() {
        use crate::cam::offsets::PolylineOffset;
        use crate::geometry::Segment;

        let mut setup = Setup::default();
        setup.tool.diameter = 0.0;
        setup.tool.speed = 0;
        setup.tool.rate_h = 800;
        setup.tool.rate_v = 800;
        setup.tool.dragoff = Some(0.5);
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::On;

        // Build a Line→Arc transition where the directions actually
        // change at the seam. A tangent-arc (line ends in +X, arc start
        // tangent is also +X) does NOT need a swivel — the bug bites
        // only when the arc's start tangent differs from the prior
        // motion's exit direction. Here the line ends pointing +X
        // (toward (15,0)) and the arc starts at (15,0) sweeping CCW
        // around (10,0) up to (10,5): start radius (5,0), CCW start
        // tangent = rotate +90° = (0,5) = +Y. So the corner is a sharp
        // 90° turn (+X → +Y) and the swivel must emit before the arc.
        // bulge for a 90° CCW arc = tan(sweep/4) = tan(22.5°) ≈ 0.4142.
        let segs = vec![
            Segment::line(p(0.0, 0.0), p(15.0, 0.0), "0", 7),
            Segment::arc(
                p(15.0, 0.0),
                p(10.0, 5.0),
                (std::f64::consts::FRAC_PI_8).tan(),
                Some(p(10.0, 0.0)),
                "0",
                7,
            ),
            Segment::line(p(10.0, 5.0), p(0.0, 5.0), "0", 7),
            Segment::line(p(0.0, 5.0), p(0.0, 0.0), "0", 7),
        ];
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
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[offset], &mut post);

        // Locate the cut arc: quarter-CCW from (15,0) to (10,5) — its
        // line will be `G3 X10 Y5 I-5 J0` (I/J relative to start). The
        // swivel arc emitted by the fix sits just before it.
        let lines: Vec<&str> = g.lines().collect();
        let cut_arc_idx = lines
            .iter()
            .position(|l| {
                (l.starts_with("G2 ") || l.starts_with("G3 "))
                    && l.contains("X10")
                    && l.contains("Y5")
                    && l.contains('I')
            })
            .unwrap_or_else(|| panic!("expected the quarter-CCW cut arc (G3 X10 Y5 ...) in output:\n{g}"));

        // The swivel must come BEFORE the cut arc — search the prior
        // few lines (the corner sequence is at most ~4 lines deep:
        // post-line linear, swivel pre-step linear, swivel arc, then
        // the cut arc).
        let preceding_lines = &lines[cut_arc_idx.saturating_sub(6)..cut_arc_idx];
        let swivel_present = preceding_lines
            .iter()
            .any(|l| (l.starts_with("G2 ") || l.starts_with("G3 ")) && l.contains('I'));
        assert!(
            swivel_present,
            "expected a swivel arc (G2/G3 with I/J) BEFORE the cut arc at line {cut_arc_idx} ({}); preceding lines: {preceding_lines:?}\n{g}",
            lines[cut_arc_idx],
        );
    }

    #[test]
    fn i6c2_post_helix_entry_lift_uses_g0_rapid() {
        // i6c2: the lift to fast_move_z that happens AFTER emit_helix_entry
        // (and before the rapid XY to the contour start + the rate_v plunge)
        // must be a G0 rapid, not a G1 cut-feed move. The helix entry has
        // already cleared the spiral disc; the lift travels through air on
        // its way to the rapid. The prior G1 ran the lift at rate_h on
        // every helix pass — pure cycle-time burn.
        //
        // Pair with the existing `gcode_helix_walk_to_start_uses_safe_feed`
        // test which already checks the rapid-to-start step; here we
        // assert the IMMEDIATE next line after the helix arcs is a G0 Z
        // (the lift), not a G1 Z.
        use crate::cam::setup::PlungeStrategy;
        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.tool.rate_h = 800;
        setup.tool.rate_v = 100;
        setup.mill.depth = -2.0;
        setup.mill.step = -2.0;
        setup.mill.fast_move_z = 5.0;
        setup.mill.plunge = PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(2.0),
        };
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::Inside;
        let sq = PolylineOffset {
            segments: vec![
                Segment::line(p(2.0, 2.0), p(28.0, 2.0), "0", 7),
                Segment::line(p(28.0, 2.0), p(28.0, 28.0), "0", 7),
                Segment::line(p(28.0, 28.0), p(2.0, 28.0), "0", 7),
                Segment::line(p(2.0, 28.0), p(2.0, 2.0), "0", 7),
            ],
            closed: true,
            level: 0,
            is_pocket: 1,
            layer: "0".into(),
            color: 7,
            source_object_idx: 0,
            tabs: Vec::new(),
            is_finish: false,
        };
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[sq], &mut post);
        let lines: Vec<&str> = g.lines().collect();
        // Find the LAST helix arc line.
        let last_arc_idx = lines
            .iter()
            .rposition(|l| l.starts_with("G2 ") || l.starts_with("G3 "))
            .unwrap_or_else(|| panic!("expected a helix arc (G2/G3) in output:\n{g}"));
        // The immediate next motion line must be a G0 Z (the lift), not G1.
        let next_motion = lines[last_arc_idx + 1..]
            .iter()
            .find(|l| l.starts_with("G0") || l.starts_with("G1"))
            .unwrap_or_else(|| {
                panic!("expected a motion line after the last helix arc:\n{g}")
            });
        assert!(
            next_motion.starts_with("G0") && next_motion.contains('Z'),
            "post-helix lift must be G0 Z (rapid), got: {next_motion}\nfull gcode:\n{g}",
        );
    }

    #[test]
    fn nj6_feedrate_zero_skipped_in_post() {
        // 4nj6: a tool with rate_v=0 or rate_h=0 (default-constructed,
        // misconfigured laser-on-mill, or a regression that lets a zero
        // slip past pipeline validation) must NEVER emit `F0` to the
        // controller. LinuxCNC raises "negative or zero feed rate" and
        // halts; GRBL returns error:11. The post is the defense of last
        // resort: drop the F line, leaving the modal at its prior value.
        let mut post = linuxcnc::Post::new();
        post.feedrate(0);
        let g = post.finish();
        assert!(
            !g.lines().any(|l| l.trim() == "F0"),
            "post must skip F0 (controllers reject it); got:\n{g}",
        );
        // Sanity: a non-zero feed still emits.
        let mut post2 = linuxcnc::Post::new();
        post2.feedrate(500);
        let g2 = post2.finish();
        assert!(
            g2.lines().any(|l| l.trim() == "F500"),
            "non-zero feed should emit F<rate>; got:\n{g2}",
        );
    }

    #[test]
    fn pxyt_trait_default_drill_honors_ms_dwell_unit() {
        // pxyt: GRBL inherits the default trait drill_simple / drill_peck /
        // drill_chip_break impls (it has no canned cycle support). Those
        // defaults previously emitted `G4 P<seconds>` via a seconds-only
        // helper, ignoring the active profile's dwell_unit. A Mach3-metric
        // profile (DwellUnit::Milliseconds) running on GRBL would emit
        // `G4 P0.5` for an intended 500 ms dwell — a 1000x mismatch.
        //
        // After the fix, the trait routes through `fmt_dwell_post`, which
        // GRBL delegates into LinuxCNC's `fmt_dwell_p` — that consults
        // PostState.profile.dwell_unit and scales seconds → ms when asked.
        use crate::gcode::post_profile::{DwellUnit, PostProfile};
        let mut profile = PostProfile::grbl_default();
        profile.dwell_unit = Some(DwellUnit::Milliseconds);
        let mut post = grbl::Post::new();
        post.set_post_profile(Some(&profile));
        // Run a default-trait drill_simple; the 0.5 s dwell must render
        // as a milliseconds integer (500), not seconds (0.5).
        post.drill_simple(0.0, 0.0, -2.0, 1.0, 100, 0.5);
        let g = post.finish();
        assert!(
            g.lines().any(|l| l.trim() == "G4 P500"),
            "GRBL with ms profile should emit `G4 P500` for 0.5 s dwell; got:\n{g}",
        );
        assert!(
            !g.lines().any(|l| l.trim() == "G4 P0.5"),
            "GRBL with ms profile must NOT emit `G4 P0.5` (seconds); got:\n{g}",
        );
    }

    #[test]
    fn pxyt_trait_default_drill_seconds_unchanged_without_profile() {
        // pxyt regression-guard: the LinuxCNC default (no profile, or
        // DwellUnit::Seconds) must still emit `G4 P<seconds>` exactly
        // as before — the fix is profile-driven, not blanket.
        let mut post = grbl::Post::new();
        post.drill_simple(0.0, 0.0, -2.0, 1.0, 100, 0.5);
        let g = post.finish();
        assert!(
            g.lines().any(|l| l.trim() == "G4 P0.5"),
            "GRBL without ms profile must keep emitting `G4 P0.5`; got:\n{g}",
        );
    }

    #[test]
    fn e2mq_program_begin_emits_explicit_g54_by_default() {
        // e2mq: the program prologue must emit an explicit `G54`
        // (the default WCS) so the controller isn't left modally on
        // a stale G55..G59 from a prior program.
        let setup = Setup::default(); // wcs defaults to G54
        let mut post = linuxcnc::Post::new();
        let _g = emit_polylines(&setup, &[], &mut post);
        let g = post.finish();
        assert!(
            g.lines().any(|l| l.trim() == "G54"),
            "program_begin must emit explicit G54 by default; got:\n{g}",
        );
    }

    #[test]
    fn e2mq_program_begin_emits_active_wcs_when_set() {
        // e2mq: when the project pins `work_offset.wcs = G55`, the
        // prologue must emit `G55` (NOT G54) so the controller is
        // pinned to the same table the user authored against.
        let mut setup = Setup::default();
        setup.wcs = crate::project::Wcs::G55;
        let mut post = linuxcnc::Post::new();
        let _g = emit_polylines(&setup, &[], &mut post);
        let g = post.finish();
        assert!(
            g.lines().any(|l| l.trim() == "G55"),
            "program_begin must emit explicit G55 when Setup.wcs = G55; got:\n{g}",
        );
        assert!(
            !g.lines().any(|l| l.trim() == "G54"),
            "must NOT emit G54 when G55 is active; got:\n{g}",
        );
    }

    #[test]
    fn e2mq_grbl_tool_z_shift_targets_active_wcs_p_number() {
        // e2mq: GRBL's tool_z_shift emits `G10 L20 P<n> Z<shift>`. The
        // `P<n>` must match the active WCS (G54=P1, G55=P2, …, G59=P6),
        // NOT a hardcoded P1. Pre-fix: a user running on G55 saw the
        // z-shift written into G54's table — silent, no error, but the
        // cuts landed at the wrong depth.
        //
        // Drive via select_wcs (the path program_begin uses) so the
        // PostState.wcs is pinned identically to the live pipeline.
        let mut post = grbl::Post::new();
        post.select_wcs(crate::project::Wcs::G55);
        post.tool_z_shift(1.5);
        let g = post.finish();
        assert!(
            g.lines().any(|l| l.contains("G10 L20 P2 Z1.5")),
            "GRBL tool_z_shift on G55 must emit `G10 L20 P2 Z1.5`; got:\n{g}",
        );
        assert!(
            !g.lines().any(|l| l.contains("G10 L20 P1")),
            "must NOT write into G54 (P1) when active WCS is G55; got:\n{g}",
        );
        // Sanity: G59 → P6
        let mut post6 = grbl::Post::new();
        post6.select_wcs(crate::project::Wcs::G59);
        post6.tool_z_shift(2.0);
        let g6 = post6.finish();
        assert!(
            g6.lines().any(|l| l.contains("G10 L20 P6 Z2")),
            "GRBL tool_z_shift on G59 must emit `G10 L20 P6 Z2...`; got:\n{g6}",
        );
    }

    /// 0t9o: drag-knife self-alignment threshold suppresses swivel
    /// arcs at shallow corners. A polyline approximating a circle as
    /// 64 chords has ~5.6° turns at each corner — well below the
    /// 30° default. The walker must NOT emit a swivel arc at each
    /// chord; the trailing offset self-aligns the blade.
    ///
    /// Build a polyline with two adjacent line segments whose
    /// included turn is ~10° (below threshold). Assert that the
    /// walker emits the second linear move directly — no intervening
    /// swivel arc and no perpendicular pre-step linear.
    #[test]
    fn dragoff_skips_swivel_below_self_align_threshold() {
        use crate::cam::offsets::PolylineOffset;
        use crate::geometry::Segment;

        let mut setup = Setup::default();
        setup.tool.diameter = 0.0;
        setup.tool.speed = 0;
        setup.tool.rate_h = 800;
        setup.tool.rate_v = 800;
        setup.tool.dragoff = Some(0.5);
        setup.tool.drag_self_align_angle_rad = 30.0_f64.to_radians();
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::On;

        let segs = vec![
            Segment::line(p(0.0, 0.0), p(10.0, 0.0), "0", 7),
            Segment::line(p(10.0, 0.0), p(20.0, 1.76), "0", 7),
            Segment::line(p(20.0, 1.76), p(20.0, 5.0), "0", 7),
            Segment::line(p(20.0, 5.0), p(0.0, 5.0), "0", 7),
            Segment::line(p(0.0, 5.0), p(0.0, 0.0), "0", 7),
        ];
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
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[offset], &mut post);
        let lines: Vec<&str> = g.lines().collect();
        // After the first line lands at X10 Y0, the IMMEDIATE next
        // motion-line MUST be the second line's endpoint (X20 Y1.76)
        // — no swivel arc + perpendicular pre-step inserted between
        // them for the shallow ~10° kink.
        let first_idx = lines
            .iter()
            .position(|l| l.starts_with("G1 ") && l.contains("X10") && !l.contains("Y1"))
            .unwrap_or_else(|| panic!("expected G1 to X10 Y0 (first line endpoint):\n{g}"));
        // The immediate next motion-emitting line (skipping any
        // pure-comment / empty lines) must be the next line segment's
        // endpoint, NOT a swivel pre-step or arc.
        let next_motion = lines
            .iter()
            .skip(first_idx + 1)
            .find(|l| {
                let t = l.trim_start();
                t.starts_with('G') && !t.starts_with("G4")
            })
            .copied()
            .unwrap_or("");
        assert!(
            next_motion.starts_with("G1 ") && next_motion.contains("X20") && next_motion.contains("Y1.76"),
            "0t9o: expected immediate next motion = G1 X20 Y1.76 (no swivel inserted for ~10° corner); got '{next_motion}' in:\n{g}",
        );
    }

    /// zpuk: Plasma mode emits a two-step Z entry — rapid to
    /// `pierce_height`, dwell `pierce_delay_sec`, then G1 to `cut_height`.
    /// The cut proceeds at constant Z = `cut_height` (`multi_pass`
    /// collapses for Plasma the same way it collapses for Drag).
    #[test]
    fn plasma_mode_emits_pierce_then_cut_height_sequence() {
        use crate::cam::offsets::PolylineOffset;
        use crate::geometry::Segment;

        let mut setup = Setup::default();
        setup.machine.mode = MachineMode::Plasma;
        setup.machine.comments = false;
        setup.tool.diameter = 0.0;
        setup.tool.speed = 100;
        setup.tool.rate_h = 800;
        setup.tool.rate_v = 800;
        setup.tool.pierce_height_mm = 4.0;
        setup.tool.cut_height_mm = 1.5;
        setup.tool.pierce_delay_sec = 0.5;
        setup.mill.depth = -1.0; // irrelevant for plasma — cut Z = cut_height
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 10.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.mill.offset = ToolOffset::On;

        let segs = vec![
            Segment::line(p(0.0, 0.0), p(10.0, 0.0), "0", 7),
            Segment::line(p(10.0, 0.0), p(10.0, 10.0), "0", 7),
            Segment::line(p(10.0, 10.0), p(0.0, 10.0), "0", 7),
            Segment::line(p(0.0, 10.0), p(0.0, 0.0), "0", 7),
        ];
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
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[offset], &mut post);
        let lines: Vec<&str> = g.lines().collect();
        // Entry should rapid to Z4 (pierce height), dwell 0.5s, then
        // G1 to Z1.5 (cut height). Find a G0 line carrying Z4.
        let pierce_idx = lines
            .iter()
            .position(|l| l.starts_with("G0 ") && l.contains("Z4"))
            .unwrap_or_else(|| panic!("zpuk: expected G0 to Z4 (pierce_height) in:\n{g}"));
        // Dwell follows.
        let dwell = lines.get(pierce_idx + 1).copied().unwrap_or("");
        assert!(
            dwell.starts_with("G4 ") && dwell.contains("P0.5"),
            "zpuk: expected G4 P0.5 dwell after pierce-height rapid; got '{dwell}' in:\n{g}",
        );
        // Then G1 to Z1.5.
        let cut_drop = lines
            .iter()
            .skip(pierce_idx)
            .find(|l| l.starts_with("G1 ") && l.contains("Z1.5"));
        assert!(
            cut_drop.is_some(),
            "zpuk: expected G1 Z1.5 (cut_height) after pierce dwell in:\n{g}",
        );
        // No cut moves at the main depth (Z=-1) — plasma collapses
        // to one pass at cut_height. NO Z-negative G1 should appear.
        for line in &lines {
            assert!(!(line.starts_with("G1 ") && line.contains("Z-")), 
                    "zpuk: plasma must not descend below stock top; got: {line}\n{g}"
                );
        }
        // Torch on emit (laser_on path) — `M3 S100`.
        assert!(
            g.contains("M3 S100") || g.contains("M3 S 100"),
            "zpuk: expected torch-on (M3 S<power>) in plasma output:\n{g}",
        );
    }

    /// 6yhs: Drag-knife mode (machine.mode = Drag) must collapse to
    /// a single pass at `setup.mill.depth` even without the global
    /// `plot_mode_z` flag. `setup_resolver` sets mode=Drag per-op for
    /// `DragKnife` ops; before the fix, `multi_pass` walked the schedule
    /// N times at incrementally negative Z (knife wear + Z-axis wear
    /// + 3x cycle time).
    ///
    /// Build a multi-pass schedule (`step = -0.5`, `depth = -1.5`)
    /// and assert the output contains exactly ONE distinct Z=
    /// negative line (= the cut Z), NOT three.
    #[test]
    fn drag_mode_collapses_multi_pass_to_one_z() {
        use crate::cam::offsets::PolylineOffset;
        use crate::geometry::Segment;

        let mut setup = Setup::default();
        setup.machine.mode = MachineMode::Drag;
        setup.machine.comments = false;
        setup.tool.diameter = 0.0;
        setup.tool.speed = 0;
        setup.tool.rate_h = 800;
        setup.tool.rate_v = 800;
        // depth = -1.5, step = -0.5 → 3 pass schedule in Mill mode.
        // Drag mode must collapse to one.
        setup.mill.depth = -1.5;
        setup.mill.step = -0.5;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.mill.offset = ToolOffset::On;

        let segs = vec![
            Segment::line(p(0.0, 0.0), p(10.0, 0.0), "0", 7),
            Segment::line(p(10.0, 0.0), p(10.0, 10.0), "0", 7),
            Segment::line(p(10.0, 10.0), p(0.0, 10.0), "0", 7),
            Segment::line(p(0.0, 10.0), p(0.0, 0.0), "0", 7),
        ];
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
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[offset], &mut post);
        // Distinct negative-Z values emitted in the body.
        let mut neg_z_values: std::collections::HashSet<String> = std::collections::HashSet::new();
        for line in g.lines() {
            for tok in line.split_whitespace() {
                if let Some(rest) = tok.strip_prefix('Z') {
                    if let Ok(z) = rest.parse::<f64>() {
                        if z < 0.0 {
                            neg_z_values.insert(format!("{z:.4}"));
                        }
                    }
                }
            }
        }
        assert_eq!(
            neg_z_values.len(),
            1,
            "6yhs: Drag mode must collapse to a single cut Z (got {neg_z_values:?}); gcode:\n{g}"
        );
        // Only Z value should be -1.5 (the configured depth).
        assert!(
            neg_z_values.contains("-1.5000"),
            "6yhs: expected single Z = -1.5 in Drag mode; got {neg_z_values:?}\n{g}"
        );
    }

    /// 0t9o: sanity that a SHARP corner (90°, above threshold) still
    /// emits the swivel — regression guard so we don't accidentally
    /// kill the g30a swivel on legitimately-sharp polyline corners.
    /// Setting `drag_self_align_angle_rad = 0.0` forces legacy
    /// behaviour (every corner swivels).
    #[test]
    fn dragoff_force_legacy_behaviour_with_zero_threshold() {
        use crate::cam::offsets::PolylineOffset;
        use crate::geometry::Segment;

        let mut setup = Setup::default();
        setup.tool.diameter = 0.0;
        setup.tool.speed = 0;
        setup.tool.rate_h = 800;
        setup.tool.rate_v = 800;
        setup.tool.dragoff = Some(0.5);
        setup.tool.drag_self_align_angle_rad = 0.0; // legacy: swivel every corner
        setup.mill.depth = -1.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.leads.r#in = LeadKind::Off;
        setup.leads.out = LeadKind::Off;
        setup.machine.comments = false;
        setup.mill.offset = ToolOffset::On;

        // Same shallow 10° corner as above; with threshold=0, the
        // swivel MUST emit at this corner (regression: doesn't matter
        // that the corner is shallow, threshold suppresses nothing).
        let segs = vec![
            Segment::line(p(0.0, 0.0), p(10.0, 0.0), "0", 7),
            Segment::line(p(10.0, 0.0), p(20.0, 1.76), "0", 7),
            Segment::line(p(20.0, 1.76), p(20.0, 5.0), "0", 7),
            Segment::line(p(20.0, 5.0), p(0.0, 5.0), "0", 7),
            Segment::line(p(0.0, 5.0), p(0.0, 0.0), "0", 7),
        ];
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
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &[offset], &mut post);
        // Confirm that with zero threshold at least one swivel arc
        // (G2/G3 with I/J) is present in the output.
        let any_swivel = g
            .lines()
            .any(|l| (l.starts_with("G2 ") || l.starts_with("G3 ")) && l.contains('I'));
        assert!(
            any_swivel,
            "0t9o legacy: with threshold=0 the swivel must still fire on the 10° corner; got:\n{g}",
        );
    }

    /// md0m: V-Carve emit must honor `pierce_sec` — laser-mode V-carve
    /// needs to dwell at the cut plane so the beam burns through the
    /// stock before lateral motion begins. The bug was that
    /// `emit_vcarve_block` plunged then immediately started cutting,
    /// dragging the first few mm of each sub-polyline through unmelted
    /// material. Mirror the gd2x ordering used by `emit_offset`.
    /// Asserts a `G4 P<pierce_sec>` appears between the plunge G1 Z
    /// (to `start_depth`) and the first lateral G1 motion.
    #[test]
    fn md0m_vcarve_emit_dwells_pierce_sec_after_plunge() {
        let mut setup = Setup::default();
        setup.tool.diameter = 1.0;
        setup.tool.tip_diameter_mm = 0.0;
        setup.tool.speed = 0;
        setup.tool.rate_v = 100;
        setup.tool.rate_h = 800;
        setup.tool.pierce_sec = 0.7;
        setup.mill.depth = -1.0;
        setup.mill.start_depth = 0.0;
        setup.mill.step = -1.0;
        setup.mill.fast_move_z = 5.0;
        setup.machine.comments = false;

        // Two-polyline V-carve so we also verify the dwell fires on
        // EACH sub-polyline's entry, not just the first. Each polyline
        // is a short cut at constant Z = -0.5.
        let polylines = vec![
            vec![(0.0, 0.0, -0.5), (5.0, 0.0, -0.5)],
            vec![(10.0, 0.0, -0.5), (15.0, 0.0, -0.5)],
        ];
        let mut post = linuxcnc::Post::new();
        let mut last_pos = Point2::new(0.0, 0.0);
        emit_vcarve_block(&setup, &polylines, &mut post, &mut last_pos);
        let g = post.finish();
        // Two pierce dwells (one per sub-polyline). `G4 P0.7` is the
        // LinuxCNC dwell form.
        let dwell_count = g.lines().filter(|l| l.trim_start().starts_with("G4 P0.7")).count();
        assert_eq!(
            dwell_count, 2,
            "md0m: expected one `G4 P0.7` per V-carve sub-polyline (got {dwell_count}); pierce dwell missing — laser drags through unmelted stock:\n{g}",
        );
        // Ordering: each dwell must follow a plunge G1 Z to start_depth
        // (Z0 here) and precede the first lateral motion.
        let lines: Vec<&str> = g.lines().collect();
        let dwell_idxs: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim_start().starts_with("G4 P0.7"))
            .map(|(i, _)| i)
            .collect();
        for &di in &dwell_idxs {
            // Find a preceding G1 Z (plunge to start_depth) — it should
            // appear somewhere before the dwell in the same poly block.
            let plunge_before = lines[..di]
                .iter()
                .rev()
                .take(10)
                .any(|l| l.starts_with("G1 ") && l.contains('Z'));
            assert!(
                plunge_before,
                "md0m: dwell at line {di} not preceded by plunge G1 Z within 10 lines:\n{g}",
            );
        }
    }
}
