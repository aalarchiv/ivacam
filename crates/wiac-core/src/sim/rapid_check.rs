//! Rapid-through-material detector. Checks whether a `MoveKind::Rapid`
//! segment would slam the cutter into stock at G0 speed by walking the
//! same swept-cell footprint `sweep_segment` carves with — but read-only.
//!
//! A rapid is "Clear" iff every cell along its swept footprint has a
//! current Z that is not strictly above the cutter surface at that cell
//! (i.e. `cell_z <= cutter_pz + tool_profile.eval(r)`). The strict `>`
//! makes "rapid Z exactly equals stock Z" Clear — matches the typical
//! machinist intent of "rapid to surface, then plunge".

use crate::gcode::preview::{MoveKind, ToolpathSegment};
use crate::sim::heightmap::{Heightmap, ToolProfile};
use crate::sim::sweep::{for_each_swept_cell, HeightmapLayout};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RapidCheck {
    Clear,
    Collision {
        worst_x: f64,
        worst_y: f64,
        worst_cell_z: f32,
        rapid_pz: f64,
    },
}

#[must_use]
pub fn check_rapid_against_stock(
    heightmap: &Heightmap,
    segment: &ToolpathSegment,
    profile: ToolProfile,
) -> RapidCheck {
    debug_assert!(matches!(segment.kind, MoveKind::Rapid));

    // Fast reject: if both endpoints stay at-or-above the un-cut top,
    // the cutter never approaches material along the way. A rising or
    // falling rapid that stays >= top_z is in air.
    let pz_min = segment.from.z.min(segment.to.z);
    if pz_min >= heightmap.top_z as f64 {
        return RapidCheck::Clear;
    }

    let layout = HeightmapLayout::of(heightmap);
    let cols = heightmap.cols as usize;
    let mut worst: Option<(f32, u32, u32, f64)> = None;

    for_each_swept_cell(&layout, segment, profile, |ix, iy, _r, cutter_pz, dz| {
        let cell_z = heightmap.data[(iy as usize) * cols + ix as usize];
        let cutter_surface_z = cutter_pz as f32 + dz;
        if cell_z > cutter_surface_z {
            let excess = cell_z - cutter_surface_z;
            match worst {
                Some((best, _, _, _)) if excess <= best => {}
                _ => worst = Some((excess, ix, iy, cutter_pz)),
            }
        }
    });

    match worst {
        None => RapidCheck::Clear,
        Some((_excess, ix, iy, rapid_pz)) => {
            let cell = heightmap.cell;
            let worst_x = heightmap.origin.x + (ix as f64 + 0.5) * cell;
            let worst_y = heightmap.origin.y + (iy as f64 + 0.5) * cell;
            let worst_cell_z = heightmap.data[(iy as usize) * cols + ix as usize];
            RapidCheck::Collision {
                worst_x,
                worst_y,
                worst_cell_z,
                rapid_pz,
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
        assert_eq!(check_rapid_against_stock(&map, &s, endmill()), RapidCheck::Clear);
    }

    #[test]
    fn collision_through_uncut_stock() {
        let map = fresh_map(20, 20, 0.0);
        let s = rapid(pose(0.0, 0.0, -2.0), pose(10.0, 0.0, -2.0));
        match check_rapid_against_stock(&map, &s, endmill()) {
            RapidCheck::Collision {
                worst_cell_z,
                rapid_pz,
                ..
            } => {
                assert!((worst_cell_z - 0.0).abs() < 1e-6);
                assert!((rapid_pz - -2.0).abs() < 1e-6);
            }
            other => panic!("expected Collision, got {other:?}"),
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
                endmill(),
            ),
            RapidCheck::Clear,
        );
    }

    #[test]
    fn pure_plunge_zero_xy() {
        let map = fresh_map(20, 20, 0.0);
        // from.x == to.x, from.y == to.y, descending into uncut stock.
        let s = rapid(pose(5.0, 5.0, 1.0), pose(5.0, 5.0, -1.0));
        match check_rapid_against_stock(&map, &s, endmill()) {
            RapidCheck::Collision { .. } => {}
            other => panic!("expected Collision, got {other:?}"),
        }
    }

    #[test]
    fn strict_inequality_at_surface() {
        // Rapid travels exactly along the un-cut top — cell_z == cutter_pz,
        // strict `>` says Clear. Fast-reject also fires here (pz_min ==
        // top_z), so Clear lands either way.
        let map = fresh_map(20, 20, 0.0);
        let s = rapid(pose(0.0, 0.0, 0.0), pose(10.0, 0.0, 0.0));
        assert_eq!(check_rapid_against_stock(&map, &s, endmill()), RapidCheck::Clear);
    }

    #[test]
    fn clear_outside_heightmap_footprint() {
        // Rapid runs entirely outside the grid — cutter is in air.
        let map = fresh_map(20, 20, 0.0);
        let s = rapid(pose(50.0, 50.0, -5.0), pose(60.0, 50.0, -5.0));
        assert_eq!(check_rapid_against_stock(&map, &s, endmill()), RapidCheck::Clear);
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
        let touched = sweep_range(&mut map, &segments, 0, segments.len(), endmill(), &[], &mut d);
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
}
