//! Cut-order selection for offset lists. Honors `Setup::mill::objectorder` (`Unordered` / `Nearest` / `PerObject`).

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::Setup;
use crate::geometry::Point2;

pub(super) fn order_offsets(
    setup: &Setup,
    offsets: &[PolylineOffset],
    start: Point2,
) -> Vec<usize> {
    use crate::project::ObjectOrder;
    let n = offsets.len();
    if n == 0 {
        return Vec::new();
    }
    match setup.mill.objectorder {
        ObjectOrder::Unordered => (0..n).collect(),
        ObjectOrder::Nearest => greedy_nearest(offsets, start),
        ObjectOrder::PerObject => {
            // Group by source_object_idx (preserving first-seen order),
            // run nearest-neighbor inside each group seeded at the
            // previous group's end.
            let mut groups: Vec<Vec<usize>> = Vec::new();
            let mut group_of: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::default();
            for (i, o) in offsets.iter().enumerate() {
                let g = *group_of.entry(o.source_object_idx).or_insert_with(|| {
                    groups.push(Vec::new());
                    groups.len() - 1
                });
                groups[g].push(i);
            }
            let mut out = Vec::with_capacity(n);
            let mut pen = start;
            for group in groups {
                let group_offsets: Vec<&PolylineOffset> =
                    group.iter().map(|&i| &offsets[i]).collect();
                let local = greedy_nearest_among(&group_offsets, pen);
                for li in local {
                    let global = group[li];
                    out.push(global);
                    pen = end_pos(&offsets[global]);
                }
            }
            out
        }
    }
}

pub(super) fn greedy_nearest(offsets: &[PolylineOffset], start: Point2) -> Vec<usize> {
    let refs: Vec<&PolylineOffset> = offsets.iter().collect();
    greedy_nearest_among(&refs, start)
}

pub(super) fn greedy_nearest_among(offsets: &[&PolylineOffset], start: Point2) -> Vec<usize> {
    let n = offsets.len();
    if n == 0 {
        return Vec::new();
    }
    let mut taken = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut pen = start;
    for _ in 0..n {
        let mut best: Option<(usize, f64, u32, bool)> = None;
        for (i, o) in offsets.iter().enumerate() {
            if taken[i] {
                continue;
            }
            let d = pen.distance(start_pos_of(o));
            // Tie-breakers (in order):
            //   1. closer distance wins,
            //   2. deeper level wins (innermost ring first — pocket
            //      cascades unwind inside-out),
            //   3. non-finish before finish (rt1.24 — the dedicated
            //      finish-wall ring runs LAST so surface quality
            //      isn't degraded by re-traversing it).
            let level = o.level;
            let is_finish = o.is_finish;
            let better = match best {
                None => true,
                Some((_, bd, bl, bf)) => {
                    // Distance tiebreaker: only fall through to level/index
                    // ordering when the squared distances are within tool
                    // tolerance, since two computed f64 distances rarely
                    // coincide bit-for-bit even at the same nominal point.
                    if (d - bd).abs() > 1e-12 {
                        d < bd
                    } else if level != bl {
                        level > bl
                    } else {
                        !is_finish && bf
                    }
                }
            };
            if better {
                best = Some((i, d, level, is_finish));
            }
        }
        let (chosen, _, _, _) = best.unwrap();
        taken[chosen] = true;
        order.push(chosen);
        pen = end_pos(offsets[chosen]);
    }
    order
}

pub(super) fn start_pos_of(offset: &PolylineOffset) -> Point2 {
    offset
        .segments
        .first()
        .map_or(Point2::new(0.0, 0.0), |s| s.start)
}

pub(super) fn end_pos(offset: &PolylineOffset) -> Point2 {
    offset
        .segments
        .last()
        .map_or(Point2::new(0.0, 0.0), |s| s.end)
}
