//! Constant-depth path walk with optional drag-knife trail correction and corner-feed reduction. Plus the chord→arc collapse used to shrink line runs to G2/G3.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

use super::wirbeln::{apply_wirbeln_with_state, WirbelnParams, WirbelnState};
use super::PostProcessor;
use crate::cam::setup::Setup;
use crate::geometry::{Point2, Segment, SegmentKind};
use crate::math;

/// Cut-pass dispatcher (3e5). When the active tool has a Wirbeln
/// overlay configured, route the path through the helical-spiral
/// emit; otherwise fall back to the standard corner-feed walker. All
/// five `multi_pass` call sites go through here so the Wirbeln check
/// lives in exactly one place.
///
/// qm9x: `wirbeln_state` carries the spiral phase + stride residual
/// across multiple `emit_cut_path` calls so the spiral doesn't reset
/// at every pass boundary. `multi_pass` instantiates ONE state before
/// the per-pass loop and reuses it across passes — matches 89n5's
/// cross-chord continuity for cross-pass continuity. Callers outside
/// `multi_pass` (the helix-cleanup pass etc.) pass a fresh state.
pub(super) fn emit_cut_path<P: PostProcessor>(
    segments: &[Segment],
    setup: &Setup,
    cut_z: f64,
    dragoff: f64,
    rate_h: u32,
    corner_feed_reduction: f64,
    wirbeln_state: &mut WirbelnState,
    post: &mut P,
) {
    if setup.tool.wirbeln_radius > 0.0 && setup.tool.wirbeln_stepover > 0.0 {
        let params = WirbelnParams {
            radius: setup.tool.wirbeln_radius,
            stepover: setup.tool.wirbeln_stepover,
            osc: setup.tool.wirbeln_osc,
            climb: setup.tool.wirbeln_climb,
        };
        let pts = apply_wirbeln_with_state(segments, cut_z, params, wirbeln_state);
        for (x, y, z) in pts {
            post.linear(Some(x), Some(y), Some(z));
        }
        return;
    }
    emit_path_with_corner_feed(
        segments,
        dragoff,
        setup.tool.drag_self_align_angle_rad,
        rate_h,
        corner_feed_reduction,
        post,
    );
}

/// oulh: reverse a polyline chain end-to-end so the cascade can
/// walk it back instead of plunging in place at the trailing
/// endpoint. Mirrors `cam::offsets::reverse_offset`'s arc handling
/// (swap endpoints and negate `bulge`) but operates on a borrowed
/// slice — the caller owns the returned Vec. Direction-sensitive
/// fields (layer, color, kind) pass through unchanged.
#[must_use]
pub(super) fn reverse_chain(segments: &[Segment]) -> Vec<Segment> {
    let mut rev: Vec<Segment> = segments.to_vec();
    rev.reverse();
    for s in &mut rev {
        std::mem::swap(&mut s.start, &mut s.end);
        s.bulge = -s.bulge;
    }
    rev
}

/// Polyline → arc collapse on emit. When `machine.arcs == true`,
/// walks `segments` and replaces consecutive `Line` runs (≥3 points)
/// with the fewest G2/G3 arcs that approximate the chord chain
/// within `effective_arc_tolerance()`. Pre-existing `Arc` / `Circle`
/// / `Point` segments are passed through verbatim — only line runs
/// are eligible. When `machine.arcs == false`, returns the input
/// untouched.
pub(crate) fn fit_line_runs(segments: &[Segment], setup: &Setup) -> Vec<Segment> {
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
    // 7iej.10: shared positive-sweep primitive (was an inline atan2 copy).
    let sweep = math::arc_sweep(center, start, end, ccw);
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
    self_align_angle_rad: f64,
    base_rate: u32,
    corner_reduction: f64,
    post: &mut P,
) {
    if corner_reduction <= 1e-6 || dragoff > 1e-9 || segments.len() < 2 {
        emit_path_with_dragoff(segments, dragoff, self_align_angle_rad, post);
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

/// Emit the drag-knife swivel arc that pivots the trailing blade
/// around `corner` from the trail offset perpendicular to `last_m`
/// to the trail offset perpendicular to `new_m`. Returns the
/// post-swivel cutter position (= `off2`), or `None` when the diff
/// is below the self-align threshold (`self_align_angle_rad`) and
/// the whole swivel + linear pre-move is skipped.
///
/// g30a: factored out so both Line and Arc branches in
/// `emit_path_with_dragoff` can call the same logic. Previously the
/// swivel was inlined only in the Line branch, so Line→Arc corners
/// emitted the arc with NO swivel — bending the blade.
///
/// 0t9o: when `self_align_angle_rad > 0`, skip the swivel + linear
/// pre-move entirely for corners whose tangent change |diff| is
/// below the threshold. Real drag knives self-align below ~30° via
/// the trailing offset, so emitting a swivel arc for every short
/// chord pivot (e.g. a 64-chord circle approximating a real arc)
/// bloats output and stresses the blade pivot. Returning `None`
/// signals "no pre-move emitted" so the caller updates `last_motion`
/// from the incoming direction rather than the post-swivel position.
fn emit_dragoff_swivel<P: PostProcessor>(
    corner: Point2,
    last_m: f64,
    new_m: f64,
    dragoff: f64,
    self_align_angle_rad: f64,
    post: &mut P,
) -> Option<(f64, f64)> {
    use std::f64::consts::{FRAC_PI_2, PI};
    let last_a = last_m + FRAC_PI_2;
    let new_a = new_m + FRAC_PI_2;
    let mut diff = new_a - last_a;
    while diff > PI {
        diff -= 2.0 * PI;
    }
    while diff < -PI {
        diff += 2.0 * PI;
    }
    // 0t9o: skip the linear pre-move + swivel arc for corners below
    // the self-align threshold. The next cut emit follows in the new
    // direction and the trailing blade snaps into alignment on its
    // own. We deliberately return None (not Some(off1)) so the
    // caller's `last_motion` tracks the incoming chord direction —
    // matters for downstream small-step chord chains where every
    // chord is just under threshold; without this, the residual
    // bias would accumulate.
    if self_align_angle_rad > 0.0 && diff.abs() < self_align_angle_rad {
        return None;
    }
    let off1 = (
        corner.x + dragoff * last_a.sin(),
        corner.y - dragoff * last_a.cos(),
    );
    let off2 = (
        corner.x + dragoff * new_a.sin(),
        corner.y - dragoff * new_a.cos(),
    );
    post.linear(Some(off1.0), Some(off1.1), None);
    if diff.abs() > 1e-6 {
        let i = corner.x - off1.0;
        let j = corner.y - off1.1;
        if diff > 0.0 {
            post.arc_ccw(Some(off2.0), Some(off2.1), None, Some(i), Some(j));
        } else {
            post.arc_cw(Some(off2.0), Some(off2.1), None, Some(i), Some(j));
        }
        Some(off2)
    } else {
        // At-threshold (>= self_align but ≤ 1e-6 diff): the linear
        // emit landed at off1 == off2; report off1 as resulting position.
        Some(off1)
    }
}

// math convention: rx_start / ry_start share the radius prefix.
#[allow(clippy::similar_names)]
fn emit_path_with_dragoff<P: PostProcessor>(
    segments: &[Segment],
    dragoff: f64,
    self_align_angle_rad: f64,
    post: &mut P,
) {
    let mut last_motion: Option<f64> = None;
    for seg in segments {
        match seg.kind {
            SegmentKind::Line => {
                let new_motion = (seg.end.y - seg.start.y).atan2(seg.end.x - seg.start.x);
                if dragoff > 1e-9 {
                    if let Some(last_m) = last_motion {
                        emit_dragoff_swivel(
                            seg.start,
                            last_m,
                            new_motion,
                            dragoff,
                            self_align_angle_rad,
                            post,
                        );
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
                // g30a: emit the drag-knife swivel arc at the
                // Line→Arc (or Arc→Arc) corner BEFORE the cut arc.
                // The arc's start tangent is the radius vector rotated
                // 90° in the arc's orientation (+90° for CCW / bulge>0,
                // -90° for CW). Without this, the blade enters the arc
                // pointing along the previous motion — bending the
                // blade and tearing material at every line→arc seam.
                let rx_start = seg.start.x - center.x;
                let ry_start = seg.start.y - center.y;
                let (sx, sy) = if seg.bulge > 0.0 {
                    (-ry_start, rx_start)
                } else {
                    (ry_start, -rx_start)
                };
                let start_tangent = sy.atan2(sx);
                if dragoff > 1e-9 {
                    if let Some(last_m) = last_motion {
                        emit_dragoff_swivel(
                            seg.start,
                            last_m,
                            start_tangent,
                            dragoff,
                            self_align_angle_rad,
                            post,
                        );
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::reverse_chain;
    use crate::geometry::{Point2, Segment};

    /// oulh: open-path cascade reversal needs `reverse_chain` to
    /// invert the polyline end-to-end so the next pass starts at
    /// the previous pass's exit. Three-segment chain: pre-reverse
    /// flows A→B→C→D; post-reverse flows D→C→B→A with each
    /// segment's endpoints swapped.
    #[test]
    fn reverse_chain_flips_endpoints_and_order() {
        let a = Point2::new(0.0, 0.0);
        let b = Point2::new(10.0, 0.0);
        let c = Point2::new(10.0, 5.0);
        let d = Point2::new(20.0, 5.0);
        let chain = vec![
            Segment::line(a, b, "0", 7),
            Segment::line(b, c, "0", 7),
            Segment::line(c, d, "0", 7),
        ];
        let rev = reverse_chain(&chain);
        assert_eq!(rev.len(), 3);
        assert_eq!(rev[0].start, d);
        assert_eq!(rev[0].end, c);
        assert_eq!(rev[1].start, c);
        assert_eq!(rev[1].end, b);
        assert_eq!(rev[2].start, b);
        assert_eq!(rev[2].end, a);
    }

    /// oulh: arc bulges must NEGATE on reversal — a CCW arc traversed
    /// backwards is a CW arc. Mirrors `cam::offsets::reverse_offset`'s
    /// arc handling.
    #[test]
    fn reverse_chain_negates_arc_bulge() {
        let a = Point2::new(0.0, 0.0);
        let b = Point2::new(10.0, 0.0);
        let mut arc = Segment::line(a, b, "0", 7);
        arc.kind = crate::geometry::SegmentKind::Arc;
        arc.bulge = 0.5;
        arc.center = Some(Point2::new(5.0, 4.33));
        let chain = vec![arc];
        let rev = reverse_chain(&chain);
        assert_eq!(rev[0].start, b);
        assert_eq!(rev[0].end, a);
        assert!((rev[0].bulge + 0.5).abs() < 1e-12);
    }

    /// Empty chain reverses to empty.
    #[test]
    fn reverse_chain_empty_is_empty() {
        let rev = reverse_chain(&[]);
        assert!(rev.is_empty());
    }
}
