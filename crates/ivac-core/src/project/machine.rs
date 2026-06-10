//! Machine wire types — the JsonSchema config that describes a machine's
//! capabilities and post-processor behavior (units, mode, tool-change
//! strategy, axis limits, post-change Z policy). These are project-level
//! wire types; the runtime-resolved [`crate::cam::setup::Setup`] bundle
//! embeds [`MachineConfig`].

// # CAM/sim pedantic-lint exemptions
// Setup helpers walk over `min`/`max` axis-limit pairs whose names mirror
// the field they project from. Serde `skip_serializing_if = "is_default_…"`
// helpers take `&T` because that's the signature serde requires.
// `MachineConfig` is a user-facing config struct (one bool per UI checkbox);
// folding bools into enums would require flattening the JSON shape and break
// the schema contract.
#![allow(
    clippy::similar_names,
    clippy::trivially_copy_pass_by_ref,
    clippy::struct_excessive_bools
)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AxisLimits {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl AxisLimits {
    #[must_use]
    pub const fn uniform(v: f64) -> Self {
        Self { x: v, y: v, z: v }
    }
}

/// How the post-processor handles a tool change at an op boundary.
/// Widened from the historical `supports_toolchange: bool` because the two
/// *manual* behaviors need different emission: grblHAL / FluidNC accept
/// `M6` as a prompt (the controller parks, prompts the operator, and can
/// run a semi-automatic tool-length probe), while stock GRBL / Marlin
/// reject `M6` (`error:20`) and must fall back to a portable `M0` program
/// pause. Serde also accepts a plain bool via
/// [`deserialize_toolchange`] (`true → Atc`, `false → ManualM0Pause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolChangeStrategy {
    /// Automatic tool changer: emit `T<n> M6`; the changer swaps the bit
    /// and the program continues without an operator pause. Maps onto the
    /// old `supports_toolchange == true`.
    Atc,
    /// Manual change with a controller-driven prompt (grblHAL / FluidNC):
    /// emit `T<n> M6`; the controller parks, prompts the operator for the
    /// swap, and can run a semi-automatic tool-length probe before resume.
    ManualM6Prompt,
    /// Manual change via a portable `M0` program pause — stock GRBL /
    /// Marlin and any controller that rejects `M6`. Default for unknown
    /// GRBL-class machines (the most portable choice). Maps onto the old
    /// `supports_toolchange == false`.
    #[default]
    ManualM0Pause,
    /// Emit no tool-change handling at all. The program runs as if every
    /// op shares one tool; the operator / sender is responsible for swaps.
    Ignore,
}

impl ToolChangeStrategy {
    /// `true` when the post emits a real `T<n> M6` (an auto changer or a
    /// controller that prompts on `M6`). These two share the `M6` emission
    /// path in `emit_toolchange_envelope`; the `M0`-pause and `Ignore`
    /// strategies do not.
    #[must_use]
    pub fn emits_m6(self) -> bool {
        matches!(self, Self::Atc | Self::ManualM6Prompt)
    }

    /// `true` only for a fully automatic tool changer (no operator
    /// intervention expected at the swap).
    #[must_use]
    pub fn is_atc(self) -> bool {
        matches!(self, Self::Atc)
    }

    /// Stable cache discriminant folded into the op cache key. Pinned so
    /// the two original variants (`Atc` / `ManualM0Pause`, which
    /// replaced a `true` / `false` bool) hash
    /// byte-identically to that `bool` encoding: `bool::hash` writes
    /// `0` / `1` as a `u8`, so `ManualM0Pause = 0` and `Atc = 1` keep the
    /// cache key stable across the widening. The new variants get fresh
    /// discriminants.
    #[must_use]
    pub fn cache_discriminant(self) -> u8 {
        match self {
            Self::ManualM0Pause => 0,
            Self::Atc => 1,
            Self::ManualM6Prompt => 2,
            Self::Ignore => 3,
        }
    }
}

/// Back-compat deserializer for [`MachineConfig::tool_change`].
/// Accepts either the new enum string (`"atc"`, `"manual_m6_prompt"`,
/// `"manual_m0_pause"`, `"ignore"`) or the legacy `supports_toolchange`
/// bool (`true → Atc`, `false → ManualM0Pause`), so projects saved before
/// the widening still load unchanged.
fn deserialize_toolchange<'de, D>(d: D) -> Result<ToolChangeStrategy, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Compat {
        Bool(bool),
        Strategy(ToolChangeStrategy),
    }
    Ok(match Compat::deserialize(d)? {
        Compat::Bool(true) => ToolChangeStrategy::Atc,
        Compat::Bool(false) => ToolChangeStrategy::ManualM0Pause,
        Compat::Strategy(s) => s,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MachineConfig {
    pub unit: UnitSystem,
    pub mode: MachineMode,
    pub comments: bool,
    /// Whether the machine emits arc commands (G2/G3).
    pub arcs: bool,
    /// Tool-change strategy. See [`ToolChangeStrategy`]. A
    /// `supports_toolchange` serde alias also accepts the boolean form
    /// `true/false` via a bool-aware deserializer (`true → Atc`,
    /// `false → ManualM0Pause`).
    #[serde(
        default,
        alias = "supports_toolchange",
        deserialize_with = "deserialize_toolchange"
    )]
    pub tool_change: ToolChangeStrategy,
    /// Per-axis acceleration in mm/s². When None the kinematic time
    /// estimator falls back to 250 mm/s² per axis (`LinuxCNC` default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accel: Option<AxisLimits>,
    /// Per-axis jerk in mm/s³. None ⇒ trapezoidal-only profiling
    /// (S-curve refinement is Phase 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jerk: Option<AxisLimits>,
    /// Tool-change time in seconds.
    #[serde(
        default = "default_toolchange_s",
        skip_serializing_if = "is_default_toolchange_s"
    )]
    pub toolchange_s: f64,
    /// Spindle-stop dwell (seconds) inserted into the M6 toolchange
    /// envelope between `M5` and the actual `T<n> M6`. Gives the spindle
    /// time to spin down before the chuck is touched. `None` (and the
    /// default `0.5 s`) covers most VFD-driven spindles; high-inertia
    /// big-iron may want 1–2 s. Set to `Some(0.0)` to skip entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spindle_stop_dwell_sec: Option<f64>,
    /// Spindle-start dwell (seconds) inserted into the M6 toolchange
    /// envelope after `M3 S<rpm>`. Lets the new tool come up to commanded
    /// RPM before the next cut. Stacks with the per-tool `ToolEntry.pause`
    /// (the per-tool warm-up); think of this as the machine-wide floor and
    /// `tool.pause` as the per-tool top-up. `None` ⇒ 0.5 s default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spindle_start_dwell_sec: Option<f64>,
    /// Rapid (G0) traverse speed in mm/min. None ⇒ 5000 mm/min default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rapid_speed: Option<f64>,
    /// When true (the default), use the accel/jerk-aware integrator.
    /// Set to false for the legacy length/feed-only estimator.
    #[serde(
        default = "default_use_kinematic",
        skip_serializing_if = "is_default_use_kinematic"
    )]
    pub use_kinematic_time_estimate: bool,
    /// Machine work area envelope in mm. Drives the stock's auto-mode
    /// fallback when no geometry is imported (the stock then sizes to
    /// the work-area XY footprint), and surfaces as the soft-limit
    /// reference in future sim warnings. Default 200×300×50 — a typical
    /// hobby gantry; users override in `MachineDialog`.
    #[serde(
        default = "default_work_area",
        skip_serializing_if = "is_default_work_area"
    )]
    pub work_area: AxisLimits,
    /// Maximum deviation (mm) between the fitted G2/G3 arc and the
    /// original chord polyline. None ⇒ 0.01 mm. Only consulted when
    /// `arcs == true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc_fit_tolerance_mm: Option<f64>,
    /// Decimal separator for emitted numbers. `'.'` (default)
    /// suits `LinuxCNC` / GRBL / Mach3 and any controller configured in
    /// US locale. `','` covers European-locale Siemens / Heidenhain
    /// controllers that require `X1,5` instead of `X1.5`. Anything
    /// other than '.' / ',' silently falls back to '.'.
    #[serde(
        default = "default_decimal_separator_char",
        skip_serializing_if = "is_default_decimal_separator"
    )]
    pub decimal_separator: char,
    /// Starting line number for `N<n>` prefixes. `None` (the
    /// default) emits unnumbered lines. `Some(10)` emits `N10`, `N20`,
    /// `N30`, … incrementing by 10. Required by some FANUC / vintage
    /// controllers; useful operator reference even on modern ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_number_start: Option<u32>,
    /// Plot-mode Z (Estlcam `c_PP.Z_Up_Dn)`: when true, the
    /// pipeline collapses every cut to ONE pass at the op's cut depth
    /// and skips the multi-step descent / ramp / helix machinery.
    /// Z values written into gcode are restricted to `fast_move_z`
    /// (pen up between cuts) and the op's `depth` (pen down on
    /// cut moves). Right setting for laser / plasma / pen plotters /
    /// 3D-printer extrusion and drag-knife controllers.
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub plot_mode_z: bool,
    /// User-configurable post-processor profile. When
    /// `Some`, the built-in posts (linuxcnc / grbl) read its
    /// templates instead of emitting their hard-coded
    /// `program_start` / `program_end` / `tool_change` / coolant lines.
    /// `None` = hard-coded defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_profile: Option<crate::gcode::post_profile::PostProfile>,
    /// Free-text identifier for the machine setup ("Shop CNC",
    /// "Garage MPCNC", …). Empty string by default; persisted into
    /// the project file + the `.ivac-machine.json` save/load files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Which op kinds the machine can run. Drives the
    /// frontend's `OpKindPicker` filter — a laser-only machine
    /// doesn't show milling ops. `mode` (above) stays as the
    /// PRIMARY mode used by the gcode emitter; capabilities is the
    /// broader set so a multi-purpose machine can pick the right
    /// op set without flipping `mode`. Empty Vec ⇒ implicitly
    /// `[mode]` (the default when `capabilities` is absent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<MachineMode>,
    /// Lower bound on the spindle RPM the controller will
    /// accept. Tool / op RPMs below this clamp UP to the min and
    /// emit a `spindle_speed_clamped_below_min` warning. `None`
    /// disables the floor (default, back-compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spindle_rpm_min: Option<u32>,
    /// Upper bound on the spindle RPM the controller will
    /// accept. Tool / op RPMs above this clamp DOWN to the max
    /// and emit a `spindle_speed_clamped_above_max` warning.
    /// Without this clamp some controllers refuse the command
    /// mid-program; others silently cap and produce wrong chipload.
    /// `None` disables the ceiling (default, back-compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spindle_rpm_max: Option<u32>,
    /// Upper bound on the cutting / plunge feed (mm/min) the
    /// machine can actually drive. Tool / op feeds above this clamp
    /// DOWN to the max and emit a `feed_clamped_above_max` warning, so
    /// an out-of-range feed (a fat-fingered op override, an aggressive
    /// tool-library value) can't reach the controller as a verbatim
    /// `F<huge>` — which some controllers fault on and others silently
    /// cap, both producing a program that doesn't run as previewed.
    /// `None` disables the ceiling (default, back-compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_feed_mm_min: Option<u32>,
    /// When true, the `program_end` footer adds a `G53 G0 X0 Y0`
    /// retract-to-machine-home before the spindle-off + M30 sequence.
    /// Most hobby controllers (`LinuxCNC`, Mach3) honor G53; GRBL accepts
    /// it from v1.1 onward. When false, falls back to a `G0 X0 Y0` in
    /// the current WCS (the work zero) — still safer than leaving the
    /// spindle parked over the part. Both modes lift to `fast_move_z`
    /// first.
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub park_at_home: bool,
    /// Optional explicit park XY (mm, in WCS coordinates). When
    /// `Some`, the `program_end` footer routes the head to this point
    /// after the safe-Z lift, overriding the machine-home / work-zero
    /// fallback. Useful for a known tool-station / load-station that
    /// isn't (0, 0) in either frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub park_xy: Option<(f64, f64)>,
    /// Optional tool-change position (mm, in MACHINE coordinates).
    /// When `Some`, the toolchange envelope rapids the head here via
    /// `G53 G0 X<x> Y<y>` after the safe-Z lift and BEFORE the M0 / M6
    /// pause, so a manual bit-swap happens at a fixed, reachable station
    /// instead of directly over the workpiece / clamps. MACHINE coords
    /// (not WCS like `park_xy`) because a tool-change station is a
    /// physical machine location independent of where the part zero
    /// sits — re-zeroing a job must not move the changer. Applies to
    /// both manual and ATC paths; on an ATC whose M6 macro homes to its
    /// own changer, leave this `None`. `None` (default) keeps the prior
    /// behavior: lift to `fast_move_z` only. Emitted via the post's
    /// `rapid_machine_xy`, which HPGL / pen posts drop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchange_xy: Option<(f64, f64)>,
    /// How the post re-establishes the new tool's Z tip position
    /// after a tool change. A manual hand-swap leaves the new tool's
    /// length unknown; the default `PostChangeZStrategy::None` applies
    /// only the static per-tool `ToolEntry.z_shift_mm`, which assumes
    /// perfectly repeatable collet seating + pre-known lengths — false
    /// for most hobby swaps. Best practice re-measures Z after every
    /// change. Maps onto grblHAL `$341` modes / Estlcam's tool-measure
    /// policies. Applied by `emit_toolchange_envelope` to NON-first
    /// changes (the first tool is operator-loaded at program start);
    /// `None` keeps existing output byte-for-byte.
    #[serde(default, skip_serializing_if = "PostChangeZStrategy::is_none")]
    pub post_change_z: PostChangeZStrategy,
    /// Opt-in tool-length compensation via the controller's tool
    /// table. When `true` on an ATC machine (`tool_change == Atc`), the
    /// toolchange envelope emits `G43 H<n>` after `T<n> M6` so the
    /// controller applies the pre-measured length for tool `<n>`, and
    /// SKIPS the static `z_shift` / `post_change_z` flow (mutually
    /// exclusive — G43 supersedes both). `program_end` cancels with
    /// `G49`. Default `false`: existing static-`z_shift` users are
    /// unaffected. Ignored on manual (non-ATC) machines, which can't run
    /// an M6 tool table.
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub use_tool_length_offsets: bool,
    /// Emit `M1` (optional stop) instead of `M0` (mandatory stop)
    /// at every program pause — both the `Pause` op and the manual
    /// (`ManualM0Pause`) tool-change halt. `M1` is honored only when the
    /// controller's optional-stop switch is ON, so a vetted program can
    /// run unattended (the switch off skips the pauses) yet still stop on
    /// demand. Default `false` keeps the mandatory `M0`.
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub optional_stop: bool,
    /// Opt-in GRBL dynamic-power laser mode. When `true` on a GRBL
    /// post, laser cuts/engraving arm + fire with `M4` instead of `M3`,
    /// so the controller ramps `S` power with the actual feed rate —
    /// corners and edges (where the head slows) don't over-burn, and
    /// rapids force `S0` automatically. GRBL-specific (`$32=1` laser
    /// mode); on `LinuxCNC` `M4` means spindle-CCW, so the flag is honored
    /// ONLY by the GRBL post (others keep `M3`). "Strongly preferred" for
    /// laser engraving. Default `false` keeps the
    /// portable `M3` output byte-for-byte.
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub laser_dynamic_power: bool,
}

/// Post-tool-change Z re-establish strategy. Internally tagged
/// (`{"mode": "...", ...}`) like [`PlungeStrategy`], so adding a variant
/// is additive in the schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PostChangeZStrategy {
    /// Apply only the static per-tool `ToolEntry.z_shift_mm` (today's
    /// behavior). Default — keeps existing output byte-for-byte.
    None,
    /// Hand touch-off: the operator jogs the new tool to the work
    /// surface and zeroes Z during the M0 pause. The envelope emits a
    /// touch-off instruction in the pre-pause prompt and skips the
    /// static `z_shift` (the operator establishes Z by hand, so a stale
    /// pre-known shift would fight them).
    ManualTouchoff,
    /// Chain a `G38.2` probe toward a touch plate after the swap, then
    /// pin work Z to the plate top. Automatic and repeatable.
    Probe {
        /// Max search distance (mm) along Z. NEGATIVE probes DOWN onto
        /// the plate (the usual case); the controller halts at the
        /// trigger and this is just the search limit.
        distance_mm: f64,
        /// Probe feedrate (mm/min). 50–200 typical for a touch-trigger
        /// probe — slow enough to trip repeatably.
        feed_mm_min: u32,
        /// Plate thickness (mm). Work Z is pinned to this at the trigger
        /// so Z0 stays the stock top (plate sits on the stock). `0`
        /// probes directly onto the work zero surface.
        #[serde(default)]
        plate_thickness_mm: f64,
    },
    /// Fixed tool-length sensor at a known MACHINE position. Rapid to it
    /// (G53), probe down (`G38.2`), and apply the measured length as a
    /// tool-length offset, differenced against the `reference_tool_id`
    /// that defines work Z0. Pairs with ATC + grblHAL `$341=2`. The
    /// numeric difference is computed by the CONTROLLER at runtime (it
    /// isn't known at CAM time) — see [`PostProcessor::apply_probed_tool_length`].
    FixedSensor {
        /// Sensor location in MACHINE coords (mm): `(x, y, approach_z)`.
        /// `approach_z` is the safe machine Z the head rapids to before
        /// (and retracts to after) probing down.
        position: (f64, f64, f64),
        /// Signed search distance (mm) from `approach_z` toward the
        /// sensor. NEGATIVE seeks DOWN.
        seek_mm: f64,
        /// Probe feedrate (mm/min).
        feed_mm_min: u32,
        /// Tool whose sensor reading defines work Z0; the operator
        /// touches it off on the workpiece instead of the sensor, and
        /// other tools' offsets are differenced from it. `None` ⇒ the
        /// program's first tool.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference_tool_id: Option<u32>,
    },
}

impl Default for PostChangeZStrategy {
    fn default() -> Self {
        Self::None
    }
}

impl PostChangeZStrategy {
    /// `true` for the [`None`](Self::None) default — used by the
    /// `skip_serializing_if` so unconfigured machines emit a clean,
    /// drift-free schema and JSON.
    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

impl MachineConfig {
    /// Effective polyline→arc fit tolerance. Falls back to 0.01 mm.
    #[must_use]
    pub fn effective_arc_tolerance(&self) -> f64 {
        self.arc_fit_tolerance_mm.unwrap_or(0.01).max(0.0)
    }

    /// Effective spindle-stop dwell (seconds) for the toolchange
    /// envelope. Defaults to 0.5 s. Clamped to ≥0 so a stray negative
    /// value can't underflow the dwell-format helper.
    #[must_use]
    pub fn effective_spindle_stop_dwell_sec(&self) -> f64 {
        self.spindle_stop_dwell_sec.unwrap_or(0.5).max(0.0)
    }

    /// Effective spindle-start dwell (seconds) for the toolchange
    /// envelope. Defaults to 0.5 s. Stacks with the per-tool
    /// `ToolEntry.pause` warm-up.
    #[must_use]
    pub fn effective_spindle_start_dwell_sec(&self) -> f64 {
        self.spindle_start_dwell_sec.unwrap_or(0.5).max(0.0)
    }

    /// The program-pause word to emit — `M1` (optional stop) when
    /// [`optional_stop`](Self::optional_stop) is set, else `M0` (mandatory
    /// stop). Used for both the `Pause` op and the manual tool-change halt.
    #[must_use]
    pub fn program_pause_code(&self) -> &'static str {
        if self.optional_stop {
            "M1"
        } else {
            "M0"
        }
    }
}

fn default_toolchange_s() -> f64 {
    5.0
}

fn is_default_toolchange_s(v: &f64) -> bool {
    (v - 5.0).abs() < 1e-9
}

fn default_use_kinematic() -> bool {
    true
}

fn is_default_use_kinematic(v: &bool) -> bool {
    *v
}

fn default_work_area() -> AxisLimits {
    AxisLimits {
        x: 200.0,
        y: 300.0,
        z: 50.0,
    }
}

fn is_default_work_area(v: &AxisLimits) -> bool {
    let d = default_work_area();
    (v.x - d.x).abs() < 1e-9 && (v.y - d.y).abs() < 1e-9 && (v.z - d.z).abs() < 1e-9
}

fn default_decimal_separator_char() -> char {
    '.'
}

fn is_default_decimal_separator(v: &char) -> bool {
    *v == '.'
}

fn is_false_bool(v: &bool) -> bool {
    !*v
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            unit: UnitSystem::Mm,
            mode: MachineMode::Mill,
            comments: true,
            arcs: true,
            tool_change: ToolChangeStrategy::ManualM0Pause,
            accel: None,
            jerk: None,
            toolchange_s: default_toolchange_s(),
            spindle_stop_dwell_sec: None,
            spindle_start_dwell_sec: None,
            rapid_speed: None,
            use_kinematic_time_estimate: default_use_kinematic(),
            arc_fit_tolerance_mm: None,
            decimal_separator: '.',
            line_number_start: None,
            plot_mode_z: false,
            post_profile: None,
            work_area: default_work_area(),
            name: String::new(),
            capabilities: Vec::new(),
            spindle_rpm_min: None,
            spindle_rpm_max: None,
            max_feed_mm_min: None,
            park_at_home: false,
            park_xy: None,
            toolchange_xy: None,
            post_change_z: PostChangeZStrategy::None,
            use_tool_length_offsets: false,
            optional_stop: false,
            laser_dynamic_power: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum UnitSystem {
    #[default]
    Mm,
    Inch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MachineMode {
    #[default]
    Mill,
    Laser,
    Drag,
    /// Plasma torch. Emits a two-step Z entry — rapid to
    /// `pierce_height_mm` above stock, dwell `pierce_delay_sec`
    /// while the arc starts and pierces, then G1 to `cut_height_mm`
    /// for the cut. The torch-on / -off lines reuse the laser
    /// helpers (M3 S<power> / M5) since most plasma controllers
    /// accept the same idioms. Tool-on emit lives in
    /// [`crate::gcode::cut_tool_on`].
    Plasma,
}

#[cfg(test)]
mod toolchange_strategy_tests {
    use super::{MachineConfig, ToolChangeStrategy};

    /// Back-compat acceptance: a project saved before the tool-change
    /// widening — with the legacy `"supports_toolchange"` bool — still
    /// loads, mapping `true → Atc` and `false → ManualM0Pause`.
    #[test]
    fn legacy_bool_deserializes() {
        let true_json =
            r#"{"unit":"mm","mode":"mill","comments":true,"arcs":true,"supports_toolchange":true}"#;
        let m: MachineConfig = serde_json::from_str(true_json).unwrap();
        assert_eq!(m.tool_change, ToolChangeStrategy::Atc);

        let false_json = r#"{"unit":"mm","mode":"mill","comments":true,"arcs":true,"supports_toolchange":false}"#;
        let m: MachineConfig = serde_json::from_str(false_json).unwrap();
        assert_eq!(m.tool_change, ToolChangeStrategy::ManualM0Pause);
    }

    /// The new enum string form round-trips through serde.
    #[test]
    fn new_enum_string_round_trips() {
        for s in [
            ToolChangeStrategy::Atc,
            ToolChangeStrategy::ManualM6Prompt,
            ToolChangeStrategy::ManualM0Pause,
            ToolChangeStrategy::Ignore,
        ] {
            let m = MachineConfig {
                tool_change: s,
                ..MachineConfig::default()
            };
            let json = serde_json::to_string(&m).unwrap();
            let back: MachineConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(back.tool_change, s);
        }
    }

    /// A missing field falls back to the portable default.
    #[test]
    fn missing_field_defaults_to_m0_pause() {
        let json = r#"{"unit":"mm","mode":"mill","comments":true,"arcs":true}"#;
        let m: MachineConfig = serde_json::from_str(json).unwrap();
        assert_eq!(m.tool_change, ToolChangeStrategy::ManualM0Pause);
    }

    /// The cache discriminant stays pinned to the original bool encoding
    /// for the two original variants so cache keys stay stable across the
    /// tool-change field widening.
    #[test]
    fn cache_discriminant_pins_original_variants() {
        assert_eq!(ToolChangeStrategy::ManualM0Pause.cache_discriminant(), 0);
        assert_eq!(ToolChangeStrategy::Atc.cache_discriminant(), 1);
        assert_eq!(ToolChangeStrategy::ManualM6Prompt.cache_discriminant(), 2);
        assert_eq!(ToolChangeStrategy::Ignore.cache_discriminant(), 3);
    }
}
