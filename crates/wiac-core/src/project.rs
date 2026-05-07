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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum OperationKind {
    /// Contour cut — equivalent to today's "mill" with a parallel-offset
    /// pass at `offset` of the tool radius.
    Profile { offset: ToolOffset },
    /// Pocket fill — cascade of inward offsets, optionally zigzag.
    Pocket { strategy: PocketStrategy },
    /// Drill cycle — point or circle smaller than tool.
    Drill,
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
}

impl OperationParams {
    /// Defaults that line up with a "first profile cut on a 2 mm sheet".
    pub fn mill_default() -> Self {
        Self {
            depth: -2.0,
            start_depth: 0.0,
            step: -1.0,
            fast_move_z: 5.0,
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
            },
            leads: LeadsConfig {
                r#in: LeadKind::Off,
                out: LeadKind::Off,
                in_lenght: 5.0,
                out_lenght: 5.0,
            },
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
