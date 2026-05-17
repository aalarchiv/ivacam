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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct OpParams {
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
    /// (0, 1). Drives the cascade step (= `tool_diameter` * (1 - overlap))
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
    /// Stufenfase (rt1.20 / Estlcam `Prog_KTD_Stufenfase)`: chamfer a
    /// drilled hole's rim immediately after the drill cycle. Honored
    /// only on `OpKind::Drill`. The post emits the drill cycle
    /// for each hole, then walks the cutter on a single revolution at
    /// the hole's edge at z = -width / `tan(tip_angle` / 2). When
    /// `Op.finish_tool_id` is set to a distinct tool, a M6 +
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
    /// `step+finish_step` schedule. Useful for non-linear schedules
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

impl OpParams {
    /// Defaults that line up with a "first profile cut on a 2 mm sheet".
    #[must_use]
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
