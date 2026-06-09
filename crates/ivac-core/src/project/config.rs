//! Cut/tool op-config wire types — the JsonSchema structs that describe
//! how a single op cuts (tool geometry, mill schedule, plunge strategy,
//! pocket flags). These are project-level wire types; the runtime-resolved
//! [`crate::cam::setup::Setup`] bundle embeds them.

// # CAM/sim pedantic-lint exemptions
// Serde `skip_serializing_if = "is_default_…"` helpers take `&T` because
// that's the signature serde requires. `MillConfig`, `PocketConfig` are
// user-facing config structs (one bool per UI checkbox); folding bools into
// enums would require flattening the JSON shape and break the schema contract.
#![allow(
    clippy::cast_precision_loss,
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
    /// post-profile templates (`<n>`). Empty by default.
    #[serde(default)]
    pub name: String,
    /// Spindle warm-up pause in seconds.
    pub pause: u32,
    pub mist: bool,
    pub flood: bool,
    /// Drag-knife offset (if present, otherwise None).
    pub dragoff: Option<f64>,
    /// Drag-knife self-alignment threshold in radians. The walk
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
    /// Laser pierce time — seconds to dwell after laser-on
    /// before each plunge so the beam burns through stock. Resolved
    /// from `ToolEntry.laser_pierce_sec` at synth time; 0 = no
    /// pierce dwell.
    #[serde(default)]
    pub pierce_sec: f64,
    /// Whirl helical-overlay spiral radius. > 0 enables the
    /// sine/cosine displacement on every cut move; 0 disables. Resolved
    /// from `ToolEntry.whirl_extra_width_mm / 2` at synth time when
    /// the tool is Whirl-tagged.
    #[serde(default)]
    pub whirl_radius: f64,
    /// Whirl stride along the toolpath per full spiral revolution
    /// (mm). Resolved from `ToolEntry.whirl_stepover_mm`, falling
    /// back to half the tool radius. Ignored when `whirl_radius`
    /// is 0.
    #[serde(default)]
    pub whirl_stepover: f64,
    /// Whirl Z-wobble amplitude (mm). The overlay adds a
    /// `cos(3θ)·osc − osc` Z ripple between revolutions when > 0.
    /// Resolved from `ToolEntry.whirl_osc_mm`.
    #[serde(default)]
    pub whirl_osc: f64,
    /// Whirl spiral rotation direction (climb = `true`, conventional
    /// = `false`). Resolved from the op's contour cut direction —
    /// matches Estlcam's `Einstellungen.Gleichlauf` flag.
    #[serde(default = "default_true")]
    pub whirl_climb: bool,
    /// Per-tool default XY overlap. Resolved from
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
    /// Spindle direction for this tool (`Cw` → M3, `Ccw` → M4).
    /// Mirrored from `ToolEntry.spindle_direction` at synth time so
    /// the gcode emitter can route between `post.spindle_cw` /
    /// `post.spindle_ccw` without reaching back into the tool
    /// library. Defaults to `Cw` (M3).
    #[serde(default)]
    pub spindle_direction: crate::project::tool::SpindleDirection,
    /// Plasma pierce height in mm (above stock top). The cut
    /// emitter does a rapid to this Z, dwells `pierce_delay_sec`,
    /// then plunges to `cut_height_mm` for the actual cut.
    /// Resolved from
    /// [`crate::project::ToolEntry::pierce_height_mm`] at synth time;
    /// 0.0 ⇒ plasma defaults at emit time (3.8 mm). Only honored
    /// when `setup.machine.mode == MachineMode::Plasma`.
    #[serde(default)]
    pub pierce_height_mm: f64,
    /// Plasma cut height (mm above stock top). Generally
    /// smaller than `pierce_height_mm`. Resolved from
    /// [`crate::project::ToolEntry::cut_height_mm`] at synth time;
    /// 0.0 ⇒ defaults to 1.5 mm at emit time.
    #[serde(default)]
    pub cut_height_mm: f64,
    /// Plasma pierce delay in seconds — torch dwells at
    /// `pierce_height` while the arc pierces. Resolved from
    /// [`crate::project::ToolEntry::pierce_delay_sec`] at synth
    /// time; 0.0 ⇒ defaults to 0.5 s at emit time.
    #[serde(default)]
    pub pierce_delay_sec: f64,
    /// V-Carve lead-in ramp angle (degrees from horizontal).
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
            whirl_radius: 0.0,
            whirl_stepover: 0.0,
            whirl_osc: 0.0,
            whirl_climb: true,
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
    /// Skip the boundary contour pass (used by HATCH-equivalent layers).
    pub nocontour: bool,
}
