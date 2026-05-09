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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PocketStrategy {
    #[default]
    Cascade,
    Zigzag,
    Spiral,
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
    /// Per-pass step (negative ⇒ down).
    pub step: f64,
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
}

fn is_zero_f64(v: &f64) -> bool {
    v.abs() < 1e-9
}

fn is_default_plunge(p: &crate::cam::setup::PlungeStrategy) -> bool {
    matches!(p, crate::cam::setup::PlungeStrategy::Direct)
}

impl OperationParams {
    /// Defaults that line up with a "first profile cut on a 2 mm sheet".
    pub fn mill_default() -> Self {
        Self {
            depth: -2.0,
            start_depth: 0.0,
            step: -1.0,
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
}
