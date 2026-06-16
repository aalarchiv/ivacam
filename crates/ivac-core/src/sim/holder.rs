//! Sampled radius profile of the non-cutting tool envelope (shank +
//! holder) above the cutting tip. The cutting flutes' XY footprint is
//! handled by `ToolProfile`; everything above the flutes lives here so
//! the deep-pocket / inner-wall collision check can compare the wall
//! Z against the height at which the holder grows past the wall offset.
//!
//! Treatment is cylindrically symmetric: set-screw flats and asymmetric
//! ER nuts get bounded by their enclosing cylinder/cone. Conservative
//! by construction — false negatives (flagging clear cuts) are unlikely
//! while genuine crashes are not missed.

use crate::project::{HolderShape, ToolEntry, ToolKind};

/// Sample list `(z_above_tip_mm, radius_mm)` describing the tool envelope
/// from the cutting tip upward. The list is built from the cutting flute
/// length, the shank diameter, then the holder geometry. `radius_at`
/// linearly interpolates between consecutive samples.
#[derive(Debug, Clone)]
pub struct HolderProfile {
    points: Vec<(f64, f64)>,
}

impl HolderProfile {
    // WHY: T-slot cutters need an extra narrow neck segment
    // between the head (the flutes) and the shank/holder above. The
    // neck is the part that sits *inside* the cut slot while the head
    // rotates: it must be modelled separately so the collision check
    // can tell the neck clears the kerf the head left.
    /// Build a profile from a project tool entry. Returns `None` when
    /// neither a holder nor a shank diameter is set: there's nothing
    /// above the cutting flutes to check against.
    ///
    /// `LaserBeam` tools have no physical body — the "tool" is a
    /// focused beam, not a cutter / shank. Even when the user sets a
    /// shank/holder (because the laser shares a tool table with mill
    /// tools), there's no shaft to drag through tall walls. Return
    /// `None` so the shank-pass / holder-pass / fixture-pass all skip
    /// laser tools entirely.
    #[must_use]
    pub fn from_tool(tool: &ToolEntry) -> Option<Self> {
        if matches!(tool.kind, ToolKind::LaserBeam) {
            return None;
        }
        if tool.holder.is_none() && tool.shank_diameter_mm.is_none() {
            return None;
        }
        // ToolProfile::radius() reports the LARGEST cross-section
        // along the cutter (max of segments for FormProfile, head radius
        // for TSlot, etc). Pulling `cutting_r` from `tool.diameter * 0.5`
        // is fine for cylindrically-uniform cutters but loses the wide-
        // base radius for form bits whose `tool.diameter` reports the
        // *tip* diameter and whose actual envelope grows past the tip
        // along the flutes. Mirror ToolProfile::radius() so the holder
        // check skips the same `r ≤ cutting_r` cells the carve actually
        // touches — otherwise the cells the sweep just lowered get
        // flagged as walls the holder is colliding with (false-positive
        // HolderCollision warnings on form cutters).
        let cutting_r =
            form_profile_max_radius(tool).unwrap_or_else(|| (tool.diameter * 0.5).max(0.0));
        // Drills with no `flute_length_mm` set would otherwise leave the
        // shank starting at z=0 above the tip — so any wall above the
        // tip plane within the shank radius would alarm as a collision.
        // Real twist drills have a flute (helix) running the full body
        // length, typically 5–8× diameter. Default to 6× diameter so
        // the shank-radius envelope only kicks in above the realistic
        // body length. Other kinds keep the zero-default — endmills /
        // V-bits / ballnose normally have flute_length wired in, and a
        // 0 there genuinely means "treat as cutter all the way up".
        let raw_flute = tool.flute_length_mm.unwrap_or(0.0).max(0.0);
        let flute_len = if matches!(tool.kind, ToolKind::Drill) && raw_flute < 1e-6 {
            (tool.diameter * 6.0).max(0.0)
        } else {
            raw_flute
        };
        let shank_r = tool
            .shank_diameter_mm
            .map_or(cutting_r, |d| d * 0.5)
            .max(0.0);

        // Sample list anchored at the tip: bottom of flutes, top of
        // flutes / start of shank, then holder transitions.
        // (The former T-slot neck segment is gone — a folded-in
        // T-slot is a FormProfile whose neck lives in its (z, r) cut
        // profile; this holder/shank model covers the stock above the
        // flutes generically.)
        let mut points: Vec<(f64, f64)> = Vec::with_capacity(8);
        points.push((0.0, cutting_r));
        // Top of cutting flutes — radius is still the cutting radius.
        points.push((flute_len, cutting_r));

        let mut z_cursor = flute_len;

        // Start of shank just above the flutes.
        // We add a separate sample even when shank_r == previous-r so
        // the radius curve has a clear "shank" segment for callers
        // that walk it.
        points.push((z_cursor, shank_r));

        // Explicit shank length (stickout) between top of flutes
        // and bottom of the holder. Defaults to 0 (legacy) so the
        // holder sits on the flutes directly. The shank segment is
        // emitted at `shank_r` from `z_cursor` to `z_cursor + stickout`
        // so callers walking the profile see the full free-shank
        // segment between flutes-top and holder-bottom.
        let stickout = tool.stickout_length_mm.unwrap_or(0.0).max(0.0);
        if stickout > 0.0 {
            z_cursor += stickout;
            points.push((z_cursor, shank_r));
        }

        let mut last_r = shank_r;

        // Holder bottom now sits at `z_cursor` (flute_top + neck +
        // stickout). Old code assumed stickout = 0, which silently
        // pulled the holder envelope down onto the flutes.
        if let Some(holder) = tool.holder {
            match holder {
                HolderShape::Cylinder {
                    diameter_mm,
                    length_mm,
                } => {
                    let r = (diameter_mm * 0.5).max(0.0);
                    let len = length_mm.max(0.0);
                    // Step up to the holder bottom radius, then extend
                    // up by `length_mm` at that same radius.
                    points.push((z_cursor, r));
                    z_cursor += len;
                    points.push((z_cursor, r));
                    last_r = r;
                }
                HolderShape::Cone {
                    bottom_diameter_mm,
                    top_diameter_mm,
                    length_mm,
                } => {
                    let bot_r = (bottom_diameter_mm * 0.5).max(0.0);
                    let top_r = (top_diameter_mm * 0.5).max(0.0);
                    let len = length_mm.max(0.0);
                    points.push((z_cursor, bot_r));
                    z_cursor += len;
                    points.push((z_cursor, top_r));
                    last_r = top_r;
                }
                HolderShape::Stepped {
                    cylinder_diameter_mm,
                    cylinder_length_mm,
                    cone_top_diameter_mm,
                    cone_length_mm,
                } => {
                    let cyl_r = (cylinder_diameter_mm * 0.5).max(0.0);
                    let cone_top_r = (cone_top_diameter_mm * 0.5).max(0.0);
                    let cyl_len = cylinder_length_mm.max(0.0);
                    let cone_len = cone_length_mm.max(0.0);
                    points.push((z_cursor, cyl_r));
                    z_cursor += cyl_len;
                    points.push((z_cursor, cyl_r));
                    z_cursor += cone_len;
                    points.push((z_cursor, cone_top_r));
                    last_r = cone_top_r;
                }
            }
        }
        let _ = last_r;
        Some(Self { points })
    }

    /// Linearly-interpolated tool radius at `z_above_tip` mm above the
    /// cutting tip. Returns `None` once `z_above_tip` is past the top of
    /// the holder.
    ///
    /// # Panics
    ///
    /// Never in practice: the trailing `self.points.last().unwrap()` is
    /// guarded by the early-return on `self.points.is_empty()` at the
    /// top of the function.
    #[must_use]
    pub fn radius_at(&self, z_above_tip: f64) -> Option<f64> {
        if self.points.is_empty() {
            return None;
        }
        if z_above_tip < 0.0 {
            return Some(self.points[0].1);
        }
        // Find the segment [points[i], points[i+1]] containing z_above_tip.
        for w in self.points.windows(2) {
            let (z0, r0) = w[0];
            let (z1, r1) = w[1];
            if z_above_tip >= z0 && z_above_tip <= z1 {
                if (z1 - z0).abs() < 1e-12 {
                    // Coincident-z step: return the larger radius so the
                    // collision check sees the conservative envelope.
                    return Some(r0.max(r1));
                }
                let t = (z_above_tip - z0) / (z1 - z0);
                return Some(r0 + t * (r1 - r0));
            }
        }
        let last = self.points.last().unwrap();
        if z_above_tip <= last.0 {
            return Some(last.1);
        }
        None
    }

    /// Largest radius anywhere along the profile. Cheap fast-reject
    /// bound for the per-cell collision sweep.
    #[must_use]
    pub fn max_radius(&self) -> f64 {
        self.points.iter().map(|p| p.1).fold(0.0_f64, f64::max)
    }

    /// Cutting (flute) radius at the very tip — `points[0].1` by
    /// construction. Used by `holder_check` to distinguish the
    /// cutter envelope (where material *is meant* to be removed) from
    /// the shank/holder envelope above the flutes.
    #[must_use]
    pub fn cutting_radius(&self) -> f64 {
        self.points.first().map_or(0.0, |p| p.1)
    }

    /// Total length of the envelope (tip → top of holder).
    #[must_use]
    pub fn total_length(&self) -> f64 {
        self.points.last().map_or(0.0, |p| p.0)
    }

    /// Read-only access to the underlying samples — used by
    /// `holder_check::check_segment_holder_against_walls` to find the
    /// lowest Z at which the envelope grows past a given radial offset.
    #[must_use]
    pub(crate) fn samples(&self) -> &[(f64, f64)] {
        &self.points
    }

    /// Lowest `z_above_tip` where the envelope radius first
    /// reaches `r`, linearly interpolating across the crossing segment.
    /// `Some(0.0)` for `r <= 0` (the tip covers any non-positive radius);
    /// `None` when the profile never reaches `r`. Walks the sample list
    /// from the tip up. Single owner for what were byte-identical copies
    /// in `holder_check` and `rapid_check`. (The `heightmap::FormProfile`
    /// eval arm is the f32-domain twin of this walk — its `1e-6` epsilon
    /// is the f32-precision analogue of the `1e-12` used here, kept
    /// separate so the per-cell sweep stays in f32.)
    #[must_use]
    pub(crate) fn lowest_z_for_radius(&self, r: f64) -> Option<f64> {
        if r <= 0.0 {
            return Some(0.0);
        }
        let pts = self.samples();
        if pts.is_empty() {
            return None;
        }
        // First point with radius ≥ r: if it's the very first sample the
        // envelope already covers `r` at the tip.
        if pts[0].1 >= r {
            return Some(pts[0].0);
        }
        for w in pts.windows(2) {
            let (z0, r0) = w[0];
            let (z1, r1) = w[1];
            if r1 >= r && r0 < r {
                // Ascending step that crosses r.
                if (r1 - r0).abs() < 1e-12 {
                    return Some(z0.min(z1));
                }
                let t = (r - r0) / (r1 - r0);
                return Some(z0 + t * (z1 - z0));
            }
            if r0 >= r {
                // Already covered at z0.
                return Some(z0);
            }
        }
        None
    }
}

/// Max profile radius for a form cutter, mirroring the same fallback
/// the `ToolProfile::FormProfile` builder uses (`tip_diameter` and diameter,
/// whichever is larger, capped at zero). Returns `None` for non-form tools
/// so the regular `tool.diameter * 0.5` path keeps owning those cases.
fn form_profile_max_radius(tool: &ToolEntry) -> Option<f64> {
    if !matches!(tool.kind, ToolKind::FormProfile) {
        return None;
    }
    let base_r = (tool.diameter * 0.5).max(0.0);
    let tip_r = tool.tip_diameter.map_or(base_r, |d| (d * 0.5).max(0.0));
    // When a real profile is entered, the widest sample radius is
    // the footprint the holder check must clear (the same value
    // `ToolProfile::FormProfile` reports via `radius()`).
    let sample_max = tool
        .form_profile_mm
        .iter()
        .map(|s| s.r_mm.max(0.0))
        .fold(0.0_f64, f64::max);
    Some(base_r.max(tip_r).max(sample_max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Coolant, HolderShape, ToolEntry, ToolKind};

    fn tool_with(holder: Option<HolderShape>, shank: Option<f64>, flute: Option<f64>) -> ToolEntry {
        ToolEntry {
            id: 1,
            name: "t".into(),
            kind: ToolKind::Endmill,
            diameter: 6.0,
            tip_diameter: None,
            tip_angle_deg: 60.0,
            dragoff: None,
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
            form_profile_mm: Vec::new(),
            whirl: false,
            whirl_stepover_mm: None,
            whirl_extra_width_mm: None,
            whirl_osc_mm: None,
            pause: 1,
            flute_length_mm: flute,
            length_mm: None,
            compression_transition_mm: None,
            thread_pitch_mm: None,
            shank_diameter_mm: shank,
            stickout_length_mm: None,
            holder,
            spindle_direction: crate::project::SpindleDirection::default(),
            drag_knife_self_align_angle_deg: None,
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            wear_offset_mm: 0.0,
            last_calibrated: None,
            vcarve_lead_in_angle_deg: None,
        }
    }

    #[test]
    fn profile_from_tool_cylinder() {
        // 6 mm endmill with 6 mm shank and a 20 mm cylinder holder.
        let t = tool_with(
            Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            }),
            Some(6.0),
            Some(15.0),
        );
        let p = HolderProfile::from_tool(&t).expect("holder set");
        assert!((p.max_radius() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn profile_radius_at_interpolates() {
        // Stepped holder: 20 mm dia × 10 mm cyl, then cone tapering up to
        // 30 mm dia over 20 mm.
        let t = tool_with(
            Some(HolderShape::Stepped {
                cylinder_diameter_mm: 20.0,
                cylinder_length_mm: 10.0,
                cone_top_diameter_mm: 30.0,
                cone_length_mm: 20.0,
            }),
            Some(6.0),
            Some(15.0),
        );
        let p = HolderProfile::from_tool(&t).expect("holder set");
        // Cylinder/cone transition is at flute_len + cyl_len = 25 mm
        // above the tip and the radius there is exactly the cylinder
        // radius (10 mm).
        let r_at_transition = p.radius_at(25.0).expect("inside profile");
        assert!(
            (r_at_transition - 10.0).abs() < 1e-9,
            "expected 10, got {r_at_transition}",
        );
        // Halfway up the cone (z = 25 + 10 = 35) the radius is the
        // linear interp between bottom (10) and top (15) = 12.5.
        let r_mid_cone = p.radius_at(35.0).expect("inside profile");
        assert!(
            (r_mid_cone - 12.5).abs() < 1e-9,
            "expected 12.5, got {r_mid_cone}",
        );
        // Above the holder top (45 mm) the radius is undefined.
        assert!(p.radius_at(60.0).is_none());
    }

    #[test]
    fn from_tool_none_when_no_holder_or_shank() {
        let t = tool_with(None, None, Some(15.0));
        assert!(HolderProfile::from_tool(&t).is_none());
    }

    #[test]
    fn from_tool_some_when_only_shank_set() {
        let t = tool_with(None, Some(6.0), Some(15.0));
        let p = HolderProfile::from_tool(&t).expect("shank-only profile is valid");
        // Without an explicit holder the envelope tops out at the shank.
        assert!((p.max_radius() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn stickout_pushes_holder_up_above_flutes() {
        // A 6 mm endmill with 15 mm flutes + 20 mm stickout +
        // 30 mm cylinder holder. Without stickout the holder bottom
        // sat at z=15; with stickout=20 it now sits at z=35.
        let mut t = tool_with(
            Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            }),
            Some(6.0),
            Some(15.0),
        );
        t.stickout_length_mm = Some(20.0);
        let p = HolderProfile::from_tool(&t).expect("holder set");
        // Top of profile: flute_len (15) + stickout (20) + holder (30) = 65.
        assert!(
            (p.total_length() - 65.0).abs() < 1e-9,
            "expected total length 65, got {}",
            p.total_length()
        );
        // At z = 30 (10 mm above flutes-top, 10 mm into the stickout
        // segment) the envelope is just the shank radius (3 mm), NOT
        // the holder radius. Pre-fix it was already inside the holder
        // here — that's the silent bug.
        let r = p.radius_at(30.0).expect("inside profile");
        assert!(
            (r - 3.0).abs() < 1e-9,
            "10 mm into stickout should be shank radius 3, got {r}",
        );
        // At z = 40 (5 mm into the holder cylinder) the radius is 10.
        let r = p.radius_at(40.0).expect("inside profile");
        assert!(
            (r - 10.0).abs() < 1e-9,
            "5 mm into holder should be holder radius 10, got {r}",
        );
    }

    #[test]
    fn no_stickout_field_is_legacy_zero() {
        // Back-compat: a tool with `stickout_length_mm = None`
        // produces the same envelope as before — holder right above
        // the flutes.
        let t = tool_with(
            Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            }),
            Some(6.0),
            Some(15.0),
        );
        let p = HolderProfile::from_tool(&t).expect("holder set");
        // total = flute (15) + holder (30) = 45 (no stickout).
        assert!((p.total_length() - 45.0).abs() < 1e-9);
    }

    /// A form cutter whose `tool.diameter` advertises the *tip*
    /// diameter but whose actual envelope grows past the tip along the
    /// flute (large-base form bit) must not report
    /// `cutting_radius() = tip_diameter * 0.5`. The carve sweep, on the
    /// other hand, uses `ToolProfile::radius()` = max of segments. Such a
    /// mismatch would flag carved cells `tip_r < r ≤ max_r` as wall
    /// collisions in the holder check. The holder's cutting envelope
    /// mirrors the carve envelope (the max profile radius) so
    /// `holder_check` skips the same cells the sweep just lowered.
    #[test]
    fn form_profile_cutting_radius_matches_max_segment_radius() {
        // 2 mm tip, 8 mm base form bit. Without the fix, cutting_r = 1
        // and the holder would see the 8 mm-wide bottom of the swept
        // path as "walls".
        let mut t = tool_with(
            Some(HolderShape::Cylinder {
                diameter_mm: 12.0,
                length_mm: 30.0,
            }),
            Some(6.0),
            Some(10.0),
        );
        t.kind = ToolKind::FormProfile;
        t.diameter = 8.0;
        t.tip_diameter = Some(2.0);
        let p = HolderProfile::from_tool(&t).expect("holder set");
        assert!(
            (p.cutting_radius() - 4.0).abs() < 1e-9,
            "form cutter cutting_r should mirror max profile radius (4 mm), got {}",
            p.cutting_radius(),
        );
        // And the opposite case: a form cutter with the TIP wider than
        // the base (truncated cone) — the larger of the two still wins.
        let mut t_inv = tool_with(
            Some(HolderShape::Cylinder {
                diameter_mm: 12.0,
                length_mm: 30.0,
            }),
            Some(6.0),
            Some(10.0),
        );
        t_inv.kind = ToolKind::FormProfile;
        t_inv.diameter = 2.0;
        t_inv.tip_diameter = Some(8.0);
        let p_inv = HolderProfile::from_tool(&t_inv).expect("holder set");
        assert!(
            (p_inv.cutting_radius() - 4.0).abs() < 1e-9,
            "form cutter cutting_r should be max(tip,base), got {}",
            p_inv.cutting_radius(),
        );
        // Non-form tools keep the old behaviour (cutting_r from diameter).
        let endmill = tool_with(
            Some(HolderShape::Cylinder {
                diameter_mm: 12.0,
                length_mm: 30.0,
            }),
            Some(6.0),
            Some(10.0),
        );
        let p_em = HolderProfile::from_tool(&endmill).expect("holder set");
        assert!((p_em.cutting_radius() - 3.0).abs() < 1e-9);
    }
}
