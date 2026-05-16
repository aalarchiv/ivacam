//! Gcode generation — port of viaConstructor's `machine_cmd.py` and the
//! three output plugins (`gcode_grbl`, `gcode_linuxcnc`, hpgl).
//!
//! `PostProcessor` is the trait every dialect implements; `emit_polylines`
//! is the dialect-agnostic orchestrator that walks offsets and writes
//! gcode through the trait.

use serde::{Deserialize, Serialize};

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::{LeadKind, MachineMode, Setup, ToolOffset, UnitSystem};
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::math;

pub mod arc_fit;
pub mod grbl;
pub mod hpgl;
pub mod linuxcnc;
pub mod post_profile;
pub mod preview;

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
/// Used by the pipeline's per-op driver when `OperationKind::Drill`.
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
    let z = setup.mill.depth;
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

/// Decide the cut order for the offsets. Honors `setup.mill.objectorder`:
/// - `Unordered` — input order, matches the upstream Python tool.
/// - `Nearest` — greedy nearest-neighbor from current pen position; ties
///   broken by deepest level (innermost) first so pocket cascades unwind
///   from the inside out.
/// - `PerObject` — group all offsets sharing `source_object_idx`, finish
///   one object before starting the next; within a group use Nearest.
fn order_offsets(setup: &Setup, offsets: &[PolylineOffset], start: Point2) -> Vec<usize> {
    use crate::cam::setup::ObjectOrder;
    let n = offsets.len();
    if n == 0 {
        return Vec::new();
    }
    match setup.mill.objectorder {
        ObjectOrder::Unordered => (0..n).collect(),
        ObjectOrder::Nearest => greedy_nearest(offsets, start),
        ObjectOrder::PerObject => {
            // Group by source_object_idx (preserving first-seen order),
            // run nearest-neighbor inside each group seeded at the
            // previous group's end.
            let mut groups: Vec<Vec<usize>> = Vec::new();
            let mut group_of: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::default();
            for (i, o) in offsets.iter().enumerate() {
                let g = *group_of.entry(o.source_object_idx).or_insert_with(|| {
                    groups.push(Vec::new());
                    groups.len() - 1
                });
                groups[g].push(i);
            }
            let mut out = Vec::with_capacity(n);
            let mut pen = start;
            for group in groups {
                let group_offsets: Vec<&PolylineOffset> =
                    group.iter().map(|&i| &offsets[i]).collect();
                let local = greedy_nearest_among(&group_offsets, pen);
                for li in local {
                    let global = group[li];
                    out.push(global);
                    pen = end_pos(&offsets[global]);
                }
            }
            out
        }
    }
}

fn greedy_nearest(offsets: &[PolylineOffset], start: Point2) -> Vec<usize> {
    let refs: Vec<&PolylineOffset> = offsets.iter().collect();
    greedy_nearest_among(&refs, start)
}

fn greedy_nearest_among(offsets: &[&PolylineOffset], start: Point2) -> Vec<usize> {
    let n = offsets.len();
    if n == 0 {
        return Vec::new();
    }
    let mut taken = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut pen = start;
    for _ in 0..n {
        let mut best: Option<(usize, f64, u32, bool)> = None;
        for (i, o) in offsets.iter().enumerate() {
            if taken[i] {
                continue;
            }
            let d = pen.distance(start_pos_of(o));
            // Tie-breakers (in order):
            //   1. closer distance wins,
            //   2. deeper level wins (innermost ring first — pocket
            //      cascades unwind inside-out),
            //   3. non-finish before finish (rt1.24 — the dedicated
            //      finish-wall ring runs LAST so surface quality
            //      isn't degraded by re-traversing it).
            let level = o.level;
            let is_finish = o.is_finish;
            let better = match best {
                None => true,
                Some((_, bd, bl, bf)) => {
                    // Distance tiebreaker: only fall through to level/index
                    // ordering when the squared distances are within tool
                    // tolerance, since two computed f64 distances rarely
                    // coincide bit-for-bit even at the same nominal point.
                    if (d - bd).abs() > 1e-12 {
                        d < bd
                    } else if level != bl {
                        level > bl
                    } else {
                        !is_finish && bf
                    }
                }
            };
            if better {
                best = Some((i, d, level, is_finish));
            }
        }
        let (chosen, _, _, _) = best.unwrap();
        taken[chosen] = true;
        order.push(chosen);
        pen = end_pos(offsets[chosen]);
    }
    order
}

fn start_pos_of(offset: &PolylineOffset) -> Point2 {
    offset
        .segments
        .first()
        .map_or(Point2::new(0.0, 0.0), |s| s.start)
}

fn end_pos(offset: &PolylineOffset) -> Point2 {
    offset
        .segments
        .last()
        .map_or(Point2::new(0.0, 0.0), |s| s.end)
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
    ctx.tool_name = setup.tool.name.clone();
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
        emit_path_with_corner_feed(
            &fitted,
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
            emit_path_with_corner_feed(
                &fitted,
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
                emit_path_with_corner_feed(
                    &fitted,
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
                    post,
                );
            } else {
                let dragoff = setup.tool.dragoff.unwrap_or(0.0);
                let fitted = fit_line_runs(segments, setup);
                emit_path_with_corner_feed(
                    &fitted,
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
        && !(setup.tabs.active && !tabs.is_empty())
        && total_path_len > 1e-6;
    if needs_ramp_cleanup {
        post.feedrate(rate_h);
        let dragoff = setup.tool.dragoff.unwrap_or(0.0);
        let fitted = fit_line_runs(segments, setup);
        emit_path_with_corner_feed(
            &fitted,
            dragoff,
            rate_h,
            setup.mill.corner_feed_reduction,
            post,
        );
    }
}

/// Walk `segments` while linearly descending Z from `from_z` to `to_z`
/// over the first `ramp_length` of arc length, then continue at `to_z`
/// for the remainder.
///
/// Line segments are *split* when they cross the `ramp_length` boundary
/// so the ramp angle is honored even if the first segment is longer
/// than `ramp_length`. Arc segments aren't split mid-arc (the math gets
/// fiddly); the ramp simply finishes at the first arc boundary that
/// crosses `ramp_length` and the rest of the path proceeds at `to_z`.
fn emit_ramp_pass<P: PostProcessor>(
    segments: &[Segment],
    from_z: f64,
    to_z: f64,
    ramp_length: f64,
    post: &mut P,
) {
    if ramp_length < 1e-9 {
        post.linear(None, None, Some(to_z));
        return;
    }
    let mut consumed = 0.0;
    let interp_z = |consumed: f64| -> f64 {
        let t = (consumed / ramp_length).min(1.0);
        from_z + (to_z - from_z) * t
    };
    for seg in segments {
        let seg_len = match seg.kind {
            SegmentKind::Line | SegmentKind::Point => seg.start.distance(seg.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(seg),
        };
        // Split this segment at ramp_length boundary if it's a line
        // and it crosses the boundary.
        let crosses_boundary = consumed < ramp_length
            && consumed + seg_len > ramp_length
            && matches!(seg.kind, SegmentKind::Line);
        if crosses_boundary {
            let remaining_ramp = ramp_length - consumed;
            let frac = remaining_ramp / seg_len;
            let mid_x = seg.start.x + (seg.end.x - seg.start.x) * frac;
            let mid_y = seg.start.y + (seg.end.y - seg.start.y) * frac;
            // Emit the ramp portion at to_z (we just arrived at depth)
            // then continue to the segment end at to_z.
            post.linear(Some(mid_x), Some(mid_y), Some(to_z));
            post.linear(Some(seg.end.x), Some(seg.end.y), Some(to_z));
            consumed += seg_len;
            continue;
        }
        consumed += seg_len;
        let z = interp_z(consumed);
        match seg.kind {
            SegmentKind::Line => post.linear(Some(seg.end.x), Some(seg.end.y), Some(z)),
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), Some(z)),
            SegmentKind::Arc | SegmentKind::Circle => {
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if seg.bulge > 0.0 {
                    post.arc_ccw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                } else {
                    post.arc_cw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                }
            }
        }
    }
}

fn is_closed_path(segments: &[Segment]) -> bool {
    if segments.len() < 3 {
        return false;
    }
    let first = segments.first().unwrap().start;
    let last = segments.last().unwrap().end;
    first.distance(last) < 1e-3
}

/// Emit one revolution around `segments` while linearly descending Z from
/// `from_z` to `to_z`. Each segment endpoint gets the interpolated Z so
/// the spiral stays smooth even with arc segments.
fn emit_helix_pass<P: PostProcessor>(segments: &[Segment], from_z: f64, to_z: f64, post: &mut P) {
    let total_len: f64 = segments
        .iter()
        .map(|s| match s.kind {
            SegmentKind::Line | SegmentKind::Point => s.start.distance(s.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(s),
        })
        .sum();
    if total_len < 1e-9 {
        post.linear(None, None, Some(to_z));
        return;
    }
    let mut consumed = 0.0;
    for seg in segments {
        let seg_len = match seg.kind {
            SegmentKind::Line | SegmentKind::Point => seg.start.distance(seg.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(seg),
        };
        consumed += seg_len;
        let t = consumed / total_len;
        let z = from_z + (to_z - from_z) * t;
        match seg.kind {
            SegmentKind::Line => post.linear(Some(seg.end.x), Some(seg.end.y), Some(z)),
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), Some(z)),
            SegmentKind::Arc | SegmentKind::Circle => {
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if seg.bulge > 0.0 {
                    post.arc_ccw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                } else {
                    post.arc_cw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                }
            }
        }
    }
}

/// Plan for a start-of-cut helical entry: where to drop, how far
/// horizontally, how deep per revolution. Produced by
/// `plan_helix_entry` and consumed by `emit_helix_entry`.
#[derive(Debug, Clone, Copy)]
struct HelixEntry {
    /// XY center of the helix circle.
    center: Point2,
    /// Helix radius in mm.
    radius: f64,
    /// Z drop per full revolution (always positive).
    dz_per_rev: f64,
    /// True if the helix winds CCW around `center` when viewed from +Z.
    /// Matches the polygon winding so the cutter spirals "into" the
    /// material in the same direction the path will run.
    ccw: bool,
    /// Starting angle of the helix on the circle (radians, atan2 of
    /// (`path_start` - center)). Helix returns to this angle at landing
    /// so the post-helix walk to `path_start` is the shortest.
    start_angle: f64,
}

/// Build a helix entry plan for `segments` if the geometry supports it.
/// Returns None when:
///   - radius < `tool_radius` (helix would carve nothing the cutter
///     doesn't already cover from the path)
///   - the helix circle doesn't fit inside the polygon (any of 8
///     sample points lies outside the boundary)
///   - the path is too short / not closed (caller already checks
///     closed; this is defensive)
///
/// The helix center is the polygon centroid offset back toward the
/// path start so the cutter lands near where the cut begins (and the
/// post-helix walk to path-start is short). The helix circle must fit
/// entirely inside the polygon — otherwise the spiral would carve into
/// the wall on its way down.
fn plan_helix_entry(
    segments: &[Segment],
    radius_mm: f64,
    tool_radius: f64,
    angle_deg: f64,
) -> Option<HelixEntry> {
    if segments.is_empty() {
        return None;
    }
    if radius_mm < tool_radius - 1e-9 {
        return None;
    }
    let radius = radius_mm.max(1e-6);
    let angle = angle_deg.clamp(0.5, 45.0).to_radians();
    let dz_per_rev = (2.0 * std::f64::consts::PI * radius * angle.tan()).abs();
    if dz_per_rev < 1e-9 {
        return None;
    }
    // Polygon vertices (line endpoints; arc endpoints, no mid-arc
    // sampling). Sufficient for the shoelace + ray-cast checks below.
    let verts = polygon_vertices(segments);
    if verts.len() < 3 {
        return None;
    }
    let area = polygon_signed_area(&verts);
    let ccw = area > 0.0;
    // Centroid as the helix center. Robust default for convex
    // pockets; for skinny / non-convex shapes the point-in-polygon
    // sampling below catches the bad cases and we fall back to Ramp.
    // We don't try to pull the center toward the path start — doing so
    // can push the helix circle into a wall on small or
    // sharply-cornered pockets, which is exactly the failure mode we
    // need helical entry to avoid. The post-helix walk to the path
    // start runs at constant z through the pocket interior, which is
    // safe because the boundary path itself is already inset from the
    // walls by tool_radius.
    let path_start = segments[0].start;
    // Pick the helix center as the point inside the polygon with the
    // largest clearance to the boundary (a "pole of inaccessibility"
    // approximation). The centroid works for convex pockets but for L /
    // U / + shapes it lands outside the polygon — and even when it
    // doesn't, a thin pocket's centroid may be too close to a wall for
    // the helix circle to fit. Picking the max-clearance point ensures
    // the helix circle has the most room to fit.
    //
    // We require the chosen center's clearance to exceed `radius +
    // tool_radius` so the helix circle clears the pocket walls by at
    // least a tool radius. If no interior point meets that bar the
    // helix can't fit and we fall back to Ramp.
    let Some(center) = polygon_pole_of_inaccessibility(&verts, radius + tool_radius) else {
        tracing::debug!(
            "helix entry: no interior point with clearance > {:.3}, falling back to Ramp",
            radius + tool_radius
        );
        return None;
    };
    // Sample 16 points on the helix circle as a final safety check;
    // all must be inside the polygon. The pole-of-inaccessibility
    // search above already guarantees the center has > radius +
    // tool_radius clearance, so this should always pass — it's a
    // backstop against numerical edge cases (e.g. polygon edges that
    // graze the helix circle at the clearance limit).
    let samples = 16;
    for i in 0..samples {
        let theta = f64::from(i) * std::f64::consts::TAU / f64::from(samples);
        let px = center.x + radius * theta.cos();
        let py = center.y + radius * theta.sin();
        if !point_in_polygon(&verts, px, py) {
            return None;
        }
    }
    // Start angle: vector from helix center toward the path start.
    // The helix lands at (center + radius·(cosθ, sinθ)) where θ =
    // start_angle, then walks the short remaining distance to the
    // path start.
    let start_angle = (path_start.y - center.y).atan2(path_start.x - center.x);
    Some(HelixEntry {
        center,
        radius,
        dz_per_rev,
        ccw,
        start_angle,
    })
}

/// Approximate "pole of inaccessibility" — the point inside the polygon
/// with the largest clearance to the boundary. Used to seat the helix
/// entry circle in pockets where the centroid sits outside (L / U / +)
/// or too close to a wall.
///
/// Algorithm: bbox-grid sample at ~64 cells per axis. For each interior
/// sample, compute the min distance to any polygon edge (line-segment
/// distance, not vertex distance — a long edge midway between two
/// vertices is what bites a helix circle). Return the sample with the
/// largest such distance, but only if it exceeds `min_clearance`.
///
/// Returns None when no interior sample meets `min_clearance` — caller
/// treats this as "helix can't fit, fall back to Ramp."
fn polygon_pole_of_inaccessibility(verts: &[Point2], min_clearance: f64) -> Option<Point2> {
    let n = verts.len();
    if n < 3 {
        return None;
    }
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for p in verts {
        if p.x < min_x {
            min_x = p.x;
        }
        if p.y < min_y {
            min_y = p.y;
        }
        if p.x > max_x {
            max_x = p.x;
        }
        if p.y > max_y {
            max_y = p.y;
        }
    }
    let width = max_x - min_x;
    let height = max_y - min_y;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    // 64 cells per axis is a balance: enough resolution to find pockets
    // ≥ 1/32 the bbox side; cheap enough for big pockets (~4096 grid
    // points × n_edges edge-distance calls).
    let cells = 64usize;
    let dx = width / (cells as f64);
    let dy = height / (cells as f64);
    let mut best: Option<(Point2, f64)> = None;
    // Try the centroid first as a likely candidate (skip the grid scan
    // entirely when it's already a great fit, e.g. a circular pocket).
    let centroid = polygon_centroid(verts);
    if point_in_polygon(verts, centroid.x, centroid.y) {
        let cd = polygon_min_distance_to_boundary(verts, centroid.x, centroid.y);
        if cd > min_clearance {
            best = Some((centroid, cd));
        }
    }
    for j in 0..cells {
        let py = min_y + (j as f64 + 0.5) * dy;
        for i in 0..cells {
            let px = min_x + (i as f64 + 0.5) * dx;
            if !point_in_polygon(verts, px, py) {
                continue;
            }
            let d = polygon_min_distance_to_boundary(verts, px, py);
            match best {
                Some((_, bd)) if d <= bd => {}
                _ => best = Some((Point2::new(px, py), d)),
            }
        }
    }
    match best {
        Some((p, d)) if d > min_clearance => Some(p),
        _ => None,
    }
}

/// Minimum distance from (x, y) to any edge of the polygon, treated as
/// a closed line-segment chain. Segment-to-point distance, not just
/// vertex-to-point distance — important for long pocket walls.
fn polygon_min_distance_to_boundary(verts: &[Point2], x: f64, y: f64) -> f64 {
    let n = verts.len();
    let mut best = f64::INFINITY;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        let ex = b.x - a.x;
        let ey = b.y - a.y;
        let len_sq = ex * ex + ey * ey;
        let d = if len_sq < 1e-18 {
            ((x - a.x) * (x - a.x) + (y - a.y) * (y - a.y)).sqrt()
        } else {
            let t = (((x - a.x) * ex) + ((y - a.y) * ey)) / len_sq;
            let t = t.clamp(0.0, 1.0);
            let px = a.x + t * ex;
            let py = a.y + t * ey;
            ((x - px) * (x - px) + (y - py) * (y - py)).sqrt()
        };
        if d < best {
            best = d;
        }
    }
    best
}

/// Polygon centroid via the shoelace formula. For a degenerate
/// (zero-area) polygon, returns the average of the vertices.
fn polygon_centroid(verts: &[Point2]) -> Point2 {
    let n = verts.len();
    if n == 0 {
        return Point2::new(0.0, 0.0);
    }
    let mut a = 0.0;
    let mut cx = 0.0;
    let mut cy = 0.0;
    for i in 0..n {
        let p = verts[i];
        let q = verts[(i + 1) % n];
        let cross = p.x * q.y - q.x * p.y;
        a += cross;
        cx += (p.x + q.x) * cross;
        cy += (p.y + q.y) * cross;
    }
    a *= 0.5;
    if a.abs() < 1e-9 {
        let mut sx = 0.0;
        let mut sy = 0.0;
        for p in verts {
            sx += p.x;
            sy += p.y;
        }
        return Point2::new(sx / n as f64, sy / n as f64);
    }
    Point2::new(cx / (6.0 * a), cy / (6.0 * a))
}

/// Emit the helical entry: descend from `from_z` to `to_z` on a circle
/// of radius `plan.radius` around `plan.center`. Each revolution drops
/// Z by `plan.dz_per_rev`; partial revolutions linearly interpolate Z.
/// The final point lands at the path-start angle so the caller's
/// follow-up `linear(start.x, start.y, to_z)` is a straight line of
/// length zero (or near-zero in the Helix circle's tangent frame).
fn emit_helix_entry<P: PostProcessor>(plan: &HelixEntry, from_z: f64, to_z: f64, post: &mut P) {
    let dz = (from_z - to_z).abs();
    if dz < 1e-9 {
        return;
    }
    // Number of full revolutions needed (always at least one — if the
    // user picks a tiny step the helix still completes a full lap so
    // the cutter doesn't dive on a chord).
    let revs_full = (dz / plan.dz_per_rev).ceil().max(1.0);
    // Each revolution drops Z by dz/revs_full so the descent is
    // distributed evenly.
    let dz_each = -(from_z - to_z).abs() / revs_full; // negative (going down)
    let n = revs_full as usize;
    // Helix start: cutter at start angle, current Z = from_z.
    let start_x = plan.center.x + plan.radius * plan.start_angle.cos();
    let start_y = plan.center.y + plan.radius * plan.start_angle.sin();
    // Move to start of helix at fast_move_z would be done by caller —
    // here we assume the cutter is already above the helix start. The
    // first emit is a linear move to the helix start at from_z so the
    // tool steps off the path-start XY (where the rapid landed it)
    // onto the helix circle at z=from_z.
    post.linear(Some(start_x), Some(start_y), Some(from_z));
    let mut cur_z = from_z;
    for i in 0..n {
        let next_z = if i + 1 == n { to_z } else { cur_z + dz_each };
        // Each revolution is two semicircles so a single G2/G3 with
        // i, j vector to center stays within the post processor's
        // arc capabilities (some posts reject full-circle arcs whose
        // endpoint == startpoint).
        let half_dz = (next_z - cur_z) * 0.5;
        let mid_angle = plan.start_angle + std::f64::consts::PI;
        let mid_x = plan.center.x + plan.radius * mid_angle.cos();
        let mid_y = plan.center.y + plan.radius * mid_angle.sin();
        // Arc 1: start → midpoint (semicircle). i, j are the offset
        // from the arc's start point to the helix center.
        let i1 = -plan.radius * plan.start_angle.cos();
        let j1 = -plan.radius * plan.start_angle.sin();
        if plan.ccw {
            post.arc_ccw(
                Some(mid_x),
                Some(mid_y),
                Some(cur_z + half_dz),
                Some(i1),
                Some(j1),
            );
        } else {
            post.arc_cw(
                Some(mid_x),
                Some(mid_y),
                Some(cur_z + half_dz),
                Some(i1),
                Some(j1),
            );
        }
        // Arc 2: midpoint → start (semicircle, completing the lap).
        let i2 = -plan.radius * mid_angle.cos();
        let j2 = -plan.radius * mid_angle.sin();
        let end_x = plan.center.x + plan.radius * plan.start_angle.cos();
        let end_y = plan.center.y + plan.radius * plan.start_angle.sin();
        if plan.ccw {
            post.arc_ccw(Some(end_x), Some(end_y), Some(next_z), Some(i2), Some(j2));
        } else {
            post.arc_cw(Some(end_x), Some(end_y), Some(next_z), Some(i2), Some(j2));
        }
        cur_z = next_z;
    }
}

/// Extract polygon vertices from a segment chain (line endpoints; arc
/// endpoints — arc midpoints aren't sampled, the polygon is just the
/// segment endpoint list). Used for signed-area + point-in-polygon
/// checks during helix planning. The returned list is the closed
/// polygon's vertex sequence with no duplicate closing vertex.
fn polygon_vertices(segments: &[Segment]) -> Vec<Point2> {
    let mut v: Vec<Point2> = Vec::with_capacity(segments.len() + 1);
    if segments.is_empty() {
        return v;
    }
    v.push(segments[0].start);
    for seg in segments {
        // Push the end of each segment; duplicates with the next
        // segment's start are filtered by the dedupe at the end.
        if matches!(seg.kind, SegmentKind::Point) {
            continue;
        }
        v.push(seg.end);
    }
    // Drop a duplicate trailing vertex (closed path: last == first).
    if v.len() >= 2 && v.first().unwrap().distance(*v.last().unwrap()) < 1e-6 {
        v.pop();
    }
    v
}

/// Shoelace signed area of a polygon given as a vertex list. Positive
/// = CCW, negative = CW. Mirrors `cam::offsets::object_signed_area`
/// but operates on vertices instead of a `VcObject`.
fn polygon_signed_area(verts: &[Point2]) -> f64 {
    let n = verts.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}

/// Even-odd ray-cast point-in-polygon test (horizontal ray to +X).
/// Edges are treated as half-open [lo.y, hi.y) so vertex hits don't
/// double-count. Sufficient for the helix-fit sanity check.
fn point_in_polygon(verts: &[Point2], x: f64, y: f64) -> bool {
    let n = verts.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        if (a.y - b.y).abs() < 1e-12 {
            continue;
        }
        let (lo, hi) = if a.y < b.y { (a, b) } else { (b, a) };
        if y < lo.y - 1e-12 || y >= hi.y - 1e-12 {
            continue;
        }
        let t = (y - lo.y) / (hi.y - lo.y);
        let xi = lo.x + t * (hi.x - lo.x);
        if xi > x {
            inside = !inside;
        }
    }
    inside
}

fn arc_length(seg: &Segment) -> f64 {
    let chord = seg.start.distance(seg.end);
    if seg.bulge.abs() < 1e-12 || chord < 1e-12 {
        return chord;
    }
    let (_, _, _, radius) = math::bulge_to_arc(seg.start, seg.end, seg.bulge);
    let theta = 4.0 * seg.bulge.atan(); // canonical bulge identity
    radius * theta.abs()
}

/// Emit the cut path with tab interruptions. For each LINE segment that
/// crosses a tab's `tab_radius` neighborhood, the cut is split: cut up to
/// the entry, lift Z to `tabs_z`, traverse to the exit, drop back to
/// `cut_z`, continue cutting (Rectangle); or ramp up / flat / ramp down
/// when `ramp_angle_deg` is `Some` (Ramp).
///
/// Arcs through tabs are tab-skipped with a straight Z lift even when
/// Ramp is requested — ramping along a curved path is a v2 follow-up.
fn emit_path_with_tabs<P: PostProcessor>(
    segments: &[Segment],
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: Option<f64>,
    post: &mut P,
) {
    for seg in segments {
        match seg.kind {
            SegmentKind::Line => {
                emit_line_with_tabs(seg, tabs, tabs_z, cut_z, tab_radius, ramp_angle_deg, post);
            }
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), None),
            SegmentKind::Arc | SegmentKind::Circle => {
                // Per-tab radius for crossing detection. Walks all tabs
                // and uses the MAX lift Z of any that touches this arc
                // (audit 3wv: per-tab overrides). The midpoint-of-chord
                // heuristic stays — exact arc-intersection math here
                // would be heavier and the chord-mid check has been the
                // shipped behavior since rt1.10.
                let fallback_width = tab_radius * 2.0;
                let fallback_lift = (tabs_z - cut_z).abs();
                let mid_x = (seg.start.x + seg.end.x) * 0.5;
                let mid_y = (seg.start.y + seg.end.y) * 0.5;
                let arc_tab_z = tabs
                    .iter()
                    .filter_map(|t| {
                        let r = t.radius(fallback_width);
                        if (mid_x - t.x).hypot(mid_y - t.y) < r {
                            Some(cut_z + t.lift(fallback_lift))
                        } else {
                            None
                        }
                    })
                    .fold(f64::NEG_INFINITY, f64::max);
                let crosses = arc_tab_z.is_finite();
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if !crosses {
                    if seg.bulge > 0.0 {
                        post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    } else {
                        post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    }
                } else if let Some(ramp) = ramp_angle_deg {
                    emit_arc_chord_with_tabs(seg, tabs, tabs_z, cut_z, tab_radius, ramp, post);
                } else {
                    post.linear(None, None, Some(arc_tab_z));
                    if seg.bulge > 0.0 {
                        post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    } else {
                        post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    }
                    post.linear(None, None, Some(cut_z));
                }
            }
        }
    }
}

fn emit_line_with_tabs<P: PostProcessor>(
    seg: &Segment,
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: Option<f64>,
    post: &mut P,
) {
    let dx = seg.end.x - seg.start.x;
    let dy = seg.end.y - seg.start.y;
    let len = dx.hypot(dy);
    if len < 1e-9 {
        return;
    }
    // Walk the segment; for every tab whose perpendicular foot is on the
    // segment within its own effective radius, compute t-entry / t-exit
    // and the per-tab effective lift Z (audit 3wv: width / height
    // overrides now flow through per-tab instead of using the op-level
    // values uniformly).
    let fallback_width = tab_radius * 2.0;
    let fallback_lift = (tabs_z - cut_z).abs();
    let mut intervals: Vec<(f64, f64, f64)> = Vec::new();
    for tab in tabs {
        let r = tab.radius(fallback_width);
        if r <= 0.0 {
            continue;
        }
        let tx = tab.x - seg.start.x;
        let ty = tab.y - seg.start.y;
        let t = (tx * dx + ty * dy) / (len * len);
        let perp_x = tx - t * dx;
        let perp_y = ty - t * dy;
        let perp = (perp_x * perp_x + perp_y * perp_y).sqrt();
        if perp > r {
            continue;
        }
        let half = (r * r - perp * perp).sqrt() / len;
        let t_in = (t - half).max(0.0);
        let t_out = (t + half).min(1.0);
        if t_out > t_in {
            let z_top = cut_z + tab.lift(fallback_lift);
            intervals.push((t_in, t_out, z_top));
        }
    }
    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Merge overlaps; overlapping tabs use the higher of their lifts
    // so the cutter clears both.
    let mut merged: Vec<(f64, f64, f64)> = Vec::new();
    for (a, b, z) in intervals {
        if let Some(last) = merged.last_mut() {
            if a <= last.1 + 1e-6 {
                last.1 = last.1.max(b);
                last.2 = last.2.max(z);
                continue;
            }
        }
        merged.push((a, b, z));
    }
    // Emit: cut up to each interval, lift / ramp, traverse, drop / ramp,
    // repeat. Per-interval `interval_z` is the (per-tab) effective lift,
    // so a tab with a non-default height override gets its own Z plateau.
    let mut cursor = 0.0;
    for (t_in, t_out, interval_z) in merged {
        if t_in > cursor + 1e-6 {
            let p = lerp(seg, t_in);
            post.linear(Some(p.0), Some(p.1), None);
        }
        let dz_here = (interval_z - cut_z).abs();
        let ramp_length = ramp_angle_deg.map(|a| {
            if dz_here < 1e-9 {
                0.0
            } else {
                dz_here / a.to_radians().tan()
            }
        });
        match ramp_length {
            Some(rl) if rl > 1e-9 => {
                let tab_world_len = (t_out - t_in) * len;
                if tab_world_len < 2.0 * rl {
                    let t_mid = 0.5 * (t_in + t_out);
                    let mid = lerp(seg, t_mid);
                    post.linear(Some(mid.0), Some(mid.1), Some(interval_z));
                    let exit = lerp(seg, t_out);
                    post.linear(Some(exit.0), Some(exit.1), Some(cut_z));
                } else {
                    let dt_ramp = rl / len;
                    let t_up_end = t_in + dt_ramp;
                    let t_down_start = t_out - dt_ramp;
                    let up_end = lerp(seg, t_up_end);
                    let down_start = lerp(seg, t_down_start);
                    let exit = lerp(seg, t_out);
                    post.linear(Some(up_end.0), Some(up_end.1), Some(interval_z));
                    post.linear(Some(down_start.0), Some(down_start.1), None);
                    post.linear(Some(exit.0), Some(exit.1), Some(cut_z));
                }
            }
            _ => {
                post.linear(None, None, Some(interval_z));
                let p_out = lerp(seg, t_out);
                post.linear(Some(p_out.0), Some(p_out.1), None);
                post.linear(None, None, Some(cut_z));
            }
        }
        cursor = t_out;
    }
    if cursor < 1.0 - 1e-6 {
        post.linear(Some(seg.end.x), Some(seg.end.y), None);
    }
}

fn lerp(seg: &Segment, t: f64) -> (f64, f64) {
    (
        seg.start.x + t * (seg.end.x - seg.start.x),
        seg.start.y + t * (seg.end.y - seg.start.y),
    )
}

/// Emit a tab-crossing arc by discretizing it into short chord
/// segments and reusing the line-tab ramp logic per chord. The chord
/// chain replaces the original G2/G3 with G1 moves that can carry the
/// trapezoid Z profile. Used only when an arc actually crosses a tab
/// and the tab type is Ramp.
fn emit_arc_chord_with_tabs<P: PostProcessor>(
    seg: &Segment,
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: f64,
    post: &mut P,
) {
    let center = seg
        .center
        .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
    let r = (seg.start.x - center.x).hypot(seg.start.y - center.y);
    if r < 1e-9 {
        // Degenerate arc — just emit the endpoints as a line.
        let line = Segment::line(seg.start, seg.end, &seg.layer, seg.color);
        emit_line_with_tabs(
            &line,
            tabs,
            tabs_z,
            cut_z,
            tab_radius,
            Some(ramp_angle_deg),
            post,
        );
        return;
    }
    let theta_start = (seg.start.y - center.y).atan2(seg.start.x - center.x);
    let theta_end = (seg.end.y - center.y).atan2(seg.end.x - center.x);
    // Bulge sign: positive ⇒ CCW (signed sweep > 0). Total swept angle
    // satisfies sweep = 4·atan(bulge), preserving sign.
    let sweep = 4.0 * seg.bulge.atan();
    // Chord count: 32 chords for a full circle is plenty (chord error
    // ~ r·(1 - cos(π/32)) ≈ r·0.005; on a 10 mm arc that's 0.05 mm —
    // visually identical and well under typical tab tolerances). Scale
    // chords linearly with sweep magnitude, with a 4-chord minimum.
    let n_chords = (32.0 * sweep.abs() / std::f64::consts::TAU).ceil().max(4.0) as usize;
    let dtheta = sweep / (n_chords as f64);
    let mut prev_theta = theta_start;
    for k in 0..n_chords {
        let next_theta = if k + 1 == n_chords {
            // Snap last endpoint to the original arc end so
            // floating-point error doesn't leave a gap.
            theta_end
        } else {
            theta_start + dtheta * ((k + 1) as f64)
        };
        let a = Point2::new(
            center.x + r * prev_theta.cos(),
            center.y + r * prev_theta.sin(),
        );
        let b = if k + 1 == n_chords {
            seg.end
        } else {
            Point2::new(
                center.x + r * next_theta.cos(),
                center.y + r * next_theta.sin(),
            )
        };
        let chord = Segment::line(a, b, &seg.layer, seg.color);
        emit_line_with_tabs(
            &chord,
            tabs,
            tabs_z,
            cut_z,
            tab_radius,
            Some(ramp_angle_deg),
            post,
        );
        prev_theta = next_theta;
    }
}

/// Emit segments with optional drag-knife trailing offset. When
/// `dragoff > 0`, every line→line corner is preceded by an arc that swivels
/// the blade around the corner point so the trail aligns with the new
/// direction. Mirrors `viaconstructor.machine_cmd.segment2machine_cmd`.
/// Build the per-pass Z schedule for `multi_pass`. When `depth_list`
/// is non-empty it wins as an explicit list (clamped to never go above
/// `start_depth` so a stale list doesn't accidentally cut air).
/// Otherwise: descend from `start_depth` by `step` (negative number)
/// per pass until reaching `total_depth`. When `finish_step` is set
/// and smaller in magnitude than `step`, the last pass cuts at
/// `total_depth` from `total_depth - finish_step` instead of one full
/// `step` higher — gives a thin finish pass for cleaner bottom finish.
fn build_z_schedule(
    start_depth: f64,
    total_depth: f64,
    step: f64,
    finish_step: Option<f64>,
    depth_list: &[f64],
) -> Vec<f64> {
    if !depth_list.is_empty() {
        // Take the user's list verbatim (clamped above start_depth so
        // we don't accidentally cut air).
        return depth_list
            .iter()
            .copied()
            .filter(|&z| z <= start_depth + 1e-9)
            .collect();
    }
    let mut out = Vec::new();
    if (step.abs() < 1e-9) || (start_depth - total_depth).abs() < 1e-9 {
        out.push(total_depth);
        return out;
    }
    // Negative step: start_depth + step < start_depth.
    let mut z = (start_depth + step).max(total_depth);
    let finish_mag = finish_step.map(f64::abs).filter(|f| *f > 1e-9);
    loop {
        // If the next pass would land at total_depth exactly and a
        // finish_step is set, splice it in: emit z one step shallower
        // than total_depth, then a final pass at total_depth.
        if z <= total_depth + 1e-9 {
            // Last pass.
            if let Some(fs) = finish_mag {
                let pre_finish = total_depth + fs;
                // Splice a pre-finish pass whenever the resulting Z
                // sits strictly between total_depth and start_depth,
                // AND it isn't a duplicate of the previous pass.
                // Bug before this: the `!out.is_empty()` guard
                // dropped the pre-finish pass on a single-pass op
                // (step >= total_depth ⇒ loop terminates with
                // `out` still empty), silently losing the finish
                // quality on thin cuts.
                let dup_of_last = out
                    .last()
                    .is_some_and(|&l| (l - pre_finish).abs() <= 1e-9);
                if !dup_of_last
                    && pre_finish < start_depth - 1e-9
                    && pre_finish > total_depth + 1e-9
                {
                    out.push(pre_finish);
                }
            }
            out.push(total_depth);
            return out;
        }
        out.push(z);
        z = (z + step).max(total_depth);
    }
}

/// Walk `segments` like `emit_path_with_dragoff` but reduce the feed
/// at sharp line-line corners by `corner_reduction` (a fraction in
/// Polyline → arc collapse on emit. When `machine.arcs == true`, walks
/// `segments` and replaces consecutive `Line` runs (≥3 points) with the
/// fewest G2/G3 arcs that approximate the chord chain within
/// `effective_arc_tolerance()`. Pre-existing `Arc` / `Circle` / `Point`
/// segments are passed through verbatim — only line runs are eligible.
/// When `machine.arcs == false`, returns the input untouched.
fn fit_line_runs(segments: &[Segment], setup: &Setup) -> Vec<Segment> {
    if !setup.machine.arcs || segments.is_empty() {
        return segments.to_vec();
    }
    let tol = setup.machine.effective_arc_tolerance();
    let mut out: Vec<Segment> = Vec::with_capacity(segments.len());
    let layer = segments[0].layer.clone();
    let color = segments[0].color;
    let mut run_pts: Vec<Point2> = Vec::new();
    let mut run_layer = layer.clone();
    let mut run_color = color;

    let flush_run =
        |run_pts: &mut Vec<Point2>, run_layer: &str, run_color: i32, out: &mut Vec<Segment>| {
            if run_pts.len() < 2 {
                run_pts.clear();
                return;
            }
            match crate::gcode::arc_fit::fit_arc_run(run_pts, tol) {
                crate::gcode::arc_fit::FitOutput::Lines(pts) => {
                    for w in pts.windows(2) {
                        out.push(Segment::line(w[0], w[1], run_layer, run_color));
                    }
                }
                crate::gcode::arc_fit::FitOutput::Arcs(arcs) => {
                    let mut cursor = run_pts[0];
                    for a in arcs {
                        let (_, _, bulge) = arc_bulge_from_center(cursor, a.end, a.center, a.ccw);
                        out.push(Segment::arc(
                            cursor,
                            a.end,
                            bulge,
                            Some(a.center),
                            run_layer,
                            run_color,
                        ));
                        cursor = a.end;
                    }
                }
            }
            run_pts.clear();
        };

    for seg in segments {
        if matches!(seg.kind, SegmentKind::Line) {
            if run_pts.is_empty() {
                run_pts.push(seg.start);
                run_layer = seg.layer.clone();
                run_color = seg.color;
            }
            run_pts.push(seg.end);
        } else {
            flush_run(&mut run_pts, &run_layer, run_color, &mut out);
            out.push(seg.clone());
        }
    }
    flush_run(&mut run_pts, &run_layer, run_color, &mut out);
    out
}

/// Derive a polyline `bulge` from a known arc geometry (start, end,
/// absolute center, direction). The sign of `bulge` matches our
/// convention: positive ⇒ CCW (G3), negative ⇒ CW (G2).
fn arc_bulge_from_center(
    start: Point2,
    end: Point2,
    center: Point2,
    ccw: bool,
) -> (Point2, f64, f64) {
    let a0 = (start.y - center.y).atan2(start.x - center.x);
    let a1 = (end.y - center.y).atan2(end.x - center.x);
    let mut sweep = if ccw { a1 - a0 } else { a0 - a1 };
    while sweep < 0.0 {
        sweep += std::f64::consts::TAU;
    }
    while sweep > std::f64::consts::TAU {
        sweep -= std::f64::consts::TAU;
    }
    let signed_sweep = if ccw { sweep } else { -sweep };
    let bulge = (signed_sweep * 0.25).tan();
    (center, sweep, bulge)
}

/// [0, 1]). Skipped when `corner_reduction <= 0`, when `dragoff > 0`
/// (drag knife trail compensation already smooths corners), or when
/// the segment list is too short to have corners.
///
/// Detection threshold: the angle change at a join >= 60° (computed
/// as the supplement of the dot product). The slowed feed is emitted
/// before the second segment; the original feed is restored after.
fn emit_path_with_corner_feed<P: PostProcessor>(
    segments: &[Segment],
    dragoff: f64,
    base_rate: u32,
    corner_reduction: f64,
    post: &mut P,
) {
    if corner_reduction <= 1e-6 || dragoff > 1e-9 || segments.len() < 2 {
        emit_path_with_dragoff(segments, dragoff, post);
        return;
    }
    let reduced_rate = (f64::from(base_rate) * (1.0 - corner_reduction)).max(1.0) as u32;
    let cos_threshold = 0.5_f64; // 60° turn → cos(angle) <= 0.5
    let mut feed_currently_reduced = false;
    let mut prev_dir: Option<(f64, f64)> = None;
    for (i, seg) in segments.iter().enumerate() {
        // Restore feed for arcs and points — they don't have sharp
        // corners by definition.
        if !matches!(seg.kind, SegmentKind::Line) {
            if feed_currently_reduced {
                post.feedrate(base_rate);
                feed_currently_reduced = false;
            }
            // Single-segment emit reusing emit_path_with_dragoff's logic
            // would be over-engineered; just inline arc/point here.
            match seg.kind {
                SegmentKind::Arc | SegmentKind::Circle => {
                    let center = seg
                        .center
                        .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                    let cx = center.x - seg.start.x;
                    let cy = center.y - seg.start.y;
                    if seg.bulge > 0.0 {
                        post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(cx), Some(cy));
                    } else {
                        post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(cx), Some(cy));
                    }
                }
                SegmentKind::Point => {
                    post.linear(Some(seg.start.x), Some(seg.start.y), None);
                }
                _ => {}
            }
            prev_dir = None;
            continue;
        }
        let dx = seg.end.x - seg.start.x;
        let dy = seg.end.y - seg.start.y;
        let len = (dx * dx + dy * dy).sqrt();
        // Zero-length segments don't have a direction; emit them as a
        // plain linear and DO NOT update prev_dir so the next real
        // segment compares against the last meaningful direction. A
        // (0,0) cur_dir would otherwise flag dot=0 (= 90° turn) and
        // spuriously slow the feed.
        if len <= 1e-9 {
            post.linear(Some(seg.end.x), Some(seg.end.y), None);
            continue;
        }
        let cur_dir = (dx / len, dy / len);
        let needs_reduction = match prev_dir {
            Some((px, py)) if i > 0 => {
                // dot product < cos_threshold means the turn is
                // sharper than ~60°.
                let dot = px * cur_dir.0 + py * cur_dir.1;
                dot < cos_threshold
            }
            _ => false,
        };
        if needs_reduction && !feed_currently_reduced {
            post.feedrate(reduced_rate);
            feed_currently_reduced = true;
        } else if !needs_reduction && feed_currently_reduced {
            post.feedrate(base_rate);
            feed_currently_reduced = false;
        }
        post.linear(Some(seg.end.x), Some(seg.end.y), None);
        prev_dir = Some(cur_dir);
    }
    if feed_currently_reduced {
        post.feedrate(base_rate);
    }
}

fn emit_path_with_dragoff<P: PostProcessor>(segments: &[Segment], dragoff: f64, post: &mut P) {
    use std::f64::consts::{FRAC_PI_2, PI};
    let mut last_motion: Option<f64> = None;
    for seg in segments {
        match seg.kind {
            SegmentKind::Line => {
                let new_motion = (seg.end.y - seg.start.y).atan2(seg.end.x - seg.start.x);
                if dragoff > 1e-9 {
                    if let Some(last_m) = last_motion {
                        let last_a = last_m + FRAC_PI_2;
                        let new_a = new_motion + FRAC_PI_2;
                        let off1 = (
                            seg.start.x + dragoff * last_a.sin(),
                            seg.start.y - dragoff * last_a.cos(),
                        );
                        let off2 = (
                            seg.start.x + dragoff * new_a.sin(),
                            seg.start.y - dragoff * new_a.cos(),
                        );
                        post.linear(Some(off1.0), Some(off1.1), None);
                        let mut diff = new_a - last_a;
                        while diff > PI {
                            diff -= 2.0 * PI;
                        }
                        while diff < -PI {
                            diff += 2.0 * PI;
                        }
                        if diff.abs() > 1e-6 {
                            let i = seg.start.x - off1.0;
                            let j = seg.start.y - off1.1;
                            if diff > 0.0 {
                                post.arc_ccw(Some(off2.0), Some(off2.1), None, Some(i), Some(j));
                            } else {
                                post.arc_cw(Some(off2.0), Some(off2.1), None, Some(i), Some(j));
                            }
                        }
                    }
                }
                post.linear(Some(seg.end.x), Some(seg.end.y), None);
                last_motion = Some(new_motion);
            }
            SegmentKind::Point => {
                post.linear(Some(seg.start.x), Some(seg.start.y), None);
                last_motion = None;
            }
            SegmentKind::Arc | SegmentKind::Circle => {
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if seg.bulge > 0.0 {
                    post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                } else {
                    post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                }
                // Tangent at end of arc: rotate radius 90° in the arc's
                // orientation. CCW arc → +90° rotation; CW → -90°.
                let rx = seg.end.x - center.x;
                let ry = seg.end.y - center.y;
                let (tx, ty) = if seg.bulge > 0.0 {
                    (-ry, rx)
                } else {
                    (ry, -rx)
                };
                last_motion = Some(ty.atan2(tx));
            }
        }
    }
}

/// Geometry of a lead-in or lead-out move.
///
/// `Straight` keeps the legacy "perpendicular hop" lead — the approach
/// (lead-in) or exit (lead-out) point sits `in_lenght` mm to the LEFT of
/// the contour tangent, and the cutter travels in a straight line.
///
/// `Arc` is a tangent roll-on / roll-off: a quarter-circle of `in_lenght`
/// mm radius whose center is `radius` perpendicular to the tangent on the
/// LEFT (same convention as Straight). The arc lands tangent to the
/// contour at the entry/exit point, so the cutter eases into / out of the
/// cut without dwelling at the start. `entry_or_exit` is the off-contour
/// endpoint of the arc (lead-in: WHERE we G0 to before arcing onto the
/// contour; lead-out: WHERE we end up after arcing off the contour).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LeadGeometry {
    None,
    Straight {
        from: Point2,
    },
    Arc {
        entry_or_exit: Point2,
        center: Point2,
        ccw: bool,
    },
}

/// Compute the unit tangent at the START of the first segment in a cut
/// path. For a Line, this is just the direction from start→end; for an
/// Arc / Circle, it's the radius vector rotated 90° in the arc's
/// orientation (CCW for positive bulge, CW for negative).
fn first_segment_start_tangent(seg: &Segment) -> Option<(f64, f64)> {
    match seg.kind {
        SegmentKind::Line | SegmentKind::Point => {
            let dx = seg.end.x - seg.start.x;
            let dy = seg.end.y - seg.start.y;
            let n = (dx * dx + dy * dy).sqrt();
            if n < 1e-12 {
                None
            } else {
                Some((dx / n, dy / n))
            }
        }
        SegmentKind::Arc | SegmentKind::Circle => {
            let center = seg
                .center
                .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
            let rx = seg.start.x - center.x;
            let ry = seg.start.y - center.y;
            let n = (rx * rx + ry * ry).sqrt();
            if n < 1e-12 {
                return None;
            }
            // CCW (bulge > 0): tangent at start = rotate radius 90° CCW.
            // CW: rotate 90° CW.
            let (tx, ty) = if seg.bulge >= 0.0 {
                (-ry / n, rx / n)
            } else {
                (ry / n, -rx / n)
            };
            Some((tx, ty))
        }
    }
}

/// Tangent at the END of the last segment.
fn last_segment_end_tangent(seg: &Segment) -> Option<(f64, f64)> {
    match seg.kind {
        SegmentKind::Line | SegmentKind::Point => {
            let dx = seg.end.x - seg.start.x;
            let dy = seg.end.y - seg.start.y;
            let n = (dx * dx + dy * dy).sqrt();
            if n < 1e-12 {
                None
            } else {
                Some((dx / n, dy / n))
            }
        }
        SegmentKind::Arc | SegmentKind::Circle => {
            let center = seg
                .center
                .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
            let rx = seg.end.x - center.x;
            let ry = seg.end.y - center.y;
            let n = (rx * rx + ry * ry).sqrt();
            if n < 1e-12 {
                return None;
            }
            let (tx, ty) = if seg.bulge >= 0.0 {
                (-ry / n, rx / n)
            } else {
                (ry / n, -rx / n)
            };
            Some((tx, ty))
        }
    }
}

/// Chord-polygon signed area of a closed-ish offset polyline. Positive
/// = CCW, negative = CW. Bulge is ignored — the sign is dominated by
/// the chord winding except in pathological >180° arcs that the offset
/// pass would not produce.
fn polyline_signed_area(segments: &[Segment]) -> f64 {
    let mut a = 0.0;
    for s in segments {
        a += s.start.x * s.end.y - s.end.x * s.start.y;
    }
    a * 0.5
}

/// Decide which side of the tangent is FREE SPACE (no stock), so the
/// lead-in rapids in through air rather than carving into the part.
///
/// Rule:
///   * Outer profile (offset expanded outside the part) — the part
///     sits in the INTERIOR of the offset polygon. Free space is on
///     the side opposite the interior.
///   * Inner profile (pocket boundary, offset contracted) — free
///     space IS the interior (pocket center).
///
/// Winding tells us where the interior is: CCW (positive signed area)
/// ⇒ interior on the LEFT of tangent; CW ⇒ on the RIGHT.
///
/// Returns true when free space is on the LEFT of the tangent
/// (perpendicular CCW = `(-ty, tx)`); false ⇒ RIGHT (`(ty, -tx)`).
fn lead_free_side_left(setup: &Setup, segments: &[Segment]) -> bool {
    let ccw = polyline_signed_area(segments) > 0.0;
    let is_outer = matches!(setup.mill.offset, ToolOffset::Outside);
    // outer + ccw → interior left → free right;  outer + cw → free left
    // inner + ccw → interior left = free left;   inner + cw → free right
    // ⇒ free_left = is_outer XOR ccw == !is_outer && ccw || is_outer && !ccw
    is_outer != ccw
}

pub(crate) fn lead_in_geometry(setup: &Setup, segments: &[Segment]) -> LeadGeometry {
    if setup.leads.r#in == LeadKind::Off || segments.is_empty() {
        return LeadGeometry::None;
    }
    let len = setup.leads.in_lenght.max(0.0);
    if len < 1e-9 {
        return LeadGeometry::None;
    }
    let first = &segments[0];
    let Some((tx, ty)) = first_segment_start_tangent(first) else {
        return LeadGeometry::None;
    };
    let free_left = lead_free_side_left(setup, segments);
    let (px, py) = if free_left { (-ty, tx) } else { (ty, -tx) };
    match setup.leads.r#in {
        LeadKind::Straight => LeadGeometry::Straight {
            from: Point2::new(first.start.x + len * px, first.start.y + len * py),
        },
        LeadKind::Arc => {
            // Quarter-arc roll-on:
            //   center    = P0 + perp_free * radius
            //   arc_start = P0 + radius * (perp_free - tangent)
            // Sweep direction follows the perpendicular hand: free-on-
            // left ⇒ CCW (G3); free-on-right ⇒ CW (G2). Either way the
            // cutter lands at P0 tangent to (+tx, +ty).
            let radius = len;
            let center = Point2::new(first.start.x + radius * px, first.start.y + radius * py);
            let arc_start = Point2::new(
                first.start.x + radius * (px - tx),
                first.start.y + radius * (py - ty),
            );
            LeadGeometry::Arc {
                entry_or_exit: arc_start,
                center,
                ccw: free_left,
            }
        }
        LeadKind::Off => LeadGeometry::None,
    }
}

pub(crate) fn lead_out_geometry(setup: &Setup, segments: &[Segment]) -> LeadGeometry {
    if setup.leads.out == LeadKind::Off || segments.is_empty() {
        return LeadGeometry::None;
    }
    let len = setup.leads.out_lenght.max(0.0);
    if len < 1e-9 {
        return LeadGeometry::None;
    }
    let last = segments.last().unwrap();
    let Some((tx, ty)) = last_segment_end_tangent(last) else {
        return LeadGeometry::None;
    };
    let free_left = lead_free_side_left(setup, segments);
    let (px, py) = if free_left { (-ty, tx) } else { (ty, -tx) };
    match setup.leads.out {
        LeadKind::Straight => LeadGeometry::Straight {
            from: Point2::new(last.end.x + len * px, last.end.y + len * py),
        },
        LeadKind::Arc => {
            // Mirror of lead-in: cutter is at Pn moving along +t.
            //   center  = Pn + perp_free * radius
            //   arc_end = Pn + radius * (perp_free + tangent)
            // Sweep direction = free_left (CCW iff free is on the left).
            let radius = len;
            let center = Point2::new(last.end.x + radius * px, last.end.y + radius * py);
            let arc_end = Point2::new(
                last.end.x + radius * (px + tx),
                last.end.y + radius * (py + ty),
            );
            LeadGeometry::Arc {
                entry_or_exit: arc_end,
                center,
                ccw: free_left,
            }
        }
        LeadKind::Off => LeadGeometry::None,
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
#[must_use] pub fn fmt_num(v: f64, sep: char) -> String {
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

    /// Regression for `hsb` (audit): `build_z_schedule` used to drop the
    /// pre-finish pass on single-pass ops because of an `!out.is_empty()`
    /// guard. With step >= depth (one pass at `total_depth`), the user's
    /// `finish_step` was silently lost.
    #[test]
    fn build_z_schedule_inserts_pre_finish_on_single_pass() {
        // Depth = -3, step = -3 (= depth, so one main pass), finish_step = 0.2.
        // Expected: pre-finish at -2.8, then total at -3.
        let s = build_z_schedule(0.0, -3.0, -3.0, Some(0.2), &[]);
        assert_eq!(
            s,
            vec![-2.8, -3.0],
            "single-pass with finish_step should splice a pre-finish at depth + finish_step",
        );
    }

    /// Same `finish_step` but multi-pass: the schedule should still
    /// include the pre-finish where it makes sense, AND not duplicate
    /// it when a regular step lands at the same Z.
    #[test]
    fn build_z_schedule_inserts_pre_finish_on_multi_pass() {
        // Depth = -5, step = -1, finish_step = 0.2.
        // Main passes: -1, -2, -3, -4. Final reaches -5 with a
        // pre-finish at -4.8 spliced in, then -5.
        let s = build_z_schedule(0.0, -5.0, -1.0, Some(0.2), &[]);
        assert_eq!(s, vec![-1.0, -2.0, -3.0, -4.0, -4.8, -5.0]);
    }

    /// Finish-step of zero behaves like None — no extra pass.
    #[test]
    fn build_z_schedule_finish_step_zero_is_noop() {
        let s = build_z_schedule(0.0, -3.0, -3.0, Some(0.0), &[]);
        assert_eq!(s, vec![-3.0]);
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
