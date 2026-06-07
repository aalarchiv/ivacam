//! Cut-direction / winding enforcement and approach-point rotation. Split
//! out of `offsets.rs` (6yst). Owns the approach-point far-rotation
//! diagnostic sink (drained by the parent's `OffsetDiagnostics`).

use super::{offset_signed_area, reverse_offset, PolylineOffset};
use crate::geometry::Point2;

/// Side of the workpiece the cutter sits on for a given offset:
/// * `Outer` — cutter is outside the part (external profile, or walking
///   around a pocket island).
/// * `Inner` — cutter is inside the part / pocket (pocket boundary,
///   pocket cascade ring, internal profile).
/// * `Skip` — winding doesn't matter (Engrave / `DragKnife` / Profile-On).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutContext {
    Outer,
    Inner,
    Skip,
}

/// Apply a desired cut direction to a closed offset by reversing its
/// traversal if the resulting winding doesn't match the convention.
///
/// For a right-hand spindle (standard CW from above, `SpindleDirection::Cw`):
///
/// |  context |   conventional   |     climb        |
/// |----------|------------------|------------------|
/// |  outer   |  CW (area < 0)   |  CCW (area > 0)  |
/// |  inner   |  CCW (area > 0)  |  CW (area < 0)   |
///
/// The "outer" and "inner" labels refer to where the *cutter* sits, not
/// the geometry's role in the program. A cutter walking around the
/// outside of a part = Outer; walking inside a pocket = Inner; walking
/// around an island inside a pocket = Outer (the cutter is outside the
/// island).
///
/// q57s: for a LEFT-hand spindle (`SpindleDirection::Ccw`, M4 mode — left-
/// hand cutter, mirror tooling), climb and conventional are physically
/// flipped because the cutting edge rotates the other way. The truth table
/// above is XOR'd with the spindle bit so that the requested intent
/// ("climb" / "conventional") matches the physical cut on either spindle.
/// Pre-q57s, climb-vs-conventional was silently inverted on M4 spindles —
/// "climb" picked CCW geometry on inner-pocket regardless of which way
/// the cutter was rotating.
pub fn enforce_winding(
    offset: &mut PolylineOffset,
    context: CutContext,
    direction: crate::project::CutDirection,
    spindle: crate::project::tool::SpindleDirection,
) {
    use crate::project::tool::SpindleDirection;
    use crate::project::CutDirection;
    if !offset.closed || matches!(context, CutContext::Skip) {
        return;
    }
    let area = offset_signed_area(offset);
    if area.abs() < 1e-9 {
        return;
    }
    // Geometric want_ccw for a right-hand (CW) spindle.
    let want_ccw_rh = match (context, direction) {
        (CutContext::Inner, CutDirection::Conventional) => true,
        (CutContext::Inner, CutDirection::Climb) => false,
        (CutContext::Outer, CutDirection::Conventional) => false,
        (CutContext::Outer, CutDirection::Climb) => true,
        (CutContext::Skip, _) => return,
    };
    // q57s: flip the geometric winding for left-hand spindles so the
    // physical chipload direction matches the user's climb/conventional
    // intent regardless of M3/M4.
    let want_ccw = match spindle {
        SpindleDirection::Cw => want_ccw_rh,
        SpindleDirection::Ccw => !want_ccw_rh,
    };
    let is_ccw = area > 0.0;
    if is_ccw != want_ccw {
        reverse_offset(offset);
    }
}

/// kzz9: any closed offset whose nearest segment-start lands more than
/// [`APPROACH_POINT_WARN_MM`] from the user-picked approach point gets
/// rotated anyway (preserving the prior behaviour), but the distance is
/// recorded in this thread-local so the per-op driver can surface a
/// `rotate_offsets_far_from_approach` warning. Typical cause: stale
/// approach point left over after the user moved the source contour.
#[derive(Debug, Clone)]
pub struct ApproachPointFarRotation {
    pub distance_mm: f64,
    pub approach: (f64, f64),
}

thread_local! {
    static APPROACH_POINT_FAR: std::cell::RefCell<Vec<ApproachPointFarRotation>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain (and clear) any far-approach-point records stashed by
/// [`rotate_offsets_to_approach_point`] on this thread.
#[must_use]
pub(super) fn take_approach_point_far_rotations() -> Vec<ApproachPointFarRotation> {
    APPROACH_POINT_FAR.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// kzz9: distance threshold (mm) above which [`rotate_offsets_to_approach_point`]
/// records a far-rotation event. The chosen value is a rule-of-thumb
/// — most users place the approach point right on the boundary, so any
/// hit > 10 mm is almost certainly stale geometry (the user moved the
/// shape after picking the approach point).
pub const APPROACH_POINT_WARN_MM: f64 = 10.0;

/// Rotate each CLOSED offset's segment list so the first segment's
/// start is closest to `ap` (rt1.26 / Estlcam Anfahrpunkt). Open
/// offsets (zigzag / spiral / trochoidal strokes) are left alone —
/// their winding has no rotational symmetry to exploit. The cutter's
/// plunge / lead-in then happens at the user-picked entry XY.
///
/// kzz9: when the chosen `ap` ends up farther than
/// [`APPROACH_POINT_WARN_MM`] from EVERY closed offset's nearest vertex
/// the rotation still falls back to the nearest start, but a record is
/// stashed in the thread-local drained by
/// [`take_approach_point_far_rotations`]. The pipeline turns that into
/// a `rotate_offsets_far_from_approach` warning attributed to the op.
pub fn rotate_offsets_to_approach_point(offsets: &mut [PolylineOffset], ap: (f64, f64)) {
    let ap_pt = Point2::new(ap.0, ap.1);
    let mut min_d_overall = f64::INFINITY;
    for offset in offsets.iter_mut() {
        if !offset.closed || offset.segments.len() < 2 {
            continue;
        }
        let mut best: Option<(usize, f64)> = None;
        for (i, seg) in offset.segments.iter().enumerate() {
            let d = seg.start.distance(ap_pt);
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        if let Some((i, d)) = best {
            if d < min_d_overall {
                min_d_overall = d;
            }
            if i > 0 {
                offset.segments.rotate_left(i);
            }
        }
    }
    if min_d_overall.is_finite() && min_d_overall > APPROACH_POINT_WARN_MM {
        APPROACH_POINT_FAR.with(|s| {
            s.borrow_mut().push(ApproachPointFarRotation {
                distance_mm: min_d_overall,
                approach: ap,
            });
        });
    }
}

/// Walk a per-op offset list and enforce climb/conventional on each
/// closed offset. The op's main `cut_direction` applies to roughing
/// passes (cascade level ≥ 1); the `finish_direction` applies to the
/// finishing pass (level = 0 — the offset that defines the wall
/// surface).
///
/// Context is derived from the op kind and per-offset `signed_area`:
/// * Profile + `ToolOffset::Outside` → all offsets are Outer
/// * Profile + `ToolOffset::Inside`  → all offsets are Inner
/// * Profile + `ToolOffset::On/None` → Skip (no winding choice)
/// * Pocket → CCW offsets are Inner (cutter inside the pocket), CW
///   offsets are Outer (cutter going around an island)
/// * Engrave / `DragKnife` → Skip
pub fn apply_cut_direction(
    offsets: &mut [PolylineOffset],
    op: &crate::project::Op,
    finish_default_for_outside_profile_only: bool,
    spindle: crate::project::tool::SpindleDirection,
) {
    use crate::project::OpKind;
    use crate::project::ToolOffset;
    let _ = finish_default_for_outside_profile_only; // currently unused; kept for future hook
                                                     // kbx5 step 2: cut directions live on ContourParams. Non-contour
                                                     // ops fall back to Conventional (the existing default).
    let (main, finish) = op.contour_params().map_or(
        (
            crate::project::CutDirection::Conventional,
            crate::project::CutDirection::Conventional,
        ),
        |c| (c.cut_direction, c.finish_cut_direction),
    );
    let context_for = |offset: &PolylineOffset| -> CutContext {
        match &op.kind {
            OpKind::Profile {
                offset: tool_offset,
                ..
            } => match tool_offset {
                ToolOffset::Outside => CutContext::Outer,
                ToolOffset::Inside => CutContext::Inner,
                ToolOffset::None | ToolOffset::On => CutContext::Skip,
            },
            OpKind::Pocket { .. } => {
                if offset_signed_area(offset) > 0.0 {
                    CutContext::Inner
                } else {
                    CutContext::Outer
                }
            }
            OpKind::Engrave { .. }
            | OpKind::DragKnife { .. }
            | OpKind::Drill { .. }
            | OpKind::Thread { .. }
            | OpKind::Chamfer { .. }
            | OpKind::Helix
            // 8n4k / rxm9: program-only kinds (Pause / Homing /
            // Probe / CycleMarker / GcodeInclude) never reach this
            // winding pass — they emit inline above run_per_op's
            // body marker — but list them explicitly so a future
            // kind doesn't fall through to a stale arm.
            | OpKind::Pause { .. }
            | OpKind::Homing { .. }
            | OpKind::Probe { .. }
            | OpKind::CycleMarker { .. }
            | OpKind::GcodeInclude { .. }
            | OpKind::VCarve { .. }
            // 3g6u/b7qz: T-slot and dovetail ride the centerline (no
            // inside/outside winding to enforce) just like Engrave.
            | OpKind::TSlot { .. }
            | OpKind::Dovetail { .. }
            // f60x: relief surfacing has its own drop-cutter driver and
            // never enters the offset cascade — no winding to enforce.
            | OpKind::ReliefMill { .. }
            // rt1.12: raster engrave has its own scanline driver; no
            // vector winding to enforce.
            | OpKind::RasterEngrave { .. } => CutContext::Skip,
        }
    };
    for offset in offsets.iter_mut() {
        let ctx = context_for(offset);
        // level=0 is the wall-defining pass for both Pocket and Profile
        // (single-pass profile is itself the finishing pass).
        let dir = if offset.level == 0 { finish } else { main };
        enforce_winding(offset, ctx, dir, spindle);
    }
}
