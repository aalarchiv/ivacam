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
//! The op/tool/machine/config inputs are hashed via their canonical
//! serde-JSON ([`hash_serde`]). The wire schema types are the single
//! source of truth, so a new or renamed field on any of them
//! automatically participates in the cache key — there is no
//! hand-maintained field list to forget, which avoids the
//! "forget a field → stale gcode" hazard. Two deliberate exceptions:
//!
//! - **Geometry** (`Segment`) keeps a lightweight direct hash
//!   ([`hash_segment`]) — it's the per-op bulk and a stable leaf type, so
//!   it stays off the JSON path for speed. It carries a field-
//!   exhaustiveness guard (full destructure, no `..`) so a new `Segment`
//!   field is a compile error rather than a silent omission.
//! - **`MachineConfig`** is hashed through serde with its estimator-only
//!   fields (accel / jerk / rapid_speed / toolchange_s /
//!   use_kinematic_time_estimate) normalized to default first: those
//!   affect the time ESTIMATE, not the emitted gcode, so tuning them must
//!   not invalidate the toolpath cache. See `op_cache_key_with_finish`.
//!
//! serde_json errors only on non-finite floats; op/geometry params are
//! never NaN, and [`hash_serde`] folds a sentinel rather than panicking
//! if one ever appears, so the key stays total.
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
use serde::Serialize;

use crate::gcode::preview::ToolpathSegment;
use crate::gcode::CapturedPostState;
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::pipeline::PipelineWarning;
use crate::project::MachineConfig;
use crate::project::{Fixture, Op, ReliefSource, TextLayer, ToolEntry, WorkOffset};

/// Pipeline output-format version, defined in the leaf [`crate::version`]
/// module so the `cam` geometry caches can fold it in without depending
/// upward on this module. Re-exported here for existing call sites.
pub use crate::version::PIPELINE_VERSION;

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
    /// `true` when the cached body contains an internal dual-tool
    /// toolchange envelope (rough→finish, drill→chamfer). Lets the
    /// per-op driver replay the correct `prev_tool_id` bias on a cache
    /// hit — without this, an op that declared a finish tool but
    /// produced no finish offsets would still pessimistically bias the
    /// next op to the finish id, causing the next same-rough-tool op
    /// to skip its M6 envelope and run with the wrong tool.
    pub internal_swap_emitted: bool,
    /// The per-op `PipelineWarning`s this op produced during its
    /// fresh emit (tool-fit / tool-kind mismatch, trochoidal, ramp-arcs,
    /// depth-limited, zero-rate, etc.). Re-attached verbatim on a cache
    /// HIT so the second+ identical Generate still surfaces them —
    /// otherwise `build_op_offsets` / the driver / `synthesize_op_setup`
    /// never re-run and a critical warning (e.g. `tool_kind_mismatch`,
    /// classified as critical) would silently vanish.
    /// Excludes the pre-cache-lookup `validate_op_source_*` warnings,
    /// which already run on both paths.
    pub warnings: Vec<PipelineWarning>,
}

#[derive(Debug)]
pub struct PipelineCache {
    inner: Mutex<LruCache<u64, OpCacheValue>>,
}

impl PipelineCache {
    /// # Panics
    ///
    /// Never in practice: `capacity.max(1)` is always `>= 1`, which
    /// satisfies `NonZeroUsize::new`. The `expect` is a defensive
    /// sentinel; if it ever fires, the `NonZeroUsize::new` contract
    /// itself is broken.
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
/// projects without text). Callers that DO consume `text_layers` should
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
        &[],
        &WorkOffset::default(),
        post_processor_tag,
    )
}

/// Cache-key constructor that folds a SECOND tool's entry into the
/// hash — used for dual-tool Pocket ops so changes to the
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
    relief_sources: &[ReliefSource],
    work_offset: &WorkOffset,
    post_processor_tag: u8,
) -> OpCacheKey {
    let mut h = SeaHasher::new();
    PIPELINE_VERSION.hash(&mut h);
    h.write_u8(post_processor_tag);
    // Every op/tool/machine/config input is hashed via its canonical
    // serde-JSON — the schema types are the single source of truth, so a
    // new field on any wire type automatically participates in the key
    // with no hand-mirror to forget. Geometry (`selected_segments`) is
    // the bulk and a stable leaf, so it keeps a lightweight direct hash.
    hash_serde(op, &mut h);
    hash_serde(tool, &mut h);
    match finish_tool {
        None => h.write_u8(0),
        Some(t) => {
            h.write_u8(1);
            hash_serde(t, &mut h);
        }
    }
    // MachineConfig is the one wire type with a deliberate gcode/estimator
    // split: accel / jerk / rapid_speed / toolchange_s /
    // use_kinematic_time_estimate affect only the TIME ESTIMATE, not the
    // emitted gcode, so tuning them must hit the cheap re-estimate path
    // rather than invalidate the expensive toolpath cache. Normalize those
    // to default, then serde-hash the rest — a new GCODE-affecting machine
    // field still auto-participates; a future estimator-only field would
    // (until added here) merely over-invalidate, never serve stale gcode.
    let mut machine_key = machine.clone();
    let machine_defaults = MachineConfig::default();
    machine_key.accel = machine_defaults.accel;
    machine_key.jerk = machine_defaults.jerk;
    machine_key.rapid_speed = machine_defaults.rapid_speed;
    machine_key.toolchange_s = machine_defaults.toolchange_s;
    machine_key.use_kinematic_time_estimate = machine_defaults.use_kinematic_time_estimate;
    hash_serde(&machine_key, &mut h);
    h.write_usize(selected_segments.len());
    for seg in selected_segments {
        hash_segment(seg, &mut h);
    }
    hash_serde(fixtures, &mut h);
    // Fold the consumed text layers into the key. Edits to font /
    // content / size / placement / alignment must invalidate the cache
    // (otherwise the user changes the engraving text and Generate
    // happily serves the old gcode). Conservative — every text_layer,
    // not just the ones this op consumes via `OpSource::Layers`.
    hash_serde(text_layers, &mut h);
    // Fold relief surface sources into the key like text_layers —
    // editing the source image (brightness grid) must invalidate the
    // cached relief toolpath. Conservative (hash all).
    hash_serde(relief_sources, &mut h);
    // Project work_offset (xyz + WCS selector) is consulted by sim
    // alignment + the WCS-origin warning, and will drive G10 L20 /
    // G54..G59 emission — hash it so cached gcode authored against a
    // different work_offset is invalidated.
    hash_serde(work_offset, &mut h);
    OpCacheKey(h.finish())
}

// ─── primitives ───────────────────────────────────────────────────────

#[inline]
/// Hash the canonical serde-JSON of a wire type. Because the schema
/// types ARE the single source of truth, a new or renamed field on any
/// of them automatically participates in the cache key — there is no
/// hand-written field list to drift out of sync (the old per-type
/// `hash_*` fns were exactly that hazard: forget a field => stale gcode).
/// serde_json only errors on non-finite floats, which op/geometry params
/// never are (see module docs); on a degenerate input we fold a sentinel
/// rather than panic, so the key stays total.
fn hash_serde<T: Serialize + ?Sized, H: Hasher>(v: &T, h: &mut H) {
    match serde_json::to_vec(v) {
        Ok(bytes) => bytes.hash(h),
        Err(_) => 0xDEAD_BEEF_u32.hash(h),
    }
}

fn hash_f64<H: Hasher>(v: f64, h: &mut H) {
    h.write_u64(v.to_bits());
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
    // Field-exhaustiveness guard: this is the one type we still hash by
    // hand (geometry leaf, kept off serde for the per-segment hot path),
    // so destructure with no `..` — a new Segment field becomes a compile
    // error here, forcing a decision about cache-relevance.
    let Segment {
        kind: _,
        start: _,
        end: _,
        bulge: _,
        center: _,
        layer: _,
        color: _,
    } = s;
}

// ─── tool ─────────────────────────────────────────────────────────────

// ─── machine ──────────────────────────────────────────────────────────

// ─── operation ────────────────────────────────────────────────────────

// ─── tabs map (helper for callers caching the project-level tabs for an op) ───

// ─── text layers ──────────────────────────────────────────────────────

// ─── fixtures ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // Types used only to construct test fixtures (the production hashing
    // is serde-based, so these are not needed at module scope).
    use crate::project::MachineMode;
    use crate::project::ToolOffset;
    use crate::project::{Coolant, FixtureKind, TextAlignment, TextLayerKind, Wcs};
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
            form_profile_mm: Vec::new(),
            whirl: false,
            whirl_stepover_mm: None,
            whirl_extra_width_mm: None,
            whirl_osc_mm: None,
            pause: 1,
            flute_length_mm: None,
            length_mm: None,
            compression_transition_mm: None,
            thread_pitch_mm: None,
            shank_diameter_mm: None,
            stickout_length_mm: None,
            holder: None,
            spindle_direction: crate::project::SpindleDirection::default(),
            drag_knife_self_align_angle_deg: None,
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            wear_offset_mm: 0.0,
            last_calibrated: None,
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
            group: None,
            pin_order: false,
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
        // (Updated when the per-type hand hashers were replaced by serde-JSON
        //  hashing + PIPELINE_VERSION 46→47.)
        assert_eq!(key.0, 0x59c3_f268_4afa_757b_u64, "got {:#018x}", key.0);
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

    /// Tabs now live on the OP (`op.params.tab_placements`),
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

    /// Regression: estimator-only `MachineConfig` fields
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
        m_accel.accel = Some(crate::project::AxisLimits {
            x: 5000.0,
            y: 5000.0,
            z: 1000.0,
        });
        let mut m_jerk = base.clone();
        m_jerk.jerk = Some(crate::project::AxisLimits {
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

        // Recompute the key by hand with a bumped version — mirrors the
        // body of `op_cache_key_with_finish` (serde-JSON per wire type +
        // direct segment hash) so this stays a faithful "version is folded
        // in" check.
        let no_fixtures: &[Fixture] = &[];
        let no_text: &[TextLayer] = &[];
        let no_relief: &[ReliefSource] = &[];
        let mut h = SeaHasher::new();
        (PIPELINE_VERSION + 1).hash(&mut h);
        h.write_u8(0); // post_processor_tag
        hash_serde(&op, &mut h);
        hash_serde(&tool, &mut h);
        h.write_u8(0); // no finish tool
        hash_serde(&machine, &mut h);
        h.write_usize(segs.len());
        for s in &segs {
            hash_segment(s, &mut h);
        }
        hash_serde(no_fixtures, &mut h);
        hash_serde(no_text, &mut h); // text layers
        hash_serde(no_relief, &mut h); // relief sources
        hash_serde(&WorkOffset::default(), &mut h); // work offset
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
                    warnings: Vec::new(),
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
            warnings: Vec::new(),
        };
        cache.put(OpCacheKey(42), v.clone());
        let got = cache.get(OpCacheKey(42)).expect("hit");
        assert_eq!(got.gcode_body, v.gcode_body);
        assert_eq!(got.closed_count, v.closed_count);
        assert_eq!(got.offset_count, v.offset_count);
    }

    /// Editing a `TextLayer` (font, size, content) invalidates the
    /// per-op cache. The `op_cache_key` wrapper passes an empty
    /// `text_layers` slice; this test exercises the wider entry point
    /// directly to assert that two different `text_layers` slices
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
        let k1 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[tl1], &[], &wo, 0);
        let k2 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[tl2], &[], &wo, 0);
        assert_ne!(k1, k2, "text content change must invalidate the cache key");
    }

    /// Changing `machine.name` / `work_area` / `capabilities`
    /// invalidates the per-op cache. These were missing from
    /// `hash_machine` before the audit fix.
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
        bigger.work_area = crate::project::AxisLimits {
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

    // ─── hash_tool kerf / stickout / spindle_direction ──────────────

    /// Editing a laser tool's kerf must invalidate the cache
    /// (the heightmap carve radius depends on `kerf_mm`).
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

    /// Tool `stickout_length_mm` participates in holder/shank
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
        assert_ne!(
            k1, k2,
            "tool.stickout_length_mm should invalidate the cache"
        );
    }

    /// Flipping the tool's `spindle_direction` routes the
    /// post between M3 and M4 — emitted gcode changes verbatim, so
    /// the cache key must change.
    // `k_cw`/`k_ccw` are an intentional pair — same key
    // computation for cw vs ccw spindle. The cache-test convention
    // reuses this naming throughout the file.
    #[allow(clippy::similar_names)]
    #[test]
    fn hash_tool_changes_when_spindle_direction_changes() {
        let segs = square(20.0);
        let op = profile_op();
        let machine = MachineConfig::default();
        let mut t_ccw = endmill();
        t_ccw.spindle_direction = crate::project::SpindleDirection::Ccw;
        let k_cw = op_cache_key(&op, &endmill(), &machine, &segs, &[], 0);
        let k_ccw = op_cache_key(&op, &t_ccw, &machine, &segs, &[], 0);
        assert_ne!(
            k_cw, k_ccw,
            "tool.spindle_direction should invalidate the cache"
        );
    }

    // ─── hash_machine RPM clamps / dwells / park ────────────────────

    /// Tweaking `spindle_rpm_min` or _max changes whether an
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
        assert_ne!(
            k1, k2,
            "machine.spindle_rpm_min should invalidate the cache"
        );
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
        assert_ne!(
            k1, k2,
            "machine.spindle_rpm_max should invalidate the cache"
        );
    }

    /// The two spindle dwell knobs are emitted as G4 P<sec>
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

    /// `park_at_home` toggles a G53 G0 X0 Y0 line into the
    /// `program_end` footer.
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

    /// Explicit `park_xy` overrides the home / work-zero
    /// fallback in the `program_end` footer.
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

    // ─── hash_operation_kind Thread radial_passes / start_angle ─────

    /// Number of radial roughing passes — driver emits one
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
            group: None,
            pin_order: false,
        };
        let k1 = op_cache_key(&thread(1, 0.0), &tool, &machine, &segs, &[], 0);
        let k2 = op_cache_key(&thread(3, 0.0), &tool, &machine, &segs, &[], 0);
        assert_ne!(k1, k2, "Thread.radial_passes should invalidate the cache");
    }

    /// Start angle rotates the helix's starting tangent point —
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
            group: None,
            pin_order: false,
        };
        let k1 = op_cache_key(&thread(0.0), &tool, &machine, &segs, &[], 0);
        let k2 = op_cache_key(&thread(1.0), &tool, &machine, &segs, &[], 0);
        assert_ne!(k1, k2, "Thread.start_angle_rad should invalidate the cache");
    }

    // ─── hash_operation_params stock_to_leave_mm ────────────────────

    /// `stock_to_leave_mm` bloats the tool offset radius in the
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

    // ─── CapturedPostState last_coolant + last_spindle_dir ──────────

    /// The captured post state struct now carries coolant and
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

    // ─── work_offset xyz + WCS selector ─────────────────────────────

    /// Bumping `project.work_offset.x_mm` must invalidate the
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
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &[], &wo_a, 0);
        let k2 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &[], &wo_b, 0);
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
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &[], &wo_a, 0);
        let k2 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &[], &wo_b, 0);
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
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &[], &wo_a, 0);
        let k2 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &[], &wo_b, 0);
        assert_ne!(k1, k2, "work_offset.z_mm should invalidate the cache");
    }

    /// The WCS selector (G54..G59) is a discriminant byte in the
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
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &[], &wo_a, 0);
        let k2 =
            op_cache_key_with_finish(&op, &tool, None, &machine, &segs, &[], &[], &[], &wo_b, 0);
        assert_ne!(k1, k2, "work_offset.wcs should invalidate the cache");
    }

    /// Changing `op.tab_mode` invalidates the cache (`tab_mode`
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
