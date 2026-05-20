//! Op model — the per-operation work unit consumed by the pipeline.
//! Carries an [`OpKind`] discriminator, the source-geometry selector
//! [`OpSource`], and a parameter bag [`super::params::OpParams`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::setup::ToolOffset;

use super::params::{ContourParams, OpParams, PocketParams, ProfileParams, VCarveParams};

/// One operation in the project's program. Carries the kind-discriminator
/// (which itself embeds the per-kind params via [`OpKind`]), the
/// universal [`OpParamsCommon`] bag (depth schedule, plunge, feed
/// overrides), tool refs, source selector, and that's it. Patterning
/// is per-kind today (only `OpKind::Drill` carries it); add it to
/// other variants if more kinds need patterning.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Op {
    pub id: u32,
    pub name: String,
    pub enabled: bool,
    pub kind: OpKind,
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
    pub source: OpSource,
    pub params: OpParams,
}

impl Op {
    /// [`ContourParams`] embedded in this op's variant for closed-contour
    /// kinds (Profile / Pocket / Engrave / `DragKnife`). `None` for kinds
    /// that don't carry contour params. (kbx5 step 2.)
    #[must_use]
    pub fn contour_params(&self) -> Option<&ContourParams> {
        match &self.kind {
            OpKind::Profile { contour, .. }
            | OpKind::Pocket { contour, .. }
            | OpKind::Engrave { contour }
            | OpKind::DragKnife { contour } => Some(contour),
            _ => None,
        }
    }

    /// Mutable view of the same [`ContourParams`]. (kbx5 step 3.)
    pub fn contour_params_mut(&mut self) -> Option<&mut ContourParams> {
        match &mut self.kind {
            OpKind::Profile { contour, .. }
            | OpKind::Pocket { contour, .. }
            | OpKind::Engrave { contour }
            | OpKind::DragKnife { contour } => Some(contour),
            _ => None,
        }
    }

    /// Mutable view of the [`PocketParams`] inside `OpKind::Pocket`.
    pub fn pocket_params_mut(&mut self) -> Option<&mut PocketParams> {
        match &mut self.kind {
            OpKind::Pocket { pocket, .. } => Some(pocket),
            _ => None,
        }
    }

    /// Mutable view of the [`ProfileParams`] inside `OpKind::Profile`.
    pub fn profile_params_mut(&mut self) -> Option<&mut ProfileParams> {
        match &mut self.kind {
            OpKind::Profile { profile, .. } => Some(profile),
            _ => None,
        }
    }

    /// Mutable view of the [`VCarveParams`] inside `OpKind::VCarve`.
    pub fn vcarve_params_mut(&mut self) -> Option<&mut VCarveParams> {
        match &mut self.kind {
            OpKind::VCarve { carve } => Some(carve),
            _ => None,
        }
    }

    /// [`PocketParams`] embedded in [`OpKind::Pocket`]. `None` for every
    /// other kind. (kbx5 step 2.)
    #[must_use]
    pub fn pocket_params(&self) -> Option<&PocketParams> {
        match &self.kind {
            OpKind::Pocket { pocket, .. } => Some(pocket),
            _ => None,
        }
    }

    /// [`ProfileParams`] embedded in [`OpKind::Profile`]. `None` elsewhere.
    /// (kbx5 step 2.)
    #[must_use]
    pub fn profile_params(&self) -> Option<&ProfileParams> {
        match &self.kind {
            OpKind::Profile { profile, .. } => Some(profile),
            _ => None,
        }
    }

    /// [`VCarveParams`] embedded in [`OpKind::VCarve`]. `None` elsewhere.
    /// (kbx5 step 2.)
    #[must_use]
    pub fn vcarve_params(&self) -> Option<&VCarveParams> {
        match &self.kind {
            OpKind::VCarve { carve } => Some(carve),
            _ => None,
        }
    }

    /// Post-drill chamfer width (Stufenfase, rt1.20) on [`OpKind::Drill`].
    /// `None` for every other kind, or when the drill op doesn't have a
    /// chamfer-after configured.
    #[must_use]
    pub fn drill_chamfer_after_width_mm(&self) -> Option<f64> {
        match &self.kind {
            OpKind::Drill {
                chamfer_after_width_mm,
                ..
            } => *chamfer_after_width_mm,
            _ => None,
        }
    }

    /// Pattern repetition. Today only [`OpKind::Drill`] carries one;
    /// other kinds always return `None`. The pipeline's pattern
    /// expansion runs unchanged — there just are no non-Drill patterned
    /// ops to expand. (kbx5 step 3.)
    #[must_use]
    pub fn pattern(&self) -> Option<PatternConfig> {
        match &self.kind {
            OpKind::Drill { pattern, .. } => *pattern,
            _ => None,
        }
    }
}

impl Default for Op {
    fn default() -> Self {
        Self {
            id: 1,
            name: "Profile".into(),
            enabled: true,
            kind: OpKind::Profile {
                offset: ToolOffset::Outside,
                contour: ContourParams::default(),
                profile: ProfileParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::default(),
        }
    }
}

/// Pattern repetition for an [`Op`]. When attached, the pipeline
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum OpKind {
    /// Contour cut — equivalent to today's "mill" with a parallel-offset
    /// pass at `offset` of the tool radius. Embedded `contour` and
    /// `profile` carry the per-kind params (kbx5 step 1); legacy
    /// payloads land them at default and the migration deserializer
    /// fills them from the flat `OpParams` bag.
    Profile {
        offset: ToolOffset,
        #[serde(default)]
        contour: ContourParams,
        #[serde(default)]
        profile: ProfileParams,
    },
    /// Pocket fill — cascade of inward offsets, optionally zigzag. Embeds
    /// `contour` (lead-in/out, cut direction, tabs) and `pocket` (xy
    /// overlap, islands, finish allowance, Pocket-Outside frame).
    Pocket {
        strategy: PocketStrategy,
        #[serde(default)]
        contour: ContourParams,
        #[serde(default)]
        pocket: PocketParams,
    },
    /// Drill cycle — point or circle smaller than tool. Carries a
    /// [`DrillCycle`] that picks G81 / G83 / G73 (or the manual G0/G1
    /// fallback for posts that don't support canned cycles). Also
    /// carries the Stufenfase post-drill chamfer width (rt1.20) and
    /// the optional pattern (kbx5 step 1 — Drill is the only kind
    /// patternable for now).
    Drill {
        cycle: DrillCycle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chamfer_after_width_mm: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<PatternConfig>,
    },
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
    /// Tool-on engraving — no offset, follows the source path. Carries
    /// `contour` params (leads / cut direction / approach point).
    Engrave {
        #[serde(default)]
        contour: ContourParams,
    },
    /// Drag-knife — emits trail-compensation moves. Carries `contour`
    /// params (mainly approach point + cut direction).
    DragKnife {
        #[serde(default)]
        contour: ContourParams,
    },
    /// Helical entry into a closed contour. Reserved for future
    /// thread-mill style expansion; no params today.
    Helix,
    /// V-Carve: drives a V-bit along the medial axis of a closed region,
    /// with depth varying per point so the V's tip dips deepest where the
    /// region is widest. The depth at each point is
    /// `z = -R_inscribed / tan(tip_angle / 2)` for the inscribed-circle
    /// radius `R_inscribed` at that point of the medial axis. Embeds the
    /// per-kind `VCarveParams` (`carve_max_width` cap, second-pass refine).
    VCarve {
        #[serde(default)]
        carve: VCarveParams,
    },
    /// rt1.34: program-level optional-stop. Emits `M5` (spindle off) +
    /// `M0` + an operator-readable comment + `M3` (spindle back on) where
    /// the op sits in the operations list. The cutter doesn't move and no
    /// source geometry is required — the op exists purely to pause the
    /// machine between two other ops so the operator can intervene
    /// (manual tool change on a machine without ATC, inspect the cut,
    /// flip the stock for double-sided work, etc.). Mirrors Estlcam's
    /// "Insert M0" entry (decompile `_I.cs:3394`).
    Pause {
        /// One-line message shown on the operator console / pendant.
        /// Empty string is allowed; the post still emits the M0 stop.
        #[serde(default)]
        message: String,
    },
}

fn default_chamfer_width() -> f64 {
    1.0
}

fn default_thread_pitch() -> f64 {
    1.0
}

fn default_thread_internal() -> bool {
    true
}

/// Drill-cycle picker for [`OpKind::Drill`]. Mirrors the canned
/// cycles G81 / G83 / G73 from the `LinuxCNC` / Fanuc dialect plus the
/// dwell-at-bottom parameter `PyCAM`'s `Drilling.py` exposes. Posts that
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
    /// Raster fill. rt1.9: `angle_deg` rotates the sweep direction —
    /// `0` (default) = horizontal sweeps (original behaviour), 90 =
    /// vertical, 45 = diagonal. Wire-compatible: serialises as the
    /// bare string `"zigzag"` when `angle_deg == 0`, otherwise as
    /// `{ "kind": "zigzag", "angle_deg": <n> }`. Pre-rt1.9 projects
    /// that wrote `"zigzag"` load with `angle_deg = 0`.
    Zigzag {
        angle_deg: f64,
    },
    Spiral,
    Trochoidal {
        engagement_angle_deg: f64,
        loop_radius_factor: f64,
    },
    /// Halfpipe (rt1.19 / Estlcam _`PK::Halfpipe)`: slot machining where
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
            // rt1.9: bare-string serialisation when the angle is the
            // default 0; tagged-object form when the user picked an
            // angle. Keeps wire size minimal for the common case AND
            // pre-rt1.9 projects re-serialise to the original `"zigzag"`
            // string, so workspace files don't churn on load.
            Self::Zigzag { angle_deg } if angle_deg.abs() < 1e-9 => ser.serialize_str("zigzag"),
            Self::Zigzag { angle_deg } => {
                let mut s = ser.serialize_struct("Zigzag", 2)?;
                s.serialize_field("kind", "zigzag")?;
                s.serialize_field("angle_deg", &angle_deg)?;
                s.end()
            }
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
                    "required": ["kind"],
                    "properties": {
                        "kind": { "type": "string", "enum": ["zigzag"] },
                        "angle_deg": { "type": "number", "format": "double" }
                    }
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
                angle_deg: Option<f64>,
                #[serde(default)]
                profile: Option<ProfileObj>,
            },
        }
        match Repr::deserialize(de)? {
            Repr::Str(s) => match s.as_str() {
                "cascade" => Ok(Self::Cascade),
                "zigzag" => Ok(Self::Zigzag { angle_deg: 0.0 }),
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
                angle_deg,
                profile,
            } => match kind.as_str() {
                "cascade" => Ok(Self::Cascade),
                "zigzag" => Ok(Self::Zigzag {
                    angle_deg: angle_deg.unwrap_or(0.0),
                }),
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
pub enum OpSource {
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

impl Default for OpSource {
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
    /// pipeline-level behavior (see pipeline.rs's `selected_set` logic).
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

pub(crate) fn is_zero_f64(v: &f64) -> bool {
    v.abs() < 1e-9
}

#[cfg(test)]
mod tests {
    use super::*;

    /// kbx5 step 3: an Op with the per-kind variant structs populated
    /// deserializes losslessly. The old legacy-flat migration paths are
    /// gone (no users carried saved files into this revision), so
    /// `OpParams` is universal-only and the variant data lives entirely
    /// on `OpKind`.
    #[test]
    fn structured_pocket_op_round_trips_through_serde_json() {
        let raw = serde_json::json!({
            "id": 1,
            "name": "Pocket",
            "enabled": true,
            "kind": {
                "type": "pocket",
                "strategy": "cascade",
                "contour": {
                    "tabs": {
                        "active": true, "width": 8.0, "height": 1.5,
                        "tab_type": "rectangle", "ramp_angle_deg": 30.0
                    },
                    "leads": {
                        "in": "straight", "out": "off",
                        "in_lenght": 4.0, "out_lenght": 0.0
                    },
                    "cut_direction": "climb"
                },
                "pocket": {
                    "xy_overlap": 0.4,
                    "pocket_islands": true,
                    "pocket_insideout": true,
                    "finish_xy_allowance_mm": 0.3
                }
            },
            "tool_id": 1,
            "source": {"kind": "all"},
            "params": {
                "depth": -2.0,
                "start_depth": 0.0,
                "fast_move_z": 5.0
            }
        });
        let op: Op = serde_json::from_value(raw).expect("Op deserialize");
        let OpKind::Pocket {
            contour, pocket, ..
        } = &op.kind
        else {
            panic!("expected Pocket kind");
        };
        assert!((pocket.xy_overlap - 0.4).abs() < 1e-9);
        assert!(pocket.pocket_islands);
        assert!(pocket.pocket_insideout);
        assert_eq!(pocket.finish_xy_allowance_mm, Some(0.3));
        assert!(contour.tabs.active);
        assert!((contour.tabs.width - 8.0).abs() < 1e-9);
        assert!(matches!(
            contour.leads.r#in,
            crate::cam::setup::LeadKind::Straight
        ));
        assert!(matches!(
            contour.cut_direction,
            crate::project::CutDirection::Climb
        ));
    }

    /// kbx5 step 3: a Drill op with an embedded pattern round-trips.
    /// `Op.pattern` is gone — only `OpKind::Drill.pattern` carries
    /// pattern repetitions now.
    #[test]
    fn drill_kind_pattern_round_trips() {
        let raw = serde_json::json!({
            "id": 2,
            "name": "Drill",
            "enabled": true,
            "kind": {
                "type": "drill",
                "cycle": {"kind": "simple"},
                "pattern": {"kind": "grid", "count_x": 2, "count_y": 3, "dx": 10.0, "dy": 12.0}
            },
            "tool_id": 1,
            "source": {"kind": "all"},
            "params": {
                "depth": -5.0, "start_depth": 0.0, "fast_move_z": 5.0
            }
        });
        let op: Op = serde_json::from_value(raw).expect("Op deserialize");
        let OpKind::Drill { pattern, .. } = &op.kind else {
            panic!("expected Drill kind");
        };
        assert!(matches!(
            pattern,
            Some(PatternConfig::Grid {
                count_x: 2,
                count_y: 3,
                ..
            })
        ));
    }

    #[test]
    fn operation_default_is_an_outside_profile_on_all_geometry() {
        let op = Op::default();
        assert!(matches!(
            op.kind,
            OpKind::Profile {
                offset: ToolOffset::Outside,
                ..
            }
        ));
        assert!(matches!(op.source, OpSource::All));
        assert!(op.enabled);
    }
}
