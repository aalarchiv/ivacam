//! Plunge-entry strategies (ramp + helix) for the start of each cut pass, plus the polygon helpers used by helix-entry planning (pole-of-inaccessibility, centroid, point-in-polygon).

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

use super::z_schedule::arc_length;
use super::PostProcessor;
use crate::geometry::{point_in_polygon, Point2, Segment, SegmentKind};
use crate::math;

/// Walk `segments` while linearly descending Z from `from_z` to `to_z`
/// over the first `ramp_length` of arc length, then continue at `to_z`
/// for the remainder.
///
/// Line segments are *split* when they cross the `ramp_length` boundary
/// so the ramp angle is honored even if the first segment is longer
/// than `ramp_length`. Arc segments aren't split mid-arc (the math gets
/// fiddly); the ramp simply finishes at the first arc boundary that
/// crosses `ramp_length` and the rest of the path proceeds at `to_z`.
pub(super) fn emit_ramp_pass<P: PostProcessor>(
    segments: &[Segment],
    from_z: f64,
    to_z: f64,
    ramp_length: f64,
    post: &mut P,
) {
    if ramp_length < 1e-9 {
        post.linear(None, None, Some(to_z));
        return;
    }
    let mut consumed = 0.0;
    let interp_z = |consumed: f64| -> f64 {
        let t = (consumed / ramp_length).min(1.0);
        from_z + (to_z - from_z) * t
    };
    for seg in segments {
        let seg_len = match seg.kind {
            SegmentKind::Line | SegmentKind::Point => seg.start.distance(seg.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(seg),
        };
        // Split this segment at ramp_length boundary if it's a line
        // and it crosses the boundary.
        let crosses_boundary = consumed < ramp_length
            && consumed + seg_len > ramp_length
            && matches!(seg.kind, SegmentKind::Line);
        if crosses_boundary {
            let remaining_ramp = ramp_length - consumed;
            let frac = remaining_ramp / seg_len;
            let mid_x = seg.start.x + (seg.end.x - seg.start.x) * frac;
            let mid_y = seg.start.y + (seg.end.y - seg.start.y) * frac;
            // Emit the ramp portion at to_z (we just arrived at depth)
            // then continue to the segment end at to_z.
            post.linear(Some(mid_x), Some(mid_y), Some(to_z));
            post.linear(Some(seg.end.x), Some(seg.end.y), Some(to_z));
            consumed += seg_len;
            continue;
        }
        consumed += seg_len;
        let z = interp_z(consumed);
        match seg.kind {
            SegmentKind::Line => post.linear(Some(seg.end.x), Some(seg.end.y), Some(z)),
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), Some(z)),
            SegmentKind::Arc | SegmentKind::Circle => {
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if seg.bulge > 0.0 {
                    post.arc_ccw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                } else {
                    post.arc_cw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                }
            }
        }
    }
}

pub(super) fn is_closed_path(segments: &[Segment]) -> bool {
    if segments.len() < 3 {
        return false;
    }
    let first = segments.first().unwrap().start;
    let last = segments.last().unwrap().end;
    first.distance(last) < 1e-3
}

/// Emit one revolution around `segments` while linearly descending Z from
/// `from_z` to `to_z`. Each segment endpoint gets the interpolated Z so
/// the spiral stays smooth even with arc segments.
pub(super) fn emit_helix_pass<P: PostProcessor>(
    segments: &[Segment],
    from_z: f64,
    to_z: f64,
    post: &mut P,
) {
    let total_len: f64 = segments
        .iter()
        .map(|s| match s.kind {
            SegmentKind::Line | SegmentKind::Point => s.start.distance(s.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(s),
        })
        .sum();
    if total_len < 1e-9 {
        post.linear(None, None, Some(to_z));
        return;
    }
    let mut consumed = 0.0;
    for seg in segments {
        let seg_len = match seg.kind {
            SegmentKind::Line | SegmentKind::Point => seg.start.distance(seg.end),
            SegmentKind::Arc | SegmentKind::Circle => arc_length(seg),
        };
        consumed += seg_len;
        let t = consumed / total_len;
        let z = from_z + (to_z - from_z) * t;
        match seg.kind {
            SegmentKind::Line => post.linear(Some(seg.end.x), Some(seg.end.y), Some(z)),
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), Some(z)),
            SegmentKind::Arc | SegmentKind::Circle => {
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if seg.bulge > 0.0 {
                    post.arc_ccw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                } else {
                    post.arc_cw(Some(seg.end.x), Some(seg.end.y), Some(z), Some(i), Some(j));
                }
            }
        }
    }
}

/// Plan for a start-of-cut helical entry: where to drop, how far
/// horizontally, how deep per revolution. Produced by
/// `plan_helix_entry` and consumed by `emit_helix_entry`.
#[derive(Debug, Clone, Copy)]
pub(super) struct HelixEntry {
    /// XY center of the helix circle.
    pub(super) center: Point2,
    /// Helix radius in mm.
    pub(super) radius: f64,
    /// Z drop per full revolution (always positive).
    pub(super) dz_per_rev: f64,
    /// True if the helix winds CCW around `center` when viewed from +Z.
    /// Matches the polygon winding so the cutter spirals "into" the
    /// material in the same direction the path will run.
    pub(super) ccw: bool,
    /// Starting angle of the helix on the circle (radians, atan2 of
    /// (`path_start` - center)). Helix returns to this angle at landing
    /// so the post-helix walk to `path_start` is the shortest.
    pub(super) start_angle: f64,
}

/// Build a helix entry plan for `segments` if the geometry supports it.
/// Returns None when:
///   - radius < `tool_radius` (helix would carve nothing the cutter
///     doesn't already cover from the path)
///   - the helix circle doesn't fit inside the polygon (any of 8
///     sample points lies outside the boundary)
///   - the path is too short / not closed (caller already checks
///     closed; this is defensive)
///
/// The helix center is the polygon centroid offset back toward the
/// path start so the cutter lands near where the cut begins (and the
/// post-helix walk to path-start is short). The helix circle must fit
/// entirely inside the polygon — otherwise the spiral would carve into
/// the wall on its way down.
pub(super) fn plan_helix_entry(
    segments: &[Segment],
    radius_mm: f64,
    tool_radius: f64,
    angle_deg: f64,
) -> Option<HelixEntry> {
    if segments.is_empty() {
        return None;
    }
    if radius_mm < tool_radius - 1e-9 {
        return None;
    }
    let radius = radius_mm.max(1e-6);
    let angle = angle_deg.clamp(0.5, 45.0).to_radians();
    let dz_per_rev = (2.0 * std::f64::consts::PI * radius * angle.tan()).abs();
    if dz_per_rev < 1e-9 {
        return None;
    }
    // Polygon vertices (line endpoints; arc endpoints, no mid-arc
    // sampling). Sufficient for the shoelace + ray-cast checks below.
    let verts = polygon_vertices(segments);
    if verts.len() < 3 {
        return None;
    }
    let area = polygon_signed_area(&verts);
    let ccw = area > 0.0;
    // Centroid as the helix center. Robust default for convex
    // pockets; for skinny / non-convex shapes the point-in-polygon
    // sampling below catches the bad cases and we fall back to Ramp.
    // We don't try to pull the center toward the path start — doing so
    // can push the helix circle into a wall on small or
    // sharply-cornered pockets, which is exactly the failure mode we
    // need helical entry to avoid. The post-helix walk to the path
    // start is NOT cut at depth — the emitter lifts to
    // fast_move_z, rapids to the contour start, then plunges at
    // rate_v. This costs one retract per pass but avoids the
    // full-immersion straight-line load that broke small-diameter
    // cutters under the old "G1 walk at rate_h" code.
    let path_start = segments[0].start;
    // Pick the helix center as the point inside the polygon with the
    // largest clearance to the boundary (a "pole of inaccessibility"
    // approximation). The centroid works for convex pockets but for L /
    // U / + shapes it lands outside the polygon — and even when it
    // doesn't, a thin pocket's centroid may be too close to a wall for
    // the helix circle to fit. Picking the max-clearance point ensures
    // the helix circle has the most room to fit.
    //
    // We require the chosen center's clearance to exceed `radius +
    // tool_radius` so the helix circle clears the pocket walls by at
    // least a tool radius. If no interior point meets that bar the
    // helix can't fit and we fall back to Ramp.
    let Some(center) = polygon_pole_of_inaccessibility(&verts, radius + tool_radius) else {
        tracing::debug!(
            "helix entry: no interior point with clearance > {:.3}, falling back to Ramp",
            radius + tool_radius
        );
        return None;
    };
    // Sample 16 points on the helix circle as a final safety check;
    // all must be inside the polygon. The pole-of-inaccessibility
    // search above already guarantees the center has > radius +
    // tool_radius clearance, so this should always pass — it's a
    // backstop against numerical edge cases (e.g. polygon edges that
    // graze the helix circle at the clearance limit).
    let samples = 16;
    for i in 0..samples {
        let theta = f64::from(i) * std::f64::consts::TAU / f64::from(samples);
        let px = center.x + radius * theta.cos();
        let py = center.y + radius * theta.sin();
        if !point_in_polygon(&verts, px, py) {
            return None;
        }
    }
    // Start angle: vector from helix center toward the path start.
    // The helix lands at (center + radius·(cosθ, sinθ)) where θ =
    // start_angle, then walks the short remaining distance to the
    // path start.
    let start_angle = (path_start.y - center.y).atan2(path_start.x - center.x);
    Some(HelixEntry {
        center,
        radius,
        dz_per_rev,
        ccw,
        start_angle,
    })
}

/// Approximate "pole of inaccessibility" — the point inside the polygon
/// with the largest clearance to the boundary. Used to seat the helix
/// entry circle in pockets where the centroid sits outside (L / U / +)
/// or too close to a wall.
///
/// Algorithm: bbox-grid sample at ~64 cells per axis. For each interior
/// sample, compute the min distance to any polygon edge (line-segment
/// distance, not vertex distance — a long edge midway between two
/// vertices is what bites a helix circle). Return the sample with the
/// largest such distance, but only if it exceeds `min_clearance`.
///
/// Returns None when no interior sample meets `min_clearance` — caller
/// treats this as "helix can't fit, fall back to Ramp."
fn polygon_pole_of_inaccessibility(verts: &[Point2], min_clearance: f64) -> Option<Point2> {
    let n = verts.len();
    if n < 3 {
        return None;
    }
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for p in verts {
        if p.x < min_x {
            min_x = p.x;
        }
        if p.y < min_y {
            min_y = p.y;
        }
        if p.x > max_x {
            max_x = p.x;
        }
        if p.y > max_y {
            max_y = p.y;
        }
    }
    let width = max_x - min_x;
    let height = max_y - min_y;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    // 64 cells per axis is a balance: enough resolution to find pockets
    // ≥ 1/32 the bbox side; cheap enough for big pockets (~4096 grid
    // points × n_edges edge-distance calls).
    let cells = 64usize;
    let dx = width / (cells as f64);
    let dy = height / (cells as f64);
    let mut best: Option<(Point2, f64)> = None;
    // Try the centroid first as a likely candidate (skip the grid scan
    // entirely when it's already a great fit, e.g. a circular pocket).
    let centroid = polygon_centroid(verts);
    if point_in_polygon(verts, centroid.x, centroid.y) {
        let cd = polygon_min_distance_to_boundary(verts, centroid.x, centroid.y);
        if cd > min_clearance {
            best = Some((centroid, cd));
        }
    }
    for j in 0..cells {
        let py = min_y + (j as f64 + 0.5) * dy;
        for i in 0..cells {
            let px = min_x + (i as f64 + 0.5) * dx;
            if !point_in_polygon(verts, px, py) {
                continue;
            }
            let d = polygon_min_distance_to_boundary(verts, px, py);
            match best {
                Some((_, bd)) if d <= bd => {}
                _ => best = Some((Point2::new(px, py), d)),
            }
        }
    }
    match best {
        Some((p, d)) if d > min_clearance => Some(p),
        _ => None,
    }
}

/// Minimum distance from (x, y) to any edge of the polygon, treated as
/// a closed line-segment chain. Segment-to-point distance, not just
/// vertex-to-point distance — important for long pocket walls.
fn polygon_min_distance_to_boundary(verts: &[Point2], x: f64, y: f64) -> f64 {
    let n = verts.len();
    let mut best = f64::INFINITY;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        let ex = b.x - a.x;
        let ey = b.y - a.y;
        let len_sq = ex * ex + ey * ey;
        let d = if len_sq < 1e-18 {
            ((x - a.x) * (x - a.x) + (y - a.y) * (y - a.y)).sqrt()
        } else {
            let t = (((x - a.x) * ex) + ((y - a.y) * ey)) / len_sq;
            let t = t.clamp(0.0, 1.0);
            let px = a.x + t * ex;
            let py = a.y + t * ey;
            ((x - px) * (x - px) + (y - py) * (y - py)).sqrt()
        };
        if d < best {
            best = d;
        }
    }
    best
}

/// Polygon centroid via the shoelace formula. For a degenerate
/// (zero-area) polygon, returns the average of the vertices.
fn polygon_centroid(verts: &[Point2]) -> Point2 {
    let n = verts.len();
    if n == 0 {
        return Point2::new(0.0, 0.0);
    }
    let mut a = 0.0;
    let mut cx = 0.0;
    let mut cy = 0.0;
    for i in 0..n {
        let p = verts[i];
        let q = verts[(i + 1) % n];
        let cross = p.x * q.y - q.x * p.y;
        a += cross;
        cx += (p.x + q.x) * cross;
        cy += (p.y + q.y) * cross;
    }
    a *= 0.5;
    if a.abs() < 1e-9 {
        let mut sx = 0.0;
        let mut sy = 0.0;
        for p in verts {
            sx += p.x;
            sy += p.y;
        }
        return Point2::new(sx / n as f64, sy / n as f64);
    }
    Point2::new(cx / (6.0 * a), cy / (6.0 * a))
}

/// Emit the helical entry: descend from `from_z` to `to_z` on a circle
/// of radius `plan.radius` around `plan.center`. Each revolution drops
/// Z by `plan.dz_per_rev`; partial revolutions linearly interpolate Z.
/// The final point lands at the path-start angle so the caller's
/// follow-up `linear(start.x, start.y, to_z)` is a straight line of
/// length zero (or near-zero in the Helix circle's tangent frame).
pub(super) fn emit_helix_entry<P: PostProcessor>(
    plan: &HelixEntry,
    from_z: f64,
    to_z: f64,
    post: &mut P,
) {
    let dz = (from_z - to_z).abs();
    if dz < 1e-9 {
        return;
    }
    // Number of full revolutions needed (always at least one — if the
    // user picks a tiny step the helix still completes a full lap so
    // the cutter doesn't dive on a chord).
    let revs_full = (dz / plan.dz_per_rev).ceil().max(1.0);
    // Each revolution drops Z by dz/revs_full so the descent is
    // distributed evenly.
    let dz_each = -(from_z - to_z).abs() / revs_full; // negative (going down)
    let n = revs_full as usize;
    // Helix start: cutter at start angle, current Z = from_z.
    let start_x = plan.center.x + plan.radius * plan.start_angle.cos();
    let start_y = plan.center.y + plan.radius * plan.start_angle.sin();
    // Move to start of helix at fast_move_z would be done by caller —
    // here we assume the cutter is already above the helix start. The
    // first emit is a linear move to the helix start at from_z so the
    // tool steps off the path-start XY (where the rapid landed it)
    // onto the helix circle at z=from_z.
    post.linear(Some(start_x), Some(start_y), Some(from_z));
    let mut cur_z = from_z;
    for i in 0..n {
        let next_z = if i + 1 == n { to_z } else { cur_z + dz_each };
        // Each revolution is two semicircles so a single G2/G3 with
        // i, j vector to center stays within the post processor's
        // arc capabilities (some posts reject full-circle arcs whose
        // endpoint == startpoint).
        let half_dz = (next_z - cur_z) * 0.5;
        let mid_angle = plan.start_angle + std::f64::consts::PI;
        let mid_x = plan.center.x + plan.radius * mid_angle.cos();
        let mid_y = plan.center.y + plan.radius * mid_angle.sin();
        // Arc 1: start → midpoint (semicircle). i, j are the offset
        // from the arc's start point to the helix center.
        let i1 = -plan.radius * plan.start_angle.cos();
        let j1 = -plan.radius * plan.start_angle.sin();
        if plan.ccw {
            post.arc_ccw(
                Some(mid_x),
                Some(mid_y),
                Some(cur_z + half_dz),
                Some(i1),
                Some(j1),
            );
        } else {
            post.arc_cw(
                Some(mid_x),
                Some(mid_y),
                Some(cur_z + half_dz),
                Some(i1),
                Some(j1),
            );
        }
        // Arc 2: midpoint → start (semicircle, completing the lap).
        let i2 = -plan.radius * mid_angle.cos();
        let j2 = -plan.radius * mid_angle.sin();
        let end_x = plan.center.x + plan.radius * plan.start_angle.cos();
        let end_y = plan.center.y + plan.radius * plan.start_angle.sin();
        if plan.ccw {
            post.arc_ccw(Some(end_x), Some(end_y), Some(next_z), Some(i2), Some(j2));
        } else {
            post.arc_cw(Some(end_x), Some(end_y), Some(next_z), Some(i2), Some(j2));
        }
        cur_z = next_z;
    }
}

/// Extract polygon vertices from a segment chain (line endpoints; arc
/// endpoints — arc midpoints aren't sampled, the polygon is just the
/// segment endpoint list). Used for signed-area + point-in-polygon
/// checks during helix planning. The returned list is the closed
/// polygon's vertex sequence with no duplicate closing vertex.
fn polygon_vertices(segments: &[Segment]) -> Vec<Point2> {
    let mut v: Vec<Point2> = Vec::with_capacity(segments.len() + 1);
    if segments.is_empty() {
        return v;
    }
    v.push(segments[0].start);
    for seg in segments {
        // Push the end of each segment; duplicates with the next
        // segment's start are filtered by the dedupe at the end.
        if matches!(seg.kind, SegmentKind::Point) {
            continue;
        }
        v.push(seg.end);
    }
    // Drop a duplicate trailing vertex (closed path: last == first).
    if v.len() >= 2 && v.first().unwrap().distance(*v.last().unwrap()) < 1e-6 {
        v.pop();
    }
    v
}

/// Shoelace signed area of a polygon given as a vertex list. Positive
/// = CCW, negative = CW. Mirrors `cam::offsets::object_signed_area`
/// but operates on vertices instead of a `VcObject`.
fn polygon_signed_area(verts: &[Point2]) -> f64 {
    let n = verts.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}
