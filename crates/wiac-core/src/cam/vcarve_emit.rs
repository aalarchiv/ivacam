//! V-Carve progressive-deepening Z emission.
//!
//! A V-bit has zero tip area, so a one-shot deep plunge into a wide
//! groove is mechanically impossible. Instead, the cutter walks the
//! medial-axis polyline at progressively greater depths (depth-per-pass
//! steps). On each forward sweep we cut one level deeper at every
//! visited point but never below the polyline's actual target Z; when
//! the polyline rises (the groove narrows), we reverse the sweep back
//! to where the previous level was last cut, then resume forward.
//!
//! This module returns a list of Z-stamped polylines (`Vec<Vec<(x, y,
//! z)>>`) ready to be turned into G-code by the standard polyline
//! emitter; it does NOT call into the post-processor itself, which
//! keeps the module decoupled from the gcode crate.

// # CAM/sim pedantic-lint exemptions
// V-carve emitter casts medial-axis sample indices (bounded by polyline
// length) to f64 for arc-fit input.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

/// One waypoint along the emitted toolpath: absolute XYZ. Multiple
/// returned polylines are connected by G0 lifts to safe Z by the
/// caller.
pub type ZPolyline = Vec<(f64, f64, f64)>;

/// Default lead-in ramp angle (degrees from horizontal). pmpk fix:
/// the medial-axis chain endpoints sit AT boundary-touching vertices
/// (R ≈ 0) so the cutter would otherwise plunge vertically by `dpp`
/// into solid stock on its first cut — fatal for a sharp V-bit which
/// has effectively zero safe plunge depth. Vectric Aspire and Estlcam
/// both use a ramp lead-in for V-carve entry; 10° from horizontal is
/// a defensible conservative default (≈ 5.7× more XY travel than
/// vertical drop). ot80: now configurable per-tool via
/// [`crate::project::ToolEntry::vcarve_lead_in_angle_deg`]; this
/// constant remains the fallback when the tool field is unset.
pub const LEAD_IN_ANGLE_DEG: f64 = 10.0;

/// Build the full V-Carve sweep for a single per-point-Z polyline.
///
/// `axis` is `(x, y, z, r)` where `z <= 0` is the geometric target
/// depth at that point and `r` is the inscribed-circle radius (kept
/// only for diagnostics). `depth_per_pass` is the per-level step
/// magnitude (always positive — the cutter goes negative). The result
/// is a list of sub-polylines whose Z monotonically respects the
/// ratchet: every segment starts at the cut-Z reached by the previous
/// segment and never violates the polyline's actual `z`. **kagr**: the
/// emitter splits its output into multiple sub-polylines so the
/// caller (V-Carve / Halfpipe drivers) can rapid (G0) between them
/// over uncut stock instead of dragging the bit along the surface at
/// feed rate. Before kagr the emitter returned a single continuous
/// polyline that contained intermediate `z=0` waypoints across uncut
/// medial-axis points; the gcode emitter then dragged the non-flat
/// V-bit tip across the workpiece surface, marring it.
///
/// **pmpk:** the first sub-polyline begins with an angled lead-in ramp
/// that walks along the chain's spine while Z descends from 0 to the
/// first-cut depth (`-dpp` or the chain's shallowest target, whichever
/// is shallower). This avoids the V-bit-snapping vertical plunge that
/// would otherwise occur at the R≈0 chain endpoint. If the chain is
/// too short to fit the ramp at [`LEAD_IN_ANGLE_DEG`], the first cut
/// is depth-limited (the ramp angle is preserved; the entry depth is
/// reduced).
// V-carve ratchet emitter packs densification, lead-in ramp, and
// progressive-deepening sweep into one state machine — extraction
// would split tightly-coupled cut_z/path state across helpers.
#[allow(clippy::too_many_lines)]
pub fn ratchet_emit(axis: &[(f64, f64, f64, f64)], depth_per_pass: f64) -> Vec<ZPolyline> {
    ratchet_emit_with_lead_in(axis, depth_per_pass, LEAD_IN_ANGLE_DEG)
}

/// ot80: same as `ratchet_emit` but with a configurable lead-in angle
/// (degrees from horizontal). Values outside (0°, 90°) silently fall
/// back to the legacy 10° default — this is the kernel of the
/// configurable lead-in ramp; the per-tool setting flows through
/// `ToolEntry::vcarve_lead_in_angle_deg` → `ToolConfig` → here.
#[allow(clippy::too_many_lines)]
pub fn ratchet_emit_with_lead_in(
    axis: &[(f64, f64, f64, f64)],
    depth_per_pass: f64,
    lead_in_angle_deg: f64,
) -> Vec<ZPolyline> {
    let lead_in_angle = if lead_in_angle_deg.is_finite()
        && lead_in_angle_deg > 0.0
        && lead_in_angle_deg < 90.0
    {
        lead_in_angle_deg
    } else {
        LEAD_IN_ANGLE_DEG
    };
    if axis.len() < 2 {
        return Vec::new();
    }
    let dpp = depth_per_pass.abs().max(1e-6);

    // Densify the polyline at each Z-level crossing so the cutter
    // doesn't skip a level between two points whose original Z values
    // straddle it.
    let z_min = axis.iter().map(|&(_, _, z, _)| z).fold(0.0_f64, f64::min);
    let n_levels = ((-z_min) / dpp).ceil() as usize;
    let mut levels: Vec<f64> = (1..=n_levels).map(|i| -(i as f64) * dpp).collect();
    levels.push(z_min);
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
    levels.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    let mut dense: Vec<(f64, f64, f64)> = Vec::with_capacity(axis.len() * 2);
    for win in axis.windows(2) {
        let (ax, ay, az, _) = win[0];
        let (bx, by, bz, _) = win[1];
        if dense.is_empty() {
            dense.push((ax, ay, az));
        }
        // Insert a waypoint at every level in (min(az,bz), max(az,bz)).
        let (lo, hi) = if az < bz { (az, bz) } else { (bz, az) };
        for &lvl in &levels {
            if lvl > lo + 1e-9 && lvl < hi - 1e-9 {
                let t = (lvl - az) / (bz - az);
                if t > 1e-9 && t < 1.0 - 1e-9 {
                    dense.push((ax + t * (bx - ax), ay + t * (by - ay), lvl));
                }
            }
        }
        dense.push((bx, by, bz));
    }
    // Coalesce consecutive duplicates introduced by level-crossing
    // insertion at the segment boundaries.
    let mut compact: Vec<(f64, f64, f64)> = Vec::with_capacity(dense.len());
    for p in dense {
        if let Some(last) = compact.last() {
            if (last.0 - p.0).abs() < 1e-9
                && (last.1 - p.1).abs() < 1e-9
                && (last.2 - p.2).abs() < 1e-9
            {
                continue;
            }
        }
        compact.push(p);
    }
    if compact.len() < 2 {
        return Vec::new();
    }

    // Ratchet sweep. cut_z[i] tracks the Z already cut at point i —
    // initialized to 0 (top of stock). Each forward pass advances cut_z
    // toward dense[i].2 by at most one DPP. When the polyline rises
    // above the current cutting front, we step back to the previous
    // level's leading edge, then resume forward at the next deeper
    // level.
    let n = compact.len();
    let target_z: Vec<f64> = compact.iter().map(|&(_, _, z)| z).collect();
    let mut cut_z: Vec<f64> = vec![0.0; n];

    let mut path: Vec<(f64, f64, f64)> = Vec::new();

    // pmpk: emit angled lead-in ramp from z=0 down to z=first_cut_z
    // along the chain's spine, replacing the original `-dpp` forward
    // sweep. The ramp slope is set by `lead_in_angle` (resolved above)
    // so the V-bit shaves a sloped sliver of material instead of
    // plunging vertically into solid stock at the R≈0 chain endpoint.
    // After the ramp finishes (or the chain ends, whichever comes
    // first), every following point is cut at the post-ramp depth
    // (-dpp). The ratchet then continues at -2*dpp, -3*dpp, ...
    let first_cut_z = (-dpp).max(z_min);
    let tan_angle = lead_in_angle.to_radians().tan();
    let lead_in_len = (-first_cut_z) / tan_angle;
    // Cumulative XY arc length along compact — used to pace the ramp.
    let mut arc: Vec<f64> = Vec::with_capacity(n);
    arc.push(0.0);
    for i in 1..n {
        let dx = compact[i].0 - compact[i - 1].0;
        let dy = compact[i].1 - compact[i - 1].1;
        arc.push(arc[i - 1] + dx.hypot(dy));
    }
    let total_arc = *arc.last().unwrap_or(&0.0);
    // If the chain is too short to fit the ramp, depth-limit the entry:
    // keep the angle and shorten the descent. The V-bit reaches only
    // `-tan(angle) * total_arc` on this first sweep — deeper ratchet
    // sweeps then resume normally (they cut through air above the
    // already-engaged groove). This is acceptance criterion (a):
    // reducing first-cut depth on too-short chains, no warning needed
    // since the depth-limit cascades into the existing depth-limited
    // diagnostics path further up the stack.
    let (ramp_end_z, ramp_target_len) = if lead_in_len <= total_arc {
        (first_cut_z, lead_in_len)
    } else {
        (-tan_angle * total_arc, total_arc)
    };

    // kagr: output is now a list of sub-polylines so the caller can
    // rapid (G0) between them over uncut stock rather than dragging
    // the cutter at feed across the workpiece surface. We accumulate
    // into `out` and use `push_path` to flush whenever a segment of
    // the sweep would otherwise have walked at z ≥ 0.
    let mut out: Vec<ZPolyline> = Vec::new();
    let push_path = |out: &mut Vec<ZPolyline>, path: &mut Vec<(f64, f64, f64)>| {
        if path.len() >= 2 {
            out.push(std::mem::take(path));
        } else {
            path.clear();
        }
    };

    // Emit the ramped first sweep. For each compact[i]:
    //   * before ramp end: z = -tan(angle) * arc[i] (clamped to
    //     ramp_end_z and to target_z[i]),
    //   * once we cross ramp_target_len mid-segment, insert a synthetic
    //     waypoint at the exact ramp-end XY at ramp_end_z so the slope
    //     is preserved,
    //   * after ramp end: z = max(ramp_end_z, target_z[i]) — the same
    //     thing the standard forward sweep at current_level=-dpp would
    //     have emitted.
    //
    // kagr: when target_z[i] = 0 (a R≈0 boundary point — the chain
    // has nothing to cut here), the natural z_i collapses to 0 too.
    // We MUST NOT emit a position move at z=0 across uncut stock —
    // the non-flat V-bit tip would scrape the surface at feed rate.
    // We break the sub-polyline at those points so the caller rapid
    // (G0) lifts over the uncut stretch.
    let surface_eps = -1e-9;
    cut_z[0] = 0.0;
    let mut ramp_finished = false;
    // Defer pushing compact[0] — it's at z=0 by definition; only
    // start the path once the first segment dives below the surface.
    for i in 1..n {
        if !ramp_finished && arc[i] > ramp_target_len + 1e-9 {
            // Cross the ramp boundary mid-segment — emit the exact
            // ramp-end waypoint first (it's by construction at
            // ramp_end_z < 0, so it's safe).
            let prev_arc = arc[i - 1];
            let seg_len = arc[i] - prev_arc;
            if seg_len > 1e-9 && prev_arc < ramp_target_len - 1e-9 {
                let t = (ramp_target_len - prev_arc) / seg_len;
                let rx = compact[i - 1].0 + t * (compact[i].0 - compact[i - 1].0);
                let ry = compact[i - 1].1 + t * (compact[i].1 - compact[i - 1].1);
                if path.is_empty() {
                    // First descent into stock — push the segment
                    // start at z=0 then the ramp-end point. The
                    // entry is a single dive segment, not a long
                    // surface walk.
                    path.push((compact[i - 1].0, compact[i - 1].1, 0.0));
                }
                path.push((rx, ry, ramp_end_z));
            }
            ramp_finished = true;
        }
        let z_i = if ramp_finished {
            ramp_end_z.max(target_z[i])
        } else {
            (-tan_angle * arc[i]).max(target_z[i])
        };
        cut_z[i] = z_i;
        if z_i < surface_eps {
            // Cutting: emit. If the path is empty, prepend an entry
            // waypoint at compact[i-1] at z=0 so the gcode emitter
            // has a starting XY (the actual descent happens between
            // this and the next waypoint at feed; the caller's lead-
            // in plunge handles the Z drop to start_depth before).
            if path.is_empty() {
                path.push((compact[i - 1].0, compact[i - 1].1, 0.0));
            }
            path.push((compact[i].0, compact[i].1, z_i));
        } else {
            // Above surface (target says don't cut here) — break the
            // sub-polyline so the caller G0-rapids to the next cut.
            push_path(&mut out, &mut path);
        }
    }
    push_path(&mut out, &mut path);

    // The lead-in ramp already played the role of the first forward
    // sweep at current_level = -dpp.
    //
    // If the chain was too short to fit the ramp at the configured
    // angle, the lead-in bottomed out shallower than -dpp. Going
    // deeper on the next sweep would re-introduce the steep plunge
    // (the bit would drop from z=0 at compact[0] to -2*dpp at
    // compact[1] with no kerf clearance — same as the original bug).
    // So we stop here and accept a shallow cut on this chain. The
    // pipeline-level depth-limited warning surfaces the under-cut to
    // the user (see acceptance criteria option (a)).
    if (ramp_end_z - first_cut_z).abs() > 1e-9 {
        return out;
    }
    let mut current_level = -2.0 * dpp;
    // kagr: subsequent forward/reverse sweeps emit a position move
    // ONLY when cut_z[i] < 0 (real cut has already happened at this
    // point). Above the surface, we flush the in-progress sub-polyline
    // and the caller will rapid (G0) to the next cut site.
    let mut path: Vec<(f64, f64, f64)> = Vec::new();
    // Emit the reverse-sweep backstroke so the next forward sweep
    // starts from compact[0] (mirroring the standard ratchet ordering).
    for i in (0..n).rev() {
        if cut_z[i] < surface_eps {
            path.push((compact[i].0, compact[i].1, cut_z[i]));
        } else {
            // Above surface — break the polyline so the caller G0-
            // lifts here instead of scraping at feed.
            push_path(&mut out, &mut path);
        }
    }
    push_path(&mut out, &mut path);
    // j1zs: iteration cap is a hard absolute bound on the ratchet loop.
    // Pre-fix the loop relied on `progressed` AND a DPP-relative break
    // (`current_level < z_min - dpp`) — the DPP-relative form could
    // race against pathological floating-point edge cases on
    // densified polylines (cut_z[i] within 1e-9 of target_z[i] for
    // long stretches), looping until `progressed` finally went false.
    // The absolute cap derived from `n_levels` (computed up top) is
    // the EXACT number of forward+reverse pairs needed; we add a
    // small slack so a recomputation-noise epsilon doesn't bail one
    // pass short of target depth.
    let max_passes = n_levels + 2;
    let mut pass = 0usize;
    loop {
        pass += 1;
        if pass > max_passes {
            // Safety bail. With a correct `dpp` and `levels` list
            // we should never hit this — but if we ever do, dumping
            // out and letting the caller emit what's there is much
            // better than spinning.
            break;
        }
        let mut progressed = false;
        let mut path: Vec<(f64, f64, f64)> = Vec::new();
        // Forward sweep at current_level: cut every point to
        // max(target_z[i], current_level), but only when that's deeper
        // than cut_z[i].
        for i in 0..n {
            let mut next_z = current_level.max(target_z[i]);
            if next_z > cut_z[i] {
                next_z = cut_z[i];
            }
            if next_z < cut_z[i] - 1e-9 {
                cut_z[i] = next_z;
                path.push((compact[i].0, compact[i].1, next_z));
                progressed = true;
            } else if i > 0 && cut_z[i] < surface_eps {
                // No new material at this point on this level but a
                // prior pass DID cut here — emit a travel move at the
                // current cut depth so the polyline stays continuous
                // INSIDE the kerf.
                path.push((compact[i].0, compact[i].1, cut_z[i]));
            } else {
                // Above surface — break the polyline so the caller
                // G0-lifts over uncut stock at fast_z instead of
                // dragging the V-bit tip across the workpiece (kagr).
                push_path(&mut out, &mut path);
            }
        }
        push_path(&mut out, &mut path);
        if !progressed {
            break;
        }
        // Reverse sweep back over the segment we just cut, at the same
        // (already-reached) depth, so the bit ends up at the start
        // ready for the next deeper level. This is the "ratchet"
        // backstroke. We don't lower Z further on this reverse pass —
        // it's a position move, not a cut. Same surface-skip rule.
        let mut path: Vec<(f64, f64, f64)> = Vec::new();
        for i in (0..n).rev() {
            if cut_z[i] < surface_eps {
                path.push((compact[i].0, compact[i].1, cut_z[i]));
            } else {
                push_path(&mut out, &mut path);
            }
        }
        push_path(&mut out, &mut path);
        current_level -= dpp;
        // j1zs: stop the moment the next pass would cut at-or-below
        // the deepest target. The previous DPP-relative form
        // (`< z_min - dpp`) was a 1-DPP slack window that combined
        // with the `progressed` flag to break — we now use the
        // tighter absolute condition AND the iteration cap above.
        if current_level + dpp < target_z.iter().fold(0.0_f64, |a, &b| a.min(b)) - 1e-9 {
            break;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_axis_returns_empty() {
        assert!(ratchet_emit(&[], 1.0).is_empty());
        assert!(ratchet_emit(&[(0.0, 0.0, -1.0, 0.5)], 1.0).is_empty());
    }

    #[test]
    fn single_pass_at_or_above_dpp() {
        // Polyline whose deepest point is shallower than DPP — should
        // cut to the target on the first level and stop.
        let axis = vec![(0.0, 0.0, -0.5, 0.25), (5.0, 0.0, -0.5, 0.25)];
        let polylines = ratchet_emit(&axis, 1.0);
        assert!(!polylines.is_empty());
        let z_min = polylines
            .iter()
            .flatten()
            .map(|t| t.2)
            .fold(0.0_f64, f64::min);
        assert!((z_min + 0.5).abs() < 1e-6, "z_min = {z_min}");
    }

    /// pmpk: a straight medial-axis chain whose endpoint sits at R≈0
    /// (target_z≈0) must not produce any segment with vertical drop
    /// > 0.05 mm at zero (or near-zero) horizontal travel. Before the
    /// fix, the first cut move dropped Z by `dpp` while XY barely
    /// moved — V-bit snap territory. The lead-in ramp now spreads the
    /// drop over `dpp / tan(LEAD_IN_ANGLE_DEG)` mm of XY travel.
    #[test]
    fn first_plunge_uses_angled_lead_in() {
        // 50 mm-long chain, target depth -3 mm at both ends → in
        // practice the endpoint depth is 0 (boundary-touching, R≈0).
        // We synthesize the worst case: start at z=0, deepen to -3 at
        // 5 mm in, stay at -3 for the rest. After my fix, the first
        // 0..lead_in_len mm of XY should ramp from 0 down to -dpp.
        let mut axis = vec![(0.0, 0.0, 0.0_f64, 0.0)];
        for i in 1..=50 {
            let x = i as f64;
            let z = if x < 5.0 { -3.0 * (x / 5.0) } else { -3.0 };
            axis.push((x, 0.0, z, 1.5));
        }
        let dpp = 1.0;
        let polylines = ratchet_emit(&axis, dpp);
        assert!(!polylines.is_empty(), "path should be non-empty");
        // Steepness check: no plunging segment may have horizontal
        // projection < (vertical_drop / tan(45°)) — i.e. no near-
        // vertical descent into solid stock. We focus on the FIRST
        // plunge (z going from 0 to a deeper value); subsequent
        // sweeps cut inside an existing kerf and so steeper slopes
        // are permitted there.
        let threshold = 0.05_f64; // mm vertical
        let mut cut_z = 0.0_f64; // tracks deepest Z reached so far
        for poly in &polylines {
            for w in poly.windows(2) {
                let (ax, ay, az) = w[0];
                let (bx, by, bz) = w[1];
                // Only check segments that go DEEPER than anything yet —
                // i.e. the very first plunge into uncarved stock.
                if bz < cut_z - 1e-9 {
                    let h = (bx - ax).hypot(by - ay);
                    let v = az - bz; // positive on plunge
                    if v > threshold {
                        assert!(
                            h > v * 0.5,
                            "first-plunge segment too steep: h={h:.4} mm, v={v:.4} mm \
                             (from ({ax:.3},{ay:.3},{az:.3}) to ({bx:.3},{by:.3},{bz:.3}))",
                        );
                    }
                    cut_z = cut_z.min(bz);
                }
            }
        }
    }

    #[test]
    fn deep_polyline_progresses_in_levels() {
        // Polyline reaching -3 mm with DPP 1 — expect at least 3
        // distinct Z-levels visited.
        let axis = vec![
            (0.0, 0.0, -3.0, 1.5),
            (5.0, 0.0, -3.0, 1.5),
            (10.0, 0.0, -3.0, 1.5),
        ];
        let polylines = ratchet_emit(&axis, 1.0);
        let mut levels: Vec<f64> = polylines.iter().flatten().map(|t| t.2).collect();
        levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
        levels.dedup_by(|a, b| (*a - *b).abs() < 0.05);
        assert!(
            levels.len() >= 3,
            "expected ≥3 distinct Z levels, got {levels:?}"
        );
    }

    /// kagr: when the medial-axis chain has uncut sections (target_z
    /// stays at 0 across long stretches because the slot is shallower
    /// than DPP at those points), the ratchet must NOT emit position
    /// moves at z=0 that walk across the workpiece surface at feed
    /// rate. Instead it splits the toolpath into sub-polylines so the
    /// caller can rapid (G0) between them at safe Z.
    ///
    /// Synthesize a chain that's deep at the middle and shallow
    /// (target_z=0) at the ends — with DPP > end-depth, only the
    /// middle is cut. The reverse / second-pass sweeps must skip the
    /// uncut ends. Each sub-polyline is allowed to BEGIN with a
    /// single z=0 waypoint (the lead-in ramp entry / re-entry XY);
    /// every other waypoint must sit below the work surface.
    /// j1zs: even a deep V-carve must terminate cleanly (the iteration
    /// cap is `n_levels + 2`, derived up-front from `z_min / dpp`). A
    /// chain reaching -10 mm at dpp 0.5 mm has 20 levels; the cap
    /// guarantees the loop never spins for longer than that even if
    /// floating-point noise on cut_z would otherwise keep `progressed`
    /// false for one extra pass.
    #[test]
    fn deep_polyline_terminates_under_iteration_cap() {
        let mut axis: Vec<(f64, f64, f64, f64)> = Vec::new();
        for i in 0..=50 {
            axis.push((f64::from(i), 0.0, -10.0, 5.0));
        }
        let polylines = ratchet_emit(&axis, 0.5);
        // The output should reach the target depth. We trust the
        // emitter to terminate — running this test under `cargo test`
        // would hang on regressions. The assertion proves we actually
        // got to -10 (the loop didn't bail early either).
        let z_min = polylines
            .iter()
            .flatten()
            .map(|t| t.2)
            .fold(0.0_f64, f64::min);
        assert!(
            z_min < -9.99,
            "expected emit to reach -10 mm; got z_min = {z_min}",
        );
    }

    #[test]
    fn no_position_moves_above_surface() {
        // 20mm-long medial axis. Middle 6mm is target_z=-1, ends are 0.
        let mut axis: Vec<(f64, f64, f64, f64)> = Vec::new();
        for i in 0..=20 {
            let x = i as f64;
            // Shallow trough centered on x=10: target_z=-1 for 7<=x<=13, else 0.
            let z = if (7.0..=13.0).contains(&x) { -1.0 } else { 0.0 };
            axis.push((x, 0.0, z, 0.5));
        }
        // DPP = 2.0 so we cut the whole trough in the first pass; the
        // bug surface area is the SUBSEQUENT reverse sweep which used
        // to emit z=cut_z[i]=0 at the uncut ends.
        let polylines = ratchet_emit(&axis, 2.0);
        assert!(!polylines.is_empty(), "expected at least one sub-polyline");
        // Each sub-polyline may BEGIN with a single z=0 waypoint
        // (re-entry XY after the caller's G0-lift / plunge). Any
        // additional z=0 waypoint is a bug — it'd drag the cutter
        // across uncut stock at feed.
        for (poly_idx, poly) in polylines.iter().enumerate() {
            let surface_count = poly.iter().filter(|t| t.2 >= -1e-9).count();
            assert!(
                surface_count <= 1,
                "polyline #{poly_idx} has {surface_count} surface waypoints (>1 = kagr bug); \
                 poly = {poly:?}",
            );
        }
    }

    /// ot80: the configurable lead-in angle changes the ramp slope.
    /// A steeper angle ⇒ shorter horizontal travel for the same
    /// descent; the wrapper `ratchet_emit_with_lead_in` flows the
    /// user-configured angle through. Out-of-range values silently
    /// fall back to the legacy 10° default.
    #[test]
    fn ot80_lead_in_angle_overrides_default_slope() {
        // 50 mm linear chain with target depth -3 mm at the entry
        // (worst case for ramp behavior). DPP 1 mm so first cut sits
        // at -1.
        let mut axis: Vec<(f64, f64, f64, f64)> = vec![(0.0, 0.0, 0.0, 0.0)];
        for i in 1..=50 {
            let x = i as f64;
            axis.push((x, 0.0, -3.0, 1.5));
        }
        let dpp = 1.0_f64;
        let polylines_default = ratchet_emit(&axis, dpp);
        let polylines_steep = ratchet_emit_with_lead_in(&axis, dpp, 45.0);
        // Steeper ramps travel less XY before reaching -dpp. Find the
        // first segment whose Z is at or below -0.9*dpp, then compare
        // the cumulative XY length to get there.
        let xy_to_first_dpp = |polylines: &[ZPolyline]| -> f64 {
            let mut total = 0.0_f64;
            for poly in polylines {
                for w in poly.windows(2) {
                    let (ax, ay, _) = w[0];
                    let (bx, by, bz) = w[1];
                    total += (bx - ax).hypot(by - ay);
                    if bz <= -0.9 * dpp {
                        return total;
                    }
                }
            }
            f64::INFINITY
        };
        let xy_default = xy_to_first_dpp(&polylines_default);
        let xy_steep = xy_to_first_dpp(&polylines_steep);
        assert!(
            xy_default.is_finite() && xy_steep.is_finite(),
            "both ramps should reach -dpp; default={xy_default} steep={xy_steep}",
        );
        // 45° ramp travels ~1 mm of XY per 1 mm of depth; the 10° default
        // travels ~5.7 mm of XY per 1 mm of depth. The steep variant
        // MUST get to -dpp in less XY travel.
        assert!(
            xy_steep < xy_default,
            "ot80: steeper lead-in must hit -dpp in less XY travel; default={xy_default:.3} steep={xy_steep:.3}",
        );
    }

    /// ot80: out-of-range / non-finite angle overrides silently revert
    /// to the legacy 10° default — defensive against bad project
    /// data.
    #[test]
    fn ot80_invalid_lead_in_angle_falls_back_to_default() {
        let mut axis: Vec<(f64, f64, f64, f64)> = vec![(0.0, 0.0, 0.0, 0.0)];
        for i in 1..=50 {
            axis.push((i as f64, 0.0, -3.0, 1.5));
        }
        let dpp = 1.0_f64;
        let pl_legacy = ratchet_emit(&axis, dpp);
        // 0°, negative, > 90°, NaN ⇒ all fall back to LEAD_IN_ANGLE_DEG.
        for bogus in &[0.0_f64, -10.0, 95.0, f64::NAN] {
            let pl = ratchet_emit_with_lead_in(&axis, dpp, *bogus);
            assert_eq!(
                pl.len(),
                pl_legacy.len(),
                "bogus angle {bogus} should fall back to default and produce the same polylines"
            );
        }
    }
}
