//! Per-segment cutter-vs-fixture collision check. Called by the sweep
//! pass for every toolpath segment (rapid included). Returns one
//! [`FixtureCheck::Collision`] per intersecting fixture so the
//! diagnostics stream gets one warning per fixture, not just the first.
//!
//! Algorithm per segment × fixture:
//! 1. Z-range gate. The cutter sweeps the segment's Z range
//!    `[min(from.z, to.z), max(from.z, to.z)]`; if that doesn't overlap
//!    `[fixture.z_bottom, fixture.z_top]` the cutter never visits the
//!    fixture's height band — Clear, skip XY work.
//! 2. XY swept-region test. The cutter's XY footprint along the segment
//!    is a stadium (capsule): two disks of radius `tool_radius` at the
//!    segment endpoints joined by a rectangle. Per fixture shape:
//!    * Box → stadium-vs-AABB via SAT.
//!    * Cylinder → distance from the segment to the fixture center.
//!    * Polygon → stadium-vs-each-edge using `lines_intersect`, plus a
//!      point-in-polygon containment fallback.
//! 3. Report the closest segment point to the fixture center as the
//!    collision's "nearest" coordinate.

// # CAM/sim pedantic-lint exemptions
// Fixture intersection math uses `min_x`/`max_x`/`min_y`/`max_y` bbox-corner
// names that are the textbook convention; cell-index casts are bounded by
// heightmap dimensions.
#![allow(clippy::cast_precision_loss, clippy::similar_names)]

use crate::cam::is_inside_polygon;
use crate::cam::lines_intersect;
use crate::gcode::preview::ToolpathSegment;
use crate::geometry::Point2;
use crate::project::{Fixture, FixtureKind};
use crate::sim::holder::HolderProfile;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FixtureCheck {
    Clear,
    Collision {
        fixture_id: u32,
        nearest_x: f64,
        nearest_y: f64,
    },
}

// WHY: the Z gate used to look only at the tip Z range, so a
// clamp/jaw sitting *above* the tip Z but inside the path of the shank
// or holder was invisible. The tool body (tip → flutes → shank → holder
// top) extends upward by `holder.total_length()`, and the XY inflation
// above the flutes must use `holder.max_radius()`, not just the cutting
// radius — a wide ER nut close to a clamp must still be tested.
#[must_use]
pub fn check_segment_against_fixtures(
    segment: &ToolpathSegment,
    tool_radius: f64,
    holder: Option<&HolderProfile>,
    fixtures: &[Fixture],
) -> Vec<FixtureCheck> {
    if fixtures.is_empty() || tool_radius <= 0.0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(fixtures.len());
    let seg_z_min = segment.from.z.min(segment.to.z);
    let seg_z_max = segment.from.z.max(segment.to.z);
    // The tool body extends `holder.total_length()` above the tip;
    // anything above the flutes sweeps a wider envelope (the holder /
    // shank radius). Fixtures *above* the tip Z but inside that band
    // were previously invisible.
    let body_top_offset = holder.map_or(0.0, HolderProfile::total_length);
    let body_z_top = seg_z_max + body_top_offset;
    let body_radius = holder.map_or(tool_radius, |h| tool_radius.max(h.max_radius()));

    for f in fixtures {
        // Z-range gate: the tool body sweeps [seg_z_min, seg_z_max +
        // body_top_offset]. If that band doesn't overlap the fixture's
        // [z_bottom, z_top] the tool never visits the fixture's height
        // band — safe.
        if body_z_top < f.z_bottom || seg_z_min > f.z_top {
            out.push(FixtureCheck::Clear);
            continue;
        }
        // XY inflation: cells *at* the cutting tip Z use the cutting
        // radius; cells above the flutes (where the holder lives) need
        // the holder's max radius. As a conservative bound that lets a
        // single XY test cover both bands, use whichever inflation
        // applies inside the fixture's height window.
        let xy_r = if f.z_bottom > seg_z_max {
            body_radius
        } else {
            // Fixture band reaches at or below the tip — the tip itself
            // could hit it, so the cutting radius governs. (If the
            // fixture extends both below and above the flutes we still
            // need the holder radius for the upper part, so take the
            // max.)
            if f.z_top > seg_z_max {
                tool_radius.max(body_radius)
            } else {
                tool_radius
            }
        };
        let collides = match &f.kind {
            FixtureKind::Box { width, depth } => {
                stadium_hits_box(segment, xy_r, f.origin, *width, *depth)
            }
            FixtureKind::Cylinder { radius } => {
                stadium_hits_cylinder(segment, xy_r, f.origin, *radius)
            }
            FixtureKind::Polygon { vertices } => {
                stadium_hits_polygon(segment, xy_r, f.origin, vertices)
            }
        };
        if collides {
            let (nx, ny) = nearest_point_on_segment_to(segment, f.origin.0, f.origin.1);
            out.push(FixtureCheck::Collision {
                fixture_id: f.id,
                nearest_x: nx,
                nearest_y: ny,
            });
        } else {
            out.push(FixtureCheck::Clear);
        }
    }
    out
}

/// Closest point on the segment to `(px, py)` in XY. Standard
/// segment-point distance with a clamped parameter.
fn nearest_point_on_segment_to(segment: &ToolpathSegment, px: f64, py: f64) -> (f64, f64) {
    let dx = segment.to.x - segment.from.x;
    let dy = segment.to.y - segment.from.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-18 {
        return (segment.from.x, segment.from.y);
    }
    let t = (((px - segment.from.x) * dx + (py - segment.from.y) * dy) / len_sq).clamp(0.0, 1.0);
    (segment.from.x + t * dx, segment.from.y + t * dy)
}

/// Distance from `(px, py)` to the XY-projected segment.
fn distance_point_to_segment(segment: &ToolpathSegment, px: f64, py: f64) -> f64 {
    let (qx, qy) = nearest_point_on_segment_to(segment, px, py);
    ((qx - px).powi(2) + (qy - py).powi(2)).sqrt()
}

/// Cylinder-vs-stadium: degenerates to "is the segment closer than
/// `tool_radius + cyl_radius` to the cylinder's center?".
fn stadium_hits_cylinder(
    segment: &ToolpathSegment,
    tool_radius: f64,
    origin: (f64, f64),
    cyl_radius: f64,
) -> bool {
    let d = distance_point_to_segment(segment, origin.0, origin.1);
    d <= tool_radius + cyl_radius
}

/// Stadium-vs-AABB via the separating-axis theorem. The stadium is the
/// Minkowski sum of the segment with a disk of radius `tool_radius`; the
/// box is `origin ± (width/2, depth/2)`. Bodies overlap iff no axis
/// separates them on every test axis. We check the box's two axes
/// (cardinal X, Y, where the stadium's projection is `[seg_min - r,
/// seg_max + r]`) and the segment-perpendicular axis (where the box's
/// projection is the AABB radius along that axis and the stadium reduces
/// to its center-line ± `tool_radius`).
fn stadium_hits_box(
    segment: &ToolpathSegment,
    tool_radius: f64,
    origin: (f64, f64),
    width: f64,
    depth: f64,
) -> bool {
    let hw = width.abs() * 0.5;
    let hd = depth.abs() * 0.5;
    let bx0 = origin.0 - hw;
    let bx1 = origin.0 + hw;
    let by0 = origin.1 - hd;
    let by1 = origin.1 + hd;

    let sx0 = segment.from.x.min(segment.to.x);
    let sx1 = segment.from.x.max(segment.to.x);
    let sy0 = segment.from.y.min(segment.to.y);
    let sy1 = segment.from.y.max(segment.to.y);

    // Axis 1: cardinal X. Stadium's X extent is the segment's X extent
    // inflated by `tool_radius`.
    if sx1 + tool_radius < bx0 || sx0 - tool_radius > bx1 {
        return false;
    }
    // Axis 2: cardinal Y.
    if sy1 + tool_radius < by0 || sy0 - tool_radius > by1 {
        return false;
    }

    // Axis 3: segment-perpendicular axis. Skip for zero-length (pure-Z)
    // segments — the previous two axis tests with the disk inflation
    // already tell us whether the disk overlaps the box.
    let dx = segment.to.x - segment.from.x;
    let dy = segment.to.y - segment.from.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-18 {
        // Pure plunge: any time the disk at (from.x, from.y) overlaps
        // the box. Use closest-point-on-AABB.
        let cx = segment.from.x.clamp(bx0, bx1);
        let cy = segment.from.y.clamp(by0, by1);
        let dxp = segment.from.x - cx;
        let dyp = segment.from.y - cy;
        return dxp * dxp + dyp * dyp <= tool_radius * tool_radius;
    }
    let len = len_sq.sqrt();
    // Perpendicular unit vector to the segment.
    let nx = -dy / len;
    let ny = dx / len;
    // Segment's projection onto its perpendicular reduces to a single
    // value c (because both endpoints share it). Stadium's projection is
    // [c - tool_radius, c + tool_radius].
    let c = segment.from.x * nx + segment.from.y * ny;
    let s_min = c - tool_radius;
    let s_max = c + tool_radius;
    // Box's projection onto the same axis: project all 4 corners.
    let p0 = bx0 * nx + by0 * ny;
    let p1 = bx1 * nx + by0 * ny;
    let p2 = bx0 * nx + by1 * ny;
    let p3 = bx1 * nx + by1 * ny;
    let b_min = p0.min(p1).min(p2).min(p3);
    let b_max = p0.max(p1).max(p2).max(p3);
    if s_max < b_min || s_min > b_max {
        return false;
    }
    true
}

/// Stadium-vs-polygon: the polygon's edges live in *world* coordinates
/// (vertices are added to `origin`). Hit if (a) any polygon edge comes
/// within `tool_radius` of the segment, or (b) either segment endpoint
/// is inside the polygon, or (c) the polygon centroid lies on the
/// segment's swept disc (containment of the polygon in the cutter).
fn stadium_hits_polygon(
    segment: &ToolpathSegment,
    tool_radius: f64,
    origin: (f64, f64),
    local_vertices: &[(f64, f64)],
) -> bool {
    if local_vertices.len() < 2 {
        return false;
    }
    let world: Vec<Point2> = local_vertices
        .iter()
        .map(|(x, y)| Point2::new(origin.0 + *x, origin.1 + *y))
        .collect();

    let from2 = Point2::new(segment.from.x, segment.from.y);
    let to2 = Point2::new(segment.to.x, segment.to.y);

    // (b) Endpoint-inside-polygon containment.
    if is_inside_polygon(&world, from2) || is_inside_polygon(&world, to2) {
        return true;
    }

    // (a) Edge proximity. For each polygon edge, find the minimum
    // distance to the toolpath segment. If <= tool_radius they touch.
    let n = world.len();
    for i in 0..n {
        let a = world[i];
        let b = world[(i + 1) % n];
        // Quick win: a true crossing certainly puts the centerline
        // inside the polygon outline.
        if lines_intersect(from2, to2, a, b).is_some() {
            return true;
        }
        let d = segment_to_segment_distance(
            (segment.from.x, segment.from.y),
            (segment.to.x, segment.to.y),
            (a.x, a.y),
            (b.x, b.y),
        );
        if d <= tool_radius {
            return true;
        }
    }

    // (c) Cutter wide enough to swallow the polygon — test the centroid
    // lies on the swept disc.
    let cx = world.iter().map(|p| p.x).sum::<f64>() / n as f64;
    let cy = world.iter().map(|p| p.y).sum::<f64>() / n as f64;
    if distance_point_to_segment(segment, cx, cy) <= tool_radius {
        return true;
    }
    false
}

/// Minimum distance between two 2D segments (a→b and c→d). Standard
/// approach: if they intersect → 0; otherwise the distance is the
/// minimum of the four endpoint-to-segment distances.
fn segment_to_segment_distance(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> f64 {
    if lines_intersect(
        Point2::new(a.0, a.1),
        Point2::new(b.0, b.1),
        Point2::new(c.0, c.1),
        Point2::new(d.0, d.1),
    )
    .is_some()
    {
        return 0.0;
    }
    let d1 = point_to_segment_2d(c, a, b);
    let d2 = point_to_segment_2d(d, a, b);
    let d3 = point_to_segment_2d(a, c, d);
    let d4 = point_to_segment_2d(b, c, d);
    d1.min(d2).min(d3).min(d4)
}

fn point_to_segment_2d(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-18 {
        return ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len_sq).clamp(0.0, 1.0);
    let qx = a.0 + t * dx;
    let qy = a.1 + t * dy;
    ((p.0 - qx).powi(2) + (p.1 - qy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcode::preview::{MoveKind, Pose3, ToolpathSegment};
    use crate::project::{Fixture, FixtureKind};

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

    fn box_fixture(
        id: u32,
        origin: (f64, f64),
        w: f64,
        d: f64,
        z_bottom: f64,
        z_top: f64,
    ) -> Fixture {
        Fixture {
            id,
            name: "box".into(),
            kind: FixtureKind::Box { width: w, depth: d },
            origin,
            z_bottom,
            z_top,
            color: 0xFFA0_50C0,
        }
    }

    fn cyl_fixture(id: u32, origin: (f64, f64), r: f64, z_bottom: f64, z_top: f64) -> Fixture {
        Fixture {
            id,
            name: "cyl".into(),
            kind: FixtureKind::Cylinder { radius: r },
            origin,
            z_bottom,
            z_top,
            color: 0xFFA0_50C0,
        }
    }

    fn poly_fixture(
        id: u32,
        origin: (f64, f64),
        v: Vec<(f64, f64)>,
        z_bottom: f64,
        z_top: f64,
    ) -> Fixture {
        Fixture {
            id,
            name: "poly".into(),
            kind: FixtureKind::Polygon { vertices: v },
            origin,
            z_bottom,
            z_top,
            color: 0xFFA0_50C0,
        }
    }

    #[test]
    fn box_inside_path() {
        // 30 wide × 50 deep box centered at origin, z 0..10. 6 mm endmill
        // (R=3) cuts horizontally through the middle at Z=5.
        let s = seg((-50.0, 0.0, 5.0), (50.0, 0.0, 5.0));
        let fix = box_fixture(7, (0.0, 0.0), 30.0, 50.0, 0.0, 10.0);
        let res = check_segment_against_fixtures(&s, 3.0, None, &[fix]);
        assert_eq!(res.len(), 1);
        match res[0] {
            FixtureCheck::Collision {
                fixture_id,
                nearest_x,
                nearest_y,
            } => {
                assert_eq!(fixture_id, 7);
                // Closest segment point to box center (0,0): right at (0,0).
                assert!(nearest_x.abs() < 1e-9);
                assert!(nearest_y.abs() < 1e-9);
            }
            FixtureCheck::Clear => panic!("expected collision, got {:?}", res[0]),
        }
    }

    #[test]
    fn box_z_band_above_segment() {
        // Same Box but z_bottom=10 so Z=5 segment never reaches it.
        let s = seg((-50.0, 0.0, 5.0), (50.0, 0.0, 5.0));
        let fix = box_fixture(7, (0.0, 0.0), 30.0, 50.0, 10.0, 20.0);
        let res = check_segment_against_fixtures(&s, 3.0, None, &[fix]);
        assert!(matches!(res[0], FixtureCheck::Clear));
    }

    #[test]
    fn box_clear_far_in_xy() {
        let s = seg((-50.0, 100.0, 5.0), (50.0, 100.0, 5.0));
        let fix = box_fixture(7, (0.0, 0.0), 30.0, 50.0, 0.0, 10.0);
        let res = check_segment_against_fixtures(&s, 3.0, None, &[fix]);
        assert!(
            matches!(res[0], FixtureCheck::Clear),
            "stadium far above box should be clear"
        );
    }

    #[test]
    fn cylinder_within_radius() {
        // Cylinder R=10 at (50, 50), z_bottom=0/z_top=20. Segment from
        // (40, 50) → (40, 60) at Z=5. Distance segment-to-center = 10,
        // R+r = 13: collision.
        let s = seg((40.0, 50.0, 5.0), (40.0, 60.0, 5.0));
        let fix = cyl_fixture(2, (50.0, 50.0), 10.0, 0.0, 20.0);
        let res = check_segment_against_fixtures(&s, 3.0, None, &[fix]);
        match res[0] {
            FixtureCheck::Collision {
                fixture_id,
                nearest_x,
                nearest_y,
            } => {
                assert_eq!(fixture_id, 2);
                // Closest segment point to (50, 50): (40, 50).
                assert!((nearest_x - 40.0).abs() < 1e-6);
                assert!((nearest_y - 50.0).abs() < 1e-6);
            }
            FixtureCheck::Clear => panic!("expected collision, got {:?}", res[0]),
        }
    }

    #[test]
    fn cylinder_far_enough() {
        // Same cylinder, segment 5 mm further left: distance 15 > R+r=13.
        let s = seg((35.0, 50.0, 5.0), (35.0, 60.0, 5.0));
        let fix = cyl_fixture(2, (50.0, 50.0), 10.0, 0.0, 20.0);
        let res = check_segment_against_fixtures(&s, 3.0, None, &[fix]);
        assert!(matches!(res[0], FixtureCheck::Clear));
    }

    #[test]
    fn cylinder_z_below_segment() {
        // Cylinder occupies z [10, 20]; segment at z=5. No overlap.
        let s = seg((40.0, 50.0, 5.0), (40.0, 60.0, 5.0));
        let fix = cyl_fixture(2, (50.0, 50.0), 10.0, 10.0, 20.0);
        let res = check_segment_against_fixtures(&s, 3.0, None, &[fix]);
        assert!(matches!(res[0], FixtureCheck::Clear));
    }

    #[test]
    fn triangle_edge_skirt() {
        // Triangle with vertices (0,0), (20,0), (10, 17), z 0..10.
        // Segment (15, 5, 5) → (25, 5, 5) skims past the right edge.
        let s = seg((15.0, 5.0, 5.0), (25.0, 5.0, 5.0));
        let fix = poly_fixture(
            9,
            (0.0, 0.0),
            vec![(0.0, 0.0), (20.0, 0.0), (10.0, 17.0)],
            0.0,
            10.0,
        );
        let res = check_segment_against_fixtures(&s, 3.0, None, &[fix]);
        assert!(matches!(
            res[0],
            FixtureCheck::Collision { fixture_id: 9, .. }
        ));
    }

    #[test]
    fn triangle_clear() {
        // Same triangle, segment well outside.
        let s = seg((50.0, 5.0, 5.0), (60.0, 5.0, 5.0));
        let fix = poly_fixture(
            9,
            (0.0, 0.0),
            vec![(0.0, 0.0), (20.0, 0.0), (10.0, 17.0)],
            0.0,
            10.0,
        );
        let res = check_segment_against_fixtures(&s, 3.0, None, &[fix]);
        assert!(matches!(res[0], FixtureCheck::Clear));
    }

    #[test]
    fn empty_fixtures_returns_empty() {
        let s = seg((0.0, 0.0, 0.0), (10.0, 0.0, 0.0));
        let res = check_segment_against_fixtures(&s, 3.0, None, &[]);
        assert!(res.is_empty());
    }

    #[test]
    fn multi_fixture_one_collision_per() {
        // Two fixtures, only the cylinder gets hit.
        let s = seg((40.0, 50.0, 5.0), (40.0, 60.0, 5.0));
        let fixes = vec![
            box_fixture(1, (-100.0, -100.0), 5.0, 5.0, 0.0, 10.0),
            cyl_fixture(2, (50.0, 50.0), 10.0, 0.0, 20.0),
        ];
        let res = check_segment_against_fixtures(&s, 3.0, None, &fixes);
        assert_eq!(res.len(), 2);
        assert!(matches!(res[0], FixtureCheck::Clear));
        assert!(matches!(
            res[1],
            FixtureCheck::Collision { fixture_id: 2, .. }
        ));
    }

    #[test]
    fn pure_plunge_into_box() {
        // Plunge straight down into the box's footprint. Segment has
        // zero XY length; the SAT path should still collide via the
        // disk-vs-AABB special case.
        let s = seg((0.0, 0.0, 5.0), (0.0, 0.0, -3.0));
        let fix = box_fixture(7, (0.0, 0.0), 30.0, 50.0, 0.0, 10.0);
        let res = check_segment_against_fixtures(&s, 3.0, None, &[fix]);
        assert!(matches!(
            res[0],
            FixtureCheck::Collision { fixture_id: 7, .. }
        ));
    }

    fn build_holder_60mm() -> HolderProfile {
        // 6 mm endmill, 25 mm flutes, 6 mm shank, 20 mm-dia × 35 mm
        // cylinder holder → total length above tip = flute_len (25) +
        // holder (35) = 60 mm.
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
            whirl: false,
            whirl_stepover_mm: None,
            whirl_extra_width_mm: None,
            whirl_osc_mm: None,
            pause: 1,
            flute_length_mm: Some(25.0),
            length_mm: None,
            compression_transition_mm: None,
            thread_pitch_mm: None,
            shank_diameter_mm: Some(6.0),
            stickout_length_mm: None,
            holder: Some(HolderShape::Cylinder {
                diameter_mm: 20.0,
                length_mm: 35.0,
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
    fn shank_strikes_fixture_above_tip() {
        // Clamp box at z_top=30 sitting above the un-cut stock.
        // Tool tip is at z=-10 cutting through the stock; flute+shank+
        // holder extend ~60 mm above the tip — so the body sweeps
        // [-10, 50] in Z and hits the clamp at z=0..30. Also confirm
        // XY inflation honors holder.max_radius() (10 mm) by placing
        // the box centerline 6 mm from the tip XY, well inside R_holder.
        let holder = build_holder_60mm();
        let s = seg((0.0, 0.0, -10.0), (10.0, 0.0, -10.0));
        // Clamp box 5x5 mm with center at y=10, z 0..30. Cutting tip
        // at y=0 — 10 mm away in Y. With R_tool=3 the old check said
        // Clear (10 > 3 + half_depth=2.5). With R_holder=10 the
        // stadium covers the box. AND the Z gate must trigger: seg_z =
        // -10 is below z_bottom=0, but the body extends up to z=50.
        let fix = box_fixture(11, (0.0, 10.0), 5.0, 5.0, 0.0, 30.0);
        let res = check_segment_against_fixtures(&s, 3.0, Some(&holder), &[fix]);
        match res[0] {
            FixtureCheck::Collision { fixture_id, .. } => {
                assert_eq!(fixture_id, 11);
            }
            FixtureCheck::Clear => {
                panic!("expected shank/holder above tip to collide with high clamp, got Clear")
            }
        }
    }

    #[test]
    fn no_holder_keeps_tip_only_z_gate() {
        // Sanity: with no holder, the old tip-Z-only behavior is
        // preserved — a clamp far above the tip's Z range is Clear.
        let s = seg((0.0, 0.0, -10.0), (10.0, 0.0, -10.0));
        let fix = box_fixture(11, (0.0, 0.0), 5.0, 5.0, 0.0, 30.0);
        let res = check_segment_against_fixtures(&s, 3.0, None, &[fix]);
        assert!(matches!(res[0], FixtureCheck::Clear));
    }
}
