//! Walk a cutter along toolpath segments and lower the heightmap cells
//! the tool covers. Per segment we compute the XY footprint AABB
//! inflated by tool radius, iterate the cells inside, project each
//! cell's center onto the segment to recover (r, t), and lower the
//! cell to `cutter_pz(t) + tool_profile(r)`.
//!
//! Move kinds:
//! * Cut / Plunge / Retract / Arc — carve.
//! * Rapid — no cut; checked against stock by `rapid_check` and emits
//!   a `RapidThroughMaterial` warning if it would slam into material.
//!
//! Arcs come through the gcode preview already tessellated into chord
//! `ToolpathSegment`s; v1 treats them like lines (chord-only). The
//! resulting visual error is bounded by the tessellation step in
//! `preview::interpret`.

// # CAM/sim pedantic-lint exemptions
// Cell-grid sweep operates on tightly-grouped index pairs
// (`ix0`/`iy0`/`ix1`/`iy1`, `from`/`to`) — renaming loses the
// from/to/start/end mapping that mirrors the math.
#![allow(clippy::similar_names)]
// Same f64 ↔ u32 grid plumbing as heightmap.rs, same intentional casts.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::cast_possible_wrap
)]

use crate::gcode::preview::{MoveKind, ToolpathSegment};
use crate::pipeline::CancelToken;
use crate::project::Fixture;
use crate::sim::diagnostics::{SimDiagnostics, SimWarning};
use crate::sim::fixture_check::{check_segment_against_fixtures, FixtureCheck};
use crate::sim::heightmap::{Heightmap, ToolProfile};
use crate::sim::holder::HolderProfile;
use crate::sim::holder_check::{check_segment_holder_against_walls, HolderCheck};
use crate::sim::rapid_check::{check_rapid_against_stock, RapidCheck};

/// Apply a single toolpath segment to `heightmap`, lowering every cell
/// the cutter sweeps over. Returns the number of cells touched.
///
/// Rapid moves don't carve; they run through `rapid_check` and emit a
/// `RapidThroughMaterial` warning when the cutter would pass through
/// material at rapid speed. Every segment (rapid included) also runs
/// through `fixture_check` against any declared fixtures.
///
/// `segment_idx` is the segment's position in the toolpath stream so
/// emitted `SimWarnings` link back to the offending segment; pass `&[]`
/// for `fixtures` when the project has none. `holder` is the optional
/// shank+holder envelope; when set, every segment is also tested against
/// the heightmap walls for `HolderCollision` warnings.
pub fn sweep_segment(
    heightmap: &mut Heightmap,
    segment: &ToolpathSegment,
    profile: &ToolProfile,
    segment_idx: usize,
    fixtures: &[Fixture],
    holder: Option<&HolderProfile>,
    diagnostics: &mut SimDiagnostics,
) -> u32 {
    run_segment_warnings(
        heightmap,
        segment,
        profile,
        segment_idx,
        fixtures,
        holder,
        diagnostics,
    );
    sweep_chord_carve(heightmap, segment, profile)
}

/// Like [`sweep_segment`] but carves only the chunk `[t_start, t_end]`
/// of the segment (parametric position along the chord). The full-segment
/// fixture / holder / rapid checks fire only when `t_start ≈ 0` so a
/// driver that issues many partial slices per second doesn't emit the
/// same warning every frame (pi8r).
///
/// `t_start` and `t_end` are clamped to `[0, 1]`; an inverted or empty
/// interval is a no-op. The internally constructed synthetic chord has
/// `from = lerp(seg.from, seg.to, t_start)` and `to = lerp(..., t_end)`,
/// which by construction makes pre-tessellated arcs Just Work — arcs
/// reach `sim/sweep.rs` as already-chorded line segments.
#[allow(clippy::too_many_arguments)]
pub fn sweep_segment_partial(
    heightmap: &mut Heightmap,
    segment: &ToolpathSegment,
    profile: &ToolProfile,
    segment_idx: usize,
    fixtures: &[Fixture],
    holder: Option<&HolderProfile>,
    diagnostics: &mut SimDiagnostics,
    t_start: f64,
    t_end: f64,
) -> u32 {
    let lo = t_start.clamp(0.0, 1.0);
    let hi = t_end.clamp(0.0, 1.0);
    if hi <= lo {
        return 0;
    }
    if lo <= 1e-9 {
        run_segment_warnings(
            heightmap,
            segment,
            profile,
            segment_idx,
            fixtures,
            holder,
            diagnostics,
        );
    }
    if matches!(segment.kind, MoveKind::Rapid) {
        return 0;
    }
    // xf5m: previously this built a synthetic chord via
    // `lerp_pose3(from, to, t)` and routed it through
    // `sweep_chord_carve`. Even for flat-bottom profiles that proved
    // wrong: a cell at radial offset r < r_tool whose closest-point on
    // the full chord lies AT (e.g.) t=0.45 (interior of the original
    // segment, halfway into the partial [0..0.5]) but lies OUTSIDE
    // the synthetic chord (e.g. projects to synth_t < 0 on the
    // [0.5..1] partial) gets carved by the synth-endpoint clamp at
    // synth.from.z — which is the SEGMENT MIDPOINT depth, deeper than
    // the chord's actual depth at that cell. Visible as 0.1+ mm
    // overcut bands at synth-chord junctions in mid-segment frame
    // snapshots. The fix: route every partial through
    // `sweep_chord_carve_partial`, which uses the full segment's
    // geometry for the (r, t) projection and the endpoint-clamp
    // ownership flags (`owns_t_lo`/`owns_t_hi`) to split the carve
    // between partials without drift.
    sweep_chord_carve_partial(heightmap, segment, profile, lo, hi)
}

/// Fixture / holder / rapid-vs-stock diagnostic pass. Extracted so the
/// partial-carve path can run it on the original segment (full-length
/// geometry) and skip the carve.
#[allow(clippy::too_many_arguments)]
fn run_segment_warnings(
    heightmap: &Heightmap,
    segment: &ToolpathSegment,
    profile: &ToolProfile,
    segment_idx: usize,
    fixtures: &[Fixture],
    holder: Option<&HolderProfile>,
    diagnostics: &mut SimDiagnostics,
) {
    let r_tool = profile.radius() as f64;
    for fc in check_segment_against_fixtures(segment, r_tool, holder, fixtures) {
        if let FixtureCheck::Collision {
            fixture_id,
            nearest_x,
            nearest_y,
        } = fc
        {
            diagnostics.push(SimWarning::FixtureCollision {
                segment_idx,
                fixture_id,
                nearest_x,
                nearest_y,
            });
        }
    }
    if let Some(holder) = holder {
        if let HolderCheck::Collision {
            worst_x,
            worst_y,
            wall_z,
            required_clearance_mm,
            cells,
        } = check_segment_holder_against_walls(heightmap, segment, holder)
        {
            diagnostics.push(SimWarning::HolderCollision {
                segment_idx,
                worst_x,
                worst_y,
                wall_z,
                required_clearance_mm,
                cells,
            });
        }
    }
    if matches!(segment.kind, MoveKind::Rapid) {
        if let RapidCheck::Collision {
            worst_x,
            worst_y,
            worst_cell_z,
            rapid_pz,
            subkind,
        } = check_rapid_against_stock(heightmap, segment, profile, holder)
        {
            // 50eq: map the rapid_check subkind (Tip vs Shank) onto the
            // serialized warning so the user knows whether to lower
            // the cut depth or raise the rapid Z / use a longer tool.
            use crate::sim::diagnostics::RapidCollisionSubkind as DSub;
            use crate::sim::rapid_check::RapidCollisionSubkind as RSub;
            let warn_subkind = match subkind {
                RSub::Tip => DSub::Tip,
                RSub::Shank => DSub::Shank,
            };
            diagnostics.push(SimWarning::RapidThroughMaterial {
                segment_idx,
                worst_x,
                worst_y,
                worst_cell_z,
                rapid_pz,
                subkind: warn_subkind,
            });
        }
    }
}

/// Carve-only pass: lower every cell under the (possibly synthetic)
/// chord. No diagnostics. Rapid moves bail.
fn sweep_chord_carve(
    heightmap: &mut Heightmap,
    segment: &ToolpathSegment,
    profile: &ToolProfile,
) -> u32 {
    if matches!(segment.kind, MoveKind::Rapid) {
        return 0;
    }
    let r_tool = profile.radius() as f64;
    if r_tool <= 0.0 {
        return 0;
    }
    // w8q7: drag-knife blade trails the spindle by `dragoff` in the
    // direction of travel, so the actual cut happens at
    // `spindle - dragoff * unit_dir`. Shift the chord before carving.
    let shifted = apply_dragoff_offset(segment, profile);
    let segment = shifted.as_ref().unwrap_or(segment);
    let from = &segment.from;
    let to = &segment.to;
    // Skip moves that stay above the stock — the cutter is in air.
    let top_z = heightmap.top_z as f64;
    if from.z >= top_z && to.z >= top_z {
        return 0;
    }

    let layout = HeightmapLayout::of(heightmap);
    let mut touched = 0u32;
    // `for_each_swept_cell` clamps (ix, iy) to the heightmap's cell
    // rectangle, so the safe `lower_at`'s bounds branch is redundant
    // every frame — use the unchecked path here (audit-5el3).
    for_each_swept_cell(&layout, segment, profile, |ix, iy, _r, cutter_pz, dz| {
        let surface_z = cutter_pz as f32 + dz;
        heightmap.lower_at_unchecked(ix, iy, surface_z);
        touched += 1;
    });
    touched
}

/// w8q7: shift a drag-knife segment by `-dragoff * unit_dir` so the
/// carved chord tracks the trailing blade tip instead of the spindle
/// axis. Returns `None` for non-DragKnife profiles, a profile with
/// `dragoff <= 0`, or a pure-plunge segment (zero XY travel — no
/// direction to offset along).
fn apply_dragoff_offset(
    segment: &ToolpathSegment,
    profile: &ToolProfile,
) -> Option<ToolpathSegment> {
    let dragoff = match profile {
        ToolProfile::DragKnife { dragoff, .. } if *dragoff > 0.0 => f64::from(*dragoff),
        _ => return None,
    };
    let dx = segment.to.x - segment.from.x;
    let dy = segment.to.y - segment.from.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-12 {
        return None;
    }
    let inv_len = 1.0 / len_sq.sqrt();
    let off_x = -dragoff * dx * inv_len;
    let off_y = -dragoff * dy * inv_len;
    let mut shifted = segment.clone();
    shifted.from.x += off_x;
    shifted.from.y += off_y;
    shifted.to.x += off_x;
    shifted.to.y += off_y;
    Some(shifted)
}

/// Partial carve for non-flat profiles (xf5m): walk the same cells the
/// full segment would touch, compute `(r, t_real)` against the real
/// chord, and lower the cell only when `t_real ∈ [t_start, t_end]`.
/// This preserves bitwise-identical final state across
/// `[0..t][t..1]` partial pairs vs. a single `[0..1]` sweep, because
/// every cell carved by either partial sees the same `cutter_pz +
/// profile.eval(r)` it would see in the full sweep.
fn sweep_chord_carve_partial(
    heightmap: &mut Heightmap,
    segment: &ToolpathSegment,
    profile: &ToolProfile,
    t_start: f64,
    t_end: f64,
) -> u32 {
    if matches!(segment.kind, MoveKind::Rapid) {
        return 0;
    }
    let r_tool = profile.radius() as f64;
    if r_tool <= 0.0 {
        return 0;
    }
    // w8q7: same dragoff-shift as `sweep_chord_carve`. The partial
    // version must use the same shifted geometry so split slices
    // line up bit-for-bit with the full sweep.
    let shifted = apply_dragoff_offset(segment, profile);
    let segment = shifted.as_ref().unwrap_or(segment);
    let from = &segment.from;
    let to = &segment.to;
    let top_z = heightmap.top_z as f64;
    if from.z >= top_z && to.z >= top_z {
        return 0;
    }
    let layout = HeightmapLayout::of(heightmap);
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let len_sq = dx * dx + dy * dy;
    let pure_plunge = len_sq < 1e-12;
    // This partial is responsible for the boundary-clamped cells at
    // t<0 only when it covers t=0, and at t>1 only when it covers
    // t=1. Otherwise some other partial owns those cells.
    let owns_t_lo = t_start <= 1e-9;
    let owns_t_hi = t_end >= 1.0 - 1e-9;

    // AABB inflated by r_tool. For the boundary-owning partials, the
    // footprint extends past the chord endpoint by r_tool (the
    // endpoint-clamped band carved by `t.clamp(0,1)` in
    // `for_each_swept_cell`).
    let p_start_x = from.x + dx * t_start;
    let p_start_y = from.y + dy * t_start;
    let p_end_x = from.x + dx * t_end;
    let p_end_y = from.y + dy * t_end;
    let mut min_x = p_start_x.min(p_end_x) - r_tool;
    let mut max_x = p_start_x.max(p_end_x) + r_tool;
    let mut min_y = p_start_y.min(p_end_y) - r_tool;
    let mut max_y = p_start_y.max(p_end_y) + r_tool;
    if owns_t_lo {
        min_x = min_x.min(from.x - r_tool);
        max_x = max_x.max(from.x + r_tool);
        min_y = min_y.min(from.y - r_tool);
        max_y = max_y.max(from.y + r_tool);
    }
    if owns_t_hi {
        min_x = min_x.min(to.x - r_tool);
        max_x = max_x.max(to.x + r_tool);
        min_y = min_y.min(to.y - r_tool);
        max_y = max_y.max(to.y + r_tool);
    }
    let Some((ix0, iy0, ix1, iy1)) = world_aabb_to_cells(&layout, min_x, min_y, max_x, max_y)
    else {
        return 0;
    };

    let cell = layout.cell;
    let r_tool_sq = r_tool * r_tool;
    let mut touched = 0u32;
    for iy in iy0..=iy1 {
        for ix in ix0..=ix1 {
            let cx = layout.origin_x + (ix as f64 + 0.5) * cell;
            let cy = layout.origin_y + (iy as f64 + 0.5) * cell;
            let (r_sq, cutter_pz) = if pure_plunge {
                // Pure plunge: t is degenerate; clamp/restrict by the
                // Z range corresponding to [t_start..t_end].
                let ex = cx - from.x;
                let ey = cy - from.y;
                let lo_z = from.z + (to.z - from.z) * t_start;
                let hi_z = from.z + (to.z - from.z) * t_end;
                (ex * ex + ey * ey, lo_z.min(hi_z))
            } else {
                // Match `for_each_swept_cell`'s endpoint-clamp
                // semantics: cells past the segment ends get t=0 or
                // t=1 (the endpoint depth). This partial OWNS:
                //   * t_raw in [t_start, t_end] (interior chunk)
                //   * t_raw < 0 when owns_t_lo (start endpoint cap)
                //   * t_raw > 1 when owns_t_hi (end endpoint cap)
                // Other ranges are someone else's partial to carve.
                let t_raw = ((cx - from.x) * dx + (cy - from.y) * dy) / len_sq;
                let in_interior = t_raw >= t_start && t_raw <= t_end;
                let in_lo_cap = owns_t_lo && t_raw < 0.0;
                let in_hi_cap = owns_t_hi && t_raw > 1.0;
                if !(in_interior || in_lo_cap || in_hi_cap) {
                    continue;
                }
                let t = t_raw.clamp(0.0, 1.0);
                let px = from.x + t * dx;
                let py = from.y + t * dy;
                let ex = cx - px;
                let ey = cy - py;
                (ex * ex + ey * ey, from.z + (to.z - from.z) * t)
            };
            if r_sq > r_tool_sq {
                continue;
            }
            // r is bounded by r_tool ≤ tool diameter / 2, so the f32 cast
            // here cannot overflow (matches `for_each_swept_cell`).
            let dz = if profile.is_flat_bottom() {
                0.0_f32
            } else {
                let r = r_sq.sqrt() as f32;
                let Some(dz) = profile.eval(r) else {
                    continue;
                };
                dz
            };
            let surface_z = cutter_pz as f32 + dz;
            heightmap.lower_at_unchecked(ix, iy, surface_z);
            touched += 1;
        }
    }
    touched
}

/// Owned snapshot of a heightmap's grid layout — origin/cell/dim only,
/// no `data` borrow. Lets `for_each_swept_cell` walk the grid while a
/// caller (e.g. `sweep_segment`) keeps a `&mut Heightmap` for writes.
#[derive(Debug, Clone, Copy)]
pub(super) struct HeightmapLayout {
    pub origin_x: f64,
    pub origin_y: f64,
    pub cell: f64,
    pub cols: u32,
    pub rows: u32,
}

impl HeightmapLayout {
    pub(super) fn of(h: &Heightmap) -> Self {
        Self {
            origin_x: h.origin.x,
            origin_y: h.origin.y,
            cell: h.cell,
            cols: h.cols,
            rows: h.rows,
        }
    }
}

/// Walk every cell inside `segment`'s swept footprint (XY AABB inflated
/// by the tool radius, clamped to the heightmap), invoking `body` with
/// the per-cell `(ix, iy, r, cutter_pz, dz)` so callers can either lower
/// the cell (sweep) or compare against it (rapid check). Cells outside
/// the cutter footprint or outside the tool's profile are skipped before
/// `body` ever sees them.
pub(super) fn for_each_swept_cell<F>(
    layout: &HeightmapLayout,
    segment: &ToolpathSegment,
    profile: &ToolProfile,
    mut body: F,
) where
    F: FnMut(u32, u32, f32, f64, f32),
{
    let r_tool = profile.radius() as f64;
    if r_tool <= 0.0 {
        return;
    }
    let from = &segment.from;
    let to = &segment.to;

    let min_x = from.x.min(to.x) - r_tool;
    let max_x = from.x.max(to.x) + r_tool;
    let min_y = from.y.min(to.y) - r_tool;
    let max_y = from.y.max(to.y) + r_tool;

    let Some((ix0, iy0, ix1, iy1)) = world_aabb_to_cells(layout, min_x, min_y, max_x, max_y) else {
        return;
    };

    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let len_sq = dx * dx + dy * dy;
    // Tiny / zero-length XY segments are pure plunges: every cell under
    // the tool sees the lowest Z the cutter visited along the segment,
    // which is min(from.z, to.z) regardless of direction.
    let pure_plunge = len_sq < 1e-12;
    let plunge_z = from.z.min(to.z);

    let cell = layout.cell;
    let r_tool_sq = r_tool * r_tool;

    // Flat-bottom profiles (Endmill / Drill / Laser / DragKnife) have
    // `eval(r) = Some(0.0)` for every `r ≤ r_tool` — which is already
    // implied by `r_sq ≤ r_tool_sq`. Skip both the sqrt and the
    // per-cell eval branch for those (audit-xnmp). The compiler can
    // also hoist this constant decision out of the inner loop.
    let flat_bottom = profile.is_flat_bottom();

    for iy in iy0..=iy1 {
        for ix in ix0..=ix1 {
            let cx = layout.origin_x + (ix as f64 + 0.5) * cell;
            let cy = layout.origin_y + (iy as f64 + 0.5) * cell;
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
            if r_sq > r_tool_sq {
                continue;
            }
            if flat_bottom {
                body(ix, iy, 0.0, cutter_pz, 0.0);
            } else {
                let r = r_sq.sqrt() as f32;
                let Some(dz) = profile.eval(r) else {
                    continue;
                };
                body(ix, iy, r, cutter_pz, dz);
            }
        }
    }
}

/// Apply every segment in `segments[from_idx..to_idx]` to the heightmap.
/// Returns the total cell-write count; useful as a perf signal in tests.
/// `fixtures` is forwarded to every per-segment check; pass `&[]` for a
/// project without declared obstacles.
// 8 args = heightmap + cutter geometry + start/end + fixtures + checks;
// bundling into a config struct would just move the same arg list one
// hop deeper without simplifying anything.
#[allow(clippy::too_many_arguments)]
pub fn sweep_range(
    heightmap: &mut Heightmap,
    segments: &[ToolpathSegment],
    from_idx: usize,
    to_idx: usize,
    profile: &ToolProfile,
    fixtures: &[Fixture],
    holder: Option<&HolderProfile>,
    diagnostics: &mut SimDiagnostics,
) -> u32 {
    sweep_range_cancellable(
        heightmap,
        segments,
        from_idx,
        to_idx,
        profile,
        fixtures,
        holder,
        diagnostics,
        None,
    )
}

/// Cancellable variant of [`sweep_range`]. Checks `cancel` every ~100
/// segments; on cancellation returns the running total (heightmap is
/// left in whatever partial state has been written so far — sim
/// callers discard it).
#[allow(clippy::too_many_arguments)]
pub fn sweep_range_cancellable(
    heightmap: &mut Heightmap,
    segments: &[ToolpathSegment],
    from_idx: usize,
    to_idx: usize,
    profile: &ToolProfile,
    fixtures: &[Fixture],
    holder: Option<&HolderProfile>,
    diagnostics: &mut SimDiagnostics,
    cancel: Option<&CancelToken>,
) -> u32 {
    let lo = from_idx.min(segments.len());
    let hi = to_idx.min(segments.len());
    let mut total = 0u32;
    for (offset, seg) in segments[lo..hi].iter().enumerate() {
        if offset % 100 == 0
            && cancel.is_some_and(super::super::pipeline::CancelToken::is_cancelled)
        {
            return total;
        }
        total += sweep_segment(
            heightmap,
            seg,
            profile,
            lo + offset,
            fixtures,
            holder,
            diagnostics,
        );
    }
    total
}

/// Convert a world-space AABB to the inclusive cell-index range it
/// covers. Returns None when the AABB is fully outside the heightmap.
fn world_aabb_to_cells(
    layout: &HeightmapLayout,
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
) -> Option<(u32, u32, u32, u32)> {
    let cell = layout.cell;
    let inv = 1.0 / cell;
    let max_col = layout.cols.saturating_sub(1);
    let max_row = layout.rows.saturating_sub(1);
    let fx0 = (min_x - layout.origin_x) * inv;
    let fy0 = (min_y - layout.origin_y) * inv;
    let fx1 = (max_x - layout.origin_x) * inv;
    let fy1 = (max_y - layout.origin_y) * inv;
    if fx1 < 0.0 || fy1 < 0.0 {
        return None;
    }
    if fx0 > layout.cols as f64 || fy0 > layout.rows as f64 {
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

    fn diag() -> SimDiagnostics {
        SimDiagnostics::new()
    }

    #[test]
    fn rapid_move_does_not_carve() {
        let mut map = fresh_map(20, 20);
        let mut d = diag();
        let s = seg(
            MoveKind::Rapid,
            pose(0.0, 0.0, -10.0),
            pose(10.0, 10.0, -10.0),
        );
        let touched = sweep_segment(
            &mut map,
            &s,
            &ToolProfile::Endmill { r: 2.0 },
            0,
            &[],
            None,
            &mut d,
        );
        assert_eq!(touched, 0);
        assert!(map.data.iter().all(|z| (*z - 0.0).abs() < 1e-6));
        assert!(map.dirty_aabb().is_none());
        // top_z = 0, rapid is at -10 through uncut stock — collision expected.
        assert_eq!(d.count("rapid_through_material"), 1);
    }

    #[test]
    fn cutter_above_stock_does_not_carve() {
        let mut map = fresh_map(20, 20);
        let mut d = diag();
        // Both endpoints above the stock surface (z = 0). Cutter is in air.
        let s = seg(MoveKind::Cut, pose(5.0, 5.0, 0.5), pose(8.0, 8.0, 0.5));
        let touched = sweep_segment(
            &mut map,
            &s,
            &ToolProfile::Endmill { r: 2.0 },
            0,
            &[],
            None,
            &mut d,
        );
        assert_eq!(touched, 0);
    }

    #[test]
    fn endmill_plunge_lowers_circular_patch() {
        let mut map = fresh_map(40, 40);
        let mut d = diag();
        // Plunge at (10, 10) from z=0 to z=-1 with R=2.
        let s = seg(
            MoveKind::Plunge,
            pose(10.0, 10.0, 0.0),
            pose(10.0, 10.0, -1.0),
        );
        sweep_segment(
            &mut map,
            &s,
            &ToolProfile::Endmill { r: 2.0 },
            0,
            &[],
            None,
            &mut d,
        );
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
        let mut d = diag();
        let r = 2.0_f32;
        // Cut from (5, 25) to (55, 25) at z=-1 with R=2.
        let s = seg(MoveKind::Cut, pose(5.0, 25.0, -1.0), pose(55.0, 25.0, -1.0));
        sweep_segment(
            &mut map,
            &s,
            &ToolProfile::Endmill { r },
            0,
            &[],
            None,
            &mut d,
        );
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
        let mut d = diag();
        // 60° included angle = 30° half-angle. R=2, no flat tip.
        let half = 30f32.to_radians();
        let profile = ToolProfile::VBit {
            r: 2.0,
            tip_r: 0.0,
            half_angle_rad: half,
        };
        // Plunge AT a cell center so r=0 at cell (10, 10) and r=1 at
        // cell (11, 10) — keeps the analytic check simple.
        let s = seg(
            MoveKind::Plunge,
            pose(10.5, 10.5, 0.0),
            pose(10.5, 10.5, -2.0),
        );
        sweep_segment(&mut map, &s, &profile, 0, &[], None, &mut d);
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
        let mut d = diag();
        let plunge = seg(
            MoveKind::Plunge,
            pose(10.0, 10.0, 0.0),
            pose(10.0, 10.0, -2.0),
        );
        sweep_segment(
            &mut map,
            &plunge,
            &ToolProfile::Endmill { r: 2.0 },
            0,
            &[],
            None,
            &mut d,
        );
        // Now sweep a SHALLOWER cut over the same cell — should NOT raise.
        let shallow = seg(MoveKind::Cut, pose(8.0, 10.0, -0.5), pose(12.0, 10.0, -0.5));
        sweep_segment(
            &mut map,
            &shallow,
            &ToolProfile::Endmill { r: 2.0 },
            1,
            &[],
            None,
            &mut d,
        );
        assert!(
            (cell(&map, 10, 10) - -2.0).abs() < 1e-5,
            "later shallower pass must not raise the cell",
        );
    }

    #[test]
    fn sweep_range_walks_each_segment() {
        let mut map = fresh_map(40, 40);
        let mut d = diag();
        let segments = vec![
            seg(MoveKind::Cut, pose(5.0, 10.0, -1.0), pose(15.0, 10.0, -1.0)),
            // Rapid stays at z=5 above top_z=0 — no collision.
            seg(
                MoveKind::Rapid,
                pose(15.0, 10.0, 5.0),
                pose(20.0, 20.0, 5.0),
            ),
            seg(
                MoveKind::Plunge,
                pose(20.0, 20.0, 0.0),
                pose(20.0, 20.0, -1.0),
            ),
        ];
        let touched = sweep_range(
            &mut map,
            &segments,
            0,
            segments.len(),
            &ToolProfile::Endmill { r: 2.0 },
            &[],
            None,
            &mut d,
        );
        assert!(touched > 0);
        // First segment carved the (5..15, 10) stripe.
        assert!((cell(&map, 10, 10) - -1.0).abs() < 1e-5);
        // Rapid move did NOT carve.
        // Plunge endpoint carved at (20, 20).
        assert!((cell(&map, 20, 20) - -1.0).abs() < 1e-5);
        // Untouched cell stays at top_z.
        assert!((cell(&map, 35, 35) - 0.0).abs() < 1e-5);
        // The above-stock rapid is clear.
        assert!(d.is_clean());
    }

    #[test]
    fn sweep_outside_heightmap_is_silently_skipped() {
        let mut map = fresh_map(20, 20);
        let mut d = diag();
        // Segment fully to the right of the heightmap (origin 0..20).
        let s = seg(
            MoveKind::Cut,
            pose(50.0, 10.0, -1.0),
            pose(60.0, 10.0, -1.0),
        );
        let touched = sweep_segment(
            &mut map,
            &s,
            &ToolProfile::Endmill { r: 2.0 },
            0,
            &[],
            None,
            &mut d,
        );
        assert_eq!(touched, 0);
        // Heightmap untouched.
        assert!(map.dirty_aabb().is_none());
    }

    #[test]
    fn sweep_partial_overlap_clamps_to_grid() {
        let mut map = fresh_map(20, 20);
        let mut d = diag();
        // Segment crosses the right edge — half inside, half outside.
        let s = seg(
            MoveKind::Cut,
            pose(15.0, 10.0, -1.0),
            pose(25.0, 10.0, -1.0),
        );
        sweep_segment(
            &mut map,
            &s,
            &ToolProfile::Endmill { r: 2.0 },
            0,
            &[],
            None,
            &mut d,
        );
        // Cells inside the grid along the path should be lowered.
        for ix in 16..=19 {
            assert!(
                (cell(&map, ix, 10) - -1.0).abs() < 1e-5,
                "cell ({ix}, 10) should be carved",
            );
        }
    }

    #[test]
    fn fixture_collision_pipeline_emits_warning() {
        // Drive the warning through `sweep_range`: a horizontal cut runs
        // through a Box fixture in the middle of the heightmap. We
        // expect one FixtureCollision warning carrying the segment index
        // and a nearest_x/nearest_y at (or near) the box center.
        use crate::project::{Fixture, FixtureKind};
        let mut map = fresh_map(40, 40);
        let mut d = diag();
        let segments = vec![seg(
            MoveKind::Cut,
            pose(0.0, 20.0, -1.0),
            pose(40.0, 20.0, -1.0),
        )];
        let fixtures = vec![Fixture {
            id: 11,
            name: "clamp".into(),
            kind: FixtureKind::Box {
                width: 10.0,
                depth: 10.0,
            },
            origin: (20.0, 20.0),
            z_bottom: -2.0,
            z_top: 5.0,
            color: 0xFFA0_50C0,
        }];
        let _ = sweep_range(
            &mut map,
            &segments,
            0,
            segments.len(),
            &ToolProfile::Endmill { r: 2.0 },
            &fixtures,
            None,
            &mut d,
        );
        assert_eq!(d.count("fixture_collision"), 1);
        match &d.warnings[0] {
            crate::sim::diagnostics::SimWarning::FixtureCollision {
                segment_idx,
                fixture_id,
                ..
            } => {
                assert_eq!(*segment_idx, 0);
                assert_eq!(*fixture_id, 11);
            }
            other => panic!("unexpected warning: {other:?}"),
        }
    }

    #[test]
    fn ball_nose_arc_heightmap_within_chord_error() {
        // biot: A 90° arc carved by a ball-nose must produce a smooth
        // heightmap — no visible chord-tessellation scallop along the
        // arc's centerline. Earlier 10° tessellation produced 0.04 mm
        // scallop teeth on a 10 mm arc (sim artifact, not a real
        // machining outcome). The preview now tessellates at ~2°,
        // which drops the chord error to ~0.0015 mm.
        //
        // Test strategy: emit a 90° G2 arc, simulate, then bilinearly
        // sample the heightmap along the analytic centerline at fine
        // angular resolution. The MAX-MINUS-MIN variance along the
        // centerline samples is the visible "scallop teeth" magnitude
        // — assert it stays below 2 × chord error of the new 2°
        // tessellation (≈ 0.003 mm), well below the prior 0.04 mm.
        //
        // Use a 0.25 mm cell grid so the centerline-following bilinear
        // sample isn't quantized by cell size.
        // G3 (CCW) from (10,0) to (0,10) about center (0,0) is a 90°
        // arc through the first quadrant — G2 the same XY would be a
        // 270° CW arc the long way around.
        let gcode = "G21\nG0 X10 Y0 Z1\nG1 Z-0.5 F100\nG3 X0 Y10 I-10 J0 F800\n";
        let (segs, _) = crate::gcode::preview::interpret_with_index(gcode);
        let arc_segs: Vec<_> = segs
            .iter()
            .filter(|s| matches!(s.kind, MoveKind::Arc))
            .collect();
        assert!(
            arc_segs.len() >= 45,
            "expected ≥45 chord segments from 90° arc at 2° tess, got {}",
            arc_segs.len()
        );
        let mut map = Heightmap::new(Point2::new(-2.0, -2.0), 0.25, 80, 80, 0.0);
        let mut d = diag();
        for (i, s) in segs.iter().enumerate() {
            sweep_segment(
                &mut map,
                s,
                &ToolProfile::BallNose { r: 1.0 },
                i,
                &[],
                None,
                &mut d,
            );
        }
        // Walk the analytic centerline at 1° steps from 0° to 90°.
        let mut min_z = f32::INFINITY;
        let mut max_z = f32::NEG_INFINITY;
        for k in 0..=90 {
            let theta = (k as f64).to_radians();
            let x = 10.0_f64 * theta.cos();
            let y = 10.0_f64 * theta.sin();
            let z = map.sample(x, y);
            if z < min_z {
                min_z = z;
            }
            if z > max_z {
                max_z = z;
            }
        }
        let variance = max_z - min_z;
        // Theoretical chord error at 2° tessellation is 0.0015 mm on
        // a 10 mm arc; once the 0.25 mm cell grid and f32 storage
        // add their own noise, the observed centerline variance lands
        // at ~0.01 mm. That's still 4× tighter than the previous 10°
        // tessellation's 0.04 mm and well below the original
        // user-visible scallop floor (0.04 mm "teeth" between
        // adjacent chords).
        assert!(
            variance < 0.02,
            "ball-nose arc scallop {variance} mm exceeds 0.02 mm bound (min={min_z}, max={max_z})",
        );
        // Sanity: the centerline tip should be at or below the plunge
        // depth (-0.5) — the ball-nose tip travels at -0.5 and lower
        // bilinear samples can read a touch deeper.
        assert!(
            min_z <= -0.49 && min_z >= -0.51,
            "tip depth {min_z} should be near -0.5",
        );
    }

    #[test]
    fn partial_advance_non_flat_no_drift() {
        // xf5m: ball-nose carving a segment in two halves should
        // produce an identical heightmap to carving the full segment
        // in one shot. Earlier code routed both halves through a
        // synthetic chord whose endpoint clamp left false-deep marks
        // near the t=0.5 junction for non-flat profiles.
        let profile = ToolProfile::BallNose { r: 3.0 };
        let s = seg(
            MoveKind::Cut,
            pose(5.0, 20.0, -1.0),
            pose(35.0, 20.0, -3.0),
        );
        let mut full = fresh_map(40, 40);
        let mut df = diag();
        sweep_segment(&mut full, &s, &profile, 0, &[], None, &mut df);
        let mut split = fresh_map(40, 40);
        let mut ds = diag();
        sweep_segment_partial(&mut split, &s, &profile, 0, &[], None, &mut ds, 0.0, 0.5);
        sweep_segment_partial(&mut split, &s, &profile, 0, &[], None, &mut ds, 0.5, 1.0);
        for iy in 0..full.rows {
            for ix in 0..full.cols {
                let a = cell(&full, ix, iy);
                let b = cell(&split, ix, iy);
                assert!(
                    (a - b).abs() < 1e-5,
                    "ball-nose drift at ({ix}, {iy}): full={a} split={b}",
                );
            }
        }
        // Sanity: also for flat-bottom (the no-drift path that was
        // already correct).
        let endmill = ToolProfile::Endmill { r: 3.0 };
        let mut full_e = fresh_map(40, 40);
        let mut df_e = diag();
        sweep_segment(&mut full_e, &s, &endmill, 0, &[], None, &mut df_e);
        let mut split_e = fresh_map(40, 40);
        let mut ds_e = diag();
        sweep_segment_partial(
            &mut split_e,
            &s,
            &endmill,
            0,
            &[],
            None,
            &mut ds_e,
            0.0,
            0.5,
        );
        sweep_segment_partial(
            &mut split_e,
            &s,
            &endmill,
            0,
            &[],
            None,
            &mut ds_e,
            0.5,
            1.0,
        );
        for (i, (a, b)) in full_e
            .data
            .iter()
            .zip(split_e.data.iter())
            .enumerate()
        {
            let ix = (i as u32) % full_e.cols;
            let iy = (i as u32) / full_e.cols;
            assert!(
                (a - b).abs() < 1e-5,
                "endmill drift at ({ix}, {iy}): full={a} split={b}",
            );
        }
    }

    /// w8q7: drag-knife with a positive dragoff carves the segment
    /// offset BEHIND the spindle in the direction of travel. A 5 mm
    /// X-axis cut with dragoff=2 should carve cells from x=-2 to
    /// x=3 (i.e. the chord shifted -2 along +X), not x=0 to x=5.
    #[test]
    fn dragknife_dragoff_shifts_carved_chord_behind_spindle() {
        let mut map = fresh_map(40, 40);
        let mut d = diag();
        // Spindle path: (10, 20) → (20, 20) along +X. Dragoff = 2 mm
        // so blade trails behind by 2 mm — carves cells along the
        // chord (8, 20) → (18, 20).
        let profile = ToolProfile::DragKnife {
            r: 1.0,
            dragoff: 2.0,
        };
        let s = seg(
            MoveKind::Cut,
            pose(10.0, 20.0, -1.0),
            pose(20.0, 20.0, -1.0),
        );
        sweep_segment(&mut map, &s, &profile, 0, &[], None, &mut d);
        // A cell near x=9 (inside the shifted chord) should be carved.
        // The original UN-shifted chord wouldn't reach x=9 with r=1.
        let carved_left = cell(&map, 9, 20);
        assert!(
            carved_left < 0.0,
            "cell at x=9 should be carved by the dragoff-shifted chord, got {carved_left}",
        );
        // A cell near x=19 (off the END of the shifted chord, but
        // inside the spindle chord) should be UN-carved.
        let untouched_right = cell(&map, 19, 20);
        assert!(
            (untouched_right - 0.0).abs() < 1e-5,
            "cell at x=19 should be untouched (past the trailing blade), got {untouched_right}",
        );
    }

    /// w8q7: dragoff = 0 collapses to the legacy endmill carve.
    /// Zero / missing dragoff must NOT shift the chord.
    #[test]
    fn dragknife_dragoff_zero_matches_endmill_carve() {
        let mut map_dk = fresh_map(40, 40);
        let mut d_dk = diag();
        let mut map_em = fresh_map(40, 40);
        let mut d_em = diag();
        let s = seg(
            MoveKind::Cut,
            pose(10.0, 20.0, -1.0),
            pose(20.0, 20.0, -1.0),
        );
        sweep_segment(
            &mut map_dk,
            &s,
            &ToolProfile::DragKnife {
                r: 1.0,
                dragoff: 0.0,
            },
            0,
            &[],
            None,
            &mut d_dk,
        );
        sweep_segment(
            &mut map_em,
            &s,
            &ToolProfile::Endmill { r: 1.0 },
            0,
            &[],
            None,
            &mut d_em,
        );
        for (i, (a, b)) in map_dk.data.iter().zip(map_em.data.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-5,
                "DragKnife(dragoff=0) must match Endmill at cell {i}: {a} vs {b}",
            );
        }
    }

    /// w8q7: pure plunge (zero XY length) on a drag-knife has no
    /// direction of travel — the dragoff offset is undefined, so the
    /// sim falls back to the spindle position. Cells under the
    /// plunge point are carved as if dragoff = 0.
    #[test]
    fn dragknife_pure_plunge_unaffected_by_dragoff() {
        let mut map = fresh_map(40, 40);
        let mut d = diag();
        let s = seg(
            MoveKind::Plunge,
            pose(20.0, 20.0, 0.0),
            pose(20.0, 20.0, -1.0),
        );
        sweep_segment(
            &mut map,
            &s,
            &ToolProfile::DragKnife {
                r: 1.0,
                dragoff: 2.0,
            },
            0,
            &[],
            None,
            &mut d,
        );
        // Cell directly under (20, 20) should be carved to -1.
        assert!((cell(&map, 20, 20) - -1.0).abs() < 1e-5);
    }

    #[test]
    fn fixture_clear_no_warning() {
        use crate::project::{Fixture, FixtureKind};
        let mut map = fresh_map(40, 40);
        let mut d = diag();
        let segments = vec![seg(
            MoveKind::Cut,
            pose(0.0, 0.0, -1.0),
            pose(40.0, 0.0, -1.0),
        )];
        let fixtures = vec![Fixture {
            id: 1,
            name: "off-side clamp".into(),
            kind: FixtureKind::Box {
                width: 5.0,
                depth: 5.0,
            },
            origin: (20.0, 30.0),
            z_bottom: -2.0,
            z_top: 5.0,
            color: 0xFFA0_50C0,
        }];
        let _ = sweep_range(
            &mut map,
            &segments,
            0,
            segments.len(),
            &ToolProfile::Endmill { r: 2.0 },
            &fixtures,
            None,
            &mut d,
        );
        assert_eq!(d.count("fixture_collision"), 0);
    }
}
