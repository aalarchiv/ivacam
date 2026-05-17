//! Project = geometry + machine + tool library + ordered list of
//! Operations. The Op is the unit of CAM work — each one carries a
//! tool reference and per-kind parameters and produces a gcode block in
//! the final program.
//!
//! Modeled after mainstream CAM tools (Carbide Create, Fusion 360 CAM,
//! Estlcum, `FreeCAD` Path Workbench) so the user's mental model translates
//! without surprises.
//!
//! This module is a thin hub: the actual types live in per-domain
//! submodules and are re-exported here so callers continue to use
//! `crate::project::X` unchanged.

// # CAM/sim pedantic-lint exemptions
// Default-impl test helpers use parallel names (`tool_a`/`tool_b`,
// `op_with`/`op_without`) that enumerate distinct test cases. Serde
// `skip_serializing_if = "is_default_…"` helpers take `&T` because that's
// the signature serde requires. `OpParams` is the user-facing
// per-op config bag — one bool per UI checkbox, so the JSON contract
// flattens the bool fields by design (see audit issue kbx5 for the
// planned move-to-OpKind-variants refactor).
#![allow(
    clippy::similar_names,
    clippy::trivially_copy_pass_by_ref,
    clippy::struct_excessive_bools
)]

pub mod fixture;
pub mod op;
pub mod params;
pub mod text;
pub mod tool;

pub use fixture::*;
pub use op::*;
pub use params::*;
pub use text::*;
pub use tool::*;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::setup::MachineConfig;
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
    pub operations: Vec<Op>,

    /// Fixtures (clamps, dogs, vise jaws, hold-downs) the cutter must
    /// avoid throughout the entire program — including rapids. The sim
    /// pass tests every toolpath segment against this set and emits
    /// `SimWarning::FixtureCollision` on overlap. Default empty: a
    /// project with no fixtures behaves exactly as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixtures: Vec<Fixture>,

    /// First-class editable text entities — content / font / size /
    /// position / rotation / spacing. The pipeline pre-pass renders each
    /// `TextLayer` to segments before any op runs so the existing
    /// `Engrave` (and friends) op can target the rendered geometry by
    /// layer name `__text_<id>`. Edits to a `TextLayer` re-run the
    /// pipeline; cache keys include `text_layers` content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_layers: Vec<TextLayer>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_default_is_empty_but_well_typed() {
        let p = Project::default();
        assert!(p.segments.is_empty());
        assert!(p.tools.is_empty());
        assert!(p.operations.is_empty());
        assert!(p.fixtures.is_empty());
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
                    color: 0xFFA0_50C0,
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
    fn project_with_no_fixtures_skips_field_on_serialize() {
        let p = Project::default();
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            !json.contains("\"fixtures\""),
            "empty fixtures should be skipped: {json}"
        );
    }
}
