//! Tool library entries — bit geometry, feed/speed defaults, holder
//! geometry, and the per-pass rate-resolution helper.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// 1wit: one cross-section sample of a form / profile cutter outline,
/// measured up from the cutting tip. The cutter is treated as
/// cylindrically symmetric, so a sorted list of these describes the
/// full profile (cove / ogee / dovetail / custom). See
/// [`ToolEntry::form_profile_mm`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormProfileSample {
    /// Height above the cutting tip (mm). 0 is the bottom face; the
    /// list runs tip → top.
    pub z_mm: f64,
    /// Cutter radius at this height (mm), `diameter / 2` at the widest
    /// point.
    pub r_mm: f64,
}

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
    /// 0t9o: drag-knife self-alignment threshold in degrees. Corners
    /// whose tangent change is smaller than this angle skip the
    /// explicit swivel arc + linear pre-move — real drag knives
    /// self-align below ~30° via the trailing offset, so emitting a
    /// swivel for every chord-of-a-circle pivot bloats output and
    /// stresses the blade pivot. Honored only when `dragoff` is also
    /// set. `None` ⇒ 30° default; `Some(0.0)` forces the legacy
    /// "swivel every corner" behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drag_knife_self_align_angle_deg: Option<f64>,
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
    /// Default XY overlap (0..1) for pocket / cascade ops that don't set
    /// their own [`crate::project::PocketParams::xy_overlap`]. Mirrors
    /// the `default_step` pattern (dr5). None = fall through to the
    /// global 0.5 default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_xy_overlap: Option<f64>,
    /// Free-text comment / description (rt1.31). Surfaced as the
    /// tooltip on the tool dropdown in `OpPropertiesPanel` and as an
    /// expandable text area in `ToolLibraryDialog`. Empty / None = no
    /// comment; doesn't affect any pipeline output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Per-tool Z origin offset (rt1.30 / Estlcam `Z_Shift`). For
    /// machines without automatic tool-length probing — the user
    /// pre-measures each tool's tip Z relative to a reference tool and
    /// records the delta here (positive = sticks out further; negative
    /// = shorter). At toolchange / program-start the post emits a
    /// `G92 Z<shift>` that pins the new tool's tip at the same work-Z
    /// the reference tool used. mm. None = no shift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_shift_mm: Option<f64>,
    /// Laser pierce time (rt1.29 / Estlcam `T_Pierce_Time)`: seconds the
    /// beam dwells at the start point BEFORE the cut begins so it
    /// burns through thick stock. Honored only when `kind ==
    /// LaserBeam`. The post emits a `G4 P<seconds>` after the
    /// laser-on before each plunge. None = no pierce dwell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub laser_pierce_sec: Option<f64>,
    /// mmu8: laser kerf width (mm) — the heightmap-side spot radius the
    /// sim carves at. Honored only when `kind == LaserBeam`. Lets the
    /// preview show actual cut width for fine-engraving (0.05 mm
    /// fiber laser) vs. aggressive-cut (0.4 mm CO2) tools instead of
    /// a uniform 0.15 mm stand-in. None = the legacy 0.15 mm default
    /// (old projects round-trip unchanged). The sim floors the
    /// effective radius at 0.05 mm so a zero / missing value still
    /// registers some carve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kerf_mm: Option<f64>,
    /// Laser lead-in distance (rt1.29 / Estlcam `T_Lead_In)`: mm of
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
    /// 1wit: form / profile cutter cross-section, tip → top. Each
    /// sample is `(z_above_tip_mm, radius_mm)`; the sim carves at the
    /// interpolated radius for each Z slice. Honored only when
    /// `kind == FormProfile` and at least two samples are present —
    /// otherwise the sim falls back to a `(tip_diameter, diameter)`
    /// 2-segment taper. The tool-library UI generates these from a
    /// dovetail (angle / tip ⌀ / cut height) preset or accepts raw
    /// rows for cove / ogee / custom bits. Empty for every other kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub form_profile_mm: Vec<FormProfileSample>,
    /// Wirbeln (rt1.25 / Estlcam `T_Wirbeln)`: automatic chip-thinning.
    /// When `true`, Pocket ops using this tool clamp their effective
    /// `xy_step` down to `wirbeln_stepover_mm.unwrap_or(tool_radius / 2)`
    /// — the classic chip-thinning rule that bounds radial engagement
    /// at half-radius. Use for hard materials where the user wants
    /// fast cascade / spiral pockets but doesn't want the cutter to
    /// overload at high-engagement points. Default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wirbeln: bool,
    /// Wirbeln stepover override (rt1.25). When `wirbeln` is `true`,
    /// this is the **stride along the toolpath per full revolution of
    /// the spiral overlay** — Estlcam's `T_Wirbel_Stepover`. mm,
    /// positive only. None = use the half-radius default. (3e5 made
    /// this the spiral stride; before 3e5 it was the cascade-step
    /// clamp, which was the "fake Wirbeln" v1 implementation.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wirbeln_stepover_mm: Option<f64>,
    /// Wirbeln extra-width (Estlcam `T_Wirbelzusatzbreite` /
    /// rt1.25 / 3e5). The *diameter* in mm by which the helical
    /// overlay widens the effective cut path: the cutter centerline
    /// scrolls on a small circle of radius `wirbeln_extra_width_mm /
    /// 2` around the cascade ring. Net cut width is
    /// `diameter + wirbeln_extra_width_mm`. None / 0 ⇒ overlay
    /// disabled even when `wirbeln == true` (which then falls back
    /// to a no-op — the v1 step clamp is gone in 3e5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wirbeln_extra_width_mm: Option<f64>,
    /// Wirbeln Z-wobble amplitude (Estlcam `T_Osc`, 3e5). When > 0,
    /// the spiral overlay adds a `cos(3·θ) · osc − osc` Z ripple so
    /// the cutter dips slightly below the cut plane between
    /// revolutions — improves chip evacuation on the wobbly cutters
    /// the feature targets. mm, positive only. None / 0 ⇒ flat
    /// (no Z motion added by the overlay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wirbeln_osc_mm: Option<f64>,
    /// Spindle warm-up pause in seconds applied once per used tool by
    /// the time estimator. Mirrors `ToolConfig.pause`.
    #[serde(
        default = "default_tool_pause",
        skip_serializing_if = "is_default_tool_pause"
    )]
    pub pause: u32,
    /// z1y0: spindle direction the post should command when this tool
    /// is selected. Most cutters are right-hand and want `Cw` (M3);
    /// left-hand cutters, reverse-threading, and a few specialty
    /// holders want `Ccw` (M4). Defaults to `Cw` so legacy projects
    /// round-trip unchanged. The default is skipped on serialize so
    /// the JSON stays small.
    #[serde(
        default,
        skip_serializing_if = "is_default_spindle_direction"
    )]
    pub spindle_direction: SpindleDirection,
    /// Length of cutting flutes (mm). None = treat entire tool as cutting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flute_length_mm: Option<f64>,
    /// Shank diameter (mm). None = same as `diameter` (parallel-shank bit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shank_diameter_mm: Option<f64>,
    /// q0kc: free shank length between the top of the cutting flutes and
    /// the bottom of the holder/collet (mm). Models the real-world case
    /// where the collet doesn't grip right above the flutes — common for
    /// reach-extension tooling. Defaults to 0 (legacy behavior — collet
    /// sits directly on the flutes), so old projects round-trip
    /// unchanged. None = same as `Some(0.0)` for the sim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stickout_length_mm: Option<f64>,
    /// Holder geometry above the shank. None = no holder check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<HolderShape>,
    /// zpuk: plasma pierce height (mm above stock). Honored only when
    /// the active machine mode is `Plasma`. The pierce arc is
    /// established at this height — too close and the torch sticks
    /// to the stock as it slags up; too far and the arc misses or
    /// drops out. Typical 3–5 mm for 1–3 mm steel. None ⇒ 3.8 mm
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pierce_height_mm: Option<f64>,
    /// zpuk: plasma cut height (mm above stock, generally < pierce
    /// height). After the pierce dwell the torch drops to this
    /// height for the actual cut. Typical 1.5–2.5 mm for thin steel.
    /// None ⇒ 1.5 mm default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_height_mm: Option<f64>,
    /// zpuk: plasma pierce delay in seconds. The torch dwells at
    /// `pierce_height_mm` for this many seconds before dropping to
    /// `cut_height_mm`. Long enough to pierce the stock; too long
    /// and the arc starts to undercut the rim of the pierce hole.
    /// Typical 0.4 s for 1 mm steel, up to ~1.5 s for 6 mm. None
    /// ⇒ 0.5 s default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pierce_delay_sec: Option<f64>,
    /// ot80: V-Carve / Stufenfase lead-in ramp angle, degrees from
    /// horizontal. Controls how steeply the cutter walks into the
    /// material at the start of each cut to avoid a vertical plunge
    /// at the R≈0 medial-axis endpoint (V-bits have effectively zero
    /// safe plunge depth). pmpk originally hardcoded this to 10°
    /// (Vectric / Estlcam default) inside
    /// [`crate::cam::vcarve_emit::ratchet_emit`]; this field lets
    /// shops dial it per-tool — harder materials want shallower
    /// (5–8°), softer materials tolerate steeper (15°+). Values
    /// outside (0°, 90°) are clamped at synth time. `None` ⇒ inherit
    /// the legacy 10° default so old projects round-trip unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcarve_lead_in_angle_deg: Option<f64>,
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

/// z1y0: per-tool spindle direction — right-hand (`Cw`, M3) for the
/// 99% of cutters, left-hand (`Ccw`, M4) for reverse-thread / mirror
/// /-helix tooling. Mirrored into `ToolConfig.spindle_direction` at
/// synth time so the gcode emitter can route between `spindle_cw`
/// and `spindle_ccw` without reaching back into the tool library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SpindleDirection {
    #[default]
    Cw,
    Ccw,
}

fn is_default_spindle_direction(d: &SpindleDirection) -> bool {
    matches!(d, SpindleDirection::Cw)
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
            drag_knife_self_align_angle_deg: None,
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
            default_xy_overlap: None,
            comment: None,
            z_shift_mm: None,
            laser_pierce_sec: None,
            laser_lead_in_mm: None,
            kerf_mm: None,
            corner_radius_mm: None,
            tslot_neck_diameter_mm: None,
            tslot_neck_length_mm: None,
            form_profile_mm: Vec::new(),
            wirbeln: false,
            wirbeln_stepover_mm: None,
            wirbeln_extra_width_mm: None,
            wirbeln_osc_mm: None,
            pause: default_tool_pause(),
            spindle_direction: SpindleDirection::default(),
            // pierce_height_mm / cut_height_mm / pierce_delay_sec — zpuk:
            // None ⇒ emission code falls back to plasma defaults
            // (3.8 / 1.5 / 0.5) at cut time. Listing them here keeps
            // the struct literal exhaustive even though Default for
            // ToolEntry is rarely the source of a plasma config.
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            flute_length_mm: None,
            shank_diameter_mm: None,
            stickout_length_mm: None,
            holder: None,
            vcarve_lead_in_angle_deg: None,
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

/// Resolve the (speed, `plunge_rate`, `feed_rate`) triplet for `tool` under
/// `pass`. Finish / Drill variants fall back to the general values when
/// their override is `None`.
#[must_use]
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

pub(crate) fn default_tip_angle_deg() -> f64 {
    60.0
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let tool = ToolEntry {
                flute_length_mm: Some(15.0),
                shank_diameter_mm: Some(6.0),
                holder: Some(s),
                ..ToolEntry::default()
            };
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
