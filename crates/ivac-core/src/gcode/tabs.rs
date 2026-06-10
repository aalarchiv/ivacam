//! Tab-aware path emission. Splits cuts where they cross a tab footprint and either lifts (Rectangle) or ramps (Ramp) Z over the tab.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

use super::PostProcessor;
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::math;

/// Emit the cut path with tab interruptions. For each LINE segment that
/// crosses a tab's `tab_radius` neighborhood, the cut is split: cut up to
/// the entry, lift Z to `tabs_z`, traverse to the exit, drop back to
/// `cut_z`, continue cutting (Rectangle); or ramp up / flat / ramp down
/// when `ramp_angle_deg` is `Some` (Ramp).
///
/// Arcs through tabs are tab-skipped with a straight Z lift even when
/// Ramp is requested — ramping along a curved path is a v2 follow-up.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_path_with_tabs<P: PostProcessor>(
    segments: &[Segment],
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: Option<f64>,
    // Drop from tabs_z back down to cut_z at PLUNGE feedrate
    // (rate_v), not cut feedrate (rate_h). The active feed when this
    // is called is rate_h; we swap to rate_v for the Z-down and
    // restore rate_h before the next horizontal cut.
    rate_v: u32,
    rate_h: u32,
    post: &mut P,
) {
    for seg in segments {
        match seg.kind {
            SegmentKind::Line => {
                emit_line_with_tabs(
                    seg,
                    tabs,
                    tabs_z,
                    cut_z,
                    tab_radius,
                    ramp_angle_deg,
                    rate_v,
                    rate_h,
                    post,
                );
            }
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), None),
            SegmentKind::Arc | SegmentKind::Circle => {
                // Proper arc-vs-tab-footprint intersection.
                // The prior chord-midpoint heuristic missed two
                // common failure modes — long arcs whose chord
                // midpoint is on the opposite side of the arc from
                // the tab, and arcs that graze the tab's BOW outside
                // the chord. It also lifted the entire arc when a
                // short chord's midpoint happened to land inside a
                // tab. Here we walk every tab, check whether the
                // tab's disc actually intersects the arc's sweep
                // (within its angular span), and pick the MAX lift
                // (per-tab overrides). The chord-midpoint behavior
                // survives as the deferred "ramping along curved path"
                // v2 fallback inside emit_arc_chord_with_tabs (which
                // already chord-tessellates the arc and reuses
                // line-tab math).
                let fallback_width = tab_radius * 2.0;
                let fallback_lift = (tabs_z - cut_z).abs();
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let arc_radius = (seg.start.x - center.x).hypot(seg.start.y - center.y);
                let arc_tab_z = tabs
                    .iter()
                    .filter_map(|t| {
                        let r = t.radius(fallback_width);
                        if r <= 0.0 {
                            return None;
                        }
                        if arc_intersects_tab(seg, center, arc_radius, t.x, t.y, r) {
                            Some(cut_z + t.lift(fallback_lift))
                        } else {
                            None
                        }
                    })
                    .fold(f64::NEG_INFINITY, f64::max);
                let crosses = arc_tab_z.is_finite();
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if !crosses {
                    if seg.bulge > 0.0 {
                        post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    } else {
                        post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    }
                } else if let Some(ramp) = ramp_angle_deg {
                    emit_arc_chord_with_tabs(
                        seg, tabs, tabs_z, cut_z, tab_radius, ramp, rate_v, rate_h, post,
                    );
                } else {
                    post.linear(None, None, Some(arc_tab_z));
                    if seg.bulge > 0.0 {
                        post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    } else {
                        post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    }
                    // Drop at plunge feed, restore cut feed.
                    post.feedrate(rate_v);
                    post.linear(None, None, Some(cut_z));
                    post.feedrate(rate_h);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_line_with_tabs<P: PostProcessor>(
    seg: &Segment,
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: Option<f64>,
    rate_v: u32,
    rate_h: u32,
    post: &mut P,
) {
    let dx = seg.end.x - seg.start.x;
    let dy = seg.end.y - seg.start.y;
    let len = dx.hypot(dy);
    if len < 1e-9 {
        return;
    }
    // Walk the segment; for every tab whose perpendicular foot is on the
    // segment within its own effective radius, compute t-entry / t-exit
    // and the per-tab effective lift Z (width / height overrides now flow
    // through per-tab instead of using the op-level values uniformly).
    let fallback_width = tab_radius * 2.0;
    let fallback_lift = (tabs_z - cut_z).abs();
    let mut intervals: Vec<(f64, f64, f64)> = Vec::new();
    for tab in tabs {
        let r = tab.radius(fallback_width);
        if r <= 0.0 {
            continue;
        }
        let tx = tab.x - seg.start.x;
        let ty = tab.y - seg.start.y;
        let t = (tx * dx + ty * dy) / (len * len);
        let perp_x = tx - t * dx;
        let perp_y = ty - t * dy;
        let perp = (perp_x * perp_x + perp_y * perp_y).sqrt();
        if perp > r {
            continue;
        }
        let half = (r * r - perp * perp).sqrt() / len;
        let t_in = (t - half).max(0.0);
        let t_out = (t + half).min(1.0);
        if t_out > t_in {
            let z_top = cut_z + tab.lift(fallback_lift);
            intervals.push((t_in, t_out, z_top));
        }
    }
    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Merge overlaps; overlapping tabs use the higher of their lifts
    // so the cutter clears both.
    let mut merged: Vec<(f64, f64, f64)> = Vec::new();
    for (a, b, z) in intervals {
        if let Some(last) = merged.last_mut() {
            if a <= last.1 + 1e-6 {
                last.1 = last.1.max(b);
                last.2 = last.2.max(z);
                continue;
            }
        }
        merged.push((a, b, z));
    }
    // Emit: cut up to each interval, lift / ramp, traverse, drop / ramp,
    // repeat. Per-interval `interval_z` is the (per-tab) effective lift,
    // so a tab with a non-default height override gets its own Z plateau.
    let mut cursor = 0.0;
    for (t_in, t_out, interval_z) in merged {
        if t_in > cursor + 1e-6 {
            let p = lerp(seg, t_in);
            post.linear(Some(p.0), Some(p.1), None);
        }
        let dz_here = (interval_z - cut_z).abs();
        let ramp_length = ramp_angle_deg.map(|a| {
            if dz_here < 1e-9 {
                0.0
            } else {
                dz_here / a.to_radians().tan()
            }
        });
        match ramp_length {
            Some(rl) if rl > 1e-9 => {
                let tab_world_len = (t_out - t_in) * len;
                if tab_world_len < 2.0 * rl {
                    let t_mid = 0.5 * (t_in + t_out);
                    let mid = lerp(seg, t_mid);
                    post.linear(Some(mid.0), Some(mid.1), Some(interval_z));
                    let exit = lerp(seg, t_out);
                    post.linear(Some(exit.0), Some(exit.1), Some(cut_z));
                } else {
                    let dt_ramp = rl / len;
                    let t_up_end = t_in + dt_ramp;
                    let t_down_start = t_out - dt_ramp;
                    let up_end = lerp(seg, t_up_end);
                    let down_start = lerp(seg, t_down_start);
                    let exit = lerp(seg, t_out);
                    post.linear(Some(up_end.0), Some(up_end.1), Some(interval_z));
                    post.linear(Some(down_start.0), Some(down_start.1), None);
                    post.linear(Some(exit.0), Some(exit.1), Some(cut_z));
                }
            }
            _ => {
                // Lift to tabs_z at the active cut feed (rate_h
                // is already set), traverse across the tab footprint
                // at cut feed, then drop back to cut_z at PLUNGE feed
                // (rate_v). Restore cut feed before the next cut move.
                post.linear(None, None, Some(interval_z));
                let p_out = lerp(seg, t_out);
                post.linear(Some(p_out.0), Some(p_out.1), None);
                post.feedrate(rate_v);
                post.linear(None, None, Some(cut_z));
                post.feedrate(rate_h);
            }
        }
        cursor = t_out;
    }
    if cursor < 1.0 - 1e-6 {
        post.linear(Some(seg.end.x), Some(seg.end.y), None);
    }
}

fn lerp(seg: &Segment, t: f64) -> (f64, f64) {
    (
        seg.start.x + t * (seg.end.x - seg.start.x),
        seg.start.y + t * (seg.end.y - seg.start.y),
    )
}

/// True arc-vs-tab-disc intersection test. The arc lives on a
/// circle of radius `arc_radius` around `center` swept from
/// `seg.start` to `seg.end` (signed sweep = 4·atan(bulge)); the tab
/// is a disc of radius `tab_radius` around `(tab_x, tab_y)`.
/// Returns true when any point of the arc's sweep lies inside the
/// disc.
///
/// Strategy:
/// 1. Cheap reject: if `dist(center, tab) > arc_radius + tab_radius`
///    or `< |arc_radius - tab_radius|`, the two circles miss entirely
///    (no two-circle intersection).
/// 2. If the tab disc fully contains the arc circle (or vice versa)
///    we're definitely inside.
/// 3. Otherwise compute the two intersection angles between the arc
///    circle and the tab circle, then check whether either lands
///    inside the arc's angular sweep — OR whether either of the
///    arc's endpoints sits inside the disc (tangential graze case).
// math convention: dx/dy components share the `d`-prefix.
#[allow(clippy::similar_names)]
fn arc_intersects_tab(
    seg: &Segment,
    center: Point2,
    arc_radius: f64,
    tab_x: f64,
    tab_y: f64,
    tab_radius: f64,
) -> bool {
    // Endpoint-in-disc shortcut: catches tangential grazes where the
    // arc lifts entirely into the tab.
    let start_in_disc = (seg.start.x - tab_x).hypot(seg.start.y - tab_y) <= tab_radius;
    if start_in_disc {
        return true;
    }
    let end_in_disc = (seg.end.x - tab_x).hypot(seg.end.y - tab_y) <= tab_radius;
    if end_in_disc {
        return true;
    }
    if arc_radius < 1e-9 || tab_radius < 1e-9 {
        return false;
    }
    let cdx = tab_x - center.x;
    let cdy = tab_y - center.y;
    let d = cdx.hypot(cdy);
    // Two circles miss entirely.
    if d > arc_radius + tab_radius + 1e-9 {
        return false;
    }
    // One contains the other with NO intersection ring — the arc
    // either sits fully outside or fully inside the disc. Endpoint
    // checks above handle the "fully inside" case; the "fully
    // contains the arc circle but center far away" case is impossible
    // when both endpoints are outside.
    if d + arc_radius < tab_radius - 1e-9 {
        // Arc circle fully inside tab disc — every arc point is in.
        return true;
    }
    if d + tab_radius < arc_radius - 1e-9 {
        // Tab disc fully inside arc circle, on the side away from
        // every point of the arc (since endpoints were both outside).
        // In that case the arc never visits the tab disc.
        return false;
    }
    // Two intersection points of the arc circle and the tab circle.
    // Place the tab center along the +X axis from arc center
    // (rotation by phi = atan2(cdy, cdx)) — angle of the two
    // intersection points relative to arc center is phi ± alpha
    // where alpha = acos((d² + arc_radius² - tab_radius²) / (2·d·arc_radius)).
    let cos_alpha =
        (d * d + arc_radius * arc_radius - tab_radius * tab_radius) / (2.0 * d * arc_radius);
    // Clamp for FP safety (near-tangent cases land just past ±1).
    let cos_alpha = cos_alpha.clamp(-1.0, 1.0);
    let alpha = cos_alpha.acos();
    let phi = cdy.atan2(cdx);
    let theta_a = phi + alpha;
    let theta_b = phi - alpha;
    let theta_start = (seg.start.y - center.y).atan2(seg.start.x - center.x);
    // Signed sweep (positive = CCW); matches bulge convention.
    let sweep = 4.0 * seg.bulge.atan();
    math::arc_contains_angle(theta_start, sweep, theta_a)
        || math::arc_contains_angle(theta_start, sweep, theta_b)
}

/// Emit a tab-crossing arc by discretizing it into short chord
/// segments and reusing the line-tab ramp logic per chord. The chord
/// chain replaces the original G2/G3 with G1 moves that can carry the
/// trapezoid Z profile. Used only when an arc actually crosses a tab
/// and the tab type is Ramp.
#[allow(clippy::too_many_arguments)]
fn emit_arc_chord_with_tabs<P: PostProcessor>(
    seg: &Segment,
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: f64,
    rate_v: u32,
    rate_h: u32,
    post: &mut P,
) {
    let center = seg
        .center
        .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
    let r = (seg.start.x - center.x).hypot(seg.start.y - center.y);
    if r < 1e-9 {
        // Degenerate arc — just emit the endpoints as a line.
        let line = Segment::line(seg.start, seg.end, seg.layer.clone(), seg.color);
        emit_line_with_tabs(
            &line,
            tabs,
            tabs_z,
            cut_z,
            tab_radius,
            Some(ramp_angle_deg),
            rate_v,
            rate_h,
            post,
        );
        return;
    }
    let theta_start = (seg.start.y - center.y).atan2(seg.start.x - center.x);
    let theta_end = (seg.end.y - center.y).atan2(seg.end.x - center.x);
    // Bulge sign: positive ⇒ CCW (signed sweep > 0). Total swept angle
    // satisfies sweep = 4·atan(bulge), preserving sign.
    let sweep = 4.0 * seg.bulge.atan();
    // Chord count: 32 chords for a full circle is plenty (chord error
    // ~ r·(1 - cos(π/32)) ≈ r·0.005; on a 10 mm arc that's 0.05 mm —
    // visually identical and well under typical tab tolerances). Scale
    // chords linearly with sweep magnitude, with a 4-chord minimum.
    let n_chords = (32.0 * sweep.abs() / std::f64::consts::TAU).ceil().max(4.0) as usize;
    let dtheta = sweep / (n_chords as f64);
    let mut prev_theta = theta_start;
    for k in 0..n_chords {
        let next_theta = if k + 1 == n_chords {
            // Snap last endpoint to the original arc end so
            // floating-point error doesn't leave a gap.
            theta_end
        } else {
            theta_start + dtheta * ((k + 1) as f64)
        };
        let a = Point2::new(
            center.x + r * prev_theta.cos(),
            center.y + r * prev_theta.sin(),
        );
        let b = if k + 1 == n_chords {
            seg.end
        } else {
            Point2::new(
                center.x + r * next_theta.cos(),
                center.y + r * next_theta.sin(),
            )
        };
        let chord = Segment::line(a, b, seg.layer.clone(), seg.color);
        emit_line_with_tabs(
            &chord,
            tabs,
            tabs_z,
            cut_z,
            tab_radius,
            Some(ramp_angle_deg),
            rate_v,
            rate_h,
            post,
        );
        prev_theta = next_theta;
    }
}
