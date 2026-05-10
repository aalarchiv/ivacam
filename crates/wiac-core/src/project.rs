//! Project = geometry + machine + tool library + ordered list of
//! Operations. The Operation is the unit of CAM work — each one carries a
//! tool reference and per-kind parameters and produces a gcode block in
//! the final program.
//!
//! Modeled after mainstream CAM tools (Carbide Create, Fusion 360 CAM,
//! Estlcum, FreeCAD Path Workbench) so the user's mental model translates
//! without surprises.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::offsets::TabPoint;
use crate::cam::setup::{
    LeadKind, LeadsConfig, MachineConfig, ObjectOrder, TabType, TabsConfig, ToolOffset,
};
use crate::cam::source_combine::FrameShape;
use crate::geometry::Segment;

// ─── top level ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    /// Imported geometry — the same `segments` the existing pipeline
    /// consumes. We keep it inline rather than referencing it by id so the
    /// project file is self-contained.
    pub segments: Vec<Segment>,

    pub machine: MachineConfig,
    pub tools: Vec<ToolEntry>,
    pub operations: Vec<Operation>,

    /// Tab placements keyed by imported-segment index. Same shape as the
    /// legacy PipelineRequest.tabs.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tabs: HashMap<u32, Vec<TabPoint>>,

    /// Fixtures (clamps, dogs, vise jaws, hold-downs) the cutter must
    /// avoid throughout the entire program — including rapids. The sim
    /// pass tests every toolpath segment against this set and emits
    /// `SimWarning::FixtureCollision` on overlap. Default empty: a
    /// project with no fixtures behaves exactly as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixtures: Vec<Fixture>,
}

// ─── fixtures ─────────────────────────────────────────────────────────────

/// A user-declared physical obstacle on the stock the cutter must miss.
/// Lives in stock-relative XY (same frame as the imported geometry) and
/// occupies a Z range; the sim collision test gates on that range first
/// then falls back to a per-shape XY swept-region check.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Fixture {
    pub id: u32,
    pub name: String,
    pub kind: FixtureKind,
    /// Center of the fixture in stock XY (mm).
    pub origin: (f64, f64),
    /// Z range the fixture occupies (relative to stock-top = 0). Typically
    /// `z_top` is positive (a clamp standing above stock); both can be
    /// negative for cleats below.
    pub z_bottom: f64,
    pub z_top: f64,
    /// Visual color in 2D / 3D previews, packed RGBA (0xRRGGBBAA).
    #[serde(default = "default_fixture_color")]
    pub color: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum FixtureKind {
    /// Axis-aligned rectangle centered on `origin`.
    Box { width: f64, depth: f64 },
    /// Cylinder centered on `origin`.
    Cylinder { radius: f64 },
    /// Polygon outline in fixture-local coordinates (origin-relative).
    Polygon { vertices: Vec<(f64, f64)> },
}

fn default_fixture_color() -> u32 {
    0xFFA0_50C0
}

// ─── tools ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolEntry {
    pub id: u32,
    pub name: String,
    pub kind: ToolKind,
    pub diameter: f64,
    /// V-bit tip diameter (None for endmill / ball nose / drag knife).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip_diameter: Option<f64>,
    /// V-bit full included tip angle in degrees (apex angle of the cone).
    /// Drives the V-Carve depth-from-width relationship
    /// `z = -R / tan(tip_angle / 2)`. Validated to (0, 180); defaults to
    /// 60° for the most common engraving V-bit.
    #[serde(default = "default_tip_angle_deg")]
    pub tip_angle_deg: f64,
    /// Drag-knife trailing offset (None for everything else).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dragoff: Option<f64>,
    pub flutes: u8,
    pub speed: u32,
    /// Plunge feedrate (mm/min).
    pub plunge_rate: u32,
    /// Cutting feedrate (mm/min).
    pub feed_rate: u32,
    pub coolant: Coolant,
    /// Default depth-per-pass (negative, mm). Operations using this tool
    /// inherit this when their own `step` is unset. None = no default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_step: Option<f64>,
    /// Spindle warm-up pause in seconds applied once per used tool by
    /// the time estimator. Mirrors `ToolConfig.pause`.
    #[serde(default = "default_tool_pause", skip_serializing_if = "is_default_tool_pause")]
    pub pause: u32,
}

fn default_tool_pause() -> u32 {
    1
}

fn is_default_tool_pause(v: &u32) -> bool {
    *v == 1
}

impl Default for ToolEntry {
    fn default() -> Self {
        Self {
            id: 1,
            name: "3 mm endmill".into(),
            kind: ToolKind::Endmill,
            diameter: 3.0,
            tip_diameter: None,
            tip_angle_deg: default_tip_angle_deg(),
            dragoff: None,
            flutes: 2,
            speed: 18_000,
            plunge_rate: 100,
            feed_rate: 800,
            coolant: Coolant::Off,
            default_step: None,
            pause: default_tool_pause(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    #[default]
    Endmill,
    BallNose,
    VBit,
    Engraver,
    DragKnife,
    Drill,
    /// Used for laser cutting / etching — no physical tool radius.
    LaserBeam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Coolant {
    #[default]
    Off,
    Mist,
    Flood,
}

// ─── operations ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Operation {
    pub id: u32,
    pub name: String,
    pub enabled: bool,
    pub kind: OperationKind,
    /// id of a `Project.tools` entry.
    pub tool_id: u32,
    pub source: OperationSource,
    pub params: OperationParams,
    /// Optional pattern repetition. When set, the op runs once per
    /// pattern instance with the source geometry translated/rotated.
    /// See [`PatternConfig`] for the concrete pattern shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<PatternConfig>,
}

impl Default for Operation {
    fn default() -> Self {
        Self {
            id: 1,
            name: "Profile".into(),
            enabled: true,
            kind: OperationKind::Profile {
                offset: ToolOffset::Outside,
            },
            tool_id: 1,
            source: OperationSource::All,
            params: OperationParams::default(),
            pattern: None,
        }
    }
}

/// Pattern repetition for an [`Operation`]. When attached, the pipeline
/// expands the op into N instances by translating (or rotating) the
/// source geometry per instance — useful for "drill the same hole
/// pattern N times" or "pocket N copies of the same shape on a grid".
///
/// The original geometry stays at the (0, 0) translation / 0° rotation
/// instance so a single-instance pattern is identical to no pattern at
/// all.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PatternConfig {
    /// 1D linear array. `count` instances total (including the original
    /// at offset (0, 0)). Each instance i is translated by (i*dx, i*dy).
    Linear { count: u32, dx: f64, dy: f64 },
    /// 2D rectangular grid. `count_x × count_y` instances total.
    /// Instance (i, j) is translated by (i*dx, j*dy).
    Grid {
        count_x: u32,
        count_y: u32,
        dx: f64,
        dy: f64,
    },
    /// Polar (rotational) array. `count` instances around
    /// (`center_x`, `center_y`), with `angle_step_deg` between
    /// consecutive instances. Instance i is rotated by
    /// i * angle_step_deg about that center.
    Polar {
        count: u32,
        center_x: f64,
        center_y: f64,
        angle_step_deg: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum OperationKind {
    /// Contour cut — equivalent to today's "mill" with a parallel-offset
    /// pass at `offset` of the tool radius.
    Profile { offset: ToolOffset },
    /// Pocket fill — cascade of inward offsets, optionally zigzag.
    Pocket { strategy: PocketStrategy },
    /// Drill cycle — point or circle smaller than tool. Carries a
    /// [`DrillCycle`] that picks G81 / G83 / G73 (or the manual G0/G1
    /// fallback for posts that don't support canned cycles).
    Drill { cycle: DrillCycle },
    /// Helical thread — bore + helical thread cut.
    Thread,
    /// V-bit edge break.
    Chamfer,
    /// Tool-on engraving — no offset, follows the source path.
    Engrave,
    /// Drag-knife — emits trail-compensation moves.
    DragKnife,
    /// Helical entry into a closed contour.
    Helix,
    /// V-Carve: drives a V-bit along the medial axis of a closed region,
    /// with depth varying per point so the V's tip dips deepest where the
    /// region is widest. The depth at each point is
    /// `z = -R_inscribed / tan(tip_angle / 2)` for the inscribed-circle
    /// radius `R_inscribed` at that point of the medial axis.
    VCarve,
}

fn default_tip_angle_deg() -> f64 {
    60.0
}

/// Drill-cycle picker for [`OperationKind::Drill`]. Mirrors the canned
/// cycles G81 / G83 / G73 from the LinuxCNC / Fanuc dialect plus the
/// dwell-at-bottom parameter PyCAM's `Drilling.py` exposes. Posts that
/// don't support canned cycles fall back to a manual G0/G1 expansion of
/// the same cycle (see `PostProcessor::drill_*` defaults).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DrillCycle {
    /// G81 — single plunge to depth, retract.
    Simple {
        /// Dwell at bottom in seconds before retract. 0 = no dwell.
        #[serde(default)]
        dwell_sec: f64,
    },
    /// G83 — peck with full retract to clearance plane between pecks.
    Peck {
        peck_step_mm: f64,
        #[serde(default)]
        dwell_sec: f64,
    },
    /// G73 — peck with chip-break (small partial retract between pecks).
    ChipBreak {
        peck_step_mm: f64,
        #[serde(default)]
        dwell_sec: f64,
    },
}

impl Default for DrillCycle {
    fn default() -> Self {
        DrillCycle::Simple { dwell_sec: 0.0 }
    }
}

/// Pocket strategy selector. Cascade / Zigzag / Spiral serialize as
/// bare strings (`"cascade"`, `"zigzag"`, `"spiral"`) for wire
/// compatibility with pre-Trochoidal payloads. Trochoidal serializes
/// as a tagged object
/// `{ "kind": "trochoidal", "engagement_angle_deg": ..., "loop_radius_factor": ... }`
/// since it carries parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PocketStrategy {
    Cascade,
    Zigzag,
    Spiral,
    Trochoidal {
        engagement_angle_deg: f64,
        loop_radius_factor: f64,
    },
}

impl Default for PocketStrategy {
    fn default() -> Self {
        Self::Cascade
    }
}

impl Serialize for PocketStrategy {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        match *self {
            Self::Cascade => ser.serialize_str("cascade"),
            Self::Zigzag => ser.serialize_str("zigzag"),
            Self::Spiral => ser.serialize_str("spiral"),
            Self::Trochoidal {
                engagement_angle_deg,
                loop_radius_factor,
            } => {
                let mut s = ser.serialize_struct("Trochoidal", 3)?;
                s.serialize_field("kind", "trochoidal")?;
                s.serialize_field("engagement_angle_deg", &engagement_angle_deg)?;
                s.serialize_field("loop_radius_factor", &loop_radius_factor)?;
                s.end()
            }
        }
    }
}

impl JsonSchema for PocketStrategy {
    fn schema_name() -> String {
        "PocketStrategy".to_string()
    }
    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        let s = serde_json::json!({
            "oneOf": [
                {
                    "type": "string",
                    "enum": ["cascade", "zigzag", "spiral"]
                },
                {
                    "type": "object",
                    "required": ["kind", "engagement_angle_deg", "loop_radius_factor"],
                    "properties": {
                        "kind": { "type": "string", "enum": ["trochoidal"] },
                        "engagement_angle_deg": { "type": "number", "format": "double" },
                        "loop_radius_factor": { "type": "number", "format": "double" }
                    }
                }
            ]
        });
        serde_json::from_value(s).expect("static PocketStrategy schema")
    }
}

impl<'de> Deserialize<'de> for PocketStrategy {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Str(String),
            Obj {
                kind: String,
                #[serde(default)]
                engagement_angle_deg: Option<f64>,
                #[serde(default)]
                loop_radius_factor: Option<f64>,
            },
        }
        match Repr::deserialize(de)? {
            Repr::Str(s) => match s.as_str() {
                "cascade" => Ok(Self::Cascade),
                "zigzag" => Ok(Self::Zigzag),
                "spiral" => Ok(Self::Spiral),
                other => Err(serde::de::Error::unknown_variant(
                    other,
                    &["cascade", "zigzag", "spiral", "trochoidal"],
                )),
            },
            Repr::Obj {
                kind,
                engagement_angle_deg,
                loop_radius_factor,
            } => match kind.as_str() {
                "cascade" => Ok(Self::Cascade),
                "zigzag" => Ok(Self::Zigzag),
                "spiral" => Ok(Self::Spiral),
                "trochoidal" => Ok(Self::Trochoidal {
                    engagement_angle_deg: engagement_angle_deg.unwrap_or(30.0),
                    loop_radius_factor: loop_radius_factor.unwrap_or(0.6),
                }),
                other => Err(serde::de::Error::unknown_variant(
                    other,
                    &["cascade", "zigzag", "spiral", "trochoidal"],
                )),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum OperationSource {
    /// Run on every chain on the listed layer names.
    Layers {
        layers: Vec<String>,
        #[serde(default, skip_serializing_if = "SourceCombine::is_default")]
        combine: SourceCombine,
    },
    /// Run on the listed chain ids only.
    Objects {
        ids: Vec<u32>,
        #[serde(default, skip_serializing_if = "SourceCombine::is_default")]
        combine: SourceCombine,
    },
    /// Run on every chained object in the project.
    All,
}

impl Default for OperationSource {
    fn default() -> Self {
        Self::All
    }
}

/// How a multi-object source selection is combined into the region(s) the
/// operation actually consumes. Default is `Auto` — containment-based,
/// which gives the user "outer + inner = annulus" behavior with no extra
/// thought. The other modes are clipper2-driven boolean ops; `None` keeps
/// each selected object as its own boundary (the pre-combine behavior,
/// surfaced for callers who really want it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceCombine {
    /// Containment-aware: nested closed objects in the selection become
    /// islands of their outermost selected ancestor. Equivalent to today's
    /// pipeline-level behavior (see pipeline.rs's selected_set logic).
    #[default]
    Auto,
    /// Boolean union of all selected closed polygons.
    Union,
    /// First selected polygon minus the union of the rest.
    Difference,
    /// Boolean intersection of all selected closed polygons.
    Intersection,
    /// Symmetric difference (xor) of all selected closed polygons.
    Xor,
    /// No combination — emit one boundary per selected object as-is. This
    /// is the pre-j7y behavior, kept for callers who explicitly want it.
    None,
}

impl SourceCombine {
    fn is_default(&self) -> bool {
        matches!(self, SourceCombine::Auto)
    }
}

/// Climb vs conventional milling. Determines the path winding the
/// generator emits — for a standard right-hand spindle:
///
/// | context (cutter location) | conventional      | climb              |
/// |---------------------------|-------------------|--------------------|
/// | outer (cutter outside)    | CW (area < 0)     | CCW (area > 0)     |
/// | inner (cutter in pocket)  | CCW (area > 0)    | CW (area < 0)      |
///
/// "Conventional" is the safer default — most hobby and older mills
/// don't have the rigidity / backlash takeup needed for clean climb
/// cuts. Climb gives a better surface finish but requires a stiff
/// machine. The finishing pass typically stays conventional regardless
/// of the main strategy because the finish wall quality matters most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CutDirection {
    #[default]
    Conventional,
    Climb,
}

impl CutDirection {
    fn is_default(&self) -> bool {
        matches!(self, CutDirection::Conventional)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct OperationParams {
    /// Final cut depth (negative number — a depth, not a height).
    pub depth: f64,
    /// Z at which the first pass starts.
    pub start_depth: f64,
    /// Per-pass step (negative ⇒ down). None = inherit from
    /// `ToolEntry.default_step`. Legacy projects wrote a bare `0.0` to
    /// mean "unset"; the deserializer maps that to None.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_step",
        skip_serializing_if = "Option::is_none"
    )]
    pub step: Option<f64>,
    /// Z for rapid moves between cuts.
    pub fast_move_z: f64,
    /// XY overlap between consecutive pocket cuts, as a fraction in
    /// (0, 1). Drives the cascade step (= tool_diameter * (1 - overlap))
    /// and the zigzag stride. 0.5 = 50% overlap = 50% stepover, a
    /// conservative default that fills tight pockets cleanly. Higher
    /// overlap = smaller step = more rings, slower cut, better finish.
    /// Lower overlap = bigger step = fewer rings, faster cut, may leave
    /// stripes. Honored only for Pocket ops; ignored elsewhere. Stored
    /// at 0.0 means "use the default" so old payloads still work.
    #[serde(default)]
    pub xy_overlap: f64,
    /// Helical descent inside a closed contour.
    #[serde(default)]
    pub helix: bool,
    /// Reverse the cut direction (climb ↔ conventional).
    #[serde(default)]
    pub reverse: bool,
    /// Cut-order strategy for multiple objects.
    #[serde(default)]
    pub objectorder: ObjectOrder,
    /// Dip into sharp inner corners so the cutter clears the geometric
    /// corner. Only meaningful for Profile ops with non-zero offset.
    #[serde(default)]
    pub overcut: bool,

    // Pocket-specific extras (only honored when kind == Pocket):
    #[serde(default)]
    pub pocket_islands: bool,
    #[serde(default)]
    pub pocket_nocontour: bool,
    #[serde(default)]
    pub pocket_insideout: bool,

    /// Per-op tabs config. The Project's `tabs` map carries the actual
    /// placement points; this controls width / height / type.
    #[serde(default)]
    pub tabs: TabsConfig,

    /// Lead-in / lead-out shape for this op.
    #[serde(default)]
    pub leads: LeadsConfig,

    /// Cut direction for the main (roughing) passes.
    /// Default: Conventional. See [`CutDirection`] for the winding rules.
    #[serde(default, skip_serializing_if = "CutDirection::is_default")]
    pub cut_direction: CutDirection,
    /// Cut direction for the finishing pass — the offset that defines
    /// the wall surface (Pocket level=0 ring; Profile single-pass cut).
    /// Default: Conventional, regardless of the main `cut_direction`.
    /// Surface quality on the finish wall is almost always best with
    /// conventional milling on hobby machines.
    #[serde(default, skip_serializing_if = "CutDirection::is_default")]
    pub finish_cut_direction: CutDirection,

    /// How the cutter descends into material at the start of each Z
    /// pass. Default Direct (straight plunge). Ramp { angle_deg } walks
    /// forward along the path while descending Z, taking a chip in both
    /// directions simultaneously — required for non-center-cutting bits
    /// and for harder materials.
    #[serde(default, skip_serializing_if = "is_default_plunge")]
    pub plunge: crate::cam::setup::PlungeStrategy,

    /// Override the tool's `feed_rate` for this op only. Some materials
    /// or finishing passes need a slower feed than the tool's default;
    /// rather than editing the tool library, set this per-op. None =
    /// use the tool's `feed_rate`. Units: mm/min.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_rate_override: Option<u32>,
    /// Override the tool's `plunge_rate` (Z feed) for this op. Useful
    /// for slowing the plunge on hard materials without changing the
    /// XY feed. Units: mm/min.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plunge_rate_override: Option<u32>,
    /// When > 0, slow the feed at sharp corners by this fraction so the
    /// machine doesn't dwell on the corner with high accel demand. 0.0
    /// = no reduction (current behavior). 0.5 = half the feed at
    /// corners. Most useful for zigzag pocket fills with their many
    /// 180° turns.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub corner_feed_reduction: f64,

    /// Optional smaller step for the FINAL Z pass, for a cleaner bottom
    /// finish. None = use the same `step` for the last pass too.
    /// Negative just like `step`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_step: Option<f64>,
    /// Cut past the nominal `depth` by this much (positive number — gets
    /// subtracted from the working depth). Useful for through-cuts on
    /// edge-clamped sheet so the cutter clears the bottom even with
    /// minor stock thickness variation. 0.0 = no extension.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub through_depth: f64,
    /// Explicit list of Z depths for each pass, overriding the
    /// step+finish_step schedule. Useful for non-linear schedules
    /// (shallower at start for tough material, deeper later, slow
    /// finish at the end). Each entry is an absolute Z (negative
    /// number); the cutter visits them in order. Empty = use the
    /// step-down loop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depth_list: Vec<f64>,

    /// V-Carve cap on the inscribed-circle radius (mm). When set, any
    /// medial-axis point with `R_inscribed > carve_max_width_mm` clips
    /// to the cap — the V doesn't carve any wider than the bit's
    /// usable diameter. None = no cap (use the geometric inscribed
    /// circle directly).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carve_max_width_mm: Option<f64>,
    /// V-Carve "second-pass" toggle. When true, the emitter runs a
    /// refinement pass that re-cuts only the points whose first pass
    /// fell short of the geometric target depth. Off by default.
    #[serde(default)]
    pub multi_pass_refine: bool,

    /// Pocket-Outside wrapper: shape of the synthetic frame the pipeline
    /// auto-prepends around `source` before pocketing. Set only on ops
    /// created via the Pocket-Outside UX. None = no frame (regular Pocket).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_shape: Option<FrameShape>,
    /// Padding added on every side of the selection bbox to size the
    /// frame. Honored only when `frame_shape` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_padding_mm: Option<f64>,
    /// Corner radius for `FrameShape::RoundedRectangle`. None ⇒ defaults
    /// to `frame_padding_mm` inside the frame builder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_corner_radius_mm: Option<f64>,
}

fn is_zero_f64(v: &f64) -> bool {
    v.abs() < 1e-9
}

fn is_default_plunge(p: &crate::cam::setup::PlungeStrategy) -> bool {
    matches!(p, crate::cam::setup::PlungeStrategy::Direct)
}

/// Accept legacy bare-number `step` (including `0.0` as the unset
/// sentinel) alongside the new `null`/missing forms.
fn deserialize_optional_step<'de, D>(de: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<f64>::deserialize(de)?;
    Ok(v.filter(|x| x.abs() >= 1e-9))
}

impl OperationParams {
    /// Defaults that line up with a "first profile cut on a 2 mm sheet".
    pub fn mill_default() -> Self {
        Self {
            depth: -2.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            xy_overlap: 0.5,
            helix: false,
            reverse: false,
            objectorder: ObjectOrder::default(),
            overcut: false,
            pocket_islands: false,
            pocket_nocontour: false,
            pocket_insideout: false,
            tabs: TabsConfig {
                active: false,
                width: 10.0,
                height: 1.0,
                tab_type: TabType::Rectangle,
                ramp_angle_deg: 30.0,
            },
            leads: LeadsConfig {
                r#in: LeadKind::Off,
                out: LeadKind::Off,
                in_lenght: 5.0,
                out_lenght: 5.0,
            },
            cut_direction: CutDirection::Conventional,
            finish_cut_direction: CutDirection::Conventional,
            plunge: crate::cam::setup::PlungeStrategy::Direct,
            feed_rate_override: None,
            plunge_rate_override: None,
            corner_feed_reduction: 0.0,
            finish_step: None,
            through_depth: 0.0,
            depth_list: Vec::new(),
            carve_max_width_mm: None,
            multi_pass_refine: false,
            frame_shape: None,
            frame_padding_mm: None,
            frame_corner_radius_mm: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::setup::ToolOffset;

    #[test]
    fn project_default_is_empty_but_well_typed() {
        let p = Project::default();
        assert!(p.segments.is_empty());
        assert!(p.tools.is_empty());
        assert!(p.operations.is_empty());
        assert!(p.tabs.is_empty());
    }

    #[test]
    fn operation_default_is_an_outside_profile_on_all_geometry() {
        let op = Operation::default();
        assert!(matches!(
            op.kind,
            OperationKind::Profile { offset: ToolOffset::Outside }
        ));
        assert!(matches!(op.source, OperationSource::All));
        assert!(op.enabled);
    }

    #[test]
    fn legacy_step_zero_deserializes_to_none() {
        let json = r#"{"depth":-2.0,"start_depth":0.0,"step":0.0,"fast_move_z":5.0}"#;
        let p: OperationParams = serde_json::from_str(json).unwrap();
        assert_eq!(p.step, None);
    }

    #[test]
    fn legacy_step_negative_deserializes_to_some() {
        let json = r#"{"depth":-2.0,"start_depth":0.0,"step":-1.0,"fast_move_z":5.0}"#;
        let p: OperationParams = serde_json::from_str(json).unwrap();
        assert_eq!(p.step, Some(-1.0));
    }

    #[test]
    fn missing_step_deserializes_to_none() {
        let json = r#"{"depth":-2.0,"start_depth":0.0,"fast_move_z":5.0}"#;
        let p: OperationParams = serde_json::from_str(json).unwrap();
        assert_eq!(p.step, None);
    }

    #[test]
    fn null_step_deserializes_to_none() {
        let json = r#"{"depth":-2.0,"start_depth":0.0,"step":null,"fast_move_z":5.0}"#;
        let p: OperationParams = serde_json::from_str(json).unwrap();
        assert_eq!(p.step, None);
    }

    #[test]
    fn step_none_skips_field_on_serialize() {
        let mut p = OperationParams::mill_default();
        p.step = None;
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("\"step\""), "step=None should be skipped: {json}");
    }

    #[test]
    fn step_some_writes_bare_number_on_serialize() {
        let mut p = OperationParams::mill_default();
        p.step = Some(-0.5);
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"step\":-0.5"), "step=Some(-0.5) should write bare number: {json}");
    }

    #[test]
    fn fixtures_round_trip() {
        let p = Project {
            fixtures: vec![
                Fixture {
                    id: 1,
                    name: "front clamp".into(),
                    kind: FixtureKind::Box { width: 30.0, depth: 50.0 },
                    origin: (15.0, -25.0),
                    z_bottom: 0.0,
                    z_top: 12.0,
                    color: 0xFFA0_50C0,
                },
                Fixture {
                    id: 2,
                    name: "dog".into(),
                    kind: FixtureKind::Cylinder { radius: 6.0 },
                    origin: (-10.0, 40.0),
                    z_bottom: -1.0,
                    z_top: 8.0,
                    color: default_fixture_color(),
                },
                Fixture {
                    id: 3,
                    name: "L-bracket".into(),
                    kind: FixtureKind::Polygon {
                        vertices: vec![
                            (0.0, 0.0),
                            (20.0, 0.0),
                            (20.0, 5.0),
                            (5.0, 5.0),
                            (5.0, 25.0),
                            (0.0, 25.0),
                        ],
                    },
                    origin: (60.0, 60.0),
                    z_bottom: 0.0,
                    z_top: 6.0,
                    color: 0x8080_8080,
                },
            ],
            ..Project::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.fixtures.len(), 3);
        assert!(matches!(
            back.fixtures[0].kind,
            FixtureKind::Box { width, depth }
                if (width - 30.0).abs() < 1e-9 && (depth - 50.0).abs() < 1e-9
        ));
        assert!(matches!(
            back.fixtures[1].kind,
            FixtureKind::Cylinder { radius } if (radius - 6.0).abs() < 1e-9
        ));
        match &back.fixtures[2].kind {
            FixtureKind::Polygon { vertices } => assert_eq!(vertices.len(), 6),
            _ => panic!("expected Polygon"),
        }
    }

    #[test]
    fn fixture_default_color_when_absent() {
        let json = r#"{
            "id": 5, "name": "x", "kind": {"shape": "cylinder", "radius": 3.0},
            "origin": [1.0, 2.0], "z_bottom": 0.0, "z_top": 5.0
        }"#;
        let f: Fixture = serde_json::from_str(json).unwrap();
        assert_eq!(f.color, default_fixture_color());
    }

    #[test]
    fn project_with_no_fixtures_skips_field_on_serialize() {
        let p = Project::default();
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("\"fixtures\""), "empty fixtures should be skipped: {json}");
    }

    #[test]
    fn fixture_step_values_round_trip_through_shim() {
        // The .vc-project.json files on disk are the frontend's camelCase
        // shape; the wire `OperationParams` (snake_case, nested under
        // `params`) is a transformed view. We still want a sanity check
        // that every `step` value those files carry survives our shim,
        // so synthesize a minimal wire payload per op and round-trip it.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.parent().unwrap().parent().unwrap();
        for name in ["test.vc-project.json", "Epropulsion_SpiritEvo_Batteriestecker_01.vc-project.json"] {
            let path = root.join(name);
            if !path.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"));
            let ops = v.get("operations").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            for (i, op) in ops.iter().enumerate() {
                let step_val = op.get("step").cloned().unwrap_or(serde_json::Value::Null);
                let wire = serde_json::json!({
                    "depth": -2.0,
                    "start_depth": 0.0,
                    "step": step_val,
                    "fast_move_z": 5.0,
                });
                let _: OperationParams = serde_json::from_value(wire)
                    .unwrap_or_else(|e| panic!("op #{i} step in {path:?}: {e}"));
            }
        }
    }
}
