//! Interactive tab placement helpers (rt1.10).
//!
//! Tabs ("bridges" / "Anbindungen" in Estlcam) are short uncut sections
//! that hold the workpiece to the stock during a through-cut. wiac
//! stores them as `(object_id, t)` where `t ∈ [0, 1)` is the
//! arc-length parameter along the chained object's segments. The
//! parameter form survives rotations / translations / scales of the
//! source geometry — Estlcam stores raw `(x, y)` and recomputes the
//! parameter at gcode time (`_I.P_Pos_Proz`); the wiac model bakes
//! the parameter in so transforms can't strand a tab off-contour.
//!
//! ## What lives here
//!
//! Pure-math helpers on densified polylines. Frontend mirrors the
//! same projection logic in TypeScript so the ghost-tab cursor lines
//! up exactly with what the backend computes — keep `polyline_project`
//! deterministic and side-effect free.
//!
//! * `polyline_arc_lengths` — cumulative arc length per vertex
//! * `polyline_project` — `(x, y)` → `t` (and the snapped point)
//! * `polyline_at_t` — `t` → `(point, tangent)`
//! * `auto_tab_ts` — N evenly spaced parameters for Auto / Mixed modes
//! * `resolve_tab_placements` — Object-id-keyed placements →
//!   segment-idx-keyed `TabPoint` map the existing
//!   `attach_tabs_to_offsets` consumes.

// # CAM/sim pedantic-lint exemptions
// Tab placement uses `t` (arc-length parameter) and per-pair (`a`, `b`)
// endpoint names from the projection-onto-segment idiom.
#![allow(
    clippy::many_single_char_names,
)]


use std::collections::HashMap;

use crate::cam::offsets::TabPoint;
use crate::cam::{segments_to_points, VcObject};
use crate::geometry::Point2;
use crate::project::TabPlacement;

/// Cumulative arc length per vertex. `out[0] = 0.0`; `out.last()` is
/// the total length of the polyline (NOT closing the loop — a closed
/// polyline's first/last points may coincide or not depending on the
/// caller).
#[must_use] pub fn polyline_arc_lengths(pts: &[Point2]) -> (Vec<f64>, f64) {
    if pts.is_empty() {
        return (Vec::new(), 0.0);
    }
    let mut acc = Vec::with_capacity(pts.len());
    acc.push(0.0);
    let mut total = 0.0;
    for w in pts.windows(2) {
        total += w[0].distance(w[1]);
        acc.push(total);
    }
    (acc, total)
}

/// Project `q` onto a (closed or open) polyline. Returns
/// `(t, snapped_point)` where `t ∈ [0, 1)`. The polyline is treated
/// as closed iff `closed` is `true`; for closed polylines the
/// closing segment from `pts.last()` → `pts[0]` is also considered.
///
/// `t = 0.0` corresponds to `pts[0]`; `t = 1.0` would be one full
/// traversal back to `pts[0]` (for closed loops) or to `pts.last()`
/// for open polylines.
#[must_use] pub fn polyline_project(pts: &[Point2], q: Point2, closed: bool) -> (f64, Point2) {
    if pts.len() < 2 {
        return (0.0, pts.first().copied().unwrap_or(Point2::new(0.0, 0.0)));
    }
    let (cum, total_open) = polyline_arc_lengths(pts);
    let total = if closed {
        total_open + pts.last().unwrap().distance(pts[0])
    } else {
        total_open
    };
    if total < 1e-12 {
        return (0.0, pts[0]);
    }
    let mut best: Option<(f64, f64, Point2)> = None; // (d², t, snap)
    let n_segs = if closed { pts.len() } else { pts.len() - 1 };
    for i in 0..n_segs {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        let seg_len = a.distance(b);
        if seg_len < 1e-12 {
            continue;
        }
        // Project q onto segment [a, b].
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let mut u = ((q.x - a.x) * dx + (q.y - a.y) * dy) / (seg_len * seg_len);
        u = u.clamp(0.0, 1.0);
        let snap = Point2::new(a.x + u * dx, a.y + u * dy);
        let d2 = (q.x - snap.x).powi(2) + (q.y - snap.y).powi(2);
        let arc_here = cum[i] + u * seg_len;
        let t = arc_here / total;
        if best.map_or(true, |(bd, _, _)| d2 < bd) {
            best = Some((d2, t, snap));
        }
    }
    let (_, t, snap) = best.unwrap_or((0.0, 0.0, pts[0]));
    let t = if closed {
        let m = t.rem_euclid(1.0);
        if m.is_nan() {
            0.0
        } else {
            m
        }
    } else {
        t.clamp(0.0, 1.0 - 1e-12)
    };
    (t, snap)
}

/// Inverse of `polyline_project`. Walks the polyline by arc length
/// to the vertex at parameter `t ∈ [0, 1)` and returns the world
/// point + the unit tangent direction at that point (forward along
/// the polyline). Tangent stays well-defined at vertices by picking
/// the OUTGOING segment.
#[must_use] pub fn polyline_at_t(pts: &[Point2], t: f64, closed: bool) -> (Point2, (f64, f64)) {
    if pts.len() < 2 {
        let p = pts.first().copied().unwrap_or(Point2::new(0.0, 0.0));
        return (p, (1.0, 0.0));
    }
    let (_cum, total_open) = polyline_arc_lengths(pts);
    let total = if closed {
        total_open + pts.last().unwrap().distance(pts[0])
    } else {
        total_open
    };
    if total < 1e-12 {
        return (pts[0], (1.0, 0.0));
    }
    let t = if closed {
        let m = t.rem_euclid(1.0);
        if m.is_nan() {
            0.0
        } else {
            m
        }
    } else {
        t.clamp(0.0, 1.0 - 1e-12)
    };
    let target = t * total;
    let n_segs = if closed { pts.len() } else { pts.len() - 1 };
    let mut acc = 0.0;
    for i in 0..n_segs {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        let seg_len = a.distance(b);
        if seg_len < 1e-12 {
            continue;
        }
        if target <= acc + seg_len {
            let u = ((target - acc) / seg_len).clamp(0.0, 1.0);
            let p = Point2::new(a.x + u * (b.x - a.x), a.y + u * (b.y - a.y));
            let tx = (b.x - a.x) / seg_len;
            let ty = (b.y - a.y) / seg_len;
            return (p, (tx, ty));
        }
        acc += seg_len;
    }
    // Fell off the end (numerical drift) — clamp to last segment.
    let i = n_segs - 1;
    let a = pts[i];
    let b = pts[(i + 1) % pts.len()];
    let seg_len = a.distance(b).max(1e-12);
    (b, ((b.x - a.x) / seg_len, (b.y - a.y) / seg_len))
}

/// N evenly spaced tab parameters. For closed contours the spacing
/// is `1/count` and the first parameter is 0. For open contours we
/// inset the first/last by `0.5/count` so tabs don't land on the
/// endpoints (cutter never reaches the very edge at full depth).
#[must_use] pub fn auto_tab_ts(count: u32, closed: bool) -> Vec<f64> {
    if count == 0 {
        return Vec::new();
    }
    let n = f64::from(count);
    if closed {
        (0..count).map(|i| f64::from(i) / n).collect()
    } else {
        // First at 0.5/n, then evenly spaced; ensures the first tab
        // doesn't sit on the endpoint.
        (0..count).map(|i| (f64::from(i) + 0.5) / n).collect()
    }
}

/// Walk a list of `TabPlacement`s + the per-op objects they refer
/// to and produce the segment-idx-keyed `TabPoint` map the existing
/// `attach_tabs_to_offsets` consumes. Placements whose `object_id`
/// is not in `objects` are silently dropped (the canvas surfaces a
/// "disconnected tabs" hint separately so the user can clean up).
///
/// `interpolate` matches the value `pocket_for_object` etc. use to
/// densify curved segments (typically 6).
#[must_use] pub fn resolve_tab_placements(
    placements: &[TabPlacement],
    objects: &[VcObject],
    interpolate: usize,
) -> HashMap<usize, Vec<TabPoint>> {
    let mut out: HashMap<usize, Vec<TabPoint>> = HashMap::new();
    for tp in placements {
        // object_id is 1-based (matches OpSource::Objects.ids).
        if tp.object_id == 0 || (tp.object_id as usize) > objects.len() {
            continue;
        }
        let obj_idx = (tp.object_id as usize) - 1;
        let obj = &objects[obj_idx];
        if obj.segments.is_empty() {
            continue;
        }
        let pts = segments_to_points(&obj.segments, interpolate);
        if pts.len() < 2 {
            continue;
        }
        let (p, _) = polyline_at_t(&pts, tp.t.rem_euclid(1.0), obj.closed);
        out.entry(obj_idx).or_default().push(TabPoint {
            x: p.x,
            y: p.y,
            width_override_mm: tp.width_override_mm,
            height_override_mm: tp.height_override_mm,
        });
    }
    out
}

#[cfg(test)]
// Cumulative arc-length on a unit square: each side is exactly 10.0 →
// running sum is bit-exact.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    /// Unit square at the origin walked CCW: (0,0)→(10,0)→(10,10)→(0,10)→(0,0).
    /// Total perimeter 40, each corner at t = 0/4, 1/4, 2/4, 3/4.
    fn square() -> Vec<Point2> {
        vec![p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)]
    }

    #[test]
    fn arc_lengths_accumulate() {
        let (cum, total) = polyline_arc_lengths(&square());
        assert_eq!(cum, vec![0.0, 10.0, 20.0, 30.0]);
        assert_eq!(total, 30.0); // not closed in the helper view
    }

    #[test]
    fn project_corner_lands_on_vertex() {
        // (10.2, 5) — slightly outside the right edge midpoint — projects
        // to (10, 5), which is t = 15/40 = 0.375 on the closed square.
        let (t, snap) = polyline_project(&square(), p(10.2, 5.0), true);
        assert!((snap.x - 10.0).abs() < 1e-9, "got snap.x={}", snap.x);
        assert!((snap.y - 5.0).abs() < 1e-9, "got snap.y={}", snap.y);
        assert!((t - 0.375).abs() < 1e-9, "got t={t}");
    }

    #[test]
    fn project_round_trips_through_at_t() {
        let pts = square();
        for &t_in in &[0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 0.999] {
            let (p_at, _tan) = polyline_at_t(&pts, t_in, true);
            let (t_back, _) = polyline_project(&pts, p_at, true);
            assert!(
                (t_back - t_in).abs() < 1e-6 || (t_back - t_in + 1.0).abs() < 1e-6,
                "round-trip failed: t_in={t_in}, t_back={t_back}"
            );
        }
    }

    #[test]
    fn at_t_corners_match_vertices() {
        let pts = square();
        // closed square: total = 40. t=0.25 → (10, 0); t=0.5 → (10, 10); t=0.75 → (0, 10)
        let (pt_1, _) = polyline_at_t(&pts, 0.25, true);
        assert!((pt_1.x - 10.0).abs() < 1e-9 && pt_1.y.abs() < 1e-9);
        let (pt_2, _) = polyline_at_t(&pts, 0.5, true);
        assert!((pt_2.x - 10.0).abs() < 1e-9 && (pt_2.y - 10.0).abs() < 1e-9);
        let (pt_3, _) = polyline_at_t(&pts, 0.75, true);
        assert!(pt_3.x.abs() < 1e-9 && (pt_3.y - 10.0).abs() < 1e-9);
    }

    #[test]
    fn tangent_along_first_edge_points_right() {
        let pts = square();
        let (_, tan) = polyline_at_t(&pts, 0.1, true);
        // First edge goes +X — tangent should be ~(1, 0).
        assert!((tan.0 - 1.0).abs() < 1e-9, "tangent.x = {}", tan.0);
        assert!(tan.1.abs() < 1e-9, "tangent.y = {}", tan.1);
    }

    #[test]
    fn auto_tab_ts_closed_starts_at_zero() {
        assert_eq!(auto_tab_ts(4, true), vec![0.0, 0.25, 0.5, 0.75]);
        assert_eq!(auto_tab_ts(1, true), vec![0.0]);
    }

    #[test]
    fn auto_tab_ts_open_insets_endpoints() {
        // 4 tabs on an open polyline: at 0.125, 0.375, 0.625, 0.875.
        // None at 0 or 1 — endpoints would be useless tab anchors.
        let ts = auto_tab_ts(4, false);
        assert_eq!(ts, vec![0.125, 0.375, 0.625, 0.875]);
    }

    #[test]
    fn auto_tab_ts_zero_count_empty() {
        assert!(auto_tab_ts(0, true).is_empty());
        assert!(auto_tab_ts(0, false).is_empty());
    }

    /// Geometric invariance: rotating the source polyline 90° around
    /// its centroid and re-resolving the SAME t yields the rotated
    /// world point. This is the headline win over Estlcam's
    /// raw-(x, y) storage — the tab tracks the geometry.
    #[test]
    fn at_t_survives_geometry_rotation() {
        let pts = square();
        // Rotate 90° CCW around (5, 5) — square stays a square.
        let rot = |q: Point2| Point2::new(5.0 - (q.y - 5.0), 5.0 + (q.x - 5.0));
        let pts_rot: Vec<Point2> = pts.iter().copied().map(rot).collect();
        for &t in &[0.1, 0.3, 0.6, 0.85] {
            let (p_pre, _) = polyline_at_t(&pts, t, true);
            let (p_post, _) = polyline_at_t(&pts_rot, t, true);
            let p_expected = rot(p_pre);
            assert!(
                (p_post.x - p_expected.x).abs() < 1e-9 && (p_post.y - p_expected.y).abs() < 1e-9,
                "t={t}: got ({}, {}), expected ({}, {})",
                p_post.x,
                p_post.y,
                p_expected.x,
                p_expected.y,
            );
        }
    }
}
