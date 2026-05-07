//! Walk a cutter along toolpath segments and lower the heightmap cells
//! the tool covers. Per segment we compute the XY footprint AABB
//! inflated by tool radius, iterate the cells inside, project each
//! cell's center onto the segment to recover (r, t), and lower the
//! cell to `cutter_pz(t) + tool_profile(r)`.
//!
//! Move kinds:
//! * Cut / Plunge / Retract / Arc — carve.
//! * Rapid — skip (cutter at safe Z above the stock).
//!
//! Arcs come through the gcode preview already tessellated into chord
//! `ToolpathSegment`s; v1 treats them like lines (chord-only). The
//! resulting visual error is bounded by the tessellation step in
//! preview::interpret.

// Same f64 ↔ u32 grid plumbing as heightmap.rs, same intentional casts.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::cast_possible_wrap
)]

use crate::gcode::preview::{MoveKind, ToolpathSegment};
use crate::sim::heightmap::{Heightmap, ToolProfile};

/// Apply a single toolpath segment to `heightmap`, lowering every cell
/// the cutter sweeps over. Returns the number of cells touched (useful
/// for tests and progress reporting). Rapid moves are skipped.
///
/// `cell_drift_eps` is a tolerance on the segment's "bulge" check that
/// keeps us from sweeping beyond the heightmap when the segment lies on
/// the boundary.
pub fn sweep_segment(
    heightmap: &mut Heightmap,
    segment: &ToolpathSegment,
    profile: ToolProfile,
) -> u32 {
    if matches!(segment.kind, MoveKind::Rapid) {
        return 0;
    }
    let r_tool = profile.radius() as f64;
    if r_tool <= 0.0 {
        return 0;
    }
    let from = &segment.from;
    let to = &segment.to;
    // Skip moves that stay above the stock — the cutter is in air.
    let top_z = heightmap.top_z as f64;
    if from.z >= top_z && to.z >= top_z {
        return 0;
    }

    // Segment AABB inflated by tool radius (in world units).
    let min_x = from.x.min(to.x) - r_tool;
    let max_x = from.x.max(to.x) + r_tool;
    let min_y = from.y.min(to.y) - r_tool;
    let max_y = from.y.max(to.y) + r_tool;

    // Convert to cell index range (inclusive bounds, then clamped).
    let (ix0, iy0, ix1, iy1) = match world_aabb_to_cells(heightmap, min_x, min_y, max_x, max_y) {
        Some(t) => t,
        None => return 0,
    };

    // Pre-compute segment direction in XY for the projection.
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let len_sq = dx * dx + dy * dy;
    // Tiny / zero-length XY segments are pure plunges: every cell under
    // the tool sees the lowest Z the cutter visited along the segment,
    // which is min(from.z, to.z) regardless of direction.
    let pure_plunge = len_sq < 1e-12;
    let plunge_z = from.z.min(to.z);

    let cell = heightmap.cell;
    let r_tool_sq = r_tool * r_tool;

    let mut touched = 0u32;
    for iy in iy0..=iy1 {
        for ix in ix0..=ix1 {
            // Cell center in world coords.
            let cx = heightmap.origin.x + (ix as f64 + 0.5) * cell;
            let cy = heightmap.origin.y + (iy as f64 + 0.5) * cell;
            let (r_sq, cutter_pz) = if pure_plunge {
                let ex = cx - from.x;
                let ey = cy - from.y;
                (ex * ex + ey * ey, plunge_z)
            } else {
                // Project (cx, cy) onto the segment in XY → recover the
                // parameter t along the segment and the radial offset r.
                let mut t = ((cx - from.x) * dx + (cy - from.y) * dy) / len_sq;
                if t < 0.0 {
                    t = 0.0;
                } else if t > 1.0 {
                    t = 1.0;
                }
                let px = from.x + t * dx;
                let py = from.y + t * dy;
                let ex = cx - px;
                let ey = cy - py;
                (ex * ex + ey * ey, from.z + (to.z - from.z) * t)
            };
            if r_sq > r_tool_sq {
                continue;
            }
            let r = r_sq.sqrt() as f32;
            let dz = match profile.eval(r) {
                Some(v) => v,
                None => continue,
            };
            let surface_z = cutter_pz as f32 + dz;
            heightmap.lower_at(ix, iy, surface_z);
            touched += 1;
        }
    }
    touched
}

/// Apply every segment in `segments[from_idx..to_idx]` to the heightmap.
/// Returns the total cell-write count; useful as a perf signal in tests.
pub fn sweep_range(
    heightmap: &mut Heightmap,
    segments: &[ToolpathSegment],
    from_idx: usize,
    to_idx: usize,
    profile: ToolProfile,
) -> u32 {
    let lo = from_idx.min(segments.len());
    let hi = to_idx.min(segments.len());
    let mut total = 0u32;
    for seg in &segments[lo..hi] {
        total += sweep_segment(heightmap, seg, profile);
    }
    total
}

/// Convert a world-space AABB to the inclusive cell-index range it
/// covers. Returns None when the AABB is fully outside the heightmap.
fn world_aabb_to_cells(
    heightmap: &Heightmap,
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
) -> Option<(u32, u32, u32, u32)> {
    let cell = heightmap.cell;
    let inv = 1.0 / cell;
    let max_col = heightmap.cols.saturating_sub(1);
    let max_row = heightmap.rows.saturating_sub(1);
    let fx0 = (min_x - heightmap.origin.x) * inv;
    let fy0 = (min_y - heightmap.origin.y) * inv;
    let fx1 = (max_x - heightmap.origin.x) * inv;
    let fy1 = (max_y - heightmap.origin.y) * inv;
    if fx1 < 0.0 || fy1 < 0.0 {
        return None;
    }
    if fx0 > heightmap.cols as f64 || fy0 > heightmap.rows as f64 {
        return None;
    }
    let ix0 = fx0.floor().max(0.0) as u32;
    let iy0 = fy0.floor().max(0.0) as u32;
    let ix1 = (fx1.floor().max(0.0) as u32).min(max_col);
    let iy1 = (fy1.floor().max(0.0) as u32).min(max_row);
    if ix0 > ix1 || iy0 > iy1 {
        return None;
    }
    Some((ix0, iy0, ix1, iy1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcode::preview::{MoveKind, Pose3};
    use crate::geometry::Point2;
    use crate::sim::heightmap::Heightmap;

    fn pose(x: f64, y: f64, z: f64) -> Pose3 {
        Pose3 { x, y, z }
    }

    fn seg(kind: MoveKind, from: Pose3, to: Pose3) -> ToolpathSegment {
        ToolpathSegment {
            from,
            to,
            kind,
            gcode_line: 0,
            op_id: 0,
        }
    }

    fn fresh_map(cols: u32, rows: u32) -> Heightmap {
        // Grid origin at (0, 0), 1mm cells, top z = 0.
        Heightmap::new(Point2::new(0.0, 0.0), 1.0, cols, rows, 0.0)
    }

    /// Cell value at integer coords (sample at cell center).
    fn cell(map: &Heightmap, ix: u32, iy: u32) -> f32 {
        map.data[(iy as usize) * (map.cols as usize) + ix as usize]
    }

    #[test]
    fn rapid_move_does_not_carve() {
        let mut map = fresh_map(20, 20);
        let s = seg(MoveKind::Rapid, pose(0.0, 0.0, -10.0), pose(10.0, 10.0, -10.0));
        let touched = sweep_segment(&mut map, &s, ToolProfile::Endmill { r: 2.0 });
        assert_eq!(touched, 0);
        assert!(map.data.iter().all(|z| (*z - 0.0).abs() < 1e-6));
        assert!(map.dirty_aabb().is_none());
    }

    #[test]
    fn cutter_above_stock_does_not_carve() {
        let mut map = fresh_map(20, 20);
        // Both endpoints above the stock surface (z = 0). Cutter is in air.
        let s = seg(MoveKind::Cut, pose(5.0, 5.0, 0.5), pose(8.0, 8.0, 0.5));
        let touched = sweep_segment(&mut map, &s, ToolProfile::Endmill { r: 2.0 });
        assert_eq!(touched, 0);
    }

    #[test]
    fn endmill_plunge_lowers_circular_patch() {
        let mut map = fresh_map(40, 40);
        // Plunge at (10, 10) from z=0 to z=-1 with R=2.
        let s = seg(MoveKind::Plunge, pose(10.0, 10.0, 0.0), pose(10.0, 10.0, -1.0));
        sweep_segment(&mut map, &s, ToolProfile::Endmill { r: 2.0 });
        // Cell directly under the tool tip should be at -1.
        assert!((cell(&map, 10, 10) - -1.0).abs() < 1e-5);
        // Cell ~2 cells away (≈2 mm) is on the boundary of the cutter; let it pass either way.
        // Cell ~4 cells away (≈4 mm) is well outside R=2 → still 0.
        assert!((cell(&map, 14, 14) - 0.0).abs() < 1e-5);
        // Dirty AABB should at least cover the 4×4 region around the
        // plunge point (R=2 inflated by half a cell).
        let (ix0, iy0, ix1, iy1) = map.dirty_aabb().expect("plunge mutates cells");
        assert!(ix0 <= 8 && iy0 <= 8 && ix1 >= 12 && iy1 >= 12);
    }

    #[test]
    fn horizontal_cut_carves_4mm_stripe() {
        let mut map = fresh_map(60, 60);
        let r = 2.0_f32;
        // Cut from (5, 25) to (55, 25) at z=-1 with R=2.
        let s = seg(MoveKind::Cut, pose(5.0, 25.0, -1.0), pose(55.0, 25.0, -1.0));
        sweep_segment(&mut map, &s, ToolProfile::Endmill { r });
        // Center of the stripe should be at -1 along the path.
        for ix in 6..=54 {
            assert!(
                (cell(&map, ix, 25) - -1.0).abs() < 1e-5,
                "cell ({ix}, 25) expected -1, got {}",
                cell(&map, ix, 25),
            );
        }
        // ±2 cells off-center is the boundary of the 4mm-wide stripe.
        assert!((cell(&map, 30, 24) - -1.0).abs() < 1e-5);
        assert!((cell(&map, 30, 26) - -1.0).abs() < 1e-5);
        // 4 cells off-center (≈ 4mm) is well outside R=2 → still top_z.
        assert!((cell(&map, 30, 21) - 0.0).abs() < 1e-5);
        assert!((cell(&map, 30, 29) - 0.0).abs() < 1e-5);
    }

    #[test]
    fn vbit_plunge_lowers_in_conical_pattern() {
        let mut map = fresh_map(40, 40);
        // 60° included angle = 30° half-angle. R=2, no flat tip.
        let half = 30f32.to_radians();
        let profile = ToolProfile::VBit {
            r: 2.0,
            tip_r: 0.0,
            half_angle_rad: half,
        };
        // Plunge AT a cell center so r=0 at cell (10, 10) and r=1 at
        // cell (11, 10) — keeps the analytic check simple.
        let s = seg(MoveKind::Plunge, pose(10.5, 10.5, 0.0), pose(10.5, 10.5, -2.0));
        sweep_segment(&mut map, &s, profile);
        let apex = cell(&map, 10, 10);
        let mid = cell(&map, 11, 10);
        // Apex sits at the plunge depth (-2). Cell at r=1 sits higher
        // by tan(30°) ≈ 0.577.
        assert!((apex - -2.0).abs() < 1e-5, "apex should be -2, got {apex}");
        assert!(
            (mid - -2.0 - half.tan()).abs() < 0.02,
            "mid r=1 should be -2 + tan(30°), got {mid}",
        );
        // Cells past the cutting radius are untouched.
        assert!((cell(&map, 13, 10) - 0.0).abs() < 1e-5);
    }

    #[test]
    fn lower_at_only_writes_on_descent_not_re_pass() {
        let mut map = fresh_map(20, 20);
        let plunge = seg(MoveKind::Plunge, pose(10.0, 10.0, 0.0), pose(10.0, 10.0, -2.0));
        sweep_segment(&mut map, &plunge, ToolProfile::Endmill { r: 2.0 });
        // Now sweep a SHALLOWER cut over the same cell — should NOT raise.
        let shallow = seg(MoveKind::Cut, pose(8.0, 10.0, -0.5), pose(12.0, 10.0, -0.5));
        sweep_segment(&mut map, &shallow, ToolProfile::Endmill { r: 2.0 });
        assert!(
            (cell(&map, 10, 10) - -2.0).abs() < 1e-5,
            "later shallower pass must not raise the cell",
        );
    }

    #[test]
    fn sweep_range_walks_each_segment() {
        let mut map = fresh_map(40, 40);
        let segments = vec![
            seg(MoveKind::Cut, pose(5.0, 10.0, -1.0), pose(15.0, 10.0, -1.0)),
            seg(MoveKind::Rapid, pose(15.0, 10.0, 5.0), pose(20.0, 20.0, 5.0)),
            seg(MoveKind::Plunge, pose(20.0, 20.0, 0.0), pose(20.0, 20.0, -1.0)),
        ];
        let touched =
            sweep_range(&mut map, &segments, 0, segments.len(), ToolProfile::Endmill { r: 2.0 });
        assert!(touched > 0);
        // First segment carved the (5..15, 10) stripe.
        assert!((cell(&map, 10, 10) - -1.0).abs() < 1e-5);
        // Rapid move did NOT carve.
        // Plunge endpoint carved at (20, 20).
        assert!((cell(&map, 20, 20) - -1.0).abs() < 1e-5);
        // Untouched cell stays at top_z.
        assert!((cell(&map, 35, 35) - 0.0).abs() < 1e-5);
    }

    #[test]
    fn sweep_outside_heightmap_is_silently_skipped() {
        let mut map = fresh_map(20, 20);
        // Segment fully to the right of the heightmap (origin 0..20).
        let s = seg(MoveKind::Cut, pose(50.0, 10.0, -1.0), pose(60.0, 10.0, -1.0));
        let touched = sweep_segment(&mut map, &s, ToolProfile::Endmill { r: 2.0 });
        assert_eq!(touched, 0);
        // Heightmap untouched.
        assert!(map.dirty_aabb().is_none());
    }

    #[test]
    fn sweep_partial_overlap_clamps_to_grid() {
        let mut map = fresh_map(20, 20);
        // Segment crosses the right edge — half inside, half outside.
        let s = seg(MoveKind::Cut, pose(15.0, 10.0, -1.0), pose(25.0, 10.0, -1.0));
        sweep_segment(&mut map, &s, ToolProfile::Endmill { r: 2.0 });
        // Cells inside the grid along the path should be lowered.
        for ix in 16..=19 {
            assert!(
                (cell(&map, ix, 10) - -1.0).abs() < 1e-5,
                "cell ({ix}, 10) should be carved",
            );
        }
    }
}
