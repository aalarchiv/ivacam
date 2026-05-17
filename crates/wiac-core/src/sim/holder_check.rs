//! Per-segment holder-vs-wall collision check. The cutting flutes can
//! clear a deep narrow pocket while the shank or holder above them
//! still slams into the un-cut wall sticking up around the flutes. This
//! pass walks the segment's swept XY footprint inflated by the holder's
//! max radius, and per cell asks: is the wall standing at this cell
//! taller than the height at which the holder envelope grows past the
//! cell's radial offset from the cutter axis?
//!
//! Signs / frames:
//! * `cutter_pz_at_t` — Z of the cutting tip along the segment.
//! * Cell stores `cell_z` — the lowest Z the heightmap reached at that
//!   cell. `cell_z > cutter_pz_at_t` means the wall there is taller
//!   than the tip is deep, i.e. there's `cell_z - cutter_pz_at_t` mm of
//!   wall above the tip.
//! * Holder envelope at radial offset `r`: lowest `z_above_tip` where
//!   `radius_at(z_above_tip) >= r`. If that height is *less* than the
//!   wall height above the tip, the holder hits — and the required
//!   clearance is `wall_height - holder_lower_z`.

// # CAM/sim pedantic-lint exemptions
// Holder collision math uses `from`/`to`/`cx`/`cy` segment-projection names;
// renaming loses the projection-onto-segment intent.
#![allow(clippy::similar_names)]

use crate::gcode::preview::ToolpathSegment;
use crate::sim::heightmap::Heightmap;
use crate::sim::holder::HolderProfile;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HolderCheck {
    Clear,
    Collision {
        worst_x: f64,
        worst_y: f64,
        wall_z: f32,
        required_clearance_mm: f32,
    },
}

#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::cast_possible_wrap
)]
pub fn check_segment_holder_against_walls(
    heightmap: &Heightmap,
    segment: &ToolpathSegment,
    holder: &HolderProfile,
) -> HolderCheck {
    let max_r = holder.max_radius();
    if max_r <= 0.0 {
        return HolderCheck::Clear;
    }

    let from = &segment.from;
    let to = &segment.to;

    // Skip moves that stay above the un-cut stock — the holder is
    // outside the material on every cell along the way.
    let top_z = heightmap.top_z as f64;
    if from.z >= top_z && to.z >= top_z {
        return HolderCheck::Clear;
    }

    let cell = heightmap.cell;
    let inv_cell = 1.0 / cell;
    let max_col = heightmap.cols.saturating_sub(1);
    let max_row = heightmap.rows.saturating_sub(1);

    // AABB of the segment in XY, inflated by the holder's max radius.
    let min_x = from.x.min(to.x) - max_r;
    let max_x = from.x.max(to.x) + max_r;
    let min_y = from.y.min(to.y) - max_r;
    let max_y = from.y.max(to.y) + max_r;

    let fx0 = (min_x - heightmap.origin.x) * inv_cell;
    let fy0 = (min_y - heightmap.origin.y) * inv_cell;
    let fx1 = (max_x - heightmap.origin.x) * inv_cell;
    let fy1 = (max_y - heightmap.origin.y) * inv_cell;
    if fx1 < 0.0 || fy1 < 0.0 {
        return HolderCheck::Clear;
    }
    if fx0 > heightmap.cols as f64 || fy0 > heightmap.rows as f64 {
        return HolderCheck::Clear;
    }
    let ix0 = fx0.floor().max(0.0) as u32;
    let iy0 = fy0.floor().max(0.0) as u32;
    let ix1 = (fx1.floor().max(0.0) as u32).min(max_col);
    let iy1 = (fy1.floor().max(0.0) as u32).min(max_row);
    if ix0 > ix1 || iy0 > iy1 {
        return HolderCheck::Clear;
    }

    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let len_sq = dx * dx + dy * dy;
    let pure_plunge = len_sq < 1e-12;
    let plunge_z = from.z.min(to.z);
    let max_r_sq = max_r * max_r;
    let cols = heightmap.cols as usize;

    let mut worst: Option<(f32, u32, u32, f32)> = None;

    for iy in iy0..=iy1 {
        for ix in ix0..=ix1 {
            let cx = heightmap.origin.x + (ix as f64 + 0.5) * cell;
            let cy = heightmap.origin.y + (iy as f64 + 0.5) * cell;
            let (r_sq, cutter_pz) = if pure_plunge {
                let ex = cx - from.x;
                let ey = cy - from.y;
                (ex * ex + ey * ey, plunge_z)
            } else {
                let t = (((cx - from.x) * dx + (cy - from.y) * dy) / len_sq).clamp(0.0, 1.0);
                let px = from.x + t * dx;
                let py = from.y + t * dy;
                let ex = cx - px;
                let ey = cy - py;
                (ex * ex + ey * ey, from.z + (to.z - from.z) * t)
            };
            if r_sq > max_r_sq {
                continue;
            }
            let r = r_sq.sqrt();
            // Lowest height above the tip at which the envelope grows
            // past `r`. None when `r > max_radius`, but we already
            // filtered that case via `max_r_sq`.
            let Some(holder_lower_z) = lowest_z_for_radius(holder, r) else {
                continue;
            };
            let cell_z = heightmap.data[(iy as usize) * cols + ix as usize];
            let wall_height = cell_z as f64 - cutter_pz;
            // Wall has to actually exist above the tip for the holder to
            // care; if `wall_height <= holder_lower_z` the holder is
            // already wider than the wall at that height — clear.
            if wall_height <= holder_lower_z {
                continue;
            }
            let required = (wall_height - holder_lower_z) as f32;
            match worst {
                Some((best, _, _, _)) if required <= best => {}
                _ => worst = Some((required, ix, iy, cell_z)),
            }
        }
    }

    match worst {
        None => HolderCheck::Clear,
        Some((required, ix, iy, wall_z)) => {
            let worst_x = heightmap.origin.x + (ix as f64 + 0.5) * cell;
            let worst_y = heightmap.origin.y + (iy as f64 + 0.5) * cell;
            HolderCheck::Collision {
                worst_x,
                worst_y,
                wall_z,
                required_clearance_mm: required,
            }
        }
    }
}

/// Lowest `z_above_tip` where `radius_at(z) >= r`. Walks the sample list
/// from the tip up looking for the first segment whose radius range
/// contains `r`; linearly interpolates inside that segment.
#[must_use]
fn lowest_z_for_radius(holder: &HolderProfile, r: f64) -> Option<f64> {
    if r <= 0.0 {
        return Some(0.0);
    }
    let pts = holder.samples();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcode::preview::{MoveKind, Pose3, ToolpathSegment};
    use crate::geometry::Point2;
    use crate::project::{Coolant, HolderShape, ToolEntry, ToolKind};

    fn tool(
        diameter: f64,
        flute_len: Option<f64>,
        shank: Option<f64>,
        holder: Option<HolderShape>,
    ) -> ToolEntry {
        ToolEntry {
            id: 1,
            name: "t".into(),
            kind: ToolKind::Endmill,
            diameter,
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
            z_shift_mm: None,
            laser_pierce_sec: None,
            laser_lead_in_mm: None,
            corner_radius_mm: None,
            tslot_neck_diameter_mm: None,
            tslot_neck_length_mm: None,
            wirbeln: false,
            wirbeln_stepover_mm: None,
            pause: 1,
            flute_length_mm: flute_len,
            shank_diameter_mm: shank,
            holder,
        }
    }

    fn seg(from: (f64, f64, f64), to: (f64, f64, f64)) -> ToolpathSegment {
        ToolpathSegment {
            from: Pose3 {
                x: from.0,
                y: from.1,
                z: from.2,
            },
            to: Pose3 {
                x: to.0,
                y: to.1,
                z: to.2,
            },
            kind: MoveKind::Cut,
            gcode_line: 0,
            op_id: 0,
        }
    }

    /// Drop a 30 mm-deep pocket into the heightmap by lowering all cells
    /// in `(ix, iy)` ranges to `floor_z`, leaving everything outside
    /// untouched (i.e. at `top_z`). The path runs along Y=mid.
    fn build_pocket(cols: u32, rows: u32, floor_z: f32, channel_half_w: f64) -> Heightmap {
        let mut hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, cols, rows, 0.0);
        let mid = f64::from(rows) * 0.5;
        for iy in 0..rows {
            let cy = f64::from(iy) + 0.5;
            if (cy - mid).abs() <= channel_half_w {
                for ix in 0..cols {
                    hm.lower_at(ix, iy, floor_z);
                }
            }
        }
        hm
    }

    #[test]
    fn deep_narrow_slot_holder_collides() {
        // 6 mm endmill, 25 mm flute length, ER11-shaped holder approximated
        // as a 20 mm-diameter cylinder × 30 mm long. Pocket is 30 mm deep
        // (floor_z = -30) with a 1 mm-wide channel from the path center
        // line — i.e. the wall is 1 mm out, well inside the holder's 10 mm
        // max radius.
        let t = tool(
            6.0,
            Some(25.0),
            Some(6.0),
            Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            }),
        );
        let holder = HolderProfile::from_tool(&t).expect("holder set");
        // Pocket is 60×60 grid. Channel width = 1 mm half-width = 2 mm
        // total. Walls are 1 mm from the path centerline.
        let hm = build_pocket(60, 60, -30.0, 1.0);
        // Cut runs along Y = 30 (grid mid) at z = -25 (i.e. flute fully
        // engaged, tip at -25 — 5 mm short of pocket bottom).
        let s = seg((5.0, 30.0, -25.0), (55.0, 30.0, -25.0));
        match check_segment_holder_against_walls(&hm, &s, &holder) {
            HolderCheck::Collision {
                required_clearance_mm,
                wall_z,
                ..
            } => {
                assert!(
                    required_clearance_mm > 0.0,
                    "required clearance must be positive, got {required_clearance_mm}",
                );
                // The wall sits at top_z = 0 (uncut), so wall_z is 0.
                assert!(
                    (wall_z - 0.0).abs() < 1e-5,
                    "wall_z expected 0, got {wall_z}"
                );
            }
            other @ HolderCheck::Clear => panic!("expected Collision, got {other:?}"),
        }
    }

    #[test]
    fn clear_when_walls_far_enough() {
        // Same tool / holder (max radius = 10 mm) — but the channel is
        // 15 mm half-width (30 mm wide) so walls sit 15 mm from the
        // centerline. Holder never reaches that radius.
        let t = tool(
            6.0,
            Some(25.0),
            Some(6.0),
            Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            }),
        );
        let holder = HolderProfile::from_tool(&t).expect("holder set");
        let hm = build_pocket(60, 60, -30.0, 15.0);
        let s = seg((5.0, 30.0, -25.0), (55.0, 30.0, -25.0));
        assert_eq!(
            check_segment_holder_against_walls(&hm, &s, &holder),
            HolderCheck::Clear,
        );
    }

    #[test]
    fn clear_when_no_holder() {
        // Tool with neither holder nor shank → HolderProfile::from_tool
        // returns None so the check should never fire. We assert that on
        // the from_tool side here; the sweep wires it up.
        let t = tool(6.0, Some(25.0), None, None);
        assert!(HolderProfile::from_tool(&t).is_none());
    }
}
