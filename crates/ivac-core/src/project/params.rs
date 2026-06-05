//! Per-op parameter bag + the helper enums it embeds (cut direction,
//! tab placements, etc.). See [`super::op::Op`] for how it slots in.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::setup::{LeadKind, LeadsConfig, ObjectOrder, TabType, TabsConfig};
use crate::cam::source_combine::FrameShape;

use super::op::is_zero_f64;

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
/// `object_id` is 1-based to match `OpSource::Objects::ids`;
/// `t ∈ [0, 1)` is the arc-length parameter along the chained
/// object's segments. `cam/tabs.rs::polyline_at_t` resolves the
/// parameter to a world point at gcode-emission time, so the tab
/// follows the geometry through transforms.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TabPlacement {
    pub object_id: u32,
    pub t: f64,
    /// Optional per-tab width override (mm). None ⇒ use
    /// `OpParams.tabs.width`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_override_mm: Option<f64>,
    /// Optional per-tab height override (mm). None ⇒ use
    /// `OpParams.tabs.height`.
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

/// Parameters shared by every closed-contour op (Profile / Pocket /
/// Engrave / `DragKnife`). Embedded in the matching [`super::op::OpKind`]
/// variants. Holds tab shape + placement, lead-in / lead-out, cut
/// direction, corner-feed reduction, and the optional user-picked
/// approach point. (kbx5 step 1.)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContourParams {
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
    /// `Mixed`; `Off` / `Auto` ignore.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tab_placements: Vec<TabPlacement>,
    /// Lead-in / lead-out shape.
    #[serde(default)]
    pub leads: LeadsConfig,
    /// Cut direction for the main (roughing) passes.
    #[serde(default, skip_serializing_if = "CutDirection::is_default")]
    pub cut_direction: CutDirection,
    /// Cut direction for the finishing pass — the offset that defines
    /// the wall surface (Pocket level=0 ring; Profile single-pass cut).
    #[serde(default, skip_serializing_if = "CutDirection::is_default")]
    pub finish_cut_direction: CutDirection,
    /// When > 0, slow the feed at sharp corners by this fraction.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub corner_feed_reduction: f64,
    /// Anfahrpunkt (rt1.26): user-picked XY where the cutter enters
    /// each closed-contour ring. `None` = auto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approach_point: Option<(f64, f64)>,
}

/// Parameters specific to [`super::op::OpKind::Pocket`]. (kbx5 step 1.)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PocketParams {
    /// XY overlap between consecutive pocket cuts (0..1). Stored at 0.0
    /// means "use the default".
    #[serde(default)]
    pub xy_overlap: f64,
    /// Treat inner contours as islands (don't cut them).
    #[serde(default)]
    pub pocket_islands: bool,
    /// Skip the wall-defining ring (contour cut).
    #[serde(default)]
    pub pocket_nocontour: bool,
    /// XY stock allowance left uncut by roughing (rt1.24).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_xy_allowance_mm: Option<f64>,
    /// Pocket-Outside wrapper shape. Set only on ops created via the
    /// Pocket-Outside UX.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_shape: Option<FrameShape>,
    /// Padding around the selection bbox to size the frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_padding_mm: Option<f64>,
    /// Corner radius for `FrameShape::RoundedRectangle`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_corner_radius_mm: Option<f64>,
}

/// Parameters specific to [`super::op::OpKind::Profile`]. (kbx5 step 1.)
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileParams {
    /// Dip into sharp inner corners so the cutter clears the geometric
    /// corner. Only meaningful when the offset is non-zero.
    #[serde(default)]
    pub overcut: bool,
    /// Reverse the cut direction (climb ↔ conventional).
    #[serde(default)]
    pub reverse: bool,
    /// Helical descent inside a closed contour.
    #[serde(default)]
    pub helix: bool,
}

/// Parameters specific to [`super::op::OpKind::VCarve`]. (kbx5 step 1.)
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VCarveParams {
    /// V-Carve cap on the inscribed-circle radius (mm). `None` = no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carve_max_width_mm: Option<f64>,
    /// V-Carve "second-pass" toggle. When true, re-cuts only the
    /// points whose first pass fell short of the geometric target.
    #[serde(default)]
    pub multi_pass_refine: bool,
    /// r8ut: trace the full medial axis (creates extra spine cuts
    /// through the interior of wide regions). Default `false` matches
    /// Estlcam's behaviour — the toolpath traces the BOUNDARY offset
    /// inward by `R = effective_r_cap`, plunged to depth
    /// `-R / tan(angle / 2)`, and the centre plateau is left
    /// untouched. Set true to recover the prior ivac behaviour for the
    /// rare "carve a depth gradient across the whole interior"
    /// workflow (think Aspire-style relief).
    #[serde(default)]
    pub full_medial_axis: bool,
    /// rt1.7: extra inward offset applied to the source region BEFORE
    /// the V-Carve pass. Used to build the "plug" side of an inlay pair:
    /// the plug is `gap_mm` smaller per side than the pocket, so when
    /// glued in it wedges into the tapered pocket walls with that
    /// clearance. The pocket side uses `None` / `0`; the plug uses the
    /// shared `gap_mm` value (typical 0.05–0.2 mm). The offset is
    /// applied to both the medial-axis and perimeter modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_inset_mm: Option<f64>,
}

/// Universal per-op parameters — fields that apply to **every** op kind.
/// Kind-specific config lives in the matching variant struct embedded
/// in [`super::op::OpKind`]:
///
/// - Closed-contour params (tabs, leads, cut direction, approach point,
///   corner-feed) → [`ContourParams`]
/// - Pocket cascade / islands / Pocket-Outside frame → [`PocketParams`]
/// - Profile overcut / reverse / helix → [`ProfileParams`]
/// - V-Carve cap / second-pass → [`VCarveParams`]
/// - Drill Stufenfase chamfer width → [`super::op::OpKind::Drill`]
///
/// (kbx5 step 3: the flat-junk-drawer `OpParams` was reduced to this
/// common struct after readers and writers all moved to the variant
/// structs.)
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct OpParamsCommon {
    /// Final cut depth (negative number — a depth, not a height).
    ///
    /// 4dxb: `#[serde(default)]` so program-only ops (Pause, Homing,
    /// Probe, `CycleMarker`, `GcodeInclude`) — which have no meaningful
    /// depth schedule and whose FE constructors omit the field —
    /// deserialize without a "missing field `depth`" error. Cutting
    /// ops always emit the field explicitly; this default only fires
    /// for program-only ops whose params bag is ignored anyway.
    #[serde(default)]
    pub depth: f64,
    /// Z at which the first pass starts.
    ///
    /// 4dxb: same rationale as `depth` — defaulted to 0.0 so
    /// program-only ops decode cleanly.
    #[serde(default)]
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
    ///
    /// 4dxb: defaulted to 0.0 alongside the other two universal
    /// scalars. Program-only ops never use this; cutting ops always
    /// emit it explicitly.
    #[serde(default)]
    pub fast_move_z: f64,
    /// Cut-order strategy for multiple objects.
    #[serde(default)]
    pub objectorder: ObjectOrder,
    /// How the cutter descends into material at the start of each Z
    /// pass. Default Direct (straight plunge). Ramp { `angle_deg` } walks
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
    /// 1mlv: leave this much XY stock unmachined on every wall (Profile
    /// inside/outside cascade, Pocket cascade). Positive number — the
    /// cutter stays this far away from the geometric wall, so a later
    /// finishing pass (different tool / op) can clean it up. Differs
    /// from `PocketParams.finish_xy_allowance_mm` which is Pocket-only
    /// and triggers an extra contour pass; `stock_to_leave_mm` applies
    /// to ALL offset-cascade ops and is the universal "rough leaves
    /// material" knob. 0.0 = cutter walks the geometric wall (the
    /// default, matching prior ivac behaviour).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub stock_to_leave_mm: f64,
    /// Explicit list of Z depths for each pass, overriding the
    /// `step+finish_step` schedule. Useful for non-linear schedules
    /// (shallower at start for tough material, deeper later, slow
    /// finish at the end). Each entry is an absolute Z (negative
    /// number); the cutter visits them in order. Empty = use the
    /// step-down loop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depth_list: Vec<f64>,
}

/// Back-compat alias — keeps existing call sites compiling.
pub type OpParams = OpParamsCommon;

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

impl OpParamsCommon {
    /// Defaults that line up with a "first profile cut on a 2 mm sheet".
    #[must_use]
    pub fn mill_default() -> Self {
        Self {
            depth: -2.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            objectorder: ObjectOrder::default(),
            plunge: crate::cam::setup::PlungeStrategy::Direct,
            feed_rate_override: None,
            plunge_rate_override: None,
            finish_step: None,
            through_depth: 0.0,
            depth_list: Vec::new(),
            stock_to_leave_mm: 0.0,
        }
    }
}

/// Sensible defaults for closed-contour params — leads off, tabs off,
/// conventional milling. Used as the `mill_default` companion for tests
/// and any constructor that wants reasonable starting values.
#[must_use]
pub fn contour_mill_default() -> ContourParams {
    ContourParams {
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
        corner_feed_reduction: 0.0,
        approach_point: None,
    }
}

/// Sensible defaults for [`PocketParams`] — 50 % overlap, no islands,
/// no Pocket-Outside frame. Used by tests that build Pocket ops.
#[must_use]
pub fn pocket_mill_default() -> PocketParams {
    PocketParams {
        xy_overlap: 0.5,
        pocket_islands: false,
        pocket_nocontour: false,
        finish_xy_allowance_mm: None,
        frame_shape: None,
        frame_padding_mm: None,
        frame_corner_radius_mm: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_step_zero_deserializes_to_none() {
        let json = r#"{"depth":-2.0,"start_depth":0.0,"step":0.0,"fast_move_z":5.0}"#;
        let p: OpParams = serde_json::from_str(json).unwrap();
        assert_eq!(p.step, None);
    }

    #[test]
    fn legacy_step_negative_deserializes_to_some() {
        let json = r#"{"depth":-2.0,"start_depth":0.0,"step":-1.0,"fast_move_z":5.0}"#;
        let p: OpParams = serde_json::from_str(json).unwrap();
        assert_eq!(p.step, Some(-1.0));
    }

    #[test]
    fn missing_step_deserializes_to_none() {
        let json = r#"{"depth":-2.0,"start_depth":0.0,"fast_move_z":5.0}"#;
        let p: OpParams = serde_json::from_str(json).unwrap();
        assert_eq!(p.step, None);
    }

    #[test]
    fn null_step_deserializes_to_none() {
        let json = r#"{"depth":-2.0,"start_depth":0.0,"step":null,"fast_move_z":5.0}"#;
        let p: OpParams = serde_json::from_str(json).unwrap();
        assert_eq!(p.step, None);
    }

    #[test]
    fn step_none_skips_field_on_serialize() {
        let mut p = OpParams::mill_default();
        p.step = None;
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            !json.contains("\"step\""),
            "step=None should be skipped: {json}"
        );
    }

    #[test]
    fn step_some_writes_bare_number_on_serialize() {
        let mut p = OpParams::mill_default();
        p.step = Some(-0.5);
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            json.contains("\"step\":-0.5"),
            "step=Some(-0.5) should write bare number: {json}"
        );
    }

    /// 4dxb: program-only ops (Pause, Homing, Probe, `CycleMarker`,
    /// `GcodeInclude`) carry no meaningful depth schedule and the
    /// frontend constructors at `project.svelte.ts` omit
    /// `depth` / `startDepth` from the op shape. `JSON.stringify`
    /// drops undefined keys, so the wire `params` bag arrives
    /// without those fields. Before this fix, the Rust deserializer
    /// bailed with `missing field depth`, breaking Generate
    /// whenever a Pause sat between cutting ops. The three universal
    /// scalars (`depth`, `start_depth`, `fast_move_z`) now decode to 0.0
    /// when omitted — program-only ops ignore them anyway.
    // juvx: exact equality vs `0.0` is the contract under test —
    // serde decoded a missing field to the float-default of zero, NOT
    // an approximation. The lint's "use approx" guidance doesn't apply.
    #[allow(clippy::float_cmp)]
    #[test]
    fn op_params_decodes_with_all_universal_scalars_missing() {
        // Mirrors what the FE serializer emits for a Pause op:
        // only `objectorder` is present in the bag.
        let json = r#"{"objectorder":"nearest"}"#;
        let p: OpParams = serde_json::from_str(json)
            .expect("OpParams must deserialize when depth / start_depth / fast_move_z are omitted");
        assert_eq!(p.depth, 0.0);
        assert_eq!(p.start_depth, 0.0);
        assert_eq!(p.fast_move_z, 0.0);
        assert_eq!(p.step, None);
    }

    /// 4dxb regression: a completely empty params bag still decodes
    /// (covers the most-pessimistic future serializer that emits
    /// nothing for a program-only op).
    // juvx: same exact-zero contract as above.
    #[allow(clippy::float_cmp)]
    #[test]
    fn op_params_decodes_from_empty_object() {
        let p: OpParams = serde_json::from_str("{}").expect("empty params bag must decode");
        assert_eq!(p.depth, 0.0);
        assert_eq!(p.start_depth, 0.0);
        assert_eq!(p.fast_move_z, 0.0);
    }

    #[test]
    fn fixture_step_values_round_trip_through_shim() {
        // The .vc-project.json files on disk are the frontend's camelCase
        // shape; the wire `OpParams` (snake_case, nested under
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
                let _: OpParams = serde_json::from_value(wire)
                    .unwrap_or_else(|e| panic!("op #{i} step in {path:?}: {e}"));
            }
        }
    }
}
