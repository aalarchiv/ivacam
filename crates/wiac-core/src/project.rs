//! Project = geometry + machine + tool library + ordered list of
//! Operations. The Operation is the unit of CAM work — each one carries a
//! tool reference and per-kind parameters and produces a gcode block in
//! the final program.
//!
//! Modeled after mainstream CAM tools (Carbide Create, Fusion 360 CAM,
//! Estlcum, FreeCAD Path Workbench) so the user's mental model translates
//! without surprises.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

    /// Fixtures (clamps, dogs, vise jaws, hold-downs) the cutter must
    /// avoid throughout the entire program — including rapids. The sim
    /// pass tests every toolpath segment against this set and emits
    /// `SimWarning::FixtureCollision` on overlap. Default empty: a
    /// project with no fixtures behaves exactly as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixtures: Vec<Fixture>,

    /// First-class editable text entities — content / font / size /
    /// position / rotation / spacing. The pipeline pre-pass renders each
    /// TextLayer to segments before any op runs so the existing
    /// `Engrave` (and friends) op can target the rendered geometry by
    /// layer name `__text_<id>`. Edits to a TextLayer re-run the
    /// pipeline; cache keys include text_layers content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_layers: Vec<TextLayer>,
}

// ─── text layers ──────────────────────────────────────────────────────────

/// Persistent editable text entity. Phase 2 of the text-engraving
/// rework: the pipeline renders these to segments at generate time so
/// edits propagate to gcode without re-baking.
///
/// Distinct from DXF TEXT/MTEXT entities currently parsed into
/// `project.segments` as opaque polylines (phase 4 will route those
/// through TextLayer too).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TextLayer {
    pub id: u32,
    pub kind: TextLayerKind,
    /// Display name in the sidebar list.
    pub name: String,
    /// Full string. For `Mtext`, lines are `\n`-separated.
    pub text: String,
    /// TTF/OTF font as a byte vector. JSON serialises as an array of
    /// integers — matches the [`crate::input::text::RenderTextRequest`]
    /// convention so the same transport-agnostic encoding applies.
    pub font_bytes: Vec<u8>,
    pub size_mm: f64,
    /// Anchor in stock XY (mm). Alignment offsets are applied relative
    /// to this point (see [`TextAlignment`]).
    pub origin: (f64, f64),
    #[serde(default)]
    pub rotation_deg: f64,
    /// Extra advance between glyphs in mm. `0.0` (default) = font's
    /// natural advance.
    #[serde(default)]
    pub letter_spacing_mm: f64,
    /// MTEXT line spacing in mm. Ignored when `kind == TextLayerKind::Text`.
    /// `0.0` (default) = ~1.2 × `size_mm`.
    #[serde(default)]
    pub line_spacing_mm: f64,
    #[serde(default = "default_alignment")]
    pub alignment: TextAlignment,
}

fn default_alignment() -> TextAlignment {
    TextAlignment::Left
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TextLayerKind {
    #[serde(rename = "TEXT")]
    Text,
    #[serde(rename = "MTEXT")]
    Mtext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextAlignment {
    Left,
    Center,
    Right,
}

/// Reserved layer name pattern for TextLayer-rendered segments. Ops
/// can target a specific text layer via `OperationSource::Layers(vec!["__text_<id>"])`.
pub fn text_layer_synthetic_layer(id: u32) -> String {
    format!("__text_{id}")
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
    /// Spindle RPM override for the finishing pass (the wall-defining
    /// level=0 ring of a Pocket). None = inherit `speed`. Hard-material
    /// finish quality usually wants a slower RPM than roughing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_finish: Option<u32>,
    /// Plunge feedrate override for the finishing pass. None = inherit
    /// `plunge_rate`. Units: mm/min.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plunge_rate_finish: Option<u32>,
    /// Cutting feedrate override for the finishing pass. None = inherit
    /// `feed_rate`. Units: mm/min.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_rate_finish: Option<u32>,
    /// Spindle RPM override when this tool is used in a Drill op. None =
    /// inherit `speed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_drill: Option<u32>,
    /// Plunge feedrate override when this tool is used in a Drill op.
    /// None = inherit `plunge_rate`. Units: mm/min.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plunge_rate_drill: Option<u32>,
    /// Cutting feedrate override when this tool is used in a Drill op.
    /// None = inherit `feed_rate`. Units: mm/min. Only meaningful for
    /// posts that emit XY-traverse feed lines between drill points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_rate_drill: Option<u32>,
    /// Default peck step (positive, mm) for `DrillCycle::Peck` /
    /// `ChipBreak` ops that don't set their own `peck_step_mm`. None =
    /// the op must specify its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_peck_step_mm: Option<f64>,
    /// Default depth-per-pass (negative, mm). Operations using this tool
    /// inherit this when their own `step` is unset. None = no default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_step: Option<f64>,
    /// Per-tool Z origin offset (rt1.30 / Estlcam Z_Shift). For
    /// machines without automatic tool-length probing — the user
    /// pre-measures each tool's tip Z relative to a reference tool and
    /// records the delta here (positive = sticks out further; negative
    /// = shorter). At toolchange / program-start the post emits a
    /// `G92 Z<shift>` that pins the new tool's tip at the same work-Z
    /// the reference tool used. mm. None = no shift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_shift_mm: Option<f64>,
    /// Laser pierce time (rt1.29 / Estlcam T_Pierce_Time): seconds the
    /// beam dwells at the start point BEFORE the cut begins so it
    /// burns through thick stock. Honored only when `kind ==
    /// LaserBeam`. The post emits a `G4 P<seconds>` after the
    /// laser-on before each plunge. None = no pierce dwell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub laser_pierce_sec: Option<f64>,
    /// Laser lead-in distance (rt1.29 / Estlcam T_Lead_In): mm of
    /// approach travel the head takes along the entry tangent before
    /// the cut starts, to reduce edge entry burn. Honored only when
    /// `kind == LaserBeam`. Wired into `LeadsConfig` at op synth time
    /// — the per-op lead-in field overrides this if set explicitly.
    /// None = no tool-level lead-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub laser_lead_in_mm: Option<f64>,
    /// Bull-nose / radius-endmill corner radius (rt1.28). Honored
    /// only when `kind == BullNose`. The corner radius produces a
    /// fillet on the cut floor edge — relevant for sim cross-section
    /// (the sim envelope falls below `corner_radius_mm` of the
    /// nominal flat floor). mm, positive only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner_radius_mm: Option<f64>,
    /// T-slot / keyway cutter neck diameter (rt1.28). Honored only
    /// when `kind == TSlot`. The undercut cutter has a wide disk
    /// (`diameter`) at the tip and a narrow neck of this diameter
    /// above. mm, positive only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tslot_neck_diameter_mm: Option<f64>,
    /// T-slot / keyway cutter neck length (rt1.28). Honored only
    /// when `kind == TSlot`. The vertical extent of the narrow neck
    /// above the disk. mm, positive only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tslot_neck_length_mm: Option<f64>,
    /// Wirbeln (rt1.25 / Estlcam T_Wirbeln): automatic chip-thinning.
    /// When `true`, Pocket ops using this tool clamp their effective
    /// `xy_step` down to `wirbeln_stepover_mm.unwrap_or(tool_radius / 2)`
    /// — the classic chip-thinning rule that bounds radial engagement
    /// at half-radius. Use for hard materials where the user wants
    /// fast cascade / spiral pockets but doesn't want the cutter to
    /// overload at high-engagement points. Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wirbeln: bool,
    /// Wirbeln stepover override (rt1.25). When `wirbeln` is `true`,
    /// the effective cascade step is `min(op.xy_step,
    /// wirbeln_stepover_mm OR tool_radius / 2)`. mm, positive only.
    /// None = use the half-radius rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wirbeln_stepover_mm: Option<f64>,
    /// Spindle warm-up pause in seconds applied once per used tool by
    /// the time estimator. Mirrors `ToolConfig.pause`.
    #[serde(
        default = "default_tool_pause",
        skip_serializing_if = "is_default_tool_pause"
    )]
    pub pause: u32,
    /// Length of cutting flutes (mm). None = treat entire tool as cutting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flute_length_mm: Option<f64>,
    /// Shank diameter (mm). None = same as `diameter` (parallel-shank bit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shank_diameter_mm: Option<f64>,
    /// Holder geometry above the shank. None = no holder check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<HolderShape>,
}

/// Geometry of the tool holder above the shank. The holder is treated as
/// cylindrically symmetric around the tool axis (Z), so set-screw flats /
/// asymmetric ER nuts get approximated by their bounding cylinder/cone —
/// good enough to flag clear collisions, conservative on tight cases.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HolderShape {
    Cylinder {
        diameter_mm: f64,
        length_mm: f64,
    },
    Cone {
        bottom_diameter_mm: f64,
        top_diameter_mm: f64,
        length_mm: f64,
    },
    Stepped {
        cylinder_diameter_mm: f64,
        cylinder_length_mm: f64,
        cone_top_diameter_mm: f64,
        cone_length_mm: f64,
    },
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
            pause: default_tool_pause(),
            flute_length_mm: None,
            shank_diameter_mm: None,
            holder: None,
        }
    }
}

/// Which set of per-tool feed/speed/plunge values to use for a given
/// emission context. `Rough` is the default and matches every legacy
/// caller. `Finish` is consulted at the wall-defining level=0 ring of a
/// Pocket / per-op finish pass. `Drill` is consulted for Drill ops so
/// the user can dial drilling RPM independently from milling RPM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassKind {
    Rough,
    Finish,
    Drill,
}

/// Resolve the (speed, plunge_rate, feed_rate) triplet for `tool` under
/// `pass`. Finish / Drill variants fall back to the general values when
/// their override is `None`.
pub fn resolve_tool_rates(tool: &ToolEntry, pass: PassKind) -> (u32, u32, u32) {
    match pass {
        PassKind::Rough => (tool.speed, tool.plunge_rate, tool.feed_rate),
        PassKind::Finish => (
            tool.speed_finish.unwrap_or(tool.speed),
            tool.plunge_rate_finish.unwrap_or(tool.plunge_rate),
            tool.feed_rate_finish.unwrap_or(tool.feed_rate),
        ),
        PassKind::Drill => (
            tool.speed_drill.unwrap_or(tool.speed),
            tool.plunge_rate_drill.unwrap_or(tool.plunge_rate),
            tool.feed_rate_drill.unwrap_or(tool.feed_rate),
        ),
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
    /// Bull-nose / radius-corner endmill (rt1.28): flat endmill with
    /// a rounded transition between the cylindrical wall and the flat
    /// floor. Cuts a fillet on the floor edge.
    /// `ToolEntry.corner_radius_mm` carries the radius.
    BullNose,
    /// Compression / up-down spiral endmill (rt1.28 / Estlcam
    /// Obenunten). Cuts down on top half, up on bottom half — clean
    /// edges on both faces of sheet material. v1 treats it like an
    /// Endmill at the cutting algorithm; the variant is here so the
    /// tool library can label it accurately for the user.
    Compression,
    /// T-slot / keyway / undercut cutter (rt1.28): plunges vertically
    /// down a narrow neck, then a wider disk at the tip cuts the
    /// undercut slot. `ToolEntry.tslot_neck_diameter_mm` /
    /// `tslot_neck_length_mm` carry the neck geometry.
    TSlot,
    /// Form / profile cutter (rt1.28 / Estlcam Profil): bull-nose /
    /// cove / ogee / dovetail / custom — a profile bit with a fixed
    /// cross-section. v1 treats as an Endmill at the algorithm; the
    /// variant labels it.
    FormProfile,
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
    /// id of a `Project.tools` entry. For dual-tool Pocket ops this is
    /// the roughing tool; the finish ring is cut by `finish_tool_id`.
    pub tool_id: u32,
    /// Optional finish tool id for dual-tool Pocket ops (rt1.33 / Estlcam
    /// TS slot). When `Some(id)` and `id != tool_id`, the pipeline emits
    /// a toolchange after the rough cascade and runs the wall-defining
    /// ring with the finish tool's geometry + finish-set feed/speed. When
    /// `None` or equal to `tool_id`, the op runs single-tool (current
    /// behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_tool_id: Option<u32>,
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
            finish_tool_id: None,
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
    /// `start_angle_deg + i * angle_step_deg` about the center —
    /// `start_angle_deg` shifts the whole ring so the first instance
    /// doesn't have to land at 0°.
    Polar {
        count: u32,
        center_x: f64,
        center_y: f64,
        angle_step_deg: f64,
        #[serde(default, skip_serializing_if = "is_zero_f64")]
        start_angle_deg: f64,
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
    /// Helical thread — single-point cutter walks a helix inside a bore
    /// (internal) or around a stud (external). Z descends linearly by
    /// `pitch_mm` per revolution between `start_depth` and `depth`. The
    /// source must be a closed circle (single Circle / closed Arc
    /// loop); the helix radius derives from the circle's radius plus
    /// tool radius for internal threads, or minus tool radius for
    /// external. (rt1.17)
    Thread {
        /// Thread pitch in mm — Z descent per full revolution.
        /// Positive; defaults to 1.0 mm (M6 fine).
        #[serde(default = "default_thread_pitch")]
        pitch_mm: f64,
        /// `true` = internal (tap-style, cutter walks INSIDE the bore);
        /// `false` = external (cutter walks AROUND a stud).
        #[serde(default = "default_thread_internal")]
        internal: bool,
        /// Climb (CCW helix on a right-hand spindle) vs conventional
        /// (CW). Default conventional. Surface quality on hobby rigs
        /// almost always favors conventional even for threading.
        #[serde(default)]
        climb: bool,
    },
    /// V-bit edge break (rt1.18). The cutter walks the source path
    /// itself at a single Z computed from the bit's cone angle and the
    /// desired chamfer width: `z = -width_mm / tan(tip_angle / 2)`.
    /// One pass at that depth carves a beveled edge whose horizontal
    /// width on the workpiece equals `width_mm`. Optionally followed by
    /// a second pass at the tool's finish-set feed/speed for surface
    /// quality. Default values for the variant fields keep legacy
    /// payloads (`{ "type": "chamfer" }`) backward-compatible.
    Chamfer {
        /// Horizontal width of the chamfer cut on the workpiece, mm.
        /// Mirrors Estlcam's Fasenabstand. Positive; defaults to 1.0.
        #[serde(default = "default_chamfer_width")]
        width_mm: f64,
        /// When `true`, the chamfer is cut twice — once at the rough
        /// feed (cleanup) and once at the tool's finish-set feed
        /// (rt1.27) for surface quality. Default `false`.
        #[serde(default)]
        finish_pass: bool,
    },
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

fn default_chamfer_width() -> f64 {
    1.0
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn default_thread_pitch() -> f64 {
    1.0
}

fn default_thread_internal() -> bool {
    true
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
    /// Halfpipe (rt1.19 / Estlcam _PK::Halfpipe): slot machining where
    /// the toolpath walks the region's MEDIAL AXIS at varying Z so the
    /// cut floor matches the configured profile. The slot's width at
    /// each medial-axis point (= 2*inscribed-circle radius) drives the
    /// depth via `profile`. Right strategy for drainage channels,
    /// decorative grooves, water-stop seals, mortise-prep for
    /// round-bottomed inlays.
    Halfpipe {
        profile: HalfpipeProfile,
    },
}

/// Half-pipe slot cross-section (rt1.19).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HalfpipeProfile {
    /// Circular arc cross-section of `radius_mm`. At a medial-axis
    /// point with inscribed-circle radius `r`, depth =
    /// `-(R - sqrt(R² - r²))`. When `r > R`, the slot is wider than
    /// the desired pipe — depth caps at `-R` (the deepest point of a
    /// circle of radius `R`). Use with a ball-nose cutter.
    CircularArc { radius_mm: f64 },
    /// V-bottom cross-section: depth = `-r / tan(half_angle)` (i.e.
    /// the V-Carve formula). Use with a V-bit. Equivalent to running
    /// V-Carve at full depth — provided here for completeness and so
    /// the strategy picker stays uniform.
    VBottom { included_angle_deg: f64 },
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
            Self::Halfpipe { profile } => {
                let mut s = ser.serialize_struct("Halfpipe", 2)?;
                s.serialize_field("kind", "halfpipe")?;
                match profile {
                    HalfpipeProfile::CircularArc { radius_mm } => {
                        s.serialize_field(
                            "profile",
                            &serde_json::json!({
                                "kind": "circular_arc",
                                "radius_mm": radius_mm,
                            }),
                        )?;
                    }
                    HalfpipeProfile::VBottom { included_angle_deg } => {
                        s.serialize_field(
                            "profile",
                            &serde_json::json!({
                                "kind": "v_bottom",
                                "included_angle_deg": included_angle_deg,
                            }),
                        )?;
                    }
                }
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
                },
                {
                    "type": "object",
                    "required": ["kind", "profile"],
                    "properties": {
                        "kind": { "type": "string", "enum": ["halfpipe"] },
                        "profile": {
                            "oneOf": [
                                {
                                    "type": "object",
                                    "required": ["kind", "radius_mm"],
                                    "properties": {
                                        "kind": { "type": "string", "enum": ["circular_arc"] },
                                        "radius_mm": { "type": "number", "format": "double" }
                                    }
                                },
                                {
                                    "type": "object",
                                    "required": ["kind", "included_angle_deg"],
                                    "properties": {
                                        "kind": { "type": "string", "enum": ["v_bottom"] },
                                        "included_angle_deg": { "type": "number", "format": "double" }
                                    }
                                }
                            ]
                        }
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
        struct ProfileObj {
            kind: String,
            #[serde(default)]
            radius_mm: Option<f64>,
            #[serde(default)]
            included_angle_deg: Option<f64>,
        }
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
                #[serde(default)]
                profile: Option<ProfileObj>,
            },
        }
        match Repr::deserialize(de)? {
            Repr::Str(s) => match s.as_str() {
                "cascade" => Ok(Self::Cascade),
                "zigzag" => Ok(Self::Zigzag),
                "spiral" => Ok(Self::Spiral),
                other => Err(serde::de::Error::unknown_variant(
                    other,
                    &["cascade", "zigzag", "spiral", "trochoidal", "halfpipe"],
                )),
            },
            Repr::Obj {
                kind,
                engagement_angle_deg,
                loop_radius_factor,
                profile,
            } => match kind.as_str() {
                "cascade" => Ok(Self::Cascade),
                "zigzag" => Ok(Self::Zigzag),
                "spiral" => Ok(Self::Spiral),
                "trochoidal" => Ok(Self::Trochoidal {
                    engagement_angle_deg: engagement_angle_deg.unwrap_or(30.0),
                    loop_radius_factor: loop_radius_factor.unwrap_or(0.6),
                }),
                "halfpipe" => {
                    let p = profile
                        .ok_or_else(|| serde::de::Error::missing_field("profile (for halfpipe)"))?;
                    let profile = match p.kind.as_str() {
                        "circular_arc" => HalfpipeProfile::CircularArc {
                            radius_mm: p.radius_mm.unwrap_or(5.0),
                        },
                        "v_bottom" => HalfpipeProfile::VBottom {
                            included_angle_deg: p.included_angle_deg.unwrap_or(60.0),
                        },
                        other => {
                            return Err(serde::de::Error::unknown_variant(
                                other,
                                &["circular_arc", "v_bottom"],
                            ));
                        }
                    };
                    Ok(Self::Halfpipe { profile })
                }
                other => Err(serde::de::Error::unknown_variant(
                    other,
                    &["cascade", "zigzag", "spiral", "trochoidal", "halfpipe"],
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

/// A user-placed tab anchored geometry-relative (rt1.10). The
/// `object_id` is 1-based to match `OperationSource::Objects::ids`;
/// `t ∈ [0, 1)` is the arc-length parameter along the chained
/// object's segments. `cam/tabs.rs::polyline_at_t` resolves the
/// parameter to a world point at gcode-emission time, so the tab
/// follows the geometry through transforms.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TabPlacement {
    pub object_id: u32,
    pub t: f64,
    /// Optional per-tab width override (mm). None ⇒ use
    /// `OperationParams.tabs.width`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_override_mm: Option<f64>,
    /// Optional per-tab height override (mm). None ⇒ use
    /// `OperationParams.tabs.height`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_override_mm: Option<f64>,
}

/// How an op sources tab positions (rt1.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TabPlacementMode {
    /// No tabs emitted. Default — matches the pre-rt1.10 behavior
    /// of "user must opt in". Distinct from `TabsConfig.active`
    /// (which is the SHAPE config's active flag).
    #[default]
    Off,
    /// N tabs evenly spaced around each closed source contour. Open
    /// contours get tabs inset by 0.5/N to avoid endpoint placement.
    Auto { count: u32 },
    /// Only user-placed `tab_placements`. Auto-spacing disabled.
    Manual,
    /// `Auto { count: auto_count }` ∪ `tab_placements`. v1 makes no
    /// attempt to dedupe: auto positions ignore manual ones and
    /// vice versa.
    Mixed { auto_count: u32 },
}

impl TabPlacementMode {
    fn is_default(&self) -> bool {
        matches!(self, TabPlacementMode::Off)
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

    /// Per-op tab SHAPE config: width / height / kind (rectangle vs
    /// ramp) / ramp angle. Effective tab POSITIONS come from
    /// `tab_placements` (manual) and / or `tab_mode` (auto-spaced).
    #[serde(default)]
    pub tabs: TabsConfig,
    /// How tab positions are sourced for this op (rt1.10).
    #[serde(default, skip_serializing_if = "TabPlacementMode::is_default")]
    pub tab_mode: TabPlacementMode,
    /// User-placed tabs, anchored geometry-relative as
    /// `(object_id, t)`. Honored when `tab_mode` is `Manual` or
    /// `Mixed`; `Off` / `Auto` ignore. Each placement may carry
    /// per-tab width / height overrides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tab_placements: Vec<TabPlacement>,

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
    /// XY stock allowance left UNCUT by the roughing pass, removed by a
    /// dedicated finish pass walking the actual boundary (rt1.24 /
    /// Estlcam Schlichtzugabe). Honored on Pocket ops only — the
    /// roughing cascade insets the cutter centerline by
    /// `tool_radius + allowance`, then a wall-defining ring at
    /// `tool_radius` runs at the tool's finish-set feed/speed
    /// (rt1.27). None / 0 = no allowance (current behavior). mm,
    /// positive only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_xy_allowance_mm: Option<f64>,
    /// Stufenfase (rt1.20 / Estlcam Prog_KTD_Stufenfase): chamfer a
    /// drilled hole's rim immediately after the drill cycle. Honored
    /// only on `OperationKind::Drill`. The post emits the drill cycle
    /// for each hole, then walks the cutter on a single revolution at
    /// the hole's edge at z = -width / tan(tip_angle / 2). When
    /// `Operation.finish_tool_id` is set to a distinct tool, a M6 +
    /// G92 toolchange happens BEFORE the chamfer revolution so the
    /// user can chamfer with a V-bit / fly-cutter different from the
    /// drill. mm, positive only. None / 0 = no countersink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chamfer_after_width_mm: Option<f64>,
    /// Anfahrpunkt (rt1.26 / Estlcam): user-picked XY where the
    /// cutter enters each pocket / closed-contour ring. Honored on
    /// Pocket / Profile ops with closed offsets. When `Some((x, y))`,
    /// each closed offset gets its segment list rotated so the start
    /// vertex is the segment vertex closest to the approach point —
    /// the plunge / lead-in then happens there instead of an
    /// arbitrary auto-picked point. `None` = auto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approach_point: Option<(f64, f64)>,
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
            tab_mode: TabPlacementMode::Off,
            tab_placements: Vec::new(),
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
            finish_xy_allowance_mm: None,
            chamfer_after_width_mm: None,
            approach_point: None,
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
        assert!(p.fixtures.is_empty());
    }

    #[test]
    fn operation_default_is_an_outside_profile_on_all_geometry() {
        let op = Operation::default();
        assert!(matches!(
            op.kind,
            OperationKind::Profile {
                offset: ToolOffset::Outside
            }
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
        assert!(
            !json.contains("\"step\""),
            "step=None should be skipped: {json}"
        );
    }

    #[test]
    fn step_some_writes_bare_number_on_serialize() {
        let mut p = OperationParams::mill_default();
        p.step = Some(-0.5);
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            json.contains("\"step\":-0.5"),
            "step=Some(-0.5) should write bare number: {json}"
        );
    }

    #[test]
    fn fixtures_round_trip() {
        let p = Project {
            fixtures: vec![
                Fixture {
                    id: 1,
                    name: "front clamp".into(),
                    kind: FixtureKind::Box {
                        width: 30.0,
                        depth: 50.0,
                    },
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
        assert!(
            !json.contains("\"fixtures\""),
            "empty fixtures should be skipped: {json}"
        );
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
        for name in [
            "test.vc-project.json",
            "Epropulsion_SpiritEvo_Batteriestecker_01.vc-project.json",
        ] {
            let path = root.join(name);
            if !path.exists() {
                continue;
            }
            let text =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
            let v: serde_json::Value =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"));
            let ops = v
                .get("operations")
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default();
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

    #[test]
    fn holder_round_trip() {
        // Each HolderShape variant survives JSON serialize/deserialize.
        let shapes = vec![
            HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            },
            HolderShape::Cone {
                bottom_diameter_mm: 25.0,
                top_diameter_mm: 40.0,
                length_mm: 35.0,
            },
            HolderShape::Stepped {
                cylinder_diameter_mm: 18.0,
                cylinder_length_mm: 12.0,
                cone_top_diameter_mm: 30.0,
                cone_length_mm: 22.0,
            },
        ];
        for s in shapes {
            let mut tool = ToolEntry::default();
            tool.flute_length_mm = Some(15.0);
            tool.shank_diameter_mm = Some(6.0);
            tool.holder = Some(s);
            let json = serde_json::to_string(&tool).expect("serialize");
            let back: ToolEntry = serde_json::from_str(&json).expect("deserialize");
            match (s, back.holder.expect("holder survives")) {
                (
                    HolderShape::Cylinder {
                        diameter_mm: d0,
                        length_mm: l0,
                    },
                    HolderShape::Cylinder {
                        diameter_mm: d1,
                        length_mm: l1,
                    },
                ) => {
                    assert!((d0 - d1).abs() < 1e-9 && (l0 - l1).abs() < 1e-9);
                }
                (
                    HolderShape::Cone {
                        bottom_diameter_mm: b0,
                        top_diameter_mm: t0,
                        length_mm: l0,
                    },
                    HolderShape::Cone {
                        bottom_diameter_mm: b1,
                        top_diameter_mm: t1,
                        length_mm: l1,
                    },
                ) => {
                    assert!(
                        (b0 - b1).abs() < 1e-9 && (t0 - t1).abs() < 1e-9 && (l0 - l1).abs() < 1e-9
                    );
                }
                (
                    HolderShape::Stepped {
                        cylinder_diameter_mm: cd0,
                        cylinder_length_mm: cl0,
                        cone_top_diameter_mm: ct0,
                        cone_length_mm: cn0,
                    },
                    HolderShape::Stepped {
                        cylinder_diameter_mm: cd1,
                        cylinder_length_mm: cl1,
                        cone_top_diameter_mm: ct1,
                        cone_length_mm: cn1,
                    },
                ) => {
                    assert!(
                        (cd0 - cd1).abs() < 1e-9
                            && (cl0 - cl1).abs() < 1e-9
                            && (ct0 - ct1).abs() < 1e-9
                            && (cn0 - cn1).abs() < 1e-9
                    );
                }
                _ => panic!("variant mismatch after round trip"),
            }
            assert_eq!(back.flute_length_mm, Some(15.0));
            assert_eq!(back.shank_diameter_mm, Some(6.0));
        }
    }

    #[test]
    fn tool_holder_fields_skip_when_none() {
        let tool = ToolEntry::default();
        let json = serde_json::to_string(&tool).expect("serialize");
        assert!(!json.contains("flute_length_mm"));
        assert!(!json.contains("shank_diameter_mm"));
        assert!(!json.contains("\"holder\""));
    }
}
