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
//! NaN out of `HashMap` keys), so every Hash impl is hand-written.
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
    LeadKind, LeadsConfig, MachineConfig, MachineMode, ObjectOrder, PlungeStrategy, TabType,
    TabsConfig, ToolOffset, UnitSystem,
};
use crate::cam::source_combine::FrameShape;
use crate::gcode::preview::ToolpathSegment;
use crate::gcode::CapturedPostState;
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::project::{
    ContourParams, Coolant, CutDirection, DrillCycle, Fixture, FixtureKind, HolderShape, Op,
    OpKind, OpParams, OpSource, PatternConfig, PocketParams, PocketStrategy, ProfileParams,
    SourceCombine, SpindleDirection, TextAlignment, TextLayer, TextLayerKind, ToolEntry, ToolKind,
    VCarveParams, Wcs, WorkOffset,
};

/// Bumped when ANY pipeline output format changes — toolpath segment
/// shape, gcode formatting, anything. Invalidates the whole cache.
pub const PIPELINE_VERSION: u32 = 34;

/// Stable hash of (op, tool, machine, selected segments, fixtures, and
/// [`PIPELINE_VERSION`]). Wrapper so callers can't accidentally pass an
/// unrelated `u64` to [`PipelineCache::get`] / [`PipelineCache::put`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpCacheKey(pub u64);

/// Cached per-op output. The header / footer and program-level state
/// are NOT cached — only this op's body.
#[derive(Debug, Clone)]
pub struct OpCacheValue {
    /// Per-op slice of toolpath segments produced by this op (`op_id`
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
    /// nguf: `true` when the cached body contains an internal dual-tool
    /// toolchange envelope (rough→finish, drill→chamfer). Lets the
    /// per-op driver replay the correct `prev_tool_id` bias on a cache
    /// hit — without this, an op that declared a finish tool but
    /// produced no finish offsets would still pessimistically bias the
    /// next op to the finish id, causing the next same-rough-tool op
    /// to skip its M6 envelope and run with the wrong tool.
    pub internal_swap_emitted: bool,
}

#[derive(Debug)]
pub struct PipelineCache {
    inner: Mutex<LruCache<u64, OpCacheValue>>,
}

impl PipelineCache {
    #[must_use]
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
/// segments + fixtures + tabs + `post_processor_tag` + `PIPELINE_VERSION`).
/// Stable across runs and across builds (modulo `PIPELINE_VERSION`
/// bumps). `post_processor_tag` is the caller's post-processor
/// discriminant — different posts produce different gcode for the same
/// inputs, so they must key separately. `tabs` carries the project's
/// segment-keyed tab placements — changing tab positions changes the
/// per-op output, so they must be part of the key.
///
/// Legacy callers that don't fold text-layer content into the key get an
/// empty `text_layers` slice (back-compat — same hash as before for
/// projects without text). Callers that DO consume text_layers should
/// route through [`op_cache_key_with_finish`] directly so font / size /
/// content / placement changes invalidate the cached gcode.
#[must_use]
pub fn op_cache_key(
    op: &Op,
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
        &[],
        &WorkOffset::default(),
        post_processor_tag,
    )
}

/// Cache-key constructor that folds a SECOND tool's entry into the
/// hash — used for dual-tool Pocket ops (rt1.33) so changes to the
/// finish tool's diameter / `feed_rate_finish` / etc. invalidate the
/// cache. Pass `finish_tool = None` for single-tool ops (legacy
/// callers route through [`op_cache_key`]).
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn op_cache_key_with_finish(
    op: &Op,
    tool: &ToolEntry,
    finish_tool: Option<&ToolEntry>,
    machine: &MachineConfig,
    selected_segments: &[Segment],
    fixtures: &[Fixture],
    text_layers: &[TextLayer],
    work_offset: &WorkOffset,
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
    // sqa3: fold the consumed text layers into the key. Edits to font /
    // content / size / placement / alignment must invalidate the cache
    // (otherwise the user changes the engraving text and Generate
    // happily serves the old gcode). Conservative — we hash every
    // text_layer rather than try to narrow to the layers this op
    // actually consumes via `OpSource::Layers { __text_<id> }`. The
    // extra discrimination is cheap and avoids miss-and-stale hits when
    // a layer renames between runs.
    h.write_usize(text_layers.len());
    for tl in text_layers {
        hash_text_layer(tl, &mut h);
    }
    // ls7y: project work_offset (xyz + WCS selector) is consulted by
    // sim alignment + the WCS-origin warning today, and on the roadmap
    // it will drive G10 L20 / G54..G59 emission. Hash it now so that
    // when WCS-driven emission lands, cached gcode for ops authored
    // against a different work_offset is correctly invalidated. Cheap:
    // 3 f64s + 1 discriminant byte per op.
    hash_work_offset(work_offset, &mut h);
    OpCacheKey(h.finish())
}

#[inline]
fn hash_work_offset<H: Hasher>(w: &WorkOffset, h: &mut H) {
    hash_f64(w.x_mm, h);
    hash_f64(w.y_mm, h);
    hash_f64(w.z_mm, h);
    let wcs: u8 = match w.wcs {
        Wcs::G54 => 0,
        Wcs::G55 => 1,
        Wcs::G56 => 2,
        Wcs::G57 => 3,
        Wcs::G58 => 4,
        Wcs::G59 => 5,
    };
    h.write_u8(wcs);
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
    hash_opt_f64(t.wirbeln_extra_width_mm, h);
    hash_opt_f64(t.wirbeln_osc_mm, h);
    let coolant: u8 = match t.coolant {
        Coolant::Off => 0,
        Coolant::Mist => 1,
        Coolant::Flood => 2,
    };
    h.write_u8(coolant);
    hash_opt_f64(t.default_step, h);
    hash_opt_f64(t.default_xy_overlap, h);
    t.pause.hash(h);
    // scwx: kerf_mm (laser spot diameter) carves into the heightmap at
    // emission time; stickout_length_mm controls the holder/shank
    // clearance check geometry; spindle_direction routes the post
    // between M3 / M4 — all three change emitted output and must
    // invalidate the cache.
    hash_opt_f64(t.kerf_mm, h);
    hash_opt_f64(t.stickout_length_mm, h);
    let sdir: u8 = match t.spindle_direction {
        SpindleDirection::Cw => 0,
        SpindleDirection::Ccw => 1,
    };
    h.write_u8(sdir);
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
        // zpuk: Plasma — keep the discriminant numerically distinct so
        // cached output isn't shared with Drag (different Z dance:
        // pierce-height → dwell → cut-height vs. plot-mode single-Z).
        MachineMode::Plasma => 3,
    };
    h.write_u8(mode);
    m.comments.hash(h);
    m.arcs.hash(h);
    m.supports_toolchange.hash(h);
    // ul60: name + work_area + capabilities. `name` rides into emitted
    // comments on some posts. `work_area` is consulted by the soft-limit
    // sim and the auto-stock fallback; tweaking it after a cache hit
    // would skip the warning re-check. `capabilities` gates the
    // frontend's op picker but ALSO future-proofs the cache against a
    // machine being repurposed (laser → mill) without changing `mode`.
    m.name.hash(h);
    hash_f64(m.work_area.x, h);
    hash_f64(m.work_area.y, h);
    hash_f64(m.work_area.z, h);
    h.write_usize(m.capabilities.len());
    for cap in &m.capabilities {
        let cap_disc: u8 = match cap {
            MachineMode::Mill => 0,
            MachineMode::Laser => 1,
            MachineMode::Drag => 2,
            MachineMode::Plasma => 3,
        };
        h.write_u8(cap_disc);
    }
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
    // 3nnj: RPM clamp window changes whether an emitted S<rpm> is
    // capped / floored (and triggers the matching warning lane), so
    // it MUST invalidate the cache.
    hash_opt_u32(m.spindle_rpm_min, h);
    hash_opt_u32(m.spindle_rpm_max, h);
    // eaeq / m8sq: toolchange spindle stop/start dwells are emitted
    // verbatim as G4 P<sec> lines around M5/M3 in the M6 envelope.
    hash_opt_f64(m.spindle_stop_dwell_sec, h);
    hash_opt_f64(m.spindle_start_dwell_sec, h);
    // syol: program_end footer routing — park_at_home flips on the
    // G53 G0 X0 Y0 retract, park_xy overrides it with an explicit
    // WCS-coord point. Both materially change the emitted footer.
    m.park_at_home.hash(h);
    match m.park_xy {
        None => h.write_u8(0),
        Some((x, y)) => {
            h.write_u8(1);
            hash_f64(x, h);
            hash_f64(y, h);
        }
    }
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

fn hash_operation<H: Hasher>(op: &Op, h: &mut H) {
    op.id.hash(h);
    op.name.hash(h);
    op.enabled.hash(h);
    // kbx5 step 3: kind-specific params (contour, pocket, profile,
    // vcarve, drill pattern + chamfer_after) are hashed inside
    // hash_operation_kind alongside the variant discriminator. The
    // remaining `params` carries only universal fields.
    hash_operation_kind(&op.kind, h);
    op.tool_id.hash(h);
    hash_opt_u32(op.finish_tool_id, h);
    hash_operation_source(&op.source, h);
    hash_operation_params(&op.params, h);
}

fn hash_operation_kind<H: Hasher>(k: &OpKind, h: &mut H) {
    match k {
        OpKind::Profile {
            offset,
            contour,
            profile,
        } => {
            h.write_u8(1);
            h.write_u8(tool_offset_disc(*offset));
            hash_contour_params(contour, h);
            hash_profile_params(*profile, h);
        }
        OpKind::Pocket {
            strategy,
            contour,
            pocket,
        } => {
            h.write_u8(2);
            hash_pocket_strategy(*strategy, h);
            hash_contour_params(contour, h);
            hash_pocket_params(pocket, h);
        }
        OpKind::Drill {
            cycle,
            chamfer_after_width_mm,
            pattern,
            spot_first,
        } => {
            h.write_u8(3);
            hash_drill_cycle(*cycle, h);
            hash_opt_f64(*chamfer_after_width_mm, h);
            match pattern {
                None => h.write_u8(0),
                Some(p) => {
                    h.write_u8(1);
                    hash_pattern(*p, h);
                }
            }
            // r2af: spot pre-pass is part of the op's emission shape;
            // changing either depth or tool MUST invalidate the cache.
            match spot_first {
                None => h.write_u8(0),
                Some(s) => {
                    h.write_u8(1);
                    s.spot_depth_mm.to_bits().hash(h);
                    h.write_u32(s.spot_tool_id);
                }
            }
        }
        OpKind::Thread {
            pitch_mm,
            internal,
            climb,
            radial_passes,
            start_angle_rad,
            thread_depth_mm,
        } => {
            h.write_u8(4);
            hash_f64(*pitch_mm, h);
            internal.hash(h);
            climb.hash(h);
            // mniu: thread_depth_mm changes the helix radius, so
            // it MUST hash into the cache key. `None` hashes to a
            // discriminant byte distinct from any `Some(d)` so
            // legacy entries (None → ISO default) stay separate
            // from explicit pins.
            match thread_depth_mm {
                None => h.write_u8(0),
                Some(d) => {
                    h.write_u8(1);
                    hash_f64(*d, h);
                }
            }
            // sqnh: radial roughing-pass schedule changes the emitted
            // helix bodies (one helix per pass vs. one at full
            // engagement). 6uns: start_angle_rad rotates the helix
            // start point so partial-thread restarts can pick up
            // where a prior run stopped. Both materially change the
            // emitted gcode and so must invalidate cache hits.
            radial_passes.hash(h);
            hash_f64(*start_angle_rad, h);
        }
        OpKind::Chamfer {
            width_mm,
            finish_pass,
        } => {
            h.write_u8(5);
            hash_f64(*width_mm, h);
            finish_pass.hash(h);
        }
        OpKind::Engrave { contour } => {
            h.write_u8(6);
            hash_contour_params(contour, h);
        }
        OpKind::DragKnife { contour } => {
            h.write_u8(7);
            hash_contour_params(contour, h);
        }
        OpKind::Helix => h.write_u8(8),
        OpKind::Pause { message } => {
            h.write_u8(10);
            message.hash(h);
        }
        OpKind::VCarve { carve } => {
            h.write_u8(9);
            hash_vcarve_params(carve, h);
        }
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
        PocketStrategy::Zigzag { angle_deg } => {
            h.write_u8(1);
            // rt1.9: only fold the angle when non-zero so legacy zigzag
            // ops (angle = 0 default) keep their pre-rt1.9 hash and
            // continue hitting the in-process cache without
            // PIPELINE_VERSION churn.
            if angle_deg.abs() >= 1e-9 {
                hash_f64(angle_deg, h);
            }
        }
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

fn hash_operation_source<H: Hasher>(s: &OpSource, h: &mut H) {
    match s {
        OpSource::All => h.write_u8(0),
        OpSource::Layers { layers, combine } => {
            h.write_u8(1);
            h.write_usize(layers.len());
            for l in layers {
                l.hash(h);
            }
            h.write_u8(combine_disc(*combine));
        }
        OpSource::Objects { ids, combine } => {
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

fn hash_operation_params<H: Hasher>(p: &OpParams, h: &mut H) {
    hash_f64(p.depth, h);
    hash_f64(p.start_depth, h);
    hash_opt_f64(p.step, h);
    hash_f64(p.fast_move_z, h);
    let oo: u8 = match p.objectorder {
        ObjectOrder::Nearest => 0,
        ObjectOrder::PerObject => 1,
        ObjectOrder::Unordered => 2,
    };
    h.write_u8(oo);
    hash_plunge(p.plunge, h);
    hash_opt_u32(p.feed_rate_override, h);
    hash_opt_u32(p.plunge_rate_override, h);
    hash_opt_f64(p.finish_step, h);
    hash_f64(p.through_depth, h);
    // 1mlv: stock_to_leave_mm enlarges the effective tool radius in
    // offset_builder for every Profile / Pocket cascade — the cutter
    // walks farther from the geometric wall. Output gcode coordinates
    // change verbatim, so this MUST be hashed.
    hash_f64(p.stock_to_leave_mm, h);
    hash_vec_f64(&p.depth_list, h);
}

fn hash_contour_params<H: Hasher>(c: &ContourParams, h: &mut H) {
    hash_tabs(&c.tabs, h);
    hash_tab_mode(c.tab_mode, h);
    h.write_usize(c.tab_placements.len());
    for tp in &c.tab_placements {
        tp.object_id.hash(h);
        hash_f64(tp.t, h);
        hash_opt_f64(tp.width_override_mm, h);
        hash_opt_f64(tp.height_override_mm, h);
    }
    hash_leads(&c.leads, h);
    h.write_u8(cut_direction_disc(c.cut_direction));
    h.write_u8(cut_direction_disc(c.finish_cut_direction));
    hash_f64(c.corner_feed_reduction, h);
    match c.approach_point {
        None => h.write_u8(0),
        Some((x, y)) => {
            h.write_u8(1);
            hash_f64(x, h);
            hash_f64(y, h);
        }
    }
}

fn hash_pocket_params<H: Hasher>(p: &PocketParams, h: &mut H) {
    hash_f64(p.xy_overlap, h);
    p.pocket_islands.hash(h);
    p.pocket_nocontour.hash(h);
    p.pocket_insideout.hash(h);
    hash_opt_f64(p.finish_xy_allowance_mm, h);
    match p.frame_shape {
        None => h.write_u8(0),
        Some(FrameShape::Rectangle) => h.write_u8(1),
        Some(FrameShape::RoundedRectangle) => h.write_u8(2),
    }
    hash_opt_f64(p.frame_padding_mm, h);
    hash_opt_f64(p.frame_corner_radius_mm, h);
}

fn hash_profile_params<H: Hasher>(p: ProfileParams, h: &mut H) {
    p.overcut.hash(h);
    p.reverse.hash(h);
    p.helix.hash(h);
}

fn hash_vcarve_params<H: Hasher>(v: &VCarveParams, h: &mut H) {
    hash_opt_f64(v.carve_max_width_mm, h);
    v.multi_pass_refine.hash(h);
    v.full_medial_axis.hash(h);
    hash_opt_f64(v.source_inset_mm, h);
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

// ─── text layers ──────────────────────────────────────────────────────

/// sqa3: stable hash of a `TextLayer` so font / content / placement
/// edits invalidate the per-op cache. Every persisted field is folded
/// in — adding a new TextLayer field is a `PIPELINE_VERSION` bump and
/// an entry here.
fn hash_text_layer<H: Hasher>(t: &TextLayer, h: &mut H) {
    t.id.hash(h);
    let kind: u8 = match t.kind {
        TextLayerKind::Text => 0,
        TextLayerKind::Mtext => 1,
    };
    h.write_u8(kind);
    t.name.hash(h);
    t.text.hash(h);
    // font_bytes can be megabytes for a fancy TTF — hash the LENGTH +
    // a per-byte fold so renaming a font file with the same bytes still
    // produces the same hash, while substituting a different font (even
    // same name) changes it. Hash impl on &[u8] does this in one call.
    h.write_usize(t.font_bytes.len());
    t.font_bytes.hash(h);
    hash_f64(t.size_mm, h);
    hash_f64(t.origin.0, h);
    hash_f64(t.origin.1, h);
    hash_f64(t.rotation_deg, h);
    hash_f64(t.letter_spacing_mm, h);
    hash_f64(t.line_spacing_mm, h);
    let align: u8 = match t.alignment {
        TextAlignment::Left => 0,
        TextAlignment::Center => 1,
        TextAlignment::Right => 2,
    };
    h.write_u8(align);
    hash_f64(t.width_scale, h);
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
    use crate::project::{Op, OpKind, OpParams, OpSource, ToolEntry, ToolKind};

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
            default_xy_overlap: None,
            comment: None,
            z_shift_mm: None,
            laser_pierce_sec: None,
            laser_lead_in_mm: None,
            kerf_mm: None,
            corner_radius_mm: None,
            tslot_neck_diameter_mm: None,
            tslot_neck_length_mm: None,
            wirbeln: false,
            wirbeln_stepover_mm: None,
            wirbeln_extra_width_mm: None,
            wirbeln_osc_mm: None,
            pause: 1,
            flute_length_mm: None,
            shank_diameter_mm: None,
            stickout_length_mm: None,
            holder: None,
            spindle_direction: crate::project::SpindleDirection::default(),
            drag_knife_self_align_angle_deg: None,
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            vcarve_lead_in_angle_deg: None,
        }
    }

    fn profile_op() -> Op {
        Op {
            id: 1,
            name: "Profile".into(),
            enabled: true,
            kind: OpKind::Profile {
                offset: ToolOffset::Outside,
                contour: crate::project::ContourParams::default(),
                profile: crate::project::ProfileParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
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
    ///     `PIPELINE_VERSION` (LEGITIMATE — update the snapshot below).
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
        assert_eq!(key.0, 0x6ed5_b6e9_675d_917f_u64, "got {:#018x}", key.0);
    }

    #[test]
    fn same_op_same_key() {
        let segs = square(20.0);
        let k1 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &segs,
            &[],
            0,
        );
        let k2 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &segs,
            &[],
            0,
        );
        assert_eq!(k1, k2);
    }

    #[test]
    fn depth_change_changes_key() {
        let segs = square(20.0);
        let mut op2 = profile_op();
        op2.params.depth -= 0.1;
        let k1 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &segs,
            &[],
            0,
        );
        let k2 = op_cache_key(&op2, &endmill(), &MachineConfig::default(), &segs, &[], 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn tool_diameter_changes_key() {
        let segs = square(20.0);
        let mut t2 = endmill();
        t2.diameter = 6.0;
        let k1 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &segs,
            &[],
            0,
        );
        let k2 = op_cache_key(&profile_op(), &t2, &MachineConfig::default(), &segs, &[], 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn segments_change_changes_key() {
        let s1 = square(20.0);
        let s2 = square(25.0);
        let k1 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &s1,
            &[],
            0,
        );
        let k2 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &s2,
            &[],
            0,
        );
        assert_ne!(k1, k2);
    }

    /// rt1.10 — tabs now live on the OP (`op.params.tab_placements`),
    /// so the cache-invalidation test exercises that path: bumping
    /// a tab in op.params changes `hash_operation`, which changes the
    /// key. Verified separately by `tab_mode_change_changes_key`
    /// below.
    #[test]
    fn fixtures_change_changes_key() {
        let segs = square(20.0);
        let fx = vec![Fixture {
            id: 1,
            name: "clamp".into(),
            kind: FixtureKind::Box {
                width: 30.0,
                depth: 50.0,
            },
            origin: (15.0, -25.0),
            z_bottom: 0.0,
            z_top: 12.0,
            color: 0xFFA0_50C0,
        }];
        let k1 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &segs,
            &[],
            0,
        );
        let k2 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &segs,
            &fx,
            0,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn post_processor_tag_changes_key() {
        let segs = square(20.0);
        let k1 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &segs,
            &[],
            0,
        );
        let k2 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &segs,
            &[],
            1,
        );
        assert_ne!(k1, k2);
    }

    /// Regression for audit-4zf: estimator-only `MachineConfig` fields
    /// (accel, jerk, `toolchange_s`, `rapid_speed`,
    /// `use_kinematic_time_estimate`) do not affect the emitted G-code
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
        m_accel.accel = Some(crate::cam::setup::AxisLimits {
            x: 5000.0,
            y: 5000.0,
            z: 1000.0,
        });
        let mut m_jerk = base.clone();
        m_jerk.jerk = Some(crate::cam::setup::AxisLimits {
            x: 1500.0,
            y: 1500.0,
            z: 500.0,
        });
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
            assert_eq!(
                key, key_base,
                "estimator-only field {name} changed the cache key"
            );
        }
    }

    /// Sanity: bumping `PIPELINE_VERSION` must change every cache key for
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
        h.write_usize(0); // no fixtures
        h.write_usize(0); // no text_layers (sqa3)
        hash_work_offset(&WorkOffset::default(), &mut h); // ls7y
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
                    internal_swap_emitted: false,
                },
            );
        }
        assert_eq!(cache.len(), 200);
        // The first 50 inserts (keys 0..50) should have been evicted.
        for i in 0..50u64 {
            assert!(
                cache.get(OpCacheKey(i)).is_none(),
                "key {i} was not evicted"
            );
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
            internal_swap_emitted: false,
        };
        cache.put(OpCacheKey(42), v.clone());
        let got = cache.get(OpCacheKey(42)).expect("hit");
        assert_eq!(got.gcode_body, v.gcode_body);
        assert_eq!(got.closed_count, v.closed_count);
        assert_eq!(got.offset_count, v.offset_count);
    }

    /// sqa3: editing a TextLayer (font, size, content) invalidates the
    /// per-op cache. The op_cache_key wrapper passes an empty
    /// text_layers slice; this test exercises the wider entry point
    /// directly to assert that two different text_layers slices
    /// produce two different keys.
    #[test]
    fn text_layer_change_changes_key() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let machine = MachineConfig::default();
        let tl1 = TextLayer {
            id: 1,
            kind: TextLayerKind::Text,
            name: "label".into(),
            text: "HELLO".into(),
            font_bytes: vec![1, 2, 3],
            size_mm: 10.0,
            origin: (0.0, 0.0),
            rotation_deg: 0.0,
            letter_spacing_mm: 0.0,
            line_spacing_mm: 0.0,
            alignment: TextAlignment::Left,
            width_scale: 1.0,
        };
        let mut tl2 = tl1.clone();
        tl2.text = "WORLD".into();
        let wo = WorkOffset::default();
        let k1 = op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[tl1], &wo, 0);
        let k2 = op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[tl2], &wo, 0);
        assert_ne!(k1, k2, "text content change must invalidate the cache key");
    }

    /// ul60: changing `machine.name` / `work_area` / `capabilities`
    /// invalidates the per-op cache. These were missing from
    /// hash_machine before the audit fix.
    #[test]
    fn machine_name_changes_key() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let base = MachineConfig::default();
        let mut renamed = base.clone();
        renamed.name = "Shop CNC".into();
        let k1 = op_cache_key(&op, &tool, &base, &segs, &[], 0);
        let k2 = op_cache_key(&op, &tool, &renamed, &segs, &[], 0);
        assert_ne!(k1, k2, "machine.name should invalidate the cache");
    }

    #[test]
    fn machine_work_area_changes_key() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let base = MachineConfig::default();
        let mut bigger = base.clone();
        bigger.work_area = crate::cam::setup::AxisLimits {
            x: bigger.work_area.x + 100.0,
            y: bigger.work_area.y,
            z: bigger.work_area.z,
        };
        let k1 = op_cache_key(&op, &tool, &base, &segs, &[], 0);
        let k2 = op_cache_key(&op, &tool, &bigger, &segs, &[], 0);
        assert_ne!(k1, k2, "machine.work_area should invalidate the cache");
    }

    #[test]
    fn machine_capabilities_changes_key() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let base = MachineConfig::default();
        let mut with_laser = base.clone();
        with_laser.capabilities = vec![MachineMode::Mill, MachineMode::Laser];
        let k1 = op_cache_key(&op, &tool, &base, &segs, &[], 0);
        let k2 = op_cache_key(&op, &tool, &with_laser, &segs, &[], 0);
        assert_ne!(k1, k2, "machine.capabilities should invalidate the cache");
    }

    // ─── scwx: hash_tool kerf / stickout / spindle_direction ────────

    /// scwx: editing a laser tool's kerf must invalidate the cache
    /// (the heightmap carve radius depends on kerf_mm).
    #[test]
    fn hash_tool_changes_when_kerf_mm_changes() {
        let segs = square(20.0);
        let op = profile_op();
        let machine = MachineConfig::default();
        let mut t1 = endmill();
        t1.kerf_mm = Some(0.05);
        let mut t2 = endmill();
        t2.kerf_mm = Some(0.4);
        let k1 = op_cache_key(&op, &t1, &machine, &segs, &[], 0);
        let k2 = op_cache_key(&op, &t2, &machine, &segs, &[], 0);
        assert_ne!(k1, k2, "tool.kerf_mm should invalidate the cache");
    }

    /// scwx: tool stickout_length_mm participates in holder/shank
    /// clearance checks — editing it must invalidate the cache.
    #[test]
    fn hash_tool_changes_when_stickout_length_changes() {
        let segs = square(20.0);
        let op = profile_op();
        let machine = MachineConfig::default();
        let mut t1 = endmill();
        t1.stickout_length_mm = Some(5.0);
        let mut t2 = endmill();
        t2.stickout_length_mm = Some(10.0);
        let k1 = op_cache_key(&op, &t1, &machine, &segs, &[], 0);
        let k2 = op_cache_key(&op, &t2, &machine, &segs, &[], 0);
        assert_ne!(k1, k2, "tool.stickout_length_mm should invalidate the cache");
    }

    /// scwx + z1y0: flipping the tool's spindle_direction routes the
    /// post between M3 and M4 — emitted gcode changes verbatim, so
    /// the cache key must change.
    #[test]
    fn hash_tool_changes_when_spindle_direction_changes() {
        let segs = square(20.0);
        let op = profile_op();
        let machine = MachineConfig::default();
        let mut t_ccw = endmill();
        t_ccw.spindle_direction = crate::project::SpindleDirection::Ccw;
        let k_cw = op_cache_key(&op, &endmill(), &machine, &segs, &[], 0);
        let k_ccw = op_cache_key(&op, &t_ccw, &machine, &segs, &[], 0);
        assert_ne!(k_cw, k_ccw, "tool.spindle_direction should invalidate the cache");
    }

    // ─── 75zr: hash_machine RPM clamps / dwells / park ─────────────

    /// 3nnj: tweaking spindle_rpm_min or _max changes whether an
    /// emitted S<rpm> is clamped (and the matching warning fires),
    /// so the cache must invalidate on either bound.
    #[test]
    fn hash_machine_changes_when_spindle_rpm_min_changes() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let base = MachineConfig::default();
        let mut floored = base.clone();
        floored.spindle_rpm_min = Some(6_000);
        let k1 = op_cache_key(&op, &tool, &base, &segs, &[], 0);
        let k2 = op_cache_key(&op, &tool, &floored, &segs, &[], 0);
        assert_ne!(k1, k2, "machine.spindle_rpm_min should invalidate the cache");
    }

    #[test]
    fn hash_machine_changes_when_spindle_rpm_max_changes() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let base = MachineConfig::default();
        let mut capped = base.clone();
        capped.spindle_rpm_max = Some(12_000);
        let k1 = op_cache_key(&op, &tool, &base, &segs, &[], 0);
        let k2 = op_cache_key(&op, &tool, &capped, &segs, &[], 0);
        assert_ne!(k1, k2, "machine.spindle_rpm_max should invalidate the cache");
    }

    /// eaeq: the two spindle dwell knobs are emitted as G4 P<sec>
    /// lines inside the M6 envelope — output bytes change with them.
    #[test]
    fn hash_machine_changes_when_spindle_stop_dwell_changes() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let base = MachineConfig::default();
        let mut dwell = base.clone();
        dwell.spindle_stop_dwell_sec = Some(1.5);
        let k1 = op_cache_key(&op, &tool, &base, &segs, &[], 0);
        let k2 = op_cache_key(&op, &tool, &dwell, &segs, &[], 0);
        assert_ne!(
            k1, k2,
            "machine.spindle_stop_dwell_sec should invalidate the cache"
        );
    }

    #[test]
    fn hash_machine_changes_when_spindle_start_dwell_changes() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let base = MachineConfig::default();
        let mut dwell = base.clone();
        dwell.spindle_start_dwell_sec = Some(2.0);
        let k1 = op_cache_key(&op, &tool, &base, &segs, &[], 0);
        let k2 = op_cache_key(&op, &tool, &dwell, &segs, &[], 0);
        assert_ne!(
            k1, k2,
            "machine.spindle_start_dwell_sec should invalidate the cache"
        );
    }

    /// syol: park_at_home toggles a G53 G0 X0 Y0 line into the
    /// program_end footer.
    #[test]
    fn hash_machine_changes_when_park_at_home_toggles() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let base = MachineConfig::default();
        let mut parked = base.clone();
        parked.park_at_home = true;
        let k1 = op_cache_key(&op, &tool, &base, &segs, &[], 0);
        let k2 = op_cache_key(&op, &tool, &parked, &segs, &[], 0);
        assert_ne!(k1, k2, "machine.park_at_home should invalidate the cache");
    }

    /// syol: explicit park_xy overrides the home / work-zero
    /// fallback in the program_end footer.
    #[test]
    fn hash_machine_changes_when_park_xy_changes() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let base = MachineConfig::default();
        let mut parked = base.clone();
        parked.park_xy = Some((150.0, 200.0));
        let k1 = op_cache_key(&op, &tool, &base, &segs, &[], 0);
        let k2 = op_cache_key(&op, &tool, &parked, &segs, &[], 0);
        assert_ne!(k1, k2, "machine.park_xy should invalidate the cache");
    }

    // ─── cgcu: hash_operation_kind Thread radial_passes / start_angle

    /// sqnh: number of radial roughing passes — driver emits one
    /// helix body per pass, so the cache key MUST react.
    #[test]
    fn hash_thread_changes_when_radial_passes_changes() {
        let segs = square(20.0);
        let tool = endmill();
        let machine = MachineConfig::default();
        let thread = |radial_passes: u32, start_angle_rad: f64| Op {
            id: 1,
            name: "Thread".into(),
            enabled: true,
            kind: OpKind::Thread {
                pitch_mm: 1.0,
                internal: true,
                climb: false,
                radial_passes,
                start_angle_rad,
                thread_depth_mm: None,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
        };
        let k1 = op_cache_key(&thread(1, 0.0), &tool, &machine, &segs, &[], 0);
        let k2 = op_cache_key(&thread(3, 0.0), &tool, &machine, &segs, &[], 0);
        assert_ne!(k1, k2, "Thread.radial_passes should invalidate the cache");
    }

    /// 6uns: start angle rotates the helix's starting tangent point —
    /// emitted gcode coordinates change with it.
    #[test]
    fn hash_thread_changes_when_start_angle_rad_changes() {
        let segs = square(20.0);
        let tool = endmill();
        let machine = MachineConfig::default();
        let thread = |start_angle_rad: f64| Op {
            id: 1,
            name: "Thread".into(),
            enabled: true,
            kind: OpKind::Thread {
                pitch_mm: 1.0,
                internal: true,
                climb: false,
                radial_passes: 1,
                start_angle_rad,
                thread_depth_mm: None,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
        };
        let k1 = op_cache_key(&thread(0.0), &tool, &machine, &segs, &[], 0);
        let k2 = op_cache_key(&thread(1.0), &tool, &machine, &segs, &[], 0);
        assert_ne!(k1, k2, "Thread.start_angle_rad should invalidate the cache");
    }

    // ─── 3xxj: hash_operation_params stock_to_leave_mm ──────────────

    /// 1mlv: stock_to_leave_mm bloats the tool offset radius in the
    /// cascade builder. Output coordinates change verbatim.
    #[test]
    fn hash_op_changes_when_stock_to_leave_mm_changes() {
        let segs = square(20.0);
        let tool = endmill();
        let machine = MachineConfig::default();
        let mut op_b = profile_op();
        op_b.params.stock_to_leave_mm = 0.3;
        let k1 = op_cache_key(&profile_op(), &tool, &machine, &segs, &[], 0);
        let k2 = op_cache_key(&op_b, &tool, &machine, &segs, &[], 0);
        assert_ne!(
            k1, k2,
            "op.params.stock_to_leave_mm should invalidate the cache"
        );
    }

    // ─── sulg: CapturedPostState last_coolant + last_spindle_dir ────

    /// sulg: the captured post state struct now carries coolant and
    /// spindle direction. Round-trip a non-default state through a
    /// real post's capture / restore and assert both fields survive
    /// the trip — that's what cached op N→N+1 splicing relies on.
    #[test]
    fn captured_post_state_round_trips_coolant_and_spindle_dir() {
        use crate::gcode::{linuxcnc, CoolantState, PostProcessor};
        use crate::project::SpindleDirection;

        // Author "op N": flip the spindle to Ccw + coolant to flood,
        // then snapshot.
        let mut post_a = linuxcnc::Post::default();
        post_a.spindle_ccw(18_000, 0);
        post_a.coolant_flood();
        let snap = post_a.capture_state();
        assert_eq!(
            snap.last_spindle_dir,
            Some(SpindleDirection::Ccw),
            "capture must preserve last_spindle_dir"
        );
        assert_eq!(
            snap.last_coolant,
            CoolantState::Flood,
            "capture must preserve last_coolant"
        );

        // Restore into a fresh post (simulating an op-N+1 cache hit
        // replay). Both modal-state fields should land in the post.
        let mut post_b = linuxcnc::Post::default();
        post_b.restore_state(&snap);

        // With Ccw restored, a same-speed spindle_cw call MUST emit
        // M3 — proves the direction was carried over (otherwise the
        // last_speed-only dedupe would suppress the M3 line).
        let mut after_restore = linuxcnc::Post::default();
        after_restore.restore_state(&snap);
        after_restore.spindle_cw(18_000, 0);
        let out = after_restore.finish();
        assert!(
            out.contains("M3 S18000") || out.contains("M3S18000"),
            "spindle_cw after restoring Ccw must emit M3 (direction flip);\
             got:\n{out}"
        );

        // Same coolant after restore: coolant_flood becomes a no-op
        // (already Flood). Use a separate fresh post to assert.
        let mut after_coolant = linuxcnc::Post::default();
        after_coolant.restore_state(&snap);
        after_coolant.coolant_flood();
        let out2 = after_coolant.finish();
        assert!(
            !out2.contains("M8"),
            "coolant_flood after restoring Flood state must dedupe (no extra M8); got:\n{out2}"
        );
    }

    // ─── ls7y: work_offset xyz + WCS selector ───────────────────────

    /// ls7y: bumping `project.work_offset.x_mm` must invalidate the
    /// per-op cache so that future WCS-driven emission (G10 L20 /
    /// G54..G59) doesn't serve gcode authored against a different
    /// origin.
    #[test]
    fn work_offset_x_changes_key() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let machine = MachineConfig::default();
        let wo_a = WorkOffset::default();
        let mut wo_b = WorkOffset::default();
        wo_b.x_mm = 50.0;
        let k1 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &wo_a, 0);
        let k2 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &wo_b, 0);
        assert_ne!(k1, k2, "work_offset.x_mm should invalidate the cache");
    }

    #[test]
    fn work_offset_y_changes_key() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let machine = MachineConfig::default();
        let wo_a = WorkOffset::default();
        let mut wo_b = WorkOffset::default();
        wo_b.y_mm = -12.5;
        let k1 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &wo_a, 0);
        let k2 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &wo_b, 0);
        assert_ne!(k1, k2, "work_offset.y_mm should invalidate the cache");
    }

    #[test]
    fn work_offset_z_changes_key() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let machine = MachineConfig::default();
        let wo_a = WorkOffset::default();
        let mut wo_b = WorkOffset::default();
        wo_b.z_mm = 3.0;
        let k1 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &wo_a, 0);
        let k2 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &wo_b, 0);
        assert_ne!(k1, k2, "work_offset.z_mm should invalidate the cache");
    }

    /// ls7y: the WCS selector (G54..G59) is a discriminant byte in the
    /// hash; switching G54 → G55 invalidates the cache even when the
    /// xyz offsets are identical (default zeros).
    #[test]
    fn work_offset_wcs_changes_key() {
        let segs = square(20.0);
        let op = profile_op();
        let tool = endmill();
        let machine = MachineConfig::default();
        let wo_a = WorkOffset::default();
        let mut wo_b = WorkOffset::default();
        wo_b.wcs = Wcs::G55;
        let k1 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &wo_a, 0);
        let k2 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &wo_b, 0);
        assert_ne!(k1, k2, "work_offset.wcs should invalidate the cache");
    }

    /// rt1.10: changing `op.tab_mode` invalidates the cache (`tab_mode`
    /// is hashed via `hash_operation_params`).
    #[test]
    fn tab_mode_change_changes_key() {
        let segs = square(20.0);
        let mut op_b = profile_op();
        if let Some(contour) = op_b.contour_params_mut() {
            contour.tab_mode = crate::project::TabPlacementMode::Auto { count: 4 };
        }
        let k1 = op_cache_key(
            &profile_op(),
            &endmill(),
            &MachineConfig::default(),
            &segs,
            &[],
            0,
        );
        let k2 = op_cache_key(&op_b, &endmill(), &MachineConfig::default(), &segs, &[], 0);
        assert_ne!(k1, k2);
    }
}
