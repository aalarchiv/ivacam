//! Constant-depth path walk with optional drag-knife trail correction and corner-feed reduction. Plus the chord→arc collapse used to shrink line runs to G2/G3.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

use super::PostProcessor;
use crate::cam::setup::Setup;
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::math;

/// Emit segments with optional drag-knife trailing offset. When
/// `dragoff > 0`, every line→line corner is preceded by an arc that swivels
/// the blade around the corner point so the trail aligns with the new
/// direction. Mirrors `viaconstructor.machine_cmd.segment2machine_cmd`.
/// Walk `segments` like `emit_path_with_dragoff` but reduce the feed
/// at sharp line-line corners by `corner_reduction` (a fraction in
/// Polyline → arc collapse on emit. When `machine.arcs == true`, walks
/// `segments` and replaces consecutive `Line` runs (≥3 points) with the
/// fewest G2/G3 arcs that approximate the chord chain within
/// `effective_arc_tolerance()`. Pre-existing `Arc` / `Circle` / `Point`
/// segments are passed through verbatim — only line runs are eligible.
/// When `machine.arcs == false`, returns the input untouched.
pub(super) fn fit_line_runs(segments: &[Segment], setup: &Setup) -> Vec<Segment> {
    if !setup.machine.arcs || segments.is_empty() {
        return segments.to_vec();
    }
    let tol = setup.machine.effective_arc_tolerance();
    let mut out: Vec<Segment> = Vec::with_capacity(segments.len());
    let layer = segments[0].layer.clone();
    let color = segments[0].color;
    let mut run_pts: Vec<Point2> = Vec::new();
    let mut run_layer = layer.clone();
    let mut run_color = color;

    let flush_run =
        |run_pts: &mut Vec<Point2>, run_layer: &str, run_color: i32, out: &mut Vec<Segment>| {
            if run_pts.len() < 2 {
                run_pts.clear();
                return;
            }
            match crate::gcode::arc_fit::fit_arc_run(run_pts, tol) {
                crate::gcode::arc_fit::FitOutput::Lines(pts) => {
                    for w in pts.windows(2) {
                        out.push(Segment::line(w[0], w[1], run_layer, run_color));
                    }
                }
                crate::gcode::arc_fit::FitOutput::Arcs(arcs) => {
                    let mut cursor = run_pts[0];
                    for a in arcs {
                        let (_, _, bulge) = arc_bulge_from_center(cursor, a.end, a.center, a.ccw);
                        out.push(Segment::arc(
                            cursor,
                            a.end,
                            bulge,
                            Some(a.center),
                            run_layer,
                            run_color,
                        ));
                        cursor = a.end;
                    }
                }
            }
            run_pts.clear();
        };

    for seg in segments {
        if matches!(seg.kind, SegmentKind::Line) {
            if run_pts.is_empty() {
                run_pts.push(seg.start);
                run_layer.clone_from(&seg.layer);
                run_color = seg.color;
            }
            run_pts.push(seg.end);
        } else {
            flush_run(&mut run_pts, &run_layer, run_color, &mut out);
            out.push(seg.clone());
        }
    }
    flush_run(&mut run_pts, &run_layer, run_color, &mut out);
    out
}

/// Derive a polyline `bulge` from a known arc geometry (start, end,
/// absolute center, direction). The sign of `bulge` matches our
/// convention: positive ⇒ CCW (G3), negative ⇒ CW (G2).
fn arc_bulge_from_center(
    start: Point2,
    end: Point2,
    center: Point2,
    ccw: bool,
) -> (Point2, f64, f64) {
    let a0 = (start.y - center.y).atan2(start.x - center.x);
    let a1 = (end.y - center.y).atan2(end.x - center.x);
    let mut sweep = if ccw { a1 - a0 } else { a0 - a1 };
    while sweep < 0.0 {
        sweep += std::f64::consts::TAU;
    }
    while sweep > std::f64::consts::TAU {
        sweep -= std::f64::consts::TAU;
    }
    let signed_sweep = if ccw { sweep } else { -sweep };
    let bulge = (signed_sweep * 0.25).tan();
    (center, sweep, bulge)
}

/// [0, 1]). Skipped when `corner_reduction <= 0`, when `dragoff > 0`
/// (drag knife trail compensation already smooths corners), or when
/// the segment list is too short to have corners.
///
/// Detection threshold: the angle change at a join >= 60° (computed
/// as the supplement of the dot product). The slowed feed is emitted
/// before the second segment; the original feed is restored after.
pub(super) fn emit_path_with_corner_feed<P: PostProcessor>(
    segments: &[Segment],
    dragoff: f64,
    base_rate: u32,
    corner_reduction: f64,
    post: &mut P,
) {
    if corner_reduction <= 1e-6 || dragoff > 1e-9 || segments.len() < 2 {
        emit_path_with_dragoff(segments, dragoff, post);
        return;
    }
    let reduced_rate = (f64::from(base_rate) * (1.0 - corner_reduction)).max(1.0) as u32;
    let cos_threshold = 0.5_f64; // 60° turn → cos(angle) <= 0.5
    let mut feed_currently_reduced = false;
    let mut prev_dir: Option<(f64, f64)> = None;
    for (i, seg) in segments.iter().enumerate() {
        // Restore feed for arcs and points — they don't have sharp
        // corners by definition.
        if !matches!(seg.kind, SegmentKind::Line) {
            if feed_currently_reduced {
                post.feedrate(base_rate);
                feed_currently_reduced = false;
            }
            // Single-segment emit reusing emit_path_with_dragoff's logic
            // would be over-engineered; just inline arc/point here.
            match seg.kind {
                SegmentKind::Arc | SegmentKind::Circle => {
                    let center = seg
                        .center
                        .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                    let cx = center.x - seg.start.x;
                    let cy = center.y - seg.start.y;
                    if seg.bulge > 0.0 {
                        post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(cx), Some(cy));
                    } else {
                        post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(cx), Some(cy));
                    }
                }
                SegmentKind::Point => {
                    post.linear(Some(seg.start.x), Some(seg.start.y), None);
                }
                SegmentKind::Line => {}
            }
            prev_dir = None;
            continue;
        }
        let dx = seg.end.x - seg.start.x;
        let dy = seg.end.y - seg.start.y;
        let len = (dx * dx + dy * dy).sqrt();
        // Zero-length segments don't have a direction; emit them as a
        // plain linear and DO NOT update prev_dir so the next real
        // segment compares against the last meaningful direction. A
        // (0,0) cur_dir would otherwise flag dot=0 (= 90° turn) and
        // spuriously slow the feed.
        if len <= 1e-9 {
            post.linear(Some(seg.end.x), Some(seg.end.y), None);
            continue;
        }
        let cur_dir = (dx / len, dy / len);
        let needs_reduction = match prev_dir {
            Some((px, py)) if i > 0 => {
                // dot product < cos_threshold means the turn is
                // sharper than ~60°.
                let dot = px * cur_dir.0 + py * cur_dir.1;
                dot < cos_threshold
            }
            _ => false,
        };
        if needs_reduction && !feed_currently_reduced {
            post.feedrate(reduced_rate);
            feed_currently_reduced = true;
        } else if !needs_reduction && feed_currently_reduced {
            post.feedrate(base_rate);
            feed_currently_reduced = false;
        }
        post.linear(Some(seg.end.x), Some(seg.end.y), None);
        prev_dir = Some(cur_dir);
    }
    if feed_currently_reduced {
        post.feedrate(base_rate);
    }
}

fn emit_path_with_dragoff<P: PostProcessor>(segments: &[Segment], dragoff: f64, post: &mut P) {
    use std::f64::consts::{FRAC_PI_2, PI};
    let mut last_motion: Option<f64> = None;
    for seg in segments {
        match seg.kind {
            SegmentKind::Line => {
                let new_motion = (seg.end.y - seg.start.y).atan2(seg.end.x - seg.start.x);
                if dragoff > 1e-9 {
                    if let Some(last_m) = last_motion {
                        let last_a = last_m + FRAC_PI_2;
                        let new_a = new_motion + FRAC_PI_2;
                        let off1 = (
                            seg.start.x + dragoff * last_a.sin(),
                            seg.start.y - dragoff * last_a.cos(),
                        );
                        let off2 = (
                            seg.start.x + dragoff * new_a.sin(),
                            seg.start.y - dragoff * new_a.cos(),
                        );
                        post.linear(Some(off1.0), Some(off1.1), None);
                        let mut diff = new_a - last_a;
                        while diff > PI {
                            diff -= 2.0 * PI;
                        }
                        while diff < -PI {
                            diff += 2.0 * PI;
                        }
                        if diff.abs() > 1e-6 {
                            let i = seg.start.x - off1.0;
                            let j = seg.start.y - off1.1;
                            if diff > 0.0 {
                                post.arc_ccw(Some(off2.0), Some(off2.1), None, Some(i), Some(j));
                            } else {
                                post.arc_cw(Some(off2.0), Some(off2.1), None, Some(i), Some(j));
                            }
                        }
                    }
                }
                post.linear(Some(seg.end.x), Some(seg.end.y), None);
                last_motion = Some(new_motion);
            }
            SegmentKind::Point => {
                post.linear(Some(seg.start.x), Some(seg.start.y), None);
                last_motion = None;
            }
            SegmentKind::Arc | SegmentKind::Circle => {
                let center = seg
                    .center
                    .unwrap_or_else(|| math::bulge_to_arc(seg.start, seg.end, seg.bulge).0);
                let i = center.x - seg.start.x;
                let j = center.y - seg.start.y;
                if seg.bulge > 0.0 {
                    post.arc_ccw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                } else {
                    post.arc_cw(Some(seg.end.x), Some(seg.end.y), None, Some(i), Some(j));
                }
                // Tangent at end of arc: rotate radius 90° in the arc's
                // orientation. CCW arc → +90° rotation; CW → -90°.
                let rx = seg.end.x - center.x;
                let ry = seg.end.y - center.y;
                let (tx, ty) = if seg.bulge > 0.0 {
                    (-ry, rx)
                } else {
                    (ry, -rx)
                };
                last_motion = Some(ty.atan2(tx));
            }
        }
    }
}
