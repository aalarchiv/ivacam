//! Op model — the per-operation work unit consumed by the pipeline.
//! Carries an [`OpKind`] discriminator, the source-geometry selector
//! [`OpSource`], and a parameter bag [`super::params::OpParams`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::setup::ToolOffset;

use super::params::OpParams;

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
    /// Optional pattern repetition. When set, the op runs once per
    /// pattern instance with the source geometry translated/rotated.
    /// See [`PatternConfig`] for the concrete pattern shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<PatternConfig>,
}

impl Default for Op {
    fn default() -> Self {
        Self {
            id: 1,
            name: "Profile".into(),
            enabled: true,
            kind: OpKind::Profile {
                offset: ToolOffset::Outside,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::default(),
            pattern: None,
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum OpKind {
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
    Zigzag,
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

    #[test]
    fn operation_default_is_an_outside_profile_on_all_geometry() {
        let op = Op::default();
        assert!(matches!(
            op.kind,
            OpKind::Profile {
                offset: ToolOffset::Outside
            }
        ));
        assert!(matches!(op.source, OpSource::All));
        assert!(op.enabled);
    }
}
