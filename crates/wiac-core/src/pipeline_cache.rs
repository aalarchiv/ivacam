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

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
use seahash::SeaHasher;

use crate::cam::offsets::TabPoint;
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
pub const PIPELINE_VERSION: u32 = 1;

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
    tabs: &HashMap<u32, Vec<TabPoint>>,
    post_processor_tag: u8,
) -> OpCacheKey {
    let mut h = SeaHasher::new();
    PIPELINE_VERSION.hash(&mut h);
    h.write_u8(post_processor_tag);
    hash_operation(op, &mut h);
    hash_tool(tool, &mut h);
    hash_machine(machine, &mut h);
    h.write_usize(selected_segments.len());
    for seg in selected_segments {
        hash_segment(seg, &mut h);
    }
    h.write_usize(fixtures.len());
    for fx in fixtures {
        hash_fixture(fx, &mut h);
    }
    hash_tabs_map(tabs, &mut h);
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

fn hash_axis_limits_opt<H: Hasher>(v: Option<&AxisLimits>, h: &mut H) {
    match v {
        None => h.write_u8(0),
        Some(a) => {
            h.write_u8(1);
            hash_f64(a.x, h);
            hash_f64(a.y, h);
            hash_f64(a.z, h);
        }
    }
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
    hash_axis_limits_opt(m.accel.as_ref(), h);
    hash_axis_limits_opt(m.jerk.as_ref(), h);
    hash_f64(m.toolchange_s, h);
    hash_opt_f64(m.rapid_speed, h);
    m.use_kinematic_time_estimate.hash(h);
}

// ─── operation ────────────────────────────────────────────────────────

fn hash_operation<H: Hasher>(op: &Operation, h: &mut H) {
    op.id.hash(h);
    op.name.hash(h);
    op.enabled.hash(h);
    hash_operation_kind(op.kind, h);
    op.tool_id.hash(h);
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
        OperationKind::Thread => h.write_u8(4),
        OperationKind::Chamfer => h.write_u8(5),
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
        } => {
            h.write_u8(2);
            count.hash(h);
            hash_f64(center_x, h);
            hash_f64(center_y, h);
            hash_f64(angle_step_deg, h);
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
    hash_leads(&p.leads, h);
    h.write_u8(cut_direction_disc(p.cut_direction));
    h.write_u8(cut_direction_disc(p.finish_cut_direction));
    hash_plunge(p.plunge, h);
    hash_opt_u32(p.feed_rate_override, h);
    hash_opt_u32(p.plunge_rate_override, h);
    hash_f64(p.corner_feed_reduction, h);
    hash_opt_f64(p.finish_step, h);
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

/// Hash a `HashMap<u32, Vec<TabPoint>>` with sorted keys so HashMap
/// iteration order doesn't leak into the cache key. Exposed for the
/// pipeline driver — when an op's source set covers segments that
/// carry tab placements, those tabs become part of the op's input.
pub fn hash_tabs_map<H: Hasher>(tabs: &HashMap<u32, Vec<TabPoint>>, h: &mut H) {
    let mut keys: Vec<u32> = tabs.keys().copied().collect();
    keys.sort_unstable();
    h.write_usize(keys.len());
    for k in keys {
        k.hash(h);
        let v = &tabs[&k];
        h.write_usize(v.len());
        for tp in v {
            hash_f64(tp.x, h);
            hash_f64(tp.y, h);
        }
    }
}

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
            default_step: None,
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
            &HashMap::new(),
            0,
        );
        // Snapshot — bump PIPELINE_VERSION when this legitimately changes.
        assert_eq!(key.0, 0x4141_40b4_955f_998e_u64, "got {:#018x}", key.0);
    }

    #[test]
    fn same_op_same_key() {
        let segs = square(20.0);
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], &HashMap::new(), 0);
        let k2 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], &HashMap::new(), 0);
        assert_eq!(k1, k2);
    }

    #[test]
    fn depth_change_changes_key() {
        let segs = square(20.0);
        let mut op2 = profile_op();
        op2.params.depth -= 0.1;
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], &HashMap::new(), 0);
        let k2 = op_cache_key(&op2, &endmill(), &MachineConfig::default(), &segs, &[], &HashMap::new(), 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn tool_diameter_changes_key() {
        let segs = square(20.0);
        let mut t2 = endmill();
        t2.diameter = 6.0;
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], &HashMap::new(), 0);
        let k2 = op_cache_key(&profile_op(), &t2, &MachineConfig::default(), &segs, &[], &HashMap::new(), 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn segments_change_changes_key() {
        let s1 = square(20.0);
        let s2 = square(25.0);
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &s1, &[], &HashMap::new(), 0);
        let k2 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &s2, &[], &HashMap::new(), 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn tabs_change_changes_key() {
        let segs = square(20.0);
        let mut t: HashMap<u32, Vec<TabPoint>> = HashMap::new();
        t.insert(0u32, vec![TabPoint { x: 5.0, y: 0.0 }]);
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], &HashMap::new(), 0);
        let k2 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], &t, 0);
        assert_ne!(k1, k2);
    }

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
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], &HashMap::new(), 0);
        let k2 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &fx, &HashMap::new(), 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn post_processor_tag_changes_key() {
        let segs = square(20.0);
        let k1 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], &HashMap::new(), 0);
        let k2 = op_cache_key(&profile_op(), &endmill(), &MachineConfig::default(), &segs, &[], &HashMap::new(), 1);
        assert_ne!(k1, k2);
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
        let real = op_cache_key(&op, &tool, &machine, &segs, &[], &HashMap::new(), 0);

        let mut h = SeaHasher::new();
        (PIPELINE_VERSION + 1).hash(&mut h);
        h.write_u8(0);
        hash_operation(&op, &mut h);
        hash_tool(&tool, &mut h);
        hash_machine(&machine, &mut h);
        h.write_usize(segs.len());
        for s in &segs {
            hash_segment(s, &mut h);
        }
        h.write_usize(0);
        hash_tabs_map(&HashMap::new(), &mut h);
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

    #[test]
    fn hash_tabs_map_is_order_independent() {
        let mut a: HashMap<u32, Vec<TabPoint>> = HashMap::new();
        a.insert(1, vec![TabPoint { x: 1.0, y: 2.0 }]);
        a.insert(2, vec![TabPoint { x: 3.0, y: 4.0 }]);
        a.insert(3, vec![TabPoint { x: 5.0, y: 6.0 }]);
        // Same data, different insertion order.
        let mut b: HashMap<u32, Vec<TabPoint>> = HashMap::new();
        b.insert(3, vec![TabPoint { x: 5.0, y: 6.0 }]);
        b.insert(1, vec![TabPoint { x: 1.0, y: 2.0 }]);
        b.insert(2, vec![TabPoint { x: 3.0, y: 4.0 }]);
        let mut ha = SeaHasher::new();
        let mut hb = SeaHasher::new();
        hash_tabs_map(&a, &mut ha);
        hash_tabs_map(&b, &mut hb);
        assert_eq!(ha.finish(), hb.finish());
    }
}
