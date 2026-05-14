//! Per-operation toolpath result cache.
//!
//! When the user re-clicks Generate without changing anything, the
//! pipeline can re-emit each enabled operation's gcode body and toolpath
//! slice from a cache instead of re-running offsets / cascade / emit.
//! With ~5 ops on a moderate project that's the difference between
//! 5–10 s of work and ~100 ms of dictionary lookups.
//!
//! ## Hashing discipline (read this before adding fields)
//!
//! When you add a `Hash` impl in this module, hash EVERY field that
//! affects the operation's output. Forgetting one means stale cache
//! hits = wrong gcode. The compiler will not catch this — the
//! `#[derive(Hash)]` macro can't be used because the project types
//! contain `f64` (which deliberately doesn't implement `Hash` to keep
//! NaN out of HashMap keys), so every Hash impl is hand-written.
//!
//! Conventions:
//! - `f64` → `state.write_u64(v.to_bits())`. Two NaNs hash differently
//!   and that's fine — we never produce NaN in op params.
//! - `Option<f64>` → discriminant byte (0/1) + bits when Some.
//! - `Vec<f64>` → length + each element's bits.
//! - `HashMap<K, V>` → SORT keys before iterating, then hash
//!   `(key, value)` pairs in sorted order. Iteration order is non-
//!   deterministic and would defeat the point of hashing.
//!
//! ## Pipeline version
//!
//! [`PIPELINE_VERSION`] gets folded into every cache key. Bump it
//! whenever ANY pipeline output format changes — toolpath segment
//! shape, gcode formatting, comment style, post-processor output,
//! anything observable. That cleanly invalidates every entry across
//! every running process so we never serve stale shapes from before
//! the change.

use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
use seahash::SeaHasher;

use crate::cam::setup::{
    AxisLimits, LeadKind, LeadsConfig, MachineConfig, MachineMode, ObjectOrder, PlungeStrategy,
    TabType, TabsConfig, ToolOffset, UnitSystem,
};
use crate::cam::source_combine::FrameShape;
use crate::gcode::preview::ToolpathSegment;
use crate::gcode::CapturedPostState;
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::project::{
    Coolant, CutDirection, DrillCycle, Fixture, FixtureKind, HolderShape, Operation, OperationKind,
    OperationParams, OperationSource, PatternConfig, PocketStrategy, SourceCombine, ToolEntry,
    ToolKind,
};

/// Bumped when ANY pipeline output format changes — toolpath segment
/// shape, gcode formatting, anything. Invalidates the whole cache.
pub const PIPELINE_VERSION: u32 = 21;

/// Stable hash of (op + tool + machine + selected segments + fixtures
/// + PIPELINE_VERSION). Wrapper so callers can't accidentally pass an
/// unrelated `u64` to [`PipelineCache::get`] / [`PipelineCache::put`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpCacheKey(pub u64);

/// Cached per-op output. The header / footer and program-level state
/// are NOT cached — only this op's body.
#[derive(Debug, Clone)]
pub struct OpCacheValue {
    /// Per-op slice of toolpath segments produced by this op (op_id
    /// stamped). Used when the caller wants per-op toolpath without a
    /// full re-interpret pass.
    pub toolpath: Vec<ToolpathSegment>,
    /// Per-op gcode body (the raw lines this op contributed to the
    /// program), without program header / footer.
    pub gcode_body: String,
    /// Closed-object count this op contributed to `PipelineStats`.
    pub closed_count: usize,
    /// Offset count this op contributed to `PipelineStats`.
    pub offset_count: usize,
    /// Post-processor delta-encoding state AFTER this op finished.
    /// Restored on a cache hit so the next op (or the footer) emits the
    /// same bytes a fresh run would.
    pub exit_state: CapturedPostState,
    /// Cutter XY position after this op finished, used by the next op's
    /// `order_offsets` to pick the nearest-first cut.
    pub exit_xy: (f64, f64),
}

#[derive(Debug)]
pub struct PipelineCache {
    inner: Mutex<LruCache<u64, OpCacheValue>>,
}

impl PipelineCache {
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).expect("non-zero capacity");
        Self {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    pub fn get(&self, key: OpCacheKey) -> Option<OpCacheValue> {
        let mut g = self.inner.lock().ok()?;
        g.get(&key.0).cloned()
    }

    pub fn put(&self, key: OpCacheKey, value: OpCacheValue) {
        if let Ok(mut g) = self.inner.lock() {
            g.put(key.0, value);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Compute a stable cache key for (op + tool + machine + selected
/// segments + fixtures + tabs + post_processor_tag + PIPELINE_VERSION).
/// Stable across runs and across builds (modulo `PIPELINE_VERSION`
/// bumps). `post_processor_tag` is the caller's post-processor
/// discriminant — different posts produce different gcode for the same
/// inputs, so they must key separately. `tabs` carries the project's
/// segment-keyed tab placements — changing tab positions changes the
/// per-op output, so they must be part of the key.
pub fn op_cache_key(
    op: &Operation,
    tool: &ToolEntry,
    machine: &MachineConfig,
    selected_segments: &[Segment],
    fixtures: &[Fixture],
    post_processor_tag: u8,
) -> OpCacheKey {
    op_cache_key_with_finish(
        op,
        tool,
        None,
        machine,
        selected_segments,
        fixtures,
        post_processor_tag,
    )
}

/// Cache-key constructor that folds a SECOND tool's entry into the
/// hash — used for dual-tool Pocket ops (rt1.33) so changes to the
/// finish tool's diameter / feed_rate_finish / etc. invalidate the
/// cache. Pass `finish_tool = None` for single-tool ops (legacy
/// callers route through [`op_cache_key`]).
#[allow(clippy::too_many_arguments)]
pub fn op_cache_key_with_finish(
    op: &Operation,
    tool: &ToolEntry,
    finish_tool: Option<&ToolEntry>,
    machine: &MachineConfig,
    selected_segments: &[Segment],
    fixtures: &[Fixture],
    post_processor_tag: u8,
) -> OpCacheKey {
    let mut h = SeaHasher::new();
    PIPELINE_VERSION.hash(&mut h);
    h.write_u8(post_processor_tag);
    hash_operation(op, &mut h);
    hash_tool(tool, &mut h);
    match finish_tool {
        None => h.write_u8(0),
        Some(t) => {
            h.write_u8(1);
            hash_tool(t, &mut h);
        }
    }
    hash_machine(machine, &mut h);
    h.write_usize(selected_segments.len());
    for seg in selected_segments {
        hash_segment(seg, &mut h);
    }
    h.write_usize(fixtures.len());
    for fx in fixtures {
        hash_fixture(fx, &mut h);
    }
    OpCacheKey(h.finish())
}

// ─── primitives ───────────────────────────────────────────────────────

#[inline]
fn hash_f64<H: Hasher>(v: f64, h: &mut H) {
    h.write_u64(v.to_bits());
}

#[inline]
fn hash_opt_f64<H: Hasher>(v: Option<f64>, h: &mut H) {
    match v {
        None => h.write_u8(0),
        Some(x) => {
            h.write_u8(1);
            hash_f64(x, h);
        }
    }
}

#[inline]
fn hash_opt_u32<H: Hasher>(v: Option<u32>, h: &mut H) {
    match v {
        None => h.write_u8(0),
        Some(x) => {
            h.write_u8(1);
            x.hash(h);
        }
    }
}

#[inline]
fn hash_vec_f64<H: Hasher>(v: &[f64], h: &mut H) {
    h.write_usize(v.len());
    for x in v {
        hash_f64(*x, h);
    }
}

#[inline]
fn hash_point<H: Hasher>(p: Point2, h: &mut H) {
    hash_f64(p.x, h);
    hash_f64(p.y, h);
}


// ─── geometry ─────────────────────────────────────────────────────────

fn hash_segment<H: Hasher>(s: &Segment, h: &mut H) {
    let kind: u8 = match s.kind {
        SegmentKind::Line => 1,
        SegmentKind::Arc => 2,
        SegmentKind::Circle => 3,
        SegmentKind::Point => 4,
    };
    h.write_u8(kind);
    hash_point(s.start, h);
    hash_point(s.end, h);
    hash_f64(s.bulge, h);
    match s.center {
        None => h.write_u8(0),
        Some(c) => {
            h.write_u8(1);
            hash_point(c, h);
        }
    }
    s.layer.hash(h);
    s.color.hash(h);
}

// ─── tool ─────────────────────────────────────────────────────────────

fn hash_tool<H: Hasher>(t: &ToolEntry, h: &mut H) {
    t.id.hash(h);
    t.name.hash(h);
    let kind: u8 = match t.kind {
        ToolKind::Endmill => 1,
        ToolKind::BallNose => 2,
        ToolKind::VBit => 3,
        ToolKind::Engraver => 4,
        ToolKind::DragKnife => 5,
        ToolKind::Drill => 6,
        ToolKind::LaserBeam => 7,
        ToolKind::BullNose => 8,
        ToolKind::Compression => 9,
        ToolKind::TSlot => 10,
        ToolKind::FormProfile => 11,
    };
    h.write_u8(kind);
    hash_f64(t.diameter, h);
    hash_opt_f64(t.tip_diameter, h);
    hash_f64(t.tip_angle_deg, h);
    hash_opt_f64(t.dragoff, h);
    t.flutes.hash(h);
    t.speed.hash(h);
    t.plunge_rate.hash(h);
    t.feed_rate.hash(h);
    hash_opt_u32(t.speed_finish, h);
    hash_opt_u32(t.plunge_rate_finish, h);
    hash_opt_u32(t.feed_rate_finish, h);
    hash_opt_u32(t.speed_drill, h);
    hash_opt_u32(t.plunge_rate_drill, h);
    hash_opt_u32(t.feed_rate_drill, h);
    hash_opt_f64(t.default_peck_step_mm, h);
    hash_opt_f64(t.z_shift_mm, h);
    hash_opt_f64(t.laser_pierce_sec, h);
    hash_opt_f64(t.laser_lead_in_mm, h);
    hash_opt_f64(t.corner_radius_mm, h);
    hash_opt_f64(t.tslot_neck_diameter_mm, h);
    hash_opt_f64(t.tslot_neck_length_mm, h);
    t.wirbeln.hash(h);
    hash_opt_f64(t.wirbeln_stepover_mm, h);
    let coolant: u8 = match t.coolant {
        Coolant::Off => 0,
        Coolant::Mist => 1,
        Coolant::Flood => 2,
    };
    h.write_u8(coolant);
    hash_opt_f64(t.default_step, h);
    t.pause.hash(h);
    hash_opt_f64(t.flute_length_mm, h);
    hash_opt_f64(t.shank_diameter_mm, h);
    match t.holder {
        None => h.write_u8(0),
        Some(HolderShape::Cylinder {
            diameter_mm,
            length_mm,
        }) => {
            h.write_u8(1);
            hash_f64(diameter_mm, h);
            hash_f64(length_mm, h);
        }
        Some(HolderShape::Cone {
            bottom_diameter_mm,
            top_diameter_mm,
            length_mm,
        }) => {
            h.write_u8(2);
            hash_f64(bottom_diameter_mm, h);
            hash_f64(top_diameter_mm, h);
            hash_f64(length_mm, h);
        }
        Some(HolderShape::Stepped {
            cylinder_diameter_mm,
            cylinder_length_mm,
            cone_top_diameter_mm,
            cone_length_mm,
        }) => {
            h.write_u8(3);
            hash_f64(cylinder_diameter_mm, h);
            hash_f64(cylinder_length_mm, h);
            hash_f64(cone_top_diameter_mm, h);
            hash_f64(cone_length_mm, h);
        }
    }
}

// ─── machine ──────────────────────────────────────────────────────────

fn hash_machine<H: Hasher>(m: &MachineConfig, h: &mut H) {
    let unit: u8 = match m.unit {
        UnitSystem::Mm => 0,
        UnitSystem::Inch => 1,
    };
    h.write_u8(unit);
    let mode: u8 = match m.mode {
        MachineMode::Mill => 0,
        MachineMode::Laser => 1,
        MachineMode::Drag => 2,
    };
    h.write_u8(mode);
    m.comments.hash(h);
    m.arcs.hash(h);
    m.supports_toolchange.hash(h);
    // accel / jerk / toolchange_s / rapid_speed / use_kinematic_time_estimate
    // are intentionally NOT hashed: these fields drive the post-toolpath
    // time estimator (sim/timing.rs) only, not the emitted G-code body.
    // The estimator re-runs on every Generate regardless of cache hits,
    // so tweaking them updates the time estimate without invalidating
    // any cached op output (audit-4zf).
    hash_opt_f64(m.arc_fit_tolerance_mm, h);
    h.write_u32(m.decimal_separator as u32);
    hash_opt_u32(m.line_number_start, h);
    m.plot_mode_z.hash(h);
    // rt1.15: post-profile templates affect program output, so they
    // must invalidate the cache. None == absent variant byte.
    match &m.post_profile {
        None => h.write_u8(0),
        Some(p) => {
            h.write_u8(1);
            p.name.hash(h);
            p.file_extension.hash(h);
            p.line_ending.hash(h);
            p.program_start.hash(h);
            p.program_end.hash(h);
            p.tool_change.hash(h);
            p.coolant_flood_on.hash(h);
            p.coolant_flood_off.hash(h);
            p.coolant_mist_on.hash(h);
            p.coolant_mist_off.hash(h);
            // hev: per-axis output config. Hash each axis word so any
            // tweak (rename / scale / format / disable) invalidates the
            // cache. None == absent variant byte.
            match &p.axes {
                None => h.write_u8(0),
                Some(a) => {
                    h.write_u8(1);
                    for af in [&a.x, &a.y, &a.z, &a.i, &a.j, &a.feed, &a.speed] {
                        af.enabled.hash(h);
                        af.name.hash(h);
                        af.format.hash(h);
                        hash_f64(af.scale, h);
                    }
                }
            }
        }
    }
}

// ─── operation ────────────────────────────────────────────────────────

fn hash_operation<H: Hasher>(op: &Operation, h: &mut H) {
    op.id.hash(h);
    op.name.hash(h);
    op.enabled.hash(h);
    hash_operation_kind(op.kind, h);
    op.tool_id.hash(h);
    hash_opt_u32(op.finish_tool_id, h);
    hash_operation_source(&op.source, h);
    hash_operation_params(&op.params, h);
    match op.pattern {
        None => h.write_u8(0),
        Some(p) => {
            h.write_u8(1);
            hash_pattern(p, h);
        }
    }
}

fn hash_operation_kind<H: Hasher>(k: OperationKind, h: &mut H) {
    match k {
        OperationKind::Profile { offset } => {
            h.write_u8(1);
            h.write_u8(tool_offset_disc(offset));
        }
        OperationKind::Pocket { strategy } => {
            h.write_u8(2);
            hash_pocket_strategy(strategy, h);
        }
        OperationKind::Drill { cycle } => {
            h.write_u8(3);
            hash_drill_cycle(cycle, h);
        }
        OperationKind::Thread { pitch_mm, internal, climb } => {
            h.write_u8(4);
            hash_f64(pitch_mm, h);
            internal.hash(h);
            climb.hash(h);
        }
        OperationKind::Chamfer { width_mm, finish_pass } => {
            h.write_u8(5);
            hash_f64(width_mm, h);
            finish_pass.hash(h);
        }
        OperationKind::Engrave => h.write_u8(6),
        OperationKind::DragKnife => h.write_u8(7),
        OperationKind::Helix => h.write_u8(8),
        OperationKind::VCarve => h.write_u8(9),
    }
}

fn tool_offset_disc(o: ToolOffset) -> u8 {
    match o {
        ToolOffset::None => 0,
        ToolOffset::Outside => 1,
        ToolOffset::Inside => 2,
        ToolOffset::On => 3,
    }
}

fn hash_pocket_strategy<H: Hasher>(s: PocketStrategy, h: &mut H) {
    use crate::project::HalfpipeProfile;
    match s {
        PocketStrategy::Cascade => h.write_u8(0),
        PocketStrategy::Zigzag => h.write_u8(1),
        PocketStrategy::Spiral => h.write_u8(2),
        PocketStrategy::Trochoidal {
            engagement_angle_deg,
            loop_radius_factor,
        } => {
            h.write_u8(3);
            hash_f64(engagement_angle_deg, h);
            hash_f64(loop_radius_factor, h);
        }
        PocketStrategy::Halfpipe { profile } => {
            h.write_u8(4);
            match profile {
                HalfpipeProfile::CircularArc { radius_mm } => {
                    h.write_u8(0);
                    hash_f64(radius_mm, h);
                }
                HalfpipeProfile::VBottom { included_angle_deg } => {
                    h.write_u8(1);
                    hash_f64(included_angle_deg, h);
                }
            }
        }
    }
}

fn hash_drill_cycle<H: Hasher>(c: DrillCycle, h: &mut H) {
    match c {
        DrillCycle::Simple { dwell_sec } => {
            h.write_u8(0);
            hash_f64(dwell_sec, h);
        }
        DrillCycle::Peck {
            peck_step_mm,
            dwell_sec,
        } => {
            h.write_u8(1);
            hash_f64(peck_step_mm, h);
            hash_f64(dwell_sec, h);
        }
        DrillCycle::ChipBreak {
            peck_step_mm,
            dwell_sec,
        } => {
            h.write_u8(2);
            hash_f64(peck_step_mm, h);
            hash_f64(dwell_sec, h);
        }
    }
}

fn hash_operation_source<H: Hasher>(s: &OperationSource, h: &mut H) {
    match s {
        OperationSource::All => h.write_u8(0),
        OperationSource::Layers { layers, combine } => {
            h.write_u8(1);
            h.write_usize(layers.len());
            for l in layers {
                l.hash(h);
            }
            h.write_u8(combine_disc(*combine));
        }
        OperationSource::Objects { ids, combine } => {
            h.write_u8(2);
            h.write_usize(ids.len());
            for id in ids {
                id.hash(h);
            }
            h.write_u8(combine_disc(*combine));
        }
    }
}

fn combine_disc(c: SourceCombine) -> u8 {
    match c {
        SourceCombine::Auto => 0,
        SourceCombine::Union => 1,
        SourceCombine::Difference => 2,
        SourceCombine::Intersection => 3,
        SourceCombine::Xor => 4,
        SourceCombine::None => 5,
    }
}

fn hash_pattern<H: Hasher>(p: PatternConfig, h: &mut H) {
    match p {
        PatternConfig::Linear { count, dx, dy } => {
            h.write_u8(0);
            count.hash(h);
            hash_f64(dx, h);
            hash_f64(dy, h);
        }
        PatternConfig::Grid {
            count_x,
            count_y,
            dx,
            dy,
        } => {
            h.write_u8(1);
            count_x.hash(h);
            count_y.hash(h);
            hash_f64(dx, h);
            hash_f64(dy, h);
        }
        PatternConfig::Polar {
            count,
            center_x,
            center_y,
            angle_step_deg,
            start_angle_deg,
        } => {
            h.write_u8(2);
            count.hash(h);
            hash_f64(center_x, h);
            hash_f64(center_y, h);
            hash_f64(angle_step_deg, h);
            hash_f64(start_angle_deg, h);
        }
    }
}

fn hash_operation_params<H: Hasher>(p: &OperationParams, h: &mut H) {
    hash_f64(p.depth, h);
    hash_f64(p.start_depth, h);
    hash_opt_f64(p.step, h);
    hash_f64(p.fast_move_z, h);
    hash_f64(p.xy_overlap, h);
    p.helix.hash(h);
    p.reverse.hash(h);
    let oo: u8 = match p.objectorder {
        ObjectOrder::Nearest => 0,
        ObjectOrder::PerObject => 1,
        ObjectOrder::Unordered => 2,
    };
    h.write_u8(oo);
    p.overcut.hash(h);
    p.pocket_islands.hash(h);
    p.pocket_nocontour.hash(h);
    p.pocket_insideout.hash(h);
    hash_tabs(&p.tabs, h);
    hash_tab_mode(p.tab_mode, h);
    h.write_usize(p.tab_placements.len());
    for tp in &p.tab_placements {
        tp.object_id.hash(h);
        hash_f64(tp.t, h);
        hash_opt_f64(tp.width_override_mm, h);
        hash_opt_f64(tp.height_override_mm, h);
    }
    hash_leads(&p.leads, h);
    h.write_u8(cut_direction_disc(p.cut_direction));
    h.write_u8(cut_direction_disc(p.finish_cut_direction));
    hash_plunge(p.plunge, h);
    hash_opt_u32(p.feed_rate_override, h);
    hash_opt_u32(p.plunge_rate_override, h);
    hash_f64(p.corner_feed_reduction, h);
    hash_opt_f64(p.finish_step, h);
    hash_opt_f64(p.finish_xy_allowance_mm, h);
    hash_opt_f64(p.chamfer_after_width_mm, h);
    match p.approach_point {
        None => h.write_u8(0),
        Some((x, y)) => {
            h.write_u8(1);
            hash_f64(x, h);
            hash_f64(y, h);
        }
    }
    hash_f64(p.through_depth, h);
    hash_vec_f64(&p.depth_list, h);
    hash_opt_f64(p.carve_max_width_mm, h);
    p.multi_pass_refine.hash(h);
    match p.frame_shape {
        None => h.write_u8(0),
        Some(FrameShape::Rectangle) => h.write_u8(1),
        Some(FrameShape::RoundedRectangle) => h.write_u8(2),
    }
    hash_opt_f64(p.frame_padding_mm, h);
    hash_opt_f64(p.frame_corner_radius_mm, h);
}

fn cut_direction_disc(c: CutDirection) -> u8 {
    match c {
        CutDirection::Conventional => 0,
        CutDirection::Climb => 1,
    }
}

fn hash_plunge<H: Hasher>(p: PlungeStrategy, h: &mut H) {
    match p {
        PlungeStrategy::Direct => h.write_u8(0),
        PlungeStrategy::Ramp { angle_deg } => {
            h.write_u8(1);
            hash_f64(angle_deg, h);
        }
        PlungeStrategy::Helix {
            angle_deg,
            radius_mm,
        } => {
            h.write_u8(2);
            hash_f64(angle_deg, h);
            hash_opt_f64(radius_mm, h);
        }
    }
}

fn hash_tabs<H: Hasher>(t: &TabsConfig, h: &mut H) {
    t.active.hash(h);
    hash_f64(t.width, h);
    hash_f64(t.height, h);
    let tt: u8 = match t.tab_type {
        TabType::Rectangle => 0,
        TabType::Ramp => 1,
    };
    h.write_u8(tt);
    hash_f64(t.ramp_angle_deg, h);
}

fn hash_tab_mode<H: Hasher>(m: crate::project::TabPlacementMode, h: &mut H) {
    use crate::project::TabPlacementMode;
    match m {
        TabPlacementMode::Off => h.write_u8(0),
        TabPlacementMode::Auto { count } => {
            h.write_u8(1);
            count.hash(h);
        }
        TabPlacementMode::Manual => h.write_u8(2),
        TabPlacementMode::Mixed { auto_count } => {
            h.write_u8(3);
            auto_count.hash(h);
        }
    }
}

fn hash_leads<H: Hasher>(l: &LeadsConfig, h: &mut H) {
    h.write_u8(lead_kind_disc(l.r#in));
    h.write_u8(lead_kind_disc(l.out));
    hash_f64(l.in_lenght, h);
    hash_f64(l.out_lenght, h);
}

fn lead_kind_disc(k: LeadKind) -> u8 {
    match k {
        LeadKind::Off => 0,
        LeadKind::Straight => 1,
        LeadKind::Arc => 2,
    }
}

// ─── tabs map (helper for callers caching the project-level tabs for an op) ───

// ─── fixtures ─────────────────────────────────────────────────────────

fn hash_fixture<H: Hasher>(f: &Fixture, h: &mut H) {
    f.id.hash(h);
    f.name.hash(h);
    match &f.kind {
        FixtureKind::Box { width, depth } => {
            h.write_u8(0);
            hash_f64(*width, h);
            hash_f64(*depth, h);
        }
        FixtureKind::Cylinder { radius } => {
            h.write_u8(1);
            hash_f64(*radius, h);
        }
        FixtureKind::Polygon { vertices } => {
            h.write_u8(2);
            h.write_usize(vertices.len());
            for (x, y) in vertices {
                hash_f64(*x, h);
                hash_f64(*y, h);
            }
        }
    }
    hash_f64(f.origin.0, h);
    hash_f64(f.origin.1, h);
    hash_f64(f.z_bottom, h);
    hash_f64(f.z_top, h);
    f.color.hash(h);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::setup::ToolOffset;
    use crate::project::{
        Operation, OperationKind, OperationParams, OperationSource, ToolEntry, ToolKind,
    };

    fn endmill() -> ToolEntry {
        ToolEntry {
            id: 1,
            name: "3 mm endmill".into(),
            kind: ToolKind::Endmill,
            diameter: 3.0,
            tip_diameter: None,
            tip_angle_deg: 60.0,
            dragoff: None,
            flutes: 2,
            speed: 18_000,
            plunge_rate: 100,
            feed_rate: 800,
            coolant: Coolant::Off,
            speed_finish: None,
            plunge_rate_finish: None,
            feed_rate_finish: None,
            speed_drill: None,
            plunge_rate_drill: None,
            feed_rate_drill: None,
            default_peck_step_mm: None,
            default_step: None,
            z_shift_mm: None,
            laser_pierce_sec: None,
            laser_lead_in_mm: None,
            corner_radius_mm: None,
            tslot_neck_diameter_mm: None,
            tslot_neck_length_mm: None,
            wirbeln: false,
            wirbeln_stepover_mm: None,
            pause: 1,
            flute_length_mm: None,
            shank_diameter_mm: None,
            holder: None,
        }
    }

    fn profile_op() -> Operation {
        Operation {
            id: 1,
            name: "Profile".into(),
            enabled: true,
            kind: OperationKind::Profile {
                offset: ToolOffset::Outside,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OperationSource::All,
            params: OperationParams::mill_default(),
            pattern: None,
        }
    }

    fn square(side: f64) -> Vec<Segment> {
        vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(side, 0.0), "0", 7),
            Segment::line(Point2::new(side, 0.0), Point2::new(side, side), "0", 7),
            Segment::line(Point2::new(side, side), Point2::new(0.0, side), "0", 7),
            Segment::line(Point2::new(0.0, side), Point2::new(0.0, 0.0), "0", 7),
        ]
    }

    /// Snapshot of the stable hash for a known-fixed input. If this test
    /// fails after a refactor, you either:
    /// (a) re-ordered fields in a Hash impl (BUG — fix the impl), or
    /// (b) intentionally added a field to a Hash impl AND bumped
    ///     PIPELINE_VERSION (LEGITIMATE — update the snapshot below).
    /// Never silently update the snapshot without auditing why it
    /// changed.
    #[test]
    fn stable_hash_regression() {
        let key = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &square(20.0),
            &[],
            0,
        );
        // Snapshot — bump PIPELINE_VERSION when this legitimately changes.
        assert_eq!(key.0, 0xc548_f2ef_ca7d_9550_u64, "got {:#018x}", key.0);
    }

    #[test]
    fn same_op_same_key() {
        let segs = square(20.0);
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], 0);
        let k2 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], 0);
        assert_eq!(k1, k2);
    }

    #[test]
    fn depth_change_changes_key() {
        let segs = square(20.0);
        let mut op2 = profile_op();
        op2.params.depth -= 0.1;
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], 0);
        let k2 = op_cache_key(&op2, &endmill(), &MachineConfig::default(), &segs, &[], 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn tool_diameter_changes_key() {
        let segs = square(20.0);
        let mut t2 = endmill();
        t2.diameter = 6.0;
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], 0);
        let k2 = op_cache_key(&profile_op(), &t2, &MachineConfig::default(), &segs, &[], 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn segments_change_changes_key() {
        let s1 = square(20.0);
        let s2 = square(25.0);
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &s1, &[], 0);
        let k2 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &s2, &[], 0);
        assert_ne!(k1, k2);
    }

    /// rt1.10 — tabs now live on the OP (`op.params.tab_placements`),
    /// so the cache-invalidation test exercises that path: bumping
    /// a tab in op.params changes hash_operation, which changes the
    /// key. Verified separately by `tab_mode_change_changes_key`
    /// below.
    #[test]
    fn fixtures_change_changes_key() {
        let segs = square(20.0);
        let fx = vec![Fixture {
            id: 1,
            name: "clamp".into(),
            kind: FixtureKind::Box { width: 30.0, depth: 50.0 },
            origin: (15.0, -25.0),
            z_bottom: 0.0,
            z_top: 12.0,
            color: 0xFFA0_50C0,
        }];
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], 0);
        let k2 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &fx, 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn post_processor_tag_changes_key() {
        let segs = square(20.0);
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], 0);
        let k2 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], 1);
        assert_ne!(k1, k2);
    }

    /// Regression for audit-4zf: estimator-only MachineConfig fields
    /// (accel, jerk, toolchange_s, rapid_speed,
    /// use_kinematic_time_estimate) do not affect the emitted G-code
    /// and must NOT be folded into the per-op cache key. Tweaking the
    /// estimator should hit the cache for every op and only re-run the
    /// (cheap) post-toolpath time estimator.
    #[test]
    fn estimator_only_machine_fields_do_not_change_key() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let base = MachineConfig::default();
        let key_base = op_cache_key(&op, &tool, &base, &segs, &[], 0);

        // Each clone tweaks one estimator-only field.
        let mut m_accel = base.clone();
        m_accel.accel = Some(crate::cam::setup::AxisLimits { x: 5000.0, y: 5000.0, z: 1000.0 });
        let mut m_jerk = base.clone();
        m_jerk.jerk = Some(crate::cam::setup::AxisLimits { x: 1500.0, y: 1500.0, z: 500.0 });
        let mut m_tc = base.clone();
        m_tc.toolchange_s = 12.5;
        let mut m_rapid = base.clone();
        m_rapid.rapid_speed = Some(9000.0);
        let mut m_kin = base.clone();
        m_kin.use_kinematic_time_estimate = !base.use_kinematic_time_estimate;

        for (name, m) in [
            ("accel", &m_accel),
            ("jerk", &m_jerk),
            ("toolchange_s", &m_tc),
            ("rapid_speed", &m_rapid),
            ("use_kinematic", &m_kin),
        ] {
            let key = op_cache_key(&op, &tool, m, &segs, &[], 0);
            assert_eq!(key, key_base, "estimator-only field {name} changed the cache key");
        }
    }

    /// Sanity: bumping PIPELINE_VERSION must change every cache key for
    /// the same logical input. Asserted by computing the key with the
    /// real constant and a manual hasher with `PIPELINE_VERSION + 1`.
    #[test]
    fn pipeline_version_bumps_invalidate() {
        let op = profile_op();
        let tool = endmill();
        let machine = MachineConfig::default();
        let segs = square(20.0);
        let real = op_cache_key(&op, &tool, &machine, &segs, &[], 0);

        let mut h = SeaHasher::new();
        (PIPELINE_VERSION + 1).hash(&mut h);
        h.write_u8(0);
        hash_operation(&op, &mut h);
        hash_tool(&tool, &mut h);
        h.write_u8(0); // no finish tool
        hash_machine(&machine, &mut h);
        h.write_usize(segs.len());
        for s in &segs {
            hash_segment(s, &mut h);
        }
        h.write_usize(0);
        let bumped = OpCacheKey(h.finish());
        assert_ne!(real, bumped);
    }

    #[test]
    fn lru_capacity_eviction() {
        let cache = PipelineCache::new(200);
        for i in 0..250u64 {
            cache.put(
                OpCacheKey(i),
                OpCacheValue {
                    toolpath: Vec::new(),
                    gcode_body: format!("op{i}"),
                    closed_count: 0,
                    offset_count: 0,
                    exit_state: CapturedPostState::default(),
                    exit_xy: (0.0, 0.0),
                },
            );
        }
        assert_eq!(cache.len(), 200);
        // The first 50 inserts (keys 0..50) should have been evicted.
        for i in 0..50u64 {
            assert!(cache.get(OpCacheKey(i)).is_none(), "key {i} was not evicted");
        }
        // The latest 200 (keys 50..250) should still be present.
        for i in 50..250u64 {
            assert!(cache.get(OpCacheKey(i)).is_some(), "key {i} was evicted");
        }
    }

    #[test]
    fn put_then_get_round_trips() {
        let cache = PipelineCache::new(10);
        let v = OpCacheValue {
            toolpath: Vec::new(),
            gcode_body: "G1 X1 Y2".into(),
            closed_count: 3,
            offset_count: 4,
            exit_state: CapturedPostState::default(),
            exit_xy: (0.0, 0.0),
        };
        cache.put(OpCacheKey(42), v.clone());
        let got = cache.get(OpCacheKey(42)).expect("hit");
        assert_eq!(got.gcode_body, v.gcode_body);
        assert_eq!(got.closed_count, v.closed_count);
        assert_eq!(got.offset_count, v.offset_count);
    }

    /// rt1.10: changing op.tab_mode invalidates the cache (tab_mode
    /// is hashed via hash_operation_params).
    #[test]
    fn tab_mode_change_changes_key() {
        let segs = square(20.0);
        let mut op_b = profile_op();
        op_b.params.tab_mode = crate::project::TabPlacementMode::Auto { count: 4 };
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], 0);
        let k2 = op_cache_key(&op_b, &endmill(), &MachineConfig::default(), &segs, &[], 0);
        assert_ne!(k1, k2);
    }
}
