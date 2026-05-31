//! Setup tree — port of viaConstructor's `setupdefaults.py`.
//!
//! Initial scope is the subset of fields that `do_pockets` and the gcode
//! emitter actually read. Missing fields land as the gcode pass needs them.

// # CAM/sim pedantic-lint exemptions
// Setup helpers walk over `min`/`max` axis-limit pairs whose names mirror the
// field they project from. Serde `skip_serializing_if = "is_default_…"`
// helpers take `&T` because that's the signature serde requires.
// `MillConfig`, `PocketConfig`, `MachineConfig` are user-facing config
// structs (one bool per UI checkbox); folding bools into enums would
// require flattening the JSON shape and break the schema contract.
#![allow(
    clippy::similar_names,
    clippy::trivially_copy_pass_by_ref,
    clippy::struct_excessive_bools
)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ToolOffset {
    #[default]
    None,
    Outside,
    Inside,
    On,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolConfig {
    pub number: u32,
    pub diameter: f64,
    pub speed: u32,
    /// User-facing tool name, for token substitution in
    /// post-profile templates (rt1.15 `<n>`). Empty by default.
    #[serde(default)]
    pub name: String,
    /// Spindle warm-up pause in seconds.
    pub pause: u32,
    pub mist: bool,
    pub flood: bool,
    /// Drag-knife offset (if present, otherwise None).
    pub dragoff: Option<f64>,
    /// 0t9o: drag-knife self-alignment threshold in radians. The walk
    /// emitter skips the swivel + linear pre-move whenever the corner's
    /// tangent change is below this threshold — real drag knives
    /// self-align below ~30° via the trailing offset. Resolved from
    /// [`crate::project::ToolEntry::drag_knife_self_align_angle_deg`]
    /// at synth time. 0.0 forces the legacy "swivel every corner"
    /// behaviour; the default 30° is applied in setup synthesis.
    #[serde(default)]
    pub drag_self_align_angle_rad: f64,
    /// Plunge feedrate (mm/min).
    pub rate_v: u32,
    /// Cutting feedrate (mm/min).
    pub rate_h: u32,
    /// Resolved RPM for the finishing pass. Equal to `speed` unless the
    /// tool library carried a `speed_finish` override. The gcode emitter
    /// switches to these on level=0 rings of a Pocket op.
    #[serde(default)]
    pub speed_finish: u32,
    /// Resolved plunge feedrate for the finishing pass. mm/min.
    #[serde(default)]
    pub rate_v_finish: u32,
    /// Resolved cutting feedrate for the finishing pass. mm/min.
    #[serde(default)]
    pub rate_h_finish: u32,
    /// Laser pierce time (rt1.29) — seconds to dwell after laser-on
    /// before each plunge so the beam burns through stock. Resolved
    /// from `ToolEntry.laser_pierce_sec` at synth time; 0 = no
    /// pierce dwell.
    #[serde(default)]
    pub pierce_sec: f64,
    /// Wirbeln helical-overlay spiral radius (3e5). > 0 enables the
    /// `cos/sin` displacement on every cut move; 0 disables. Resolved
    /// from `ToolEntry.wirbeln_extra_width_mm / 2` at synth time when
    /// the tool is Wirbeln-tagged.
    #[serde(default)]
    pub wirbeln_radius: f64,
    /// Wirbeln stride along the toolpath per full spiral revolution
    /// (mm). Resolved from `ToolEntry.wirbeln_stepover_mm`, falling
    /// back to half the tool radius. Ignored when `wirbeln_radius`
    /// is 0.
    #[serde(default)]
    pub wirbeln_stepover: f64,
    /// Wirbeln Z-wobble amplitude (mm). The overlay adds a
    /// `cos(3θ)·osc − osc` Z ripple between revolutions when > 0.
    /// Resolved from `ToolEntry.wirbeln_osc_mm`.
    #[serde(default)]
    pub wirbeln_osc: f64,
    /// Wirbeln spiral rotation direction (climb = `true`, conventional
    /// = `false`). Resolved from the op's contour cut direction —
    /// matches Estlcam's `Einstellungen.Gleichlauf` flag.
    #[serde(default = "default_true")]
    pub wirbeln_climb: bool,
    /// Per-tool default XY overlap (dr5). Resolved from
    /// [`crate::project::ToolEntry::default_xy_overlap`] at synth time;
    /// `None` = no tool-level default, fall through to global 0.5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_xy_overlap: Option<f64>,
    /// Full apex angle of the tool tip cone, in degrees. Default 60°
    /// (V-bit shape). The drill emitter uses this together with
    /// `tip_diameter_mm` to compute `tip_cone_length()` — the extra
    /// depth to extend a through-drill cycle so the FULL bore
    /// diameter reaches the bottom of the stock.
    #[serde(default = "default_tip_angle_deg")]
    pub tip_angle_deg: f64,
    /// Diameter at the tip of the cone in mm. 0 = sharp point
    /// (drill, V-bit). > 0 = flat tip (engraver). For flat-bottom
    /// tools (endmill / ball-nose / bull-nose / compression /
    /// t-slot / form-profile / drag-knife / laser) the synth step
    /// sets this equal to `diameter` so `tip_cone_length()` returns
    /// 0 — no auto-extend needed.
    #[serde(default)]
    pub tip_diameter_mm: f64,
    /// z1y0: spindle direction for this tool (`Cw` → M3, `Ccw` → M4).
    /// Mirrored from `ToolEntry.spindle_direction` at synth time so
    /// the gcode emitter can route between `post.spindle_cw` /
    /// `post.spindle_ccw` without reaching back into the tool
    /// library. Default `Cw` keeps legacy projects unchanged.
    #[serde(default)]
    pub spindle_direction: crate::project::tool::SpindleDirection,
    /// zpuk: plasma pierce height in mm (above stock top). The cut
    /// emitter does a rapid to this Z, dwells `pierce_delay_sec`,
    /// then plunges to `cut_height_mm` for the actual cut.
    /// Resolved from
    /// [`crate::project::ToolEntry::pierce_height_mm`] at synth time;
    /// 0.0 ⇒ plasma defaults at emit time (3.8 mm). Only honored
    /// when `setup.machine.mode == MachineMode::Plasma`.
    #[serde(default)]
    pub pierce_height_mm: f64,
    /// zpuk: plasma cut height (mm above stock top). Generally
    /// smaller than `pierce_height_mm`. Resolved from
    /// [`crate::project::ToolEntry::cut_height_mm`] at synth time;
    /// 0.0 ⇒ defaults to 1.5 mm at emit time.
    #[serde(default)]
    pub cut_height_mm: f64,
    /// zpuk: plasma pierce delay in seconds — torch dwells at
    /// `pierce_height` while the arc pierces. Resolved from
    /// [`crate::project::ToolEntry::pierce_delay_sec`] at synth
    /// time; 0.0 ⇒ defaults to 0.5 s at emit time.
    #[serde(default)]
    pub pierce_delay_sec: f64,
    /// ot80: V-Carve lead-in ramp angle (degrees from horizontal).
    /// Resolved from
    /// [`crate::project::ToolEntry::vcarve_lead_in_angle_deg`] at
    /// synth time; clamped to (0°, 90°). 0.0 ⇒ inherit the legacy
    /// 10° default at emit time inside
    /// [`crate::cam::vcarve_emit::ratchet_emit`].
    #[serde(default)]
    pub vcarve_lead_in_angle_deg: f64,
}

fn default_tip_angle_deg() -> f64 {
    60.0
}

impl ToolConfig {
    /// Axial distance from the FULL-diameter shoulder to the tip
    /// point. For a drill / V-bit (`tip_diameter_mm == 0`) this is
    /// `R / tan(apex / 2)`. Engravers (`tip_diameter_mm > 0`)
    /// shorten it by their tip radius. Flat-bottom tools (`tip_dia`
    /// == diameter) return 0.
    #[must_use]
    pub fn tip_cone_length(&self) -> f64 {
        let r = self.diameter * 0.5;
        let tip_r = self.tip_diameter_mm.max(0.0) * 0.5;
        let cut_r = (r - tip_r).max(0.0);
        if cut_r <= 0.0 {
            return 0.0;
        }
        let half = (self.tip_angle_deg.clamp(1.0, 179.0) * 0.5).to_radians();
        let tan_half = half.tan();
        if tan_half < 1e-6 {
            0.0
        } else {
            cut_r / tan_half
        }
    }
}

fn default_true() -> bool {
    true
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            number: 1,
            diameter: 3.0,
            tip_angle_deg: default_tip_angle_deg(),
            // Default tip_diameter = diameter = flat-bottom; the drill
            // synthesis path overrides to 0 (sharp point) when the
            // tool's kind warrants it.
            tip_diameter_mm: 3.0,
            spindle_direction: crate::project::tool::SpindleDirection::Cw,
            speed: 18000,
            name: String::new(),
            pause: 1,
            mist: false,
            flood: false,
            dragoff: None,
            drag_self_align_angle_rad: 30.0_f64.to_radians(),
            rate_v: 100,
            rate_h: 800,
            speed_finish: 18000,
            rate_v_finish: 100,
            rate_h_finish: 800,
            pierce_sec: 0.0,
            wirbeln_radius: 0.0,
            wirbeln_stepover: 0.0,
            wirbeln_osc: 0.0,
            wirbeln_climb: true,
            default_xy_overlap: None,
            pierce_height_mm: 0.0,
            cut_height_mm: 0.0,
            pierce_delay_sec: 0.0,
            vcarve_lead_in_angle_deg: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MillConfig {
    pub active: bool,
    pub depth: f64,
    pub start_depth: f64,
    /// Per-pass z step (negative ⇒ down).
    pub step: f64,
    pub fast_move_z: f64,
    pub helix_mode: bool,
    pub reverse: bool,
    pub objectorder: ObjectOrder,
    pub offset: ToolOffset,
    /// When true, dip into sharp inner corners so the cutter fully clears
    /// them. Mirrors viaConstructor's `mill.overcut`.
    #[serde(default)]
    pub overcut: bool,
    /// How the cutter descends into material at the start of each Z
    /// pass. `Direct` is a straight plunge; `Ramp` walks the first
    /// `ramp_length` of the path while linearly descending Z so the
    /// cutter takes a chip in both Z and XY simultaneously.
    #[serde(default)]
    pub plunge: PlungeStrategy,
    /// When > 0, slow the feedrate at sharp line-to-line corners by
    /// this fraction so the machine doesn't dwell on the corner with
    /// high accel demand. 0.0 = no reduction (current behavior). 0.5 =
    /// half the feed at corners. Most useful for zigzag pocket fills
    /// with their many 180° turns.
    #[serde(default)]
    pub corner_feed_reduction: f64,
    /// Optional smaller step for the FINAL Z pass (cleaner bottom
    /// finish). None = use `step` for every pass.
    #[serde(default)]
    pub finish_step: Option<f64>,
    /// Cut past `depth` by this many mm (positive). Used for
    /// through-cuts on edge-clamped sheet.
    #[serde(default)]
    pub through_depth: f64,
    /// Explicit ordered list of Z depths to cut at. When non-empty,
    /// overrides the step / `finish_step` / `through_depth` schedule.
    #[serde(default)]
    pub depth_list: Vec<f64>,
}

/// Per-pass entry strategy.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlungeStrategy {
    /// Straight plunge — current behavior. Safe for end mills with
    /// center-cutting geometry on shallow steps; risky on harder
    /// materials or non-center-cutting bits.
    Direct,
    /// Linear ramp into the first cut: descend Z at `angle_deg` from
    /// the previous Z to the current pass Z while walking forward
    /// along the path. The horizontal distance traveled during the
    /// ramp is `step / tan(angle_deg)`. Falls back to Direct if the
    /// path is shorter than the required ramp.
    Ramp { angle_deg: f64 },
    /// Helical descent: spiral down on a circle of `radius_mm` around
    /// a point inside the closed pocket boundary, descending Z at
    /// `angle_deg` per revolution. `radius_mm = None` ⇒ auto-fit to
    /// the largest inscribed circle inside the pocket boundary at
    /// gcode-emission time. Falls back to Ramp (and then Direct)
    /// when the helix circle can't fit.
    Helix {
        angle_deg: f64,
        #[serde(deserialize_with = "deserialize_helix_radius")]
        radius_mm: Option<f64>,
    },
}

/// Accept the new `null` form AND the legacy bare-number form
/// (`"radius_mm": 5.0`) saved by pre-rt1.2 projects. Required for
/// project-file backward compatibility.
fn deserialize_helix_radius<'de, D>(de: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct HelixRadiusVisitor;
    impl<'de> Visitor<'de> for HelixRadiusVisitor {
        type Value = Option<f64>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a number, null, or an absent field")
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_any(HelixRadiusVisitor)
        }
        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v as f64))
        }
        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v as f64))
        }
        fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Err(de::Error::custom("radius_mm must be a number or null"))
        }
    }
    de.deserialize_any(HelixRadiusVisitor)
}

impl Default for PlungeStrategy {
    fn default() -> Self {
        Self::Direct
    }
}

impl Default for MillConfig {
    fn default() -> Self {
        Self {
            active: true,
            depth: -2.0,
            start_depth: 0.0,
            step: -1.0,
            fast_move_z: 5.0,
            helix_mode: false,
            reverse: false,
            objectorder: ObjectOrder::default(),
            offset: ToolOffset::None,
            overcut: false,
            plunge: PlungeStrategy::default(),
            corner_feed_reduction: 0.0,
            finish_step: None,
            through_depth: 0.0,
            depth_list: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObjectOrder {
    #[default]
    Nearest,
    PerObject,
    Unordered,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PocketConfig {
    pub active: bool,
    pub islands: bool,
    pub zigzag: bool,
    pub insideout: bool,
    /// Skip the boundary contour pass (used by HATCH-equivalent layers).
    pub nocontour: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TabsConfig {
    pub active: bool,
    pub width: f64,
    /// Z height the cutter lifts to over a tab (positive distance above
    /// the cut floor). The actual tab Z is `mill.depth + tabs.height`.
    pub height: f64,
    pub tab_type: TabType,
    /// Ramp angle in degrees, used only when `tab_type == Ramp`. The
    /// horizontal length of each ramp into / out of a tab is
    /// `tabs.height / tan(ramp_angle_deg)`. 30° gives a 1:√3 slope.
    /// Ignored for Rectangle tabs.
    #[serde(
        default = "default_ramp_angle",
        skip_serializing_if = "is_default_ramp_angle"
    )]
    pub ramp_angle_deg: f64,
}

fn default_ramp_angle() -> f64 {
    30.0
}

fn is_default_ramp_angle(angle: &f64) -> bool {
    (angle - 30.0).abs() < 1e-9
}

impl Default for TabsConfig {
    fn default() -> Self {
        Self {
            active: false,
            width: 10.0,
            height: 1.0,
            tab_type: TabType::Rectangle,
            ramp_angle_deg: default_ramp_angle(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TabType {
    #[default]
    Rectangle,
    Ramp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LeadsConfig {
    pub r#in: LeadKind,
    pub out: LeadKind,
    /// Lead-in size. Interpreted by `LeadKind`:
    /// * `Straight`: straight-line LENGTH (mm) of the approach.
    /// * `Arc`: tangent roll-on arc RADIUS (mm). The arc sweeps a quarter
    ///   turn from the approach point (radius away on the perpendicular)
    ///   to the contour start, landing tangent to the first cut segment.
    /// * `Off`: ignored.
    pub in_lenght: f64,
    /// Lead-out size. Same interpretation as `in_lenght` but applied at
    /// the END of the cut path (cutter rolls off the contour at Pn).
    pub out_lenght: f64,
}

impl Default for LeadsConfig {
    fn default() -> Self {
        Self {
            r#in: LeadKind::Off,
            out: LeadKind::Off,
            in_lenght: 5.0,
            out_lenght: 5.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LeadKind {
    #[default]
    Off,
    Straight,
    Arc,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MachineConfig {
    pub unit: UnitSystem,
    pub mode: MachineMode,
    pub comments: bool,
    /// Whether the machine emits arc commands (G2/G3).
    pub arcs: bool,
    pub supports_toolchange: bool,
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
    /// See bd issues eaeq / m8sq / rwv8 / rfow.
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
    /// Decimal separator for emitted numbers (rt1.36). `'.'` (default)
    /// suits `LinuxCNC` / GRBL / Mach3 and any controller configured in
    /// US locale. `','` covers European-locale Siemens / Heidenhain
    /// controllers that require `X1,5` instead of `X1.5`. Anything
    /// other than '.' / ',' silently falls back to '.'.
    #[serde(
        default = "default_decimal_separator_char",
        skip_serializing_if = "is_default_decimal_separator"
    )]
    pub decimal_separator: char,
    /// Starting line number for `N<n>` prefixes (rt1.36). `None` (the
    /// default) emits unnumbered lines. `Some(10)` emits `N10`, `N20`,
    /// `N30`, … incrementing by 10. Required by some FANUC / vintage
    /// controllers; useful operator reference even on modern ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_number_start: Option<u32>,
    /// Plot-mode Z (rt1.35 / Estlcam `c_PP.Z_Up_Dn)`: when true, the
    /// pipeline collapses every cut to ONE pass at the op's cut depth
    /// and skips the multi-step descent / ramp / helix machinery.
    /// Z values written into gcode are restricted to `fast_move_z`
    /// (pen up between cuts) and the op's `depth` (pen down on
    /// cut moves). Right setting for laser / plasma / pen plotters /
    /// 3D-printer extrusion and drag-knife controllers.
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub plot_mode_z: bool,
    /// User-configurable post-processor profile (rt1.15). When
    /// `Some`, the built-in posts (linuxcnc / grbl) read its
    /// templates instead of emitting their hard-coded
    /// `program_start` / `program_end` / `tool_change` / coolant lines.
    /// `None` = hard-coded defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_profile: Option<crate::gcode::post_profile::PostProfile>,
    /// h0tx: free-text identifier for the machine setup ("Shop CNC",
    /// "Garage MPCNC", …). Empty string by default; persisted into
    /// the project file + the `.wiac-machine.json` save/load files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// h0tx: which op kinds the machine can run. Drives the
    /// frontend's `OpKindPicker` filter — a laser-only machine
    /// doesn't show milling ops. `mode` (above) stays as the
    /// PRIMARY mode used by the gcode emitter; capabilities is the
    /// broader set so a multi-purpose machine can pick the right
    /// op set without flipping `mode`. Empty Vec ⇒ implicitly
    /// `[mode]` (back-compat default for old project files).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<MachineMode>,
    /// 3nnj: lower bound on the spindle RPM the controller will
    /// accept. Tool / op RPMs below this clamp UP to the min and
    /// emit a `spindle_speed_clamped_below_min` warning. `None`
    /// disables the floor (default, back-compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spindle_rpm_min: Option<u32>,
    /// 3nnj: upper bound on the spindle RPM the controller will
    /// accept. Tool / op RPMs above this clamp DOWN to the max
    /// and emit a `spindle_speed_clamped_above_max` warning.
    /// Without this clamp some controllers refuse the command
    /// mid-program; others silently cap and produce wrong chipload.
    /// `None` disables the ceiling (default, back-compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spindle_rpm_max: Option<u32>,
    /// jcmx: upper bound on the cutting / plunge feed (mm/min) the
    /// machine can actually drive. Tool / op feeds above this clamp
    /// DOWN to the max and emit a `feed_clamped_above_max` warning, so
    /// an out-of-range feed (a fat-fingered op override, an aggressive
    /// tool-library value) can't reach the controller as a verbatim
    /// `F<huge>` — which some controllers fault on and others silently
    /// cap, both producing a program that doesn't run as previewed.
    /// `None` disables the ceiling (default, back-compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_feed_mm_min: Option<u32>,
    /// syol: when true, the `program_end` footer adds a `G53 G0 X0 Y0`
    /// retract-to-machine-home before the spindle-off + M30 sequence.
    /// Most hobby controllers (`LinuxCNC`, Mach3) honor G53; GRBL accepts
    /// it from v1.1 onward. When false, falls back to a `G0 X0 Y0` in
    /// the current WCS (the work zero) — still safer than leaving the
    /// spindle parked over the part. Both modes lift to `fast_move_z`
    /// first.
    #[serde(default, skip_serializing_if = "is_false_bool")]
    pub park_at_home: bool,
    /// syol: optional explicit park XY (mm, in WCS coordinates). When
    /// `Some`, the `program_end` footer routes the head to this point
    /// after the safe-Z lift, overriding the machine-home / work-zero
    /// fallback. Useful for a known tool-station / load-station that
    /// isn't (0, 0) in either frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub park_xy: Option<(f64, f64)>,
    /// ad0v: optional tool-change position (mm, in MACHINE coordinates).
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
            supports_toolchange: false,
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
    /// zpuk: plasma torch. Emits a two-step Z entry — rapid to
    /// `pierce_height_mm` above stock, dwell `pierce_delay_sec`
    /// while the arc starts and pierces, then G1 to `cut_height_mm`
    /// for the cut. The torch-on / -off lines reuse the laser
    /// helpers (M3 S<power> / M5) since most plasma controllers
    /// accept the same idioms. Tool-on emit lives in
    /// [`crate::gcode::cut_tool_on`].
    Plasma,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Setup {
    pub machine: MachineConfig,
    pub tool: ToolConfig,
    pub mill: MillConfig,
    pub pockets: PocketConfig,
    pub tabs: TabsConfig,
    pub leads: LeadsConfig,
    /// e2mq: program-active work coordinate system. Threaded in from
    /// `Project.work_offset.wcs` by the pipeline `setup_resolver` /
    /// `header_setup_for` builders. The post's `program_begin`
    /// emits the explicit `G54..G59` from this and pins the same
    /// value into `PostState.wcs` so `tool_z_shift` writes its
    /// `G10 L20 P<n>` against the *active* WCS (P1=G54, …, P6=G59),
    /// not a hardcoded P1. Defaults to G54 — back-compat for
    /// projects that don't set `work_offset.wcs`.
    #[serde(default, skip_serializing_if = "is_default_wcs")]
    pub wcs: crate::project::Wcs,
}

fn is_default_wcs(v: &crate::project::Wcs) -> bool {
    matches!(v, crate::project::Wcs::G54)
}
