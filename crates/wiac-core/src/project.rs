//! Project = geometry + machine + tool library + ordered list of
//! Operations. The Operation is the unit of CAM work — each one carries a
//! tool reference and per-kind parameters and produces a gcode block in the
//! final program.
//!
//! This is the data model behind the UX rework tracked in `dlr` /
//! `gua / 9dx / 5vc / …`. The shape lines up with mainstream CAM tools
//! (Carbide Create, Fusion 360 CAM, Estlcam, FreeCAD Path Workbench) so
//! the user's mental model translates without surprises.
//!
//! Backwards compatibility: every existing call site still hands the
//! pipeline a flat (segments, Setup, tabs) tuple. `migrate_legacy` lifts
//! that triple into a single-Profile-op Project so the pipeline can keep
//! running unchanged while the UI rolls over to the new model.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::offsets::TabPoint;
use crate::cam::setup::{
    LeadKind, LeadsConfig, MachineConfig, MillConfig, ObjectOrder, PocketConfig, Setup,
    TabsConfig, TabType, ToolConfig, ToolOffset,
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
    Layers { layers: Vec<String> },
    /// Run on the listed chain ids only.
    Objects { ids: Vec<u32> },
    /// Run on every chained object in the project.
    All,
}

impl Default for OperationSource {
    fn default() -> Self {
        Self::All
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

// ─── legacy migration ─────────────────────────────────────────────────────

/// Build a single-Profile-op Project from the legacy
/// (segments, setup, tabs) triple the old PipelineRequest carried. Used
/// during the rollout so existing transports keep producing identical
/// gcode while the UI flips to the new operations model.
pub fn migrate_legacy(
    segments: Vec<Segment>,
    setup: Setup,
    tabs: HashMap<u32, Vec<TabPoint>>,
) -> Project {
    let tool = ToolEntry {
        id: 1,
        name: format!("{:.1} mm tool", setup.tool.diameter),
        kind: kind_from_machine_mode(&setup),
        diameter: setup.tool.diameter,
        tip_diameter: None,
        dragoff: setup.tool.dragoff,
        flutes: 2,
        speed: setup.tool.speed,
        plunge_rate: setup.tool.rate_v,
        feed_rate: setup.tool.rate_h,
        coolant: coolant_from_setup(&setup),
    };
    let kind = kind_from_setup(&setup);
    let params = params_from_setup(&setup);
    let op = Operation {
        id: 1,
        name: name_for_kind(&kind),
        enabled: true,
        kind,
        tool_id: tool.id,
        source: OperationSource::All,
        params,
    };
    Project {
        segments,
        machine: setup.machine,
        tools: vec![tool],
        operations: vec![op],
        tabs,
    }
}

fn kind_from_machine_mode(setup: &Setup) -> ToolKind {
    use crate::cam::setup::MachineMode;
    match setup.machine.mode {
        MachineMode::Drag => ToolKind::DragKnife,
        MachineMode::Laser => ToolKind::LaserBeam,
        MachineMode::Mill if setup.tool.dragoff.is_some() => ToolKind::DragKnife,
        MachineMode::Mill => ToolKind::Endmill,
    }
}

fn coolant_from_setup(setup: &Setup) -> Coolant {
    if setup.tool.flood {
        Coolant::Flood
    } else if setup.tool.mist {
        Coolant::Mist
    } else {
        Coolant::Off
    }
}

fn kind_from_setup(setup: &Setup) -> OperationKind {
    if setup.pockets.active {
        OperationKind::Pocket {
            strategy: if setup.pockets.zigzag {
                PocketStrategy::Zigzag
            } else {
                PocketStrategy::Cascade
            },
        }
    } else {
        OperationKind::Profile {
            offset: setup.mill.offset,
        }
    }
}

fn name_for_kind(kind: &OperationKind) -> String {
    match kind {
        OperationKind::Profile { .. } => "Profile",
        OperationKind::Pocket { .. } => "Pocket",
        OperationKind::Drill => "Drill",
        OperationKind::Thread => "Thread",
        OperationKind::Chamfer => "Chamfer",
        OperationKind::Engrave => "Engrave",
        OperationKind::DragKnife => "Drag-knife",
        OperationKind::Helix => "Helix",
    }
    .into()
}

fn params_from_setup(setup: &Setup) -> OperationParams {
    OperationParams {
        depth: setup.mill.depth,
        start_depth: setup.mill.start_depth,
        step: setup.mill.step,
        fast_move_z: setup.mill.fast_move_z,
        helix: setup.mill.helix_mode,
        reverse: setup.mill.reverse,
        objectorder: setup.mill.objectorder,
        overcut: setup.mill.overcut,
        pocket_islands: setup.pockets.islands,
        pocket_nocontour: setup.pockets.nocontour,
        pocket_insideout: setup.pockets.insideout,
        tabs: setup.tabs.clone(),
        leads: setup.leads.clone(),
    }
}

/// Reverse direction: collapse a single-op Project back to a Setup so the
/// existing `run_pipeline` body can keep running until UX-3 lands. When
/// the project has more than one op, only the first one round-trips.
pub fn collapse_to_setup(project: &Project) -> (Setup, HashMap<u32, Vec<TabPoint>>) {
    let mut setup = Setup {
        machine: project.machine.clone(),
        ..Setup::default()
    };
    if let Some(tool) = project.tools.first() {
        setup.tool = ToolConfig {
            number: tool.id,
            diameter: tool.diameter,
            speed: tool.speed,
            pause: 1,
            mist: tool.coolant == Coolant::Mist,
            flood: tool.coolant == Coolant::Flood,
            dragoff: tool.dragoff,
            rate_v: tool.plunge_rate,
            rate_h: tool.feed_rate,
        };
    }
    if let Some(op) = project.operations.iter().find(|o| o.enabled) {
        setup.mill = MillConfig {
            active: true,
            depth: op.params.depth,
            start_depth: op.params.start_depth,
            step: op.params.step,
            fast_move_z: op.params.fast_move_z,
            helix_mode: op.params.helix,
            reverse: op.params.reverse,
            objectorder: op.params.objectorder,
            offset: match op.kind {
                OperationKind::Profile { offset } => offset,
                OperationKind::Pocket { .. } => ToolOffset::None,
                OperationKind::Engrave => ToolOffset::On,
                OperationKind::DragKnife => ToolOffset::On,
                _ => ToolOffset::None,
            },
            overcut: op.params.overcut,
        };
        setup.pockets = match op.kind {
            OperationKind::Pocket { strategy } => PocketConfig {
                active: true,
                islands: op.params.pocket_islands,
                zigzag: strategy == PocketStrategy::Zigzag,
                insideout: op.params.pocket_insideout,
                nocontour: op.params.pocket_nocontour,
            },
            _ => PocketConfig::default(),
        };
        setup.tabs = op.params.tabs.clone();
        setup.leads = op.params.leads.clone();
    }
    (setup, project.tabs.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::setup::{MachineMode, ToolOffset};
    use crate::geometry::{Point2, Segment};

    fn closed_square() -> Vec<Segment> {
        vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(10.0, 0.0), "0", 7),
            Segment::line(Point2::new(10.0, 0.0), Point2::new(10.0, 10.0), "0", 7),
            Segment::line(Point2::new(10.0, 10.0), Point2::new(0.0, 10.0), "0", 7),
            Segment::line(Point2::new(0.0, 10.0), Point2::new(0.0, 0.0), "0", 7),
        ]
    }

    #[test]
    fn migrate_legacy_synthesises_a_single_profile_op() {
        let mut setup = Setup::default();
        setup.tool.diameter = 6.0;
        setup.tool.speed = 12_000;
        setup.tool.flood = true;
        setup.mill.offset = ToolOffset::Outside;
        setup.mill.depth = -3.0;

        let project = migrate_legacy(closed_square(), setup, HashMap::new());
        assert_eq!(project.tools.len(), 1);
        assert_eq!(project.tools[0].diameter, 6.0);
        assert_eq!(project.tools[0].coolant, Coolant::Flood);
        assert_eq!(project.operations.len(), 1);
        assert!(matches!(
            project.operations[0].kind,
            OperationKind::Profile { offset: ToolOffset::Outside }
        ));
        assert_eq!(project.operations[0].params.depth, -3.0);
    }

    #[test]
    fn migrate_legacy_picks_pocket_when_setup_says_so() {
        let mut setup = Setup::default();
        setup.pockets.active = true;
        setup.pockets.zigzag = true;
        let project = migrate_legacy(closed_square(), setup, HashMap::new());
        assert!(matches!(
            project.operations[0].kind,
            OperationKind::Pocket {
                strategy: PocketStrategy::Zigzag
            }
        ));
    }

    #[test]
    fn migrate_legacy_picks_dragknife_for_drag_machine_mode() {
        let mut setup = Setup::default();
        setup.machine.mode = MachineMode::Drag;
        setup.tool.dragoff = Some(0.5);
        let project = migrate_legacy(closed_square(), setup, HashMap::new());
        assert_eq!(project.tools[0].kind, ToolKind::DragKnife);
        assert_eq!(project.tools[0].dragoff, Some(0.5));
    }

    #[test]
    fn collapse_to_setup_round_trips_a_single_op() {
        let mut original = Setup::default();
        original.tool.diameter = 4.0;
        original.tool.speed = 15_000;
        original.mill.offset = ToolOffset::Inside;
        original.mill.depth = -1.5;

        let project = migrate_legacy(closed_square(), original.clone(), HashMap::new());
        let (back, _) = collapse_to_setup(&project);
        assert_eq!(back.tool.diameter, original.tool.diameter);
        assert_eq!(back.tool.speed, original.tool.speed);
        assert_eq!(back.mill.offset, ToolOffset::Inside);
        assert_eq!(back.mill.depth, -1.5);
    }
}
