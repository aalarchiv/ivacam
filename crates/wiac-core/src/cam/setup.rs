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
}

fn default_tip_angle_deg() -> f64 {
    60.0
}

impl ToolConfig {
    /// Axial distance from the FULL-diameter shoulder to the tip
    /// point. For a drill / V-bit (`tip_diameter_mm == 0`) this is
    /// `R / tan(apex / 2)`. Engravers (`tip_diameter_mm > 0`)
    /// shorten it by their tip radius. Flat-bottom tools (tip_dia
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
            speed: 18000,
            name: String::new(),
            pause: 1,
            mist: false,
            flood: false,
            dragoff: None,
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
    /// frontend's OpKindPicker filter — a laser-only machine
    /// doesn't show milling ops. `mode` (above) stays as the
    /// PRIMARY mode used by the gcode emitter; capabilities is the
    /// broader set so a multi-purpose machine can pick the right
    /// op set without flipping `mode`. Empty Vec ⇒ implicitly
    /// `[mode]` (back-compat default for old project files).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<MachineMode>,
}

impl MachineConfig {
    /// Effective polyline→arc fit tolerance. Falls back to 0.01 mm.
    #[must_use]
    pub fn effective_arc_tolerance(&self) -> f64 {
        self.arc_fit_tolerance_mm.unwrap_or(0.01).max(0.0)
    }

    /// Effective op-kind capability set (h0tx). Falls back to a vec
    /// containing the primary `mode` so projects that predate the
    /// `capabilities` field still pass through cleanly.
    #[must_use]
    pub fn effective_capabilities(&self) -> Vec<MachineMode> {
        if self.capabilities.is_empty() {
            vec![self.mode]
        } else {
            self.capabilities.clone()
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
            supports_toolchange: false,
            accel: None,
            jerk: None,
            toolchange_s: default_toolchange_s(),
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Setup {
    pub machine: MachineConfig,
    pub tool: ToolConfig,
    pub mill: MillConfig,
    pub pockets: PocketConfig,
    pub tabs: TabsConfig,
    pub leads: LeadsConfig,
}
