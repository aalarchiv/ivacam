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
pub(super) fn emit_path_with_tabs<P: PostProcessor>(
    segments: &[Segment],
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: Option<f64>,
    post: &mut P,
) {
    for seg in segments {
        match seg.kind {
            SegmentKind::Line => {
                emit_line_with_tabs(seg, tabs, tabs_z, cut_z, tab_radius, ramp_angle_deg, post);
            }
            SegmentKind::Point => post.linear(Some(seg.start.x), Some(seg.start.y), None),
            SegmentKind::Arc | SegmentKind::Circle => {
                // Per-tab radius for crossing detection. Walks all tabs
                // and uses the MAX lift Z of any that touches this arc
                // (audit 3wv: per-tab overrides). The midpoint-of-chord
                // heuristic stays — exact arc-intersection math here
                // would be heavier and the chord-mid check has been the
                // shipped behavior since rt1.10.
                let fallback_width = tab_radius * 2.0;
                let fallback_lift = (tabs_z - cut_z).abs();
                let mid_x = (seg.start.x + seg.end.x) * 0.5;
                let mid_y = (seg.start.y + seg.end.y) * 0.5;
                let arc_tab_z = tabs
                    .iter()
                    .filter_map(|t| {
                        let r = t.radius(fallback_width);
                        if (mid_x - t.x).hypot(mid_y - t.y) < r {
                            Some(cut_z + t.lift(fallback_lift))
                        } else {
                            None
                        }
                    })
                    .fold(f64::NEG_INFINITY, f64::max);
                let crosses = arc_tab_z.is_finite();
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if !crosses {
                    if seg.bulge > 0.0 {
                        post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    } else {
                        post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    }
                } else if let Some(ramp) = ramp_angle_deg {
                    emit_arc_chord_with_tabs(seg, tabs, tabs_z, cut_z, tab_radius, ramp, post);
                } else {
                    post.linear(None, None, Some(arc_tab_z));
                    if seg.bulge > 0.0 {
                        post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    } else {
                        post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                    }
                    post.linear(None, None, Some(cut_z));
                }
            }
        }
    }
}

fn emit_line_with_tabs<P: PostProcessor>(
    seg: &Segment,
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: Option<f64>,
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
    // and the per-tab effective lift Z (audit 3wv: width / height
    // overrides now flow through per-tab instead of using the op-level
    // values uniformly).
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
                post.linear(None, None, Some(interval_z));
                let p_out = lerp(seg, t_out);
                post.linear(Some(p_out.0), Some(p_out.1), None);
                post.linear(None, None, Some(cut_z));
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

/// Emit a tab-crossing arc by discretizing it into short chord
/// segments and reusing the line-tab ramp logic per chord. The chord
/// chain replaces the original G2/G3 with G1 moves that can carry the
/// trapezoid Z profile. Used only when an arc actually crosses a tab
/// and the tab type is Ramp.
fn emit_arc_chord_with_tabs<P: PostProcessor>(
    seg: &Segment,
    tabs: &[crate::cam::offsets::TabPoint],
    tabs_z: f64,
    cut_z: f64,
    tab_radius: f64,
    ramp_angle_deg: f64,
    post: &mut P,
) {
    let center = seg
        .center
        .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
    let r = (seg.start.x - center.x).hypot(seg.start.y - center.y);
    if r < 1e-9 {
        // Degenerate arc — just emit the endpoints as a line.
        let line = Segment::line(seg.start, seg.end, &seg.layer, seg.color);
        emit_line_with_tabs(
            &line,
            tabs,
            tabs_z,
            cut_z,
            tab_radius,
            Some(ramp_angle_deg),
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
        let chord = Segment::line(a, b, &seg.layer, seg.color);
        emit_line_with_tabs(
            &chord,
            tabs,
            tabs_z,
            cut_z,
            tab_radius,
            Some(ramp_angle_deg),
            post,
        );
        prev_theta = next_theta;
    }
}
