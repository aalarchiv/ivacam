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

pub mod config;
pub mod fixture;
pub mod machine;
pub mod op;
pub mod params;
pub mod text;
pub mod tool;

pub use config::*;
pub use fixture::*;
pub use machine::*;
pub use op::*;
pub use params::*;
pub use text::*;
pub use tool::*;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

    /// i5g4 (MVP): explicit work-offset between the geometry frame
    /// (where the DXF / SVG was drawn) and the gcode WCS origin
    /// (where the user zeros the spindle on the real machine). All
    /// zeros (default) means "geometry origin = WCS origin". Full
    /// G54..G59 + per-fixture origins are a future feature; this
    /// field gives a single offset the sim and the WCS warning
    /// consult. Persisted into project files; legacy files lacking
    /// the field default to zeros and behave exactly as before.
    #[serde(default, skip_serializing_if = "WorkOffset::is_default")]
    pub work_offset: WorkOffset,

    /// vrrr: physical stock envelope, resolved to an axis-aligned box in
    /// the geometry frame. The frontend derives this from its auto/manual
    /// stock UI (margin / custom dims / offset) via `computeFootprint`
    /// and sends the resolved box; a CLI / server consumer sets the
    /// dimensions directly. `None` (default) skips the `out_of_stock`
    /// scan, so a transport that doesn't model stock simply gets no
    /// out-of-stock checks. The stock
    /// top sits at z = 0 (the WCS / geometry origin plane); the body
    /// extends downward by `thickness_mm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stock: Option<StockConfig>,

    /// f60x: relief / 3-axis surfacing sources — the target Z(x,y) surfaces
    /// that [`OpKind::ReliefMill`] ops finish. Stored at project level (like
    /// `text_layers`) and referenced by `source_id`, not embedded in the op,
    /// because a surface grid is large and ops get cloned + hashed. Each
    /// carries a normalized-brightness grid; the op maps it to Z at planning
    /// time. Default empty: projects with no relief ops are unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relief_sources: Vec<ReliefSource>,

    /// l8lk: when `true`, the pipeline runs an optional tool-change-order
    /// optimization that groups consecutive same-tool work so a
    /// `T1 / T2 / T1` program emits `T1, T1, T2` with ONE tool change
    /// instead of two. Matters most on manual machines, where every swap
    /// is minutes + a re-probe + operator-error risk. The reorder is
    /// barrier-aware: program-only ops (Pause / Homing / …) and any op
    /// with [`Op::pin_order`] stay put and nothing moves across them, so
    /// a deliberate cut order (tabs, thin walls) is preserved. `false`
    /// (default) keeps the declared op order unchanged. See
    /// `order_ops_by_tool` in the pipeline.
    #[serde(default, skip_serializing_if = "crate::project::op::is_false")]
    pub group_ops_by_tool: bool,
}

/// f60x: a target surface source for relief / ball-nose surfacing. Holds a
/// row-major normalized-brightness grid (each value in `[0, 1]`) plus its
/// world placement; the depth mapping (brightness → Z) lives on the
/// [`OpKind::ReliefMill`] op so the user can retune depth without
/// re-uploading the image. The first producer (f60x-D) decodes a grayscale
/// image frontend-side; a future STL rasterizer would populate the same
/// grid. The driver turns it into a [`crate::cam::surface::SurfaceField`]
/// via `SurfaceField::from_grayscale`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReliefSource {
    /// Stable id referenced by [`OpKind::ReliefMill::source_id`].
    pub id: u32,
    /// Human-readable label (e.g. the source filename). Optional.
    #[serde(default)]
    pub name: String,
    /// World XY of the grid's min corner (the (0,0) cell's lower-left).
    pub origin: crate::geometry::Point2,
    /// Cell size in mm (square cells / pixel pitch in world units).
    pub cell: f64,
    pub cols: u32,
    pub rows: u32,
    /// Row-major normalized brightness in `[0, 1]`. Length must be
    /// `cols * rows`.
    pub brightness: Vec<f32>,
}

/// vrrr: resolved stock box. See [`Project::stock`]. Kept deliberately
/// thin — the auto/manual/margin derivation lives frontend-side (it's a
/// UI convenience for sizing the box to imported geometry); the core
/// only needs the final axis-aligned envelope for the `out_of_stock`
/// scan (and, in future, stock-aware sim / rapid / holder checks).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StockConfig {
    /// Min corner (x, y) of the stock box in the geometry frame (mm).
    #[serde(default)]
    pub origin: [f64; 2],
    /// X extent of the stock box (mm).
    pub width_mm: f64,
    /// Y extent of the stock box (mm).
    pub height_mm: f64,
    /// Material thickness (mm). The stock body spans
    /// z ∈ [`top_z_mm` − thickness, `top_z_mm`].
    pub thickness_mm: f64,
    /// ya00: Z of the stock TOP plane (mm) in the WCS frame. Default 0 ⇒
    /// the top sits at the WCS origin plane (the legacy assumption), body
    /// extending down to `-thickness_mm`. A non-zero value models zeroing
    /// the machine somewhere other than the stock top (e.g. on the bed,
    /// `top_z_mm = +thickness`); the `out_of_stock` scan and the sim
    /// heightmap shift with it. Distinct from `WorkOffset::z_mm` (which
    /// moves the WCS origin relative to the geometry) — this moves the
    /// stock material relative to that origin.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub top_z_mm: f64,
}

/// i5g4: program-level work-coordinate offset. Defaults to all
/// zeros — geometry origin == WCS origin. When the user zeros the
/// machine somewhere different from the geometry origin, set this
/// so the sim can align the heightmap to the WCS frame. The full
/// per-fixture / G54..G59 selector is a follow-up feature.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkOffset {
    /// X offset (mm) from geometry origin to WCS origin.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub x_mm: f64,
    /// Y offset (mm) from geometry origin to WCS origin.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub y_mm: f64,
    /// Z offset (mm) from geometry origin to WCS origin.
    /// Positive means the WCS Z=0 is ABOVE the geometry's z=0.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub z_mm: f64,
    /// Which work coordinate system this offset applies to. The
    /// gcode emitter doesn't (yet) flip between G54..G59 — this is
    /// a labelling field for the UI + future expansion.
    #[serde(default, skip_serializing_if = "Wcs::is_default")]
    pub wcs: Wcs,
}

impl WorkOffset {
    fn is_default(v: &Self) -> bool {
        is_zero_f64(&v.x_mm)
            && is_zero_f64(&v.y_mm)
            && is_zero_f64(&v.z_mm)
            && Wcs::is_default(&v.wcs)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum Wcs {
    #[default]
    G54,
    G55,
    G56,
    G57,
    G58,
    G59,
}

impl Wcs {
    fn is_default(v: &Self) -> bool {
        matches!(v, Self::G54)
    }

    /// The gcode word that activates this WCS (`G54`..`G59`). e2mq:
    /// consumed by the post-processor prologue so the controller's
    /// active WCS matches `Project.work_offset.wcs` even when the
    /// boot-default isn't G54.
    #[must_use]
    pub fn gcode_word(self) -> &'static str {
        match self {
            Self::G54 => "G54",
            Self::G55 => "G55",
            Self::G56 => "G56",
            Self::G57 => "G57",
            Self::G58 => "G58",
            Self::G59 => "G59",
        }
    }

    /// The `P<n>` operand for `G10 L20 P<n>` that targets this WCS.
    /// `G54 = P1`, `G55 = P2`, …, `G59 = P6` per RS-274 / Mach3 / GRBL
    /// >= 1.1 / LinuxCNC convention. e2mq: GRBL's `tool_z_shift`
    /// > previously hardcoded `P1`, so a user-active G55 had its
    /// > z-shift written into the wrong WCS.
    #[must_use]
    pub fn p_number(self) -> u32 {
        match self {
            Self::G54 => 1,
            Self::G55 => 2,
            Self::G56 => 3,
            Self::G57 => 4,
            Self::G58 => 5,
            Self::G59 => 6,
        }
    }
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

/// Register this module's wire types in the OpenAPI components map.
/// Co-located with the type definitions (kb1y) so adding a wire type is
/// a same-file edit; `crate::schema::components_schemas` composes these.
pub(crate) fn register_schemas(map: &mut crate::schema::SchemaMap) {
    crate::schema::insert::<Project>(map, "Project");
    crate::schema::insert::<Op>(map, "Op");
    crate::schema::insert::<OpKind>(map, "OpKind");
    crate::schema::insert::<DrillCycle>(map, "DrillCycle");
    crate::schema::insert::<OpParams>(map, "OpParams");
    crate::schema::insert::<OpSource>(map, "OpSource");
    crate::schema::insert::<SourceCombine>(map, "SourceCombine");
    crate::schema::insert::<CutDirection>(map, "CutDirection");
    crate::schema::insert::<PlungeStrategy>(map, "PlungeStrategy");
    crate::schema::insert::<PocketStrategy>(map, "PocketStrategy");
    crate::schema::insert::<PatternConfig>(map, "PatternConfig");
    crate::schema::insert::<ToolEntry>(map, "ToolEntry");
    crate::schema::insert::<ToolKind>(map, "ToolKind");
    crate::schema::insert::<Coolant>(map, "Coolant");
    crate::schema::insert::<Fixture>(map, "Fixture");
    crate::schema::insert::<FixtureKind>(map, "FixtureKind");
    crate::schema::insert::<TextLayer>(map, "TextLayer");
    crate::schema::insert::<TextLayerKind>(map, "TextLayerKind");
    crate::schema::insert::<TextAlignment>(map, "TextAlignment");
}
