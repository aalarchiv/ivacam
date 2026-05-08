//! Setup tree — port of viaConstructor's `setupdefaults.py`.
//!
//! Initial scope is the subset of fields that `do_pockets` and the gcode
//! emitter actually read. Missing fields land as the gcode pass needs them.

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
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            number: 1,
            diameter: 3.0,
            speed: 18000,
            pause: 1,
            mist: false,
            flood: false,
            dragoff: None,
            rate_v: 100,
            rate_h: 800,
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
    /// `angle_deg` per revolution. After the helix lands at the pass
    /// depth the cutter walks to the path's actual start XY and
    /// continues normally. Each revolution drops Z by
    /// `2 * π * radius_mm * tan(angle_deg)`. Only meaningful for
    /// closed pocket paths whose boundary fits the helix circle —
    /// falls back to Ramp (and then Direct) otherwise. Standard for
    /// non-center-cutting endmills and harder materials.
    Helix { angle_deg: f64, radius_mm: f64 },
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    #[serde(default = "default_ramp_angle", skip_serializing_if = "is_default_ramp_angle")]
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LeadsConfig {
    pub r#in: LeadKind,
    pub out: LeadKind,
    pub in_lenght: f64,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MachineConfig {
    pub unit: UnitSystem,
    pub mode: MachineMode,
    pub comments: bool,
    /// Whether the machine emits arc commands (G2/G3).
    pub arcs: bool,
    pub supports_toolchange: bool,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            unit: UnitSystem::Mm,
            mode: MachineMode::Mill,
            comments: true,
            arcs: true,
            supports_toolchange: false,
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
