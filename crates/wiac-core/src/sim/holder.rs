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
    // WHY: 3oly — T-slot cutters need an extra narrow neck segment
    // between the head (the flutes) and the shank/holder above. The
    // neck is the part that sits *inside* the cut slot while the head
    // rotates: it must be modelled separately so the collision check
    // can tell the neck clears the kerf the head left.
    /// Build a profile from a project tool entry. Returns `None` when
    /// neither a holder nor a shank diameter is set: there's nothing
    /// above the cutting flutes to check against.
    #[must_use]
    pub fn from_tool(tool: &ToolEntry) -> Option<Self> {
        if tool.holder.is_none() && tool.shank_diameter_mm.is_none() {
            return None;
        }
        let cutting_r = (tool.diameter * 0.5).max(0.0);
        let flute_len = tool.flute_length_mm.unwrap_or(0.0).max(0.0);
        let shank_r = tool
            .shank_diameter_mm
            .map_or(cutting_r, |d| d * 0.5)
            .max(0.0);

        // 3oly: T-slot neck — sits between the head and the shank.
        // The head's cutting radius is the full `cutting_r`; the neck
        // above is narrower so it clears the slot the head cut. We
        // only emit the neck segment when both pieces are set AND the
        // neck is genuinely narrower than the head (otherwise it's
        // just a regular endmill that happens to have the kind flag).
        let tslot_neck = if matches!(tool.kind, ToolKind::TSlot) {
            match (tool.tslot_neck_diameter_mm, tool.tslot_neck_length_mm) {
                (Some(d), Some(l)) if d > 0.0 && l > 0.0 => {
                    let nr = (d * 0.5).max(0.0);
                    if nr < cutting_r {
                        Some((nr, l.max(0.0)))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        } else {
            None
        };

        // Sample list anchored at the tip: bottom of flutes, top of
        // flutes / start of shank, then holder transitions.
        let mut points: Vec<(f64, f64)> = Vec::with_capacity(8);
        points.push((0.0, cutting_r));
        // Top of cutting flutes — radius is still the cutting radius.
        points.push((flute_len, cutting_r));

        // 3oly: insert the T-slot neck segment between the head top
        // and the shank — at flute_len the cutter drops from
        // cutting_r down to neck_r, then runs at neck_r for
        // neck_length, then transitions up to shank_r.
        let mut z_cursor = flute_len;
        if let Some((neck_r, neck_len)) = tslot_neck {
            // Step down to the neck at the head's top.
            points.push((z_cursor, neck_r));
            z_cursor += neck_len;
            // Top of neck.
            points.push((z_cursor, neck_r));
        }

        // Start of shank just above the flutes (or neck for T-slot).
        // We add a separate sample even when shank_r == previous-r so
        // the radius curve has a clear "shank" segment for callers
        // that walk it.
        points.push((z_cursor, shank_r));

        // q0kc: explicit shank length (stickout) between top of flutes
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
        // pulled the holder envelope down onto the flutes — see q0kc.
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
    /// construction. Used by `holder_check` (hrex) to distinguish the
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
            corner_radius_mm: None,
            tslot_neck_diameter_mm: None,
            tslot_neck_length_mm: None,
            wirbeln: false,
            wirbeln_stepover_mm: None,
            wirbeln_extra_width_mm: None,
            wirbeln_osc_mm: None,
            pause: 1,
            flute_length_mm: flute,
            shank_diameter_mm: shank,
            stickout_length_mm: None,
            holder,
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
        // q0kc: a 6 mm endmill with 15 mm flutes + 20 mm stickout +
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
        // q0kc back-compat: a tool with `stickout_length_mm = None`
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

    fn tslot_tool(
        head_diameter: f64,
        head_thickness: f64,
        neck_diameter: f64,
        neck_length: f64,
        shank_diameter: f64,
        holder: Option<HolderShape>,
    ) -> ToolEntry {
        let mut t = tool_with(holder, Some(shank_diameter), Some(head_thickness));
        t.diameter = head_diameter;
        t.kind = ToolKind::TSlot;
        t.tslot_neck_diameter_mm = Some(neck_diameter);
        t.tslot_neck_length_mm = Some(neck_length);
        t
    }

    #[test]
    fn tslot_neck_is_encoded_above_head() {
        // 3oly: a T-slot cutter with a 16 mm head (8 mm radius), 4 mm
        // thick, sitting on a 4 mm neck (2 mm radius) × 10 mm long,
        // then a 6 mm shank (3 mm radius), and a 20 mm cylinder
        // holder × 25 mm. Without an explicit shank-length field on
        // ToolEntry, the shank sits at z=14 with zero length — the
        // holder takes over immediately.
        let t = tslot_tool(
            16.0,
            4.0,
            4.0,
            10.0,
            6.0,
            Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 25.0,
            }),
        );
        let p = HolderProfile::from_tool(&t).expect("holder set");
        // Cutting (head) radius at the very tip.
        assert!((p.cutting_radius() - 8.0).abs() < 1e-9);
        // At z = 2 (inside the head), still 8 mm.
        assert!((p.radius_at(2.0).unwrap() - 8.0).abs() < 1e-9);
        // At z = 5 (just above the head, inside the neck), narrower
        // to 2 mm. This is the key invariant: above head_z_top the
        // envelope is the NECK radius, NOT the head radius.
        assert!(
            (p.radius_at(5.0).unwrap() - 2.0).abs() < 1e-9,
            "neck radius 5 mm above tip should be 2, got {}",
            p.radius_at(5.0).unwrap()
        );
        // At z = 13 (still inside the neck), still 2 mm.
        assert!((p.radius_at(13.0).unwrap() - 2.0).abs() < 1e-9);
        // Just above the neck (z = 14.5) the cylinder holder has
        // already taken over (no shank length) → radius 10.
        let r_above = p.radius_at(14.5).unwrap();
        assert!(
            (r_above - 10.0).abs() < 1e-9,
            "above neck the holder dominates (no shank length), got {r_above}",
        );
        // Total length = head_thickness (4) + neck (10) + holder (25)
        // = 39.
        let total = p.total_length();
        assert!(
            (total - 39.0).abs() < 1e-9,
            "total length 4+10+25 = 39, got {total}",
        );
        // Max radius = the holder cylinder radius (10 mm).
        assert!((p.max_radius() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn tslot_neck_clears_kerf_above_head() {
        // 3oly acceptance test: an undercut slot whose kerf is the
        // head width (16 mm), neck is narrower (4 mm). Above the
        // head_top a *kerf-wide* wall would block an Endmill-equivalent
        // model — but the neck is half that radius, so the holder
        // check sees clearance.
        use crate::gcode::preview::{MoveKind, Pose3, ToolpathSegment};
        use crate::sim::heightmap::Heightmap;
        use crate::sim::holder_check::{check_segment_holder_against_walls, HolderCheck};

        let t = tslot_tool(16.0, 4.0, 4.0, 10.0, 6.0, None);
        let holder = HolderProfile::from_tool(&t).expect("shank set");
        // Build a heightmap where the slot is already cut to z = -4
        // (head thickness) in a 16 mm-wide channel. ABOVE the head
        // (between z = -4 and z = 0) the kerf is the same 16 mm wide
        // (because the head cut it). Walls *beyond* the kerf are at
        // top_z = 0 (uncut).
        let mut hm = Heightmap::new(crate::geometry::Point2::new(0.0, 0.0), 1.0, 60, 60, 0.0);
        // Channel along Y=30, half-width 8 (16 mm wide), depth -4.
        for iy in 0..60 {
            let cy = f64::from(iy) + 0.5;
            if (cy - 30.0).abs() <= 8.0 {
                for ix in 0..60 {
                    hm.lower_at(ix, iy, -4.0);
                }
            }
        }
        // Cut along the channel with the head fully engaged. Tip at
        // z=-4 means the head bottom is at -4 and the head top is at
        // z=0 — flush with the un-cut surface. Walls (at z=0) are 8 mm
        // from the path centerline at iy = {22, 38}. The neck radius
        // is 2 mm — far less than 8 mm. Holder should NOT collide.
        let s = ToolpathSegment {
            from: Pose3 {
                x: 5.0,
                y: 30.0,
                z: -4.0,
            },
            to: Pose3 {
                x: 55.0,
                y: 30.0,
                z: -4.0,
            },
            kind: MoveKind::Cut,
            gcode_line: 0,
            op_id: 0,
        };
        let res = check_segment_holder_against_walls(&hm, &s, &holder);
        assert_eq!(
            res,
            HolderCheck::Clear,
            "neck (r=2) must clear 16 mm-wide kerf, got {res:?}",
        );
    }
}
