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
#[derive(Debug, Clone, PartialEq)]
pub enum HolderCheck {
    Clear,
    Collision {
        worst_x: f64,
        worst_y: f64,
        wall_z: f32,
        required_clearance_mm: f32,
        /// 24ht: every cell that exceeds the holder envelope, not just the
        /// worst-excess one. Sorted by `required_clearance_mm` descending
        /// so element 0 mirrors `worst_x/worst_y/wall_z/required_clearance_mm`
        /// for back-compat. Without this list, mid-range collisions stayed
        /// hidden behind the worst cell — the user couldn't see the
        /// breadth of the obstacle.
        cells: Vec<HolderCollisionCell>,
    },
}

/// 24ht: per-cell holder-wall overlap record. `required_clearance_mm` is
/// how much extra clearance the holder would need at this cell for the
/// envelope to fit; `wall_z` is the cell's current heightmap value (the
/// top of the wall the holder is hitting).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct HolderCollisionCell {
    pub cell_x: f64,
    pub cell_y: f64,
    pub wall_z: f32,
    pub required_clearance_mm: f32,
}

// WHY: hrex — the cutter envelope (r <= cutting_radius) is the cutter's
// own sweep; material there is about to be removed and must NOT count as
// a holder collision. Only r > cutting_radius — the shank/holder
// territory above the flutes — gets the wall-vs-envelope test.
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
    // hrex: cells inside the cutter's own sweep (r <= cutting_radius)
    // are about to be removed by the flutes themselves — material there
    // is NOT a shank/holder collision. Treat them as part of the cutter
    // envelope, not the wall.
    let cutting_r = holder.cutting_radius();
    let cutting_r_sq = cutting_r * cutting_r;
    let cols = heightmap.cols as usize;

    // 24ht: collect EVERY offending cell, not just the worst-excess one.
    // The previous code kept a single (max-required) tuple — mid-range
    // collisions were silently dropped, and the UI had no way to surface
    // the breadth of the obstacle. We push all offenders here and let
    // the caller decide how to render them.
    let mut offenders: Vec<HolderCollisionCell> = Vec::new();

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
            // hrex: skip cells inside the cutter envelope — the flutes
            // sweep them clean, so any "wall" reading there belongs to
            // the cutter's own work, not to a holder collision.
            if r_sq <= cutting_r_sq {
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
            offenders.push(HolderCollisionCell {
                cell_x: heightmap.origin.x + (ix as f64 + 0.5) * cell,
                cell_y: heightmap.origin.y + (iy as f64 + 0.5) * cell,
                wall_z: cell_z,
                required_clearance_mm: required,
            });
        }
    }

    if offenders.is_empty() {
        return HolderCheck::Clear;
    }
    // Sort worst-first so element 0 keeps the "worst cell" semantics for
    // back-compat callers (24ht).
    offenders.sort_by(|a, b| {
        b.required_clearance_mm
            .partial_cmp(&a.required_clearance_mm)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let worst = offenders[0];
    HolderCheck::Collision {
        worst_x: worst.cell_x,
        worst_y: worst.cell_y,
        wall_z: worst.wall_z,
        required_clearance_mm: worst.required_clearance_mm,
        cells: offenders,
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
            default_xy_overlap: None,
            comment: None,
            z_shift_mm: None,
            laser_pierce_sec: None,
            laser_lead_in_mm: None,
            kerf_mm: None,
            corner_radius_mm: None,
            tslot_neck_diameter_mm: None,
            tslot_neck_length_mm: None,
            wirbeln: false,
            wirbeln_stepover_mm: None,
            wirbeln_extra_width_mm: None,
            wirbeln_osc_mm: None,
            pause: 1,
            flute_length_mm: flute_len,
            shank_diameter_mm: shank,
            stickout_length_mm: None,
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
        // 6 mm endmill, 15 mm flute length, ER11-shaped holder
        // approximated as a 20 mm-diameter cylinder × 30 mm long.
        // Walls 5 mm from the path centerline (well outside cutter
        // R=3) but inside the holder's 10 mm max radius. Pocket is
        // 30 mm deep, cutter tip at -25 → wall height above the tip
        // is 25 mm, but holder grows past r=5 only at z_above_tip=15
        // (top of flutes, where the shank ends and the cylinder
        // begins) → required clearance = 25 - 15 = 10 mm.
        //
        // After hrex: cells at r ≤ cutting_r (3 mm) are skipped — the
        // collision is detected at r ∈ (3, 10] where the holder is the
        // genuine threat.
        let t = tool(
            6.0,
            Some(15.0),
            Some(6.0),
            Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            }),
        );
        let holder = HolderProfile::from_tool(&t).expect("holder set");
        // Pocket is 60×60 grid. Channel half-width = 5 mm so walls
        // sit ≥ 5 mm from the centerline — outside the cutter, inside
        // the holder.
        let hm = build_pocket(60, 60, -30.0, 5.0);
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

    #[test]
    fn fresh_plunge_does_not_emit_holder_collision() {
        // hrex: a first plunge into uncut stock with a holder set used
        // to emit one HolderCollision per cell directly under the
        // cutter — because the heightmap was still at top_z under the
        // tip when the diagnostic ran *before* the carve, and the old
        // `lowest_z_for_radius` returned 0 for any r <= cutting_radius
        // (the very first sample is at the tip). With the fix, cells
        // inside the cutter envelope are skipped and only true
        // shank/holder territory (r > cutting_radius) is tested.
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
        // Fresh stock — no carving done. Heightmap stays at top_z = 0.
        let hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 60, 60, 0.0);
        // Plunge into the middle of the stock from above-stock down to
        // z = -10 (10 mm deep). Cutter is well inside the cutting_r
        // envelope of every nearby cell.
        let s = seg((30.0, 30.0, 0.0), (30.0, 30.0, -10.0));
        let res = check_segment_holder_against_walls(&hm, &s, &holder);
        assert_eq!(
            res,
            HolderCheck::Clear,
            "fresh plunge into uncut stock must not emit holder collision (got {res:?})",
        );
    }

    #[test]
    fn deep_slot_emits_full_cell_list_not_just_worst() {
        // 24ht: a deep narrow pocket should now report EVERY cell where
        // the holder envelope hits the wall, not just the worst one.
        // The 6 mm endmill with 20 mm holder above sweeps a half-width
        // band [3..10] mm around the path centerline on both sides; in
        // a 60 mm-long channel that's ~60 * 14 cells of holder vs wall
        // contact. We assert there are >> 1 cells reported and that
        // they're sorted worst-first.
        let t = tool(
            6.0,
            Some(15.0),
            Some(6.0),
            Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            }),
        );
        let holder = HolderProfile::from_tool(&t).expect("holder set");
        let hm = build_pocket(60, 60, -30.0, 5.0);
        let s = seg((5.0, 30.0, -25.0), (55.0, 30.0, -25.0));
        match check_segment_holder_against_walls(&hm, &s, &holder) {
            HolderCheck::Collision { cells, .. } => {
                assert!(
                    cells.len() > 10,
                    "expected many offending cells, got {}",
                    cells.len()
                );
                // Sorted worst-first.
                for w in cells.windows(2) {
                    assert!(
                        w[0].required_clearance_mm >= w[1].required_clearance_mm,
                        "cells must be sorted worst-first, got {:?} then {:?}",
                        w[0],
                        w[1],
                    );
                }
                // Every cell has positive required clearance (since they
                // all passed the wall-vs-envelope test).
                for c in &cells {
                    assert!(
                        c.required_clearance_mm > 0.0,
                        "cell has zero required clearance: {c:?}",
                    );
                }
            }
            other => panic!("expected Collision, got {other:?}"),
        }
    }

    #[test]
    fn fresh_plunge_pipeline_zero_holder_warnings() {
        // hrex (end-to-end through sweep_range): plunging into uncut
        // stock with a holder must emit zero `holder_collision`
        // warnings via the sim pipeline. Mirrors the unit test above
        // but goes through `sweep_range` so the wiring is exercised.
        use crate::sim::diagnostics::SimDiagnostics;
        use crate::sim::heightmap::ToolProfile;
        use crate::sim::sweep::sweep_range;

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
        let mut hm = Heightmap::new(Point2::new(0.0, 0.0), 1.0, 60, 60, 0.0);
        let segments = vec![seg((30.0, 30.0, 0.0), (30.0, 30.0, -10.0))];
        let mut d = SimDiagnostics::new();
        sweep_range(
            &mut hm,
            &segments,
            0,
            segments.len(),
            &ToolProfile::Endmill { r: 3.0 },
            &[],
            Some(&holder),
            &mut d,
        );
        assert_eq!(d.count("holder_collision"), 0);
    }
}
