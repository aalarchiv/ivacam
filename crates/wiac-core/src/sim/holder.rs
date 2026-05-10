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

use crate::project::{HolderShape, ToolEntry};

/// Sample list `(z_above_tip_mm, radius_mm)` describing the tool envelope
/// from the cutting tip upward. The list is built from the cutting flute
/// length, the shank diameter, then the holder geometry. `radius_at`
/// linearly interpolates between consecutive samples.
#[derive(Debug, Clone)]
pub struct HolderProfile {
    points: Vec<(f64, f64)>,
}

impl HolderProfile {
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
            .map(|d| d * 0.5)
            .unwrap_or(cutting_r)
            .max(0.0);

        // Sample list anchored at the tip: bottom of flutes, top of
        // flutes / start of shank, then holder transitions.
        let mut points: Vec<(f64, f64)> = Vec::with_capacity(6);
        points.push((0.0, cutting_r));
        // Top of cutting flutes — radius is still the cutting radius.
        points.push((flute_len, cutting_r));
        // Start of shank just above the flutes. We add a separate sample
        // even when shank_r == cutting_r so the radius curve has a clear
        // "shank" segment for callers that walk it.
        points.push((flute_len, shank_r));

        let mut z_cursor = flute_len;
        let mut last_r = shank_r;

        // Some holder shapes describe a "shank length" implicitly by
        // their distance from the cutting tip — we model the holder as
        // sitting directly on top of the flutes (no explicit shank
        // length on the tool entry today). The shank radius is the gap
        // between flutes-top and holder-bottom; even if `flute_length`
        // was unspecified the bottom-of-holder still lands at z = 0.
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
        self.points
            .iter()
            .map(|p| p.1)
            .fold(0.0_f64, f64::max)
    }

    /// Total length of the envelope (tip → top of holder).
    #[must_use]
    pub fn total_length(&self) -> f64 {
        self.points.last().map(|p| p.0).unwrap_or(0.0)
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
            default_step: None,
            pause: 1,
            flute_length_mm: flute,
            shank_diameter_mm: shank,
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
}
