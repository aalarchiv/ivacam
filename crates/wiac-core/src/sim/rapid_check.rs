//! Rapid-through-material detector. Checks whether a `MoveKind::Rapid`
//! segment would slam the cutter into stock at G0 speed by walking the
//! same swept-cell footprint `sweep_segment` carves with — but read-only.
//!
//! A rapid is "Clear" iff every cell along its swept footprint has a
//! current Z that is not strictly above the cutter surface at that cell
//! (i.e. `cell_z <= cutter_pz + tool_profile.eval(r)`). The strict `>`
//! makes "rapid Z exactly equals stock Z" Clear — matches the typical
//! machinist intent of "rapid to surface, then plunge".
//!
//! 50eq: when a `HolderProfile` is wired in, we additionally walk the
//! wider shank/holder footprint and emit a `ShankRapid` subkind so
//! "the rapid clears the cutter tip but drags the shank through tall
//! uncut walls" gets flagged — the canonical broken-collet scenario.

// # CAM/sim pedantic-lint exemptions
// Rapid-collision sweep casts bounded cell indices to f64; cutter Z is
// converted f64→f32 because the heightmap stores f32 to halve memory.
// Shank-pass AABB→cell math mirrors holder_check.rs: bounded by
// heightmap dimensions, signs pre-clamped with `.max(0.0)`. Similar
// names `ix0`/`iy0`/`fx0`/`fy0` (and shank/tip pass duplicates) follow
// the established grid-projection convention in sim/sweep.rs.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::cast_possible_wrap,
    clippy::similar_names,
    clippy::too_many_lines
)]

use crate::gcode::preview::{MoveKind, ToolpathSegment};
use crate::sim::heightmap::{Heightmap, ToolProfile};
use crate::sim::holder::HolderProfile;
use crate::sim::sweep::{for_each_swept_cell, HeightmapLayout};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RapidCheck {
    Clear,
    Collision {
        worst_x: f64,
        worst_y: f64,
        worst_cell_z: f32,
        rapid_pz: f64,
        /// 50eq: which envelope was struck — the cutting flutes (Tip)
        /// or the shank/holder above them (Shank). Surface this so the
        /// user knows whether the fix is "lower the cut" vs "raise the
        /// retract / use a longer tool".
        subkind: RapidCollisionSubkind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RapidCollisionSubkind {
    Tip,
    Shank,
}

// WHY: 50eq — only walking the cutting-flute footprint left rapids
// that drag the shank/holder through uncut walls invisible. With a
// `HolderProfile` we now also walk the wider holder envelope and
// compare `cell_z` against `rapid_pz + holder_lower_z(r)` where
// `holder_lower_z(r)` is the Z above the tip at which the envelope
// first reaches radius `r`.
#[must_use]
pub fn check_rapid_against_stock(
    heightmap: &Heightmap,
    segment: &ToolpathSegment,
    profile: &ToolProfile,
    holder: Option<&HolderProfile>,
) -> RapidCheck {
    debug_assert!(matches!(segment.kind, MoveKind::Rapid));

    let layout = HeightmapLayout::of(heightmap);
    let cols = heightmap.cols as usize;

    // ─────────── (a) cutter-tip envelope walk ───────────
    // Fast reject: if both endpoints stay at-or-above the un-cut top,
    // the cutter tip never approaches material. (The holder check
    // below has its own gate.)
    let pz_min = segment.from.z.min(segment.to.z);
    let mut tip_worst: Option<(f32, u32, u32, f64)> = None;
    if pz_min < f64::from(heightmap.top_z) {
        for_each_swept_cell(&layout, segment, profile, |ix, iy, _r, cutter_pz, dz| {
            let cell_z = heightmap.data[(iy as usize) * cols + ix as usize];
            let cutter_surface_z = cutter_pz as f32 + dz;
            if cell_z > cutter_surface_z {
                let excess = cell_z - cutter_surface_z;
                match tip_worst {
                    Some((best, _, _, _)) if excess <= best => {}
                    _ => tip_worst = Some((excess, ix, iy, cutter_pz)),
                }
            }
        });
    }

    if let Some((_excess, ix, iy, rapid_pz)) = tip_worst {
        let cell = heightmap.cell;
        let worst_x = heightmap.origin.x + (f64::from(ix) + 0.5) * cell;
        let worst_y = heightmap.origin.y + (f64::from(iy) + 0.5) * cell;
        let worst_cell_z = heightmap.data[(iy as usize) * cols + ix as usize];
        return RapidCheck::Collision {
            worst_x,
            worst_y,
            worst_cell_z,
            rapid_pz,
            subkind: RapidCollisionSubkind::Tip,
        };
    }

    // ─────────── (b) shank/holder envelope walk ───────────
    let Some(holder) = holder else {
        return RapidCheck::Clear;
    };
    let max_r = holder.max_radius();
    if max_r <= 0.0 {
        return RapidCheck::Clear;
    }
    let cutting_r = holder.cutting_radius();
    // The shank/holder is only above the tip; if the *entire* tool
    // body (tip + total_length) stays above the heightmap top, no
    // hit possible. The body top is `rapid_pz + total_length`.
    // Conservatively: if even the lowest tip + 0 mm (the tip itself)
    // is already at top_z+ we'd have bailed (a) wouldn't have run; but
    // the shank can collide even when the tip itself is above top_z
    // — that's the whole point of this pass.

    let cell = heightmap.cell;
    let inv_cell = 1.0 / cell;
    let max_col = heightmap.cols.saturating_sub(1);
    let max_row = heightmap.rows.saturating_sub(1);

    let from = &segment.from;
    let to = &segment.to;
    let min_x = from.x.min(to.x) - max_r;
    let max_x = from.x.max(to.x) + max_r;
    let min_y = from.y.min(to.y) - max_r;
    let max_y = from.y.max(to.y) + max_r;
    let fx0 = (min_x - heightmap.origin.x) * inv_cell;
    let fy0 = (min_y - heightmap.origin.y) * inv_cell;
    let fx1 = (max_x - heightmap.origin.x) * inv_cell;
    let fy1 = (max_y - heightmap.origin.y) * inv_cell;
    if fx1 < 0.0 || fy1 < 0.0 {
        return RapidCheck::Clear;
    }
    if fx0 > f64::from(heightmap.cols) || fy0 > f64::from(heightmap.rows) {
        return RapidCheck::Clear;
    }
    let ix0 = fx0.floor().max(0.0) as u32;
    let iy0 = fy0.floor().max(0.0) as u32;
    let ix1 = (fx1.floor().max(0.0) as u32).min(max_col);
    let iy1 = (fy1.floor().max(0.0) as u32).min(max_row);
    if ix0 > ix1 || iy0 > iy1 {
        return RapidCheck::Clear;
    }

    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let len_sq = dx * dx + dy * dy;
    let pure_plunge = len_sq < 1e-12;
    let plunge_z = from.z.min(to.z);
    let max_r_sq = max_r * max_r;
    let cutting_r_sq = cutting_r * cutting_r;
    let mut shank_worst: Option<(f32, u32, u32, f64)> = None;

    for iy in iy0..=iy1 {
        for ix in ix0..=ix1 {
            let cx = heightmap.origin.x + (f64::from(ix) + 0.5) * cell;
            let cy = heightmap.origin.y + (f64::from(iy) + 0.5) * cell;
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
            // Below the cutting radius is the tip envelope; already
            // tested in pass (a). Avoid double-emit.
            if r_sq <= cutting_r_sq {
                continue;
            }
            let r = r_sq.sqrt();
            let Some(holder_lower_z) = holder.lowest_z_for_radius(r) else {
                continue;
            };
            let cell_z = heightmap.data[(iy as usize) * cols + ix as usize];
            // Shank/holder surface at radial offset r is at
            // `cutter_pz + holder_lower_z`. A cell strictly above that
            // height is a collision.
            let body_z = cutter_pz + holder_lower_z;
            if f64::from(cell_z) > body_z {
                let excess = (f64::from(cell_z) - body_z) as f32;
                match shank_worst {
                    Some((best, _, _, _)) if excess <= best => {}
                    _ => shank_worst = Some((excess, ix, iy, cutter_pz)),
                }
            }
        }
    }

    match shank_worst {
        None => RapidCheck::Clear,
        Some((_excess, ix, iy, rapid_pz)) => {
            let worst_x = heightmap.origin.x + (f64::from(ix) + 0.5) * cell;
            let worst_y = heightmap.origin.y + (f64::from(iy) + 0.5) * cell;
            let worst_cell_z = heightmap.data[(iy as usize) * cols + ix as usize];
            RapidCheck::Collision {
                worst_x,
                worst_y,
                worst_cell_z,
                rapid_pz,
                subkind: RapidCollisionSubkind::Shank,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcode::preview::{MoveKind, Pose3, ToolpathSegment};
    use crate::geometry::Point2;
    use crate::sim::diagnostics::SimDiagnostics;
    use crate::sim::sweep::sweep_range;

    fn pose(x: f64, y: f64, z: f64) -> Pose3 {
        Pose3 { x, y, z }
    }

    fn rapid(from: Pose3, to: Pose3) -> ToolpathSegment {
        ToolpathSegment {
            from,
            to,
            kind: MoveKind::Rapid,
            gcode_line: 0,
            op_id: 0,
        }
    }

    fn fresh_map(cols: u32, rows: u32, top_z: f32) -> Heightmap {
        Heightmap::new(Point2::new(0.0, 0.0), 1.0, cols, rows, top_z)
    }

    fn endmill() -> ToolProfile {
        ToolProfile::Endmill { r: 2.0 }
    }

    #[test]
    fn clear_above_stock() {
        let map = fresh_map(20, 20, 0.0);
        let s = rapid(pose(0.0, 0.0, 5.0), pose(10.0, 0.0, 5.0));
        assert_eq!(
            check_rapid_against_stock(&map, &s, &endmill(), None),
            RapidCheck::Clear
        );
    }

    #[test]
    fn collision_through_uncut_stock() {
        let map = fresh_map(20, 20, 0.0);
        let s = rapid(pose(0.0, 0.0, -2.0), pose(10.0, 0.0, -2.0));
        match check_rapid_against_stock(&map, &s, &endmill(), None) {
            RapidCheck::Collision {
                worst_cell_z,
                rapid_pz,
                subkind,
                ..
            } => {
                assert!((worst_cell_z - 0.0).abs() < 1e-6);
                assert!((rapid_pz - -2.0).abs() < 1e-6);
                assert_eq!(subkind, RapidCollisionSubkind::Tip);
            }
            other @ RapidCheck::Clear => panic!("expected Collision, got {other:?}"),
        }
    }

    #[test]
    fn clear_when_descending_late() {
        // Descending rapid (5 → -2). Pre-lower the cells past x≈7 to
        // z=-3 so the late part of the path is over already-cleared
        // material. Earlier cells still sit at top_z=0 but the cutter
        // is well above them at small t (cutter_pz lerp(5, -2, t)).
        // By the time the path reaches the lowered region (t≥0.7),
        // cutter_pz ≤ 0.1, still above -3 → Clear.
        let mut map = fresh_map(20, 20, 0.0);
        for ix in 7..20 {
            for iy in 0..3 {
                map.lower_at(ix, iy, -3.0);
            }
        }
        assert_eq!(
            check_rapid_against_stock(
                &map,
                &rapid(pose(0.0, 0.0, 5.0), pose(10.0, 0.0, -2.0)),
                &endmill(),
                None,
            ),
            RapidCheck::Clear,
        );
    }

    #[test]
    fn pure_plunge_zero_xy() {
        let map = fresh_map(20, 20, 0.0);
        // from.x == to.x, from.y == to.y, descending into uncut stock.
        let s = rapid(pose(5.0, 5.0, 1.0), pose(5.0, 5.0, -1.0));
        match check_rapid_against_stock(&map, &s, &endmill(), None) {
            RapidCheck::Collision { .. } => {}
            other @ RapidCheck::Clear => panic!("expected Collision, got {other:?}"),
        }
    }

    #[test]
    fn strict_inequality_at_surface() {
        // Rapid travels exactly along the un-cut top — cell_z == cutter_pz,
        // strict `>` says Clear. Fast-reject also fires here (pz_min ==
        // top_z), so Clear lands either way.
        let map = fresh_map(20, 20, 0.0);
        let s = rapid(pose(0.0, 0.0, 0.0), pose(10.0, 0.0, 0.0));
        assert_eq!(
            check_rapid_against_stock(&map, &s, &endmill(), None),
            RapidCheck::Clear
        );
    }

    #[test]
    fn clear_outside_heightmap_footprint() {
        // Rapid runs entirely outside the grid — cutter is in air.
        let map = fresh_map(20, 20, 0.0);
        let s = rapid(pose(50.0, 50.0, -5.0), pose(60.0, 50.0, -5.0));
        assert_eq!(
            check_rapid_against_stock(&map, &s, &endmill(), None),
            RapidCheck::Clear
        );
    }

    #[test]
    fn pipeline_integration_emits_warning() {
        // End-to-end through `sweep_range`: a rapid through uncut stock
        // produces one RapidThroughMaterial warning, surrounding cuts
        // and plunges still carve correctly, and the warning carries
        // the rapid's index in the toolpath stream.
        let mut map = fresh_map(40, 40, 0.0);
        let mut d = SimDiagnostics::new();
        let segments = vec![
            ToolpathSegment {
                from: pose(5.0, 10.0, -1.0),
                to: pose(15.0, 10.0, -1.0),
                kind: MoveKind::Cut,
                gcode_line: 0,
                op_id: 0,
            },
            ToolpathSegment {
                from: pose(15.0, 20.0, -2.0),
                to: pose(25.0, 20.0, -2.0),
                kind: MoveKind::Rapid,
                gcode_line: 0,
                op_id: 0,
            },
            ToolpathSegment {
                from: pose(20.0, 30.0, 0.0),
                to: pose(20.0, 30.0, -1.0),
                kind: MoveKind::Plunge,
                gcode_line: 0,
                op_id: 0,
            },
        ];
        let touched = sweep_range(
            &mut map,
            &segments,
            0,
            segments.len(),
            &endmill(),
            &[],
            None,
            &mut d,
        );
        assert!(touched > 0, "cuts/plunges should still carve");
        assert_eq!(d.count("rapid_through_material"), 1);
        match &d.warnings[0] {
            crate::sim::diagnostics::SimWarning::RapidThroughMaterial {
                segment_idx,
                rapid_pz,
                ..
            } => {
                assert_eq!(*segment_idx, 1);
                assert!((rapid_pz - -2.0).abs() < 1e-6);
            }
            other => panic!("unexpected warning: {other:?}"),
        }
    }

    fn holder_20mm_dia() -> HolderProfile {
        // 6 mm endmill, 25 mm flutes, 6 mm shank, 20 mm-dia × 30 mm
        // cylinder holder. max_radius = 10 mm; the holder body sits
        // from z_above_tip = 25 (top of flutes) upward.
        use crate::project::{Coolant, HolderShape, ToolEntry, ToolKind};
        let t = ToolEntry {
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
            wirbeln: false,
            wirbeln_stepover_mm: None,
            wirbeln_extra_width_mm: None,
            wirbeln_osc_mm: None,
            pause: 1,
            flute_length_mm: Some(25.0),
            length_mm: None,
            compression_transition_mm: None,
            thread_pitch_mm: None,
            shank_diameter_mm: Some(6.0),
            stickout_length_mm: None,
            holder: Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            }),
            spindle_direction: crate::project::SpindleDirection::default(),
            drag_knife_self_align_angle_deg: None,
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            vcarve_lead_in_angle_deg: None,
        };
        HolderProfile::from_tool(&t).expect("holder set")
    }

    #[test]
    fn rapid_shank_through_stock_emits_warning() {
        // 50eq: the cutter tip clears the deep pocket but the shank
        // and holder above it slam into the surrounding tall walls.
        // 50×50×1 mm heightmap, top_z = 50. Carve a channel along
        // Y=25 with half-width 4 (cells at iy 21..28) down to z=0.
        // That leaves walls at iy < 21 / iy > 28 at z=50.
        //
        // Tool: 4 mm endmill (R=2), 25 mm flutes, 6 mm shank, 20 mm
        // cylinder holder (R=10). Rapid runs along the channel at
        // tip z=10. Cutter (R=2) only sweeps iy 23..27 — all cleared,
        // tip is Clear. But holder (R=10) sweeps iy 15..35; cells at
        // iy ≤ 20 / iy ≥ 28 are walls at z=50. The holder envelope
        // jumps to 10 at z_above_tip = 25 (top of flutes) →
        // body_z = 10 + 25 = 35 < 50 → Shank collision.
        let holder = holder_20mm_dia();
        let mut map = fresh_map(50, 50, 50.0);
        // 8 mm-wide channel (half-width 4) along Y=25 down to z=0.
        for ix in 0..50 {
            for iy in 21..=28 {
                map.lower_at(ix, iy, 0.0);
            }
        }
        // 4 mm endmill — smaller than the channel so the cutter tip
        // itself clears in XY.
        let small_endmill = ToolProfile::Endmill { r: 2.0 };
        let s = rapid(pose(5.0, 25.0, 10.0), pose(45.0, 25.0, 10.0));
        match check_rapid_against_stock(&map, &s, &small_endmill, Some(&holder)) {
            RapidCheck::Collision { subkind, .. } => {
                assert_eq!(
                    subkind,
                    RapidCollisionSubkind::Shank,
                    "expected Shank subkind (tip is in the cleared channel)",
                );
            }
            other @ RapidCheck::Clear => panic!("expected Shank collision, got {other:?}"),
        }
    }

    #[test]
    fn shank_clear_when_walls_far_enough() {
        // Mirror: clear out a wide enough channel (walls > holder
        // max_radius from path) so the shank also clears.
        let holder = holder_20mm_dia();
        let mut map = fresh_map(60, 60, 50.0);
        // 30 mm-wide channel (15 mm half-width) — walls are ≥ 15 mm
        // from the path. holder.max_radius() = 10 mm < 15 mm. Clear.
        for ix in 0..60 {
            for iy in 15..=45 {
                map.lower_at(ix, iy, 0.0);
            }
        }
        let small_endmill = ToolProfile::Endmill { r: 2.0 };
        let s = rapid(pose(5.0, 30.0, 10.0), pose(55.0, 30.0, 10.0));
        assert_eq!(
            check_rapid_against_stock(&map, &s, &small_endmill, Some(&holder)),
            RapidCheck::Clear,
        );
    }

    /// ityc: a `LaserBeam` tool has no physical shank to drag through
    /// walls. Even when the user wires a shank diameter on the laser
    /// tool entry (because they share a project-wide tool table with
    /// mill tools), `HolderProfile::from_tool` must return `None` so
    /// the rapid-check skips the shank pass and doesn't spuriously
    /// alarm on every rapid that flies above tall walls.
    #[test]
    fn laser_tool_skips_shank_pass_entirely() {
        use crate::project::{Coolant, HolderShape, ToolEntry, ToolKind};
        let t = ToolEntry {
            id: 1,
            name: "laser".into(),
            kind: ToolKind::LaserBeam,
            diameter: 0.2,
            tip_diameter: None,
            tip_angle_deg: 60.0,
            dragoff: None,
            flutes: 0,
            speed: 1000,
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
            wirbeln: false,
            wirbeln_stepover_mm: None,
            wirbeln_extra_width_mm: None,
            wirbeln_osc_mm: None,
            pause: 0,
            // User accidentally / by-template set a shank + holder on
            // the laser entry — pre-fix this triggered the shank pass.
            flute_length_mm: None,
            length_mm: None,
            compression_transition_mm: None,
            thread_pitch_mm: None,
            shank_diameter_mm: Some(6.0),
            stickout_length_mm: None,
            holder: Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            }),
            spindle_direction: crate::project::SpindleDirection::default(),
            drag_knife_self_align_angle_deg: None,
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            vcarve_lead_in_angle_deg: None,
        };
        // No HolderProfile for laser tools.
        assert!(
            HolderProfile::from_tool(&t).is_none(),
            "ityc: laser tools must not produce a HolderProfile (skips shank/holder pass)"
        );
    }

    /// ityc: a drill with NO `flute_length_mm` set used to leave the
    /// shank starting at z=0 above the tip. Any wall above the tip
    /// plane within the shank radius alarmed as a shank collision —
    /// silly false-positive on every drill program. The fix synthesizes
    /// a default body length of `diameter * 6` (typical L/D ratio for
    /// twist drills) so the shank envelope only kicks in above a
    /// realistic body height.
    #[test]
    fn drill_no_flute_length_uses_diameter_times_six_body() {
        use crate::project::{Coolant, ToolEntry, ToolKind};
        let t = ToolEntry {
            id: 1,
            name: "3mm drill".into(),
            kind: ToolKind::Drill,
            diameter: 3.0,
            tip_diameter: None,
            tip_angle_deg: 118.0,
            dragoff: None,
            flutes: 2,
            speed: 3000,
            plunge_rate: 100,
            feed_rate: 200,
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
            wirbeln: false,
            wirbeln_stepover_mm: None,
            wirbeln_extra_width_mm: None,
            wirbeln_osc_mm: None,
            pause: 0,
            flute_length_mm: None, // ← the bug: pre-fix shank started at z=0
            length_mm: None,
            compression_transition_mm: None,
            thread_pitch_mm: None,
            shank_diameter_mm: Some(3.0),
            stickout_length_mm: None,
            holder: None,
            spindle_direction: crate::project::SpindleDirection::default(),
            drag_knife_self_align_angle_deg: None,
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            vcarve_lead_in_angle_deg: None,
        };
        let holder = HolderProfile::from_tool(&t).expect("drill has shank profile");
        // diameter * 6 = 18mm. At z=17 above the tip the radius is
        // still the cutting radius (1.5); at z=19 it would be the
        // shank radius. Test radius at a probe point inside the
        // synthetic flute region.
        let r_mid = holder
            .radius_at(15.0)
            .expect("z=15 still inside synthetic flute span (18mm)");
        assert!(
            (r_mid - 1.5).abs() < 1e-9,
            "ityc: at z=15mm above tip the drill's synthetic flute region must \
             still report cutting_r=1.5, got r={r_mid}"
        );
    }

    /// ityc: a rapid over walls that previously slammed the shank-pass
    /// (laser tool with shank-on-tip pre-fix) now clears.
    #[test]
    fn laser_rapid_over_walls_does_not_alarm() {
        use crate::project::{Coolant, HolderShape, ToolEntry, ToolKind};
        let laser = ToolEntry {
            id: 1,
            name: "laser".into(),
            kind: ToolKind::LaserBeam,
            diameter: 0.2,
            tip_diameter: None,
            tip_angle_deg: 60.0,
            dragoff: None,
            flutes: 0,
            speed: 1000,
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
            wirbeln: false,
            wirbeln_stepover_mm: None,
            wirbeln_extra_width_mm: None,
            wirbeln_osc_mm: None,
            pause: 0,
            flute_length_mm: None,
            length_mm: None,
            compression_transition_mm: None,
            thread_pitch_mm: None,
            shank_diameter_mm: Some(6.0),
            stickout_length_mm: None,
            holder: Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 30.0,
            }),
            spindle_direction: crate::project::SpindleDirection::default(),
            drag_knife_self_align_angle_deg: None,
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            vcarve_lead_in_angle_deg: None,
        };
        // 50×50×1 mm heightmap with tall walls everywhere outside a
        // narrow channel — exactly the geometry that triggered the
        // pre-fix bug.
        let holder = HolderProfile::from_tool(&laser);
        let mut map = fresh_map(50, 50, 50.0);
        for ix in 0..50 {
            for iy in 24..=26 {
                map.lower_at(ix, iy, 0.0);
            }
        }
        let beam = ToolProfile::Endmill { r: 0.1 };
        let s = rapid(pose(5.0, 25.0, 10.0), pose(45.0, 25.0, 10.0));
        assert_eq!(
            check_rapid_against_stock(&map, &s, &beam, holder.as_ref()),
            RapidCheck::Clear,
            "ityc: laser rapid above tall walls must be Clear (no shank to drag)"
        );
    }
}
