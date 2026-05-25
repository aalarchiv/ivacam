//! Per-op pipeline warning helpers. Each runs against a single
//! operation + its inputs and pushes [`PipelineWarning`] entries onto
//! the caller's vector. Sanity warnings (`push_tool_fit_kind_warnings`,
//! `push_trochoidal_warnings`, `push_ramp_with_arcs_warning`) fire
//! before the offset cascade runs; size-fit warnings
//! (`push_tool_fit_size_warning`) fire after, because they need the
//! emitted offset list.

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::{Setup, ToolOffset};
use crate::cam::VcObject;
use crate::project::{Op, OpKind, OpSource, PocketStrategy, Project};

use super::{op_includes_object, PipelineWarning};

/// i5g4 (MVP): surface a warning when the imported geometry's
/// bounding box does NOT contain the gcode origin (0,0). The full
/// WCS / G54..G59 / per-fixture-origin fix is a feature (see the
/// follow-up issue) — but the silent-misalignment case the audit
/// caught is the user who drew a part centered around (0,0) in
/// their DXF and then zeroed the machine to a stock CORNER. The
/// sim heightmap shows cuts where the gcode origin lands; if that
/// origin is far from where the user actually zeroed the spindle,
/// the sim looks normal but the real machine cuts in the wrong
/// place. The fix is small and loud: warn whenever the geometry
/// bbox doesn't include (0,0).
///
/// We accept a 0.001 mm slack so paths drawn EXACTLY to the origin
/// edge don't warn (very common — "draw a square from 0,0 to 100,100").
pub(super) fn push_wcs_origin_warning(
    project: &Project,
    warnings: &mut Vec<PipelineWarning>,
) {
    if project.segments.is_empty() {
        return;
    }
    let bbox = crate::geometry::BBox::from_segments(&project.segments);
    if !bbox.is_finite() {
        return;
    }
    // The "gcode origin" in geometry coordinates is the WCS origin
    // expressed in the geometry frame: project.work_offset gives the
    // offset from geometry origin to WCS origin, so the WCS-zero
    // lives at (work_offset.x_mm, work_offset.y_mm) in geometry-space.
    // We check whether that point falls within (or essentially on)
    // the geometry footprint — if not, the sim heightmap and the
    // emitted cuts will diverge.
    let slack = 1e-3_f64;
    let gx = project.work_offset.x_mm;
    let gy = project.work_offset.y_mm;
    let contains_wcs = bbox.min_x - slack <= gx
        && gx <= bbox.max_x + slack
        && bbox.min_y - slack <= gy
        && gy <= bbox.max_y + slack;
    if !contains_wcs {
        warnings.push(PipelineWarning {
            op_id: None,
            kind: "stock_origin_outside_geometry_bbox".into(),
            message: format!(
                "Geometry bbox ({:.2}, {:.2}) → ({:.2}, {:.2}) does NOT contain the WCS origin ({:.2}, {:.2}) in geometry coordinates. The simulator aligns its heightmap to the geometry footprint while the controller cuts at the WCS / G54 origin — if you zeroed the machine somewhere else (e.g. a stock corner) the cuts will land in the wrong place. Translate the geometry, or set Project.work_offset so the WCS origin matches the spot you zeroed against.",
                bbox.min_x, bbox.min_y, bbox.max_x, bbox.max_y, gx, gy
            ),
        });
    }
}

/// v0ez: post-emit work-area envelope scan. Until now the ONLY check
/// that emitted cuts stay inside the machine travel box lived in the
/// frontend (`GenerateBar.boundsScan`), so any non-frontend consumer
/// (CLI / server / wasm called directly) got no soft-limit guard at all.
/// This moves the work-area half of that scan into the pipeline so every
/// transport is protected. The stock half stays frontend-side: the core
/// `Project` has no stock model yet (stock dims live only in the
/// frontend's `StockConfig`).
///
/// Mirrors the frontend logic exactly so behavior is unchanged for FE
/// users (the frontend drops its own work-area synthesis now that this
/// emits the same `out_of_work_area` kind): scan Cut / Plunge / Arc
/// segment END points against X ∈ [0, wa.x], Y ∈ [0, wa.y],
/// Z ∈ [-wa.z, 0] (origin at stock top) with a 1e-6 mm slack. Rapids and
/// retracts are excluded — they legitimately fly to clearance / park
/// positions outside the cut envelope. Emits a single `out_of_work_area`
/// warning (the frontend classifies that kind as critical, so the
/// block-on-critical gate refuses to ship the program). Skipped when the
/// work area is unset / zero on any axis.
pub(super) fn push_work_area_warning(
    toolpath: &[crate::gcode::preview::ToolpathSegment],
    machine: &crate::cam::setup::MachineConfig,
    warnings: &mut Vec<PipelineWarning>,
) {
    use crate::gcode::preview::MoveKind;
    let wa = machine.work_area;
    if !(wa.x > 0.0 && wa.y > 0.0 && wa.z > 0.0) {
        return;
    }
    let eps = 1e-6;
    let mut count = 0usize;
    let mut first_line = 0u32;
    for seg in toolpath {
        if !matches!(seg.kind, MoveKind::Cut | MoveKind::Plunge | MoveKind::Arc) {
            continue;
        }
        let p = seg.to;
        let outside = p.x < -eps
            || p.x > wa.x + eps
            || p.y < -eps
            || p.y > wa.y + eps
            || p.z < -wa.z - eps
            || p.z > eps;
        if outside {
            count += 1;
            if first_line == 0 {
                first_line = seg.gcode_line;
            }
        }
    }
    if count == 0 {
        return;
    }
    let plural = if count == 1 { "" } else { "s" };
    let where_line = if first_line != 0 {
        format!(" (first at gcode line {first_line})")
    } else {
        String::new()
    };
    warnings.push(PipelineWarning {
        op_id: None,
        kind: "out_of_work_area".into(),
        message: format!(
            "{count} cut move{plural} outside the machine work area{where_line}. The controller may refuse the move (soft-limit fault) or, worse, crash into the gantry. Set Project.work_offset so the cuts land inside the work envelope."
        ),
    });
}

/// tnxu: scan the enabled-op sequence for obviously wrong orderings —
/// the classic "Profile cuts the part free → Drill on the loose part
/// fails" sequence. We don't auto-reorder (the user may have a real
/// reason for the order, e.g. a jig + manual reset), but we surface
/// a per-offender `op_order_suspect` warning so the
/// `block_on_critical` gate (94sf) can refuse to ship gcode that's
/// almost certainly going to misbehave. Two patterns:
///
/// * `Drill` appearing AFTER a `Profile` (Outside / Inside or
///   through-cut) on the same source — the part is loose by the
///   time the drill runs, so the drill positions never register.
/// * Finish-before-rough — two contour-style ops on the same
///   source where the FIRST uses a SMALLER tool than the second.
///   The finish pass belongs after the rough that opens up
///   clearance; smaller-tool-first is almost never intentional.
///
/// Same-tool-back-to-back Profile or Pocket ops are NOT flagged —
/// that's a common pattern for layered passes and the user
/// frequently does it on purpose.
pub(super) fn push_op_order_warnings(
    project: &Project,
    warnings: &mut Vec<PipelineWarning>,
) {
    let enabled: Vec<&Op> = project.operations.iter().filter(|o| o.enabled).collect();
    if enabled.len() < 2 {
        return;
    }
    // Profile that cuts the part free: either an explicit through_depth > 0
    // OR an outside / inside profile with depth deep enough that we'd
    // expect it to part the stock. We don't have stock thickness here so
    // the through_depth signal is the canonical one; outside-profile is
    // also strong evidence (typically the user is cutting the outline).
    let cuts_part_free = |op: &Op| -> bool {
        match &op.kind {
            OpKind::Profile { offset, .. } => {
                op.params.through_depth > 1e-9
                    || matches!(offset, ToolOffset::Outside | ToolOffset::Inside)
            }
            _ => false,
        }
    };
    for (i, op_a) in enabled.iter().enumerate() {
        if !cuts_part_free(op_a) {
            continue;
        }
        for op_b in &enabled[i + 1..] {
            if !ops_share_source(op_a, op_b) {
                continue;
            }
            // Only Drill is firmly broken here. Other op kinds can
            // legitimately follow a profile (chamfer the edge AFTER
            // the profile is cut; engrave a code on remaining stock).
            // Drill positions depend on a held part — the user
            // almost never wants this order.
            if !matches!(op_b.kind, OpKind::Drill { .. }) {
                continue;
            }
            warnings.push(PipelineWarning {
                op_id: Some(op_b.id),
                kind: "op_order_suspect".into(),
                message: format!(
                    "Operation '{}' (drill_after_profile) runs AFTER profile op '{}' which cuts the part free. Drilling acts on a loose / flown piece. Reorder so the drill precedes the part-freeing profile.",
                    op_b.name, op_a.name
                ),
            });
        }
    }
    // Finish-before-rough: two ops on the same source where the first
    // uses a smaller tool than the second. "Same source" + smaller-tool-first
    // is rarely intentional — finish passes belong AFTER the rough that
    // opens up clearance.
    for (i, op_a) in enabled.iter().enumerate() {
        for op_b in &enabled[i + 1..] {
            if !ops_share_source(op_a, op_b) {
                continue;
            }
            let (Some(tool_a), Some(tool_b)) = (
                project.tools.iter().find(|t| t.id == op_a.tool_id),
                project.tools.iter().find(|t| t.id == op_b.tool_id),
            ) else {
                continue;
            };
            // Smaller tool first, on contour-style ops only — leaving
            // a drill or chamfer specifically out (their tool sizes
            // don't follow the rough/finish convention).
            let contour_kind = |op: &Op| {
                matches!(
                    op.kind,
                    OpKind::Profile { .. } | OpKind::Pocket { .. } | OpKind::Engrave { .. }
                )
            };
            if !contour_kind(op_a) || !contour_kind(op_b) {
                continue;
            }
            if tool_a.diameter + 1e-9 < tool_b.diameter {
                warnings.push(PipelineWarning {
                    op_id: Some(op_a.id),
                    kind: "op_order_suspect".into(),
                    message: format!(
                        "Operation '{}' (tool dia {:.2}) runs BEFORE '{}' (tool dia {:.2}) on the same source — likely a finish-before-rough order. Move the larger tool first so the finish pass has clearance.",
                        op_a.name, tool_a.diameter, op_b.name, tool_b.diameter
                    ),
                });
            }
        }
    }
}

/// Two ops "share source" when one's `OpSource` overlaps the other's:
/// either both `All`, intersecting `Layers` lists, or intersecting
/// `Objects` id sets. `Objects` vs `Layers` cross-comparison is
/// conservatively true (we don't know the chain→layer mapping without
/// re-running selection) — the warning is an order check, not an
/// emit-time error, so a few extra warnings are cheaper than a missed
/// real one.
fn ops_share_source(a: &Op, b: &Op) -> bool {
    match (&a.source, &b.source) {
        (OpSource::All, _) | (_, OpSource::All) => true,
        (OpSource::Layers { layers: la, .. }, OpSource::Layers { layers: lb, .. }) => {
            la.iter().any(|x| lb.iter().any(|y| x == y))
        }
        (OpSource::Objects { ids: ia, .. }, OpSource::Objects { ids: ib, .. }) => {
            ia.iter().any(|x| ib.contains(x))
        }
        // Mixed Layers/Objects: conservative true (see fn comment).
        _ => true,
    }
}

/// Surface the v1 limitation that the ramp-pass emitter treats
/// boundary-crossing arcs as regular segments (instant Z descent at
/// the arc's start), not as ramped sections. Users with ramp plunge
/// on an arc-heavy source need to know the cutter dives at the arc
/// instead of sloping through it (audit 8so).
pub(super) fn push_ramp_with_arcs_warning(
    op: &Op,
    objects: &[VcObject],
    warnings: &mut Vec<PipelineWarning>,
) {
    use crate::geometry::SegmentKind;
    if !matches!(
        op.params.plunge,
        crate::cam::setup::PlungeStrategy::Ramp { .. }
    ) {
        return;
    }
    let has_arc = objects.iter().enumerate().any(|(idx, obj)| {
        op_includes_object(op, obj, idx)
            && obj
                .segments
                .iter()
                .any(|s| matches!(s.kind, SegmentKind::Arc | SegmentKind::Circle))
    });
    if has_arc {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "ramp_arcs_at_boundary".into(),
            message: format!(
                "op '{}': ramp plunge with arc / circle source segments. The cutter ramps along line segments correctly but dives straight down at the start of any arc that crosses the ramp boundary — surface finish near arc entries may show a small step. Use Helix plunge or a finer ramp angle for a smoother entry.",
                op.name
            ),
        });
    }
}

pub(super) fn push_trochoidal_warnings(op: &Op, warnings: &mut Vec<PipelineWarning>) {
    if !matches!(
        op.kind,
        OpKind::Pocket {
            strategy: PocketStrategy::Trochoidal { .. },
            ..
        }
    ) {
        return;
    }
    if op.contour_params().is_some_and(|c| c.tabs.active) {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tabs_with_trochoidal_unsupported".into(),
            message: format!(
                "op '{}': tabs are not supported on a Trochoidal pocket; ignoring tabs.",
                op.name
            ),
        });
    }
    if !matches!(
        op.params.plunge,
        crate::cam::setup::PlungeStrategy::Helix { .. }
    ) {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "plunge_overridden".into(),
            message: format!(
                "op '{}': trochoidal pockets require helical descent; overriding plunge to Helix.",
                op.name
            ),
        });
    }
}

/// Sanity warnings that don't depend on whether the offset cascade
/// succeeded. Run before the heavy work.
pub(super) fn push_tool_fit_kind_warnings(
    op: &Op,
    project: &Project,
    setup: &Setup,
    warnings: &mut Vec<PipelineWarning>,
) {
    use crate::project::ToolKind;
    let Some(tool) = project.tools.iter().find(|t| t.id == op.tool_id) else {
        return;
    };
    // Impossible tool geometry: tip diameter ≥ shank diameter.
    if let Some(tip) = tool.tip_diameter {
        if tip >= tool.diameter {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "tool_geometry_impossible".into(),
                message: format!(
                    "tool '{}': tip diameter {tip} ≥ shank diameter {}",
                    tool.name, tool.diameter
                ),
            });
        }
    }
    // Tool kind mismatched with op kind. We warn rather than error
    // because the gcode emitter still produces something usable in many
    // cases (a drag knife on a Profile is fine, for instance), but a
    // drill on a Pocket really doesn't make sense.
    let mismatch = match (&op.kind, tool.kind) {
        (OpKind::Pocket { .. }, ToolKind::Drill) => Some("pocket op assigned a drill bit"),
        (OpKind::Pocket { .. }, ToolKind::DragKnife) => {
            Some("pocket op assigned a drag knife (cut path won't carve area)")
        }
        (OpKind::Profile { .. }, ToolKind::Drill) => Some("profile op assigned a drill bit"),
        // lo4j: Thread ops require a rotating side-cutting tool — drag
        // knives don't cut, laser beams can't form a helix, drills only
        // plunge axially.
        (OpKind::Thread { .. }, ToolKind::DragKnife) => {
            Some("thread op assigned a drag knife (can't cut a helix)")
        }
        (OpKind::Thread { .. }, ToolKind::LaserBeam) => {
            Some("thread op assigned a laser beam (no XY-helix cutting)")
        }
        (OpKind::Thread { .. }, ToolKind::Drill) => {
            Some("thread op assigned a drill bit (drill cuts axially, not helically)")
        }
        _ => None,
    };
    if let Some(msg) = mismatch {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tool_kind_mismatch".into(),
            message: format!(
                "{msg} — '{}' on op '{}'. Pick a different tool kind.",
                tool.name, op.name
            ),
        });
    }
    let _ = setup; // reserved for future feed/speed sanity checks
}

/// Post-build warning: a closed boundary was supplied but the offset
/// cascade produced nothing — the tool diameter doesn't fit the
/// geometry (slot too narrow, pocket smaller than the tool, etc.).
pub(super) fn push_tool_fit_size_warning(
    op: &Op,
    setup: &Setup,
    closed_count: usize,
    offsets: &[PolylineOffset],
    warnings: &mut Vec<PipelineWarning>,
) {
    if closed_count == 0 {
        return; // nothing closed → not a tool-fit problem, just no work
    }
    // Profile-on / Engrave / DragKnife emit straight contour walks even
    // when offsets is empty in the cascade sense, so don't flag them.
    let needs_offset = matches!(
        op.kind,
        OpKind::Pocket { .. }
            | OpKind::Profile {
                offset: crate::cam::setup::ToolOffset::Outside
                    | crate::cam::setup::ToolOffset::Inside,
                ..
            }
    );
    if !needs_offset {
        return;
    }
    if offsets.is_empty() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tool_too_large".into(),
            message: format!(
                "tool diameter {:.2} mm doesn't fit op '{}' — offset/cascade produced no toolpath. Try a smaller tool.",
                setup.tool.diameter, op.name,
            ),
        });
        return;
    }
    // Pocket-specific second pass: the boundary contour fits but the
    // cascade carved no inward rings → the cutter is wide enough to
    // reach the wall but not to chew out the interior. The user gets
    // a hollow pocket (just the wall trace), which can look like
    // "pocketing isn't working". Surface this so they can pick a
    // smaller tool. PolylineOffset.is_pocket == 0 is the boundary,
    // is_pocket >= 1 is a cascade ring or zigzag fill.
    if matches!(op.kind, OpKind::Pocket { .. })
        && offsets.iter().any(|o| o.is_pocket == 0)
        && !offsets.iter().any(|o| o.is_pocket >= 1)
    {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "pocket_fill_incomplete".into(),
            message: format!(
                "tool diameter {:.2} mm fits the pocket boundary in op '{}' but not the interior — only the wall is cut, not the fill. Use a smaller tool to pocket the inside.",
                setup.tool.diameter, op.name,
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::setup::ToolOffset;
    use crate::pipeline::test_helpers::{
        closed_square, closed_square_offset, drill_op, endmill, pocket_op, profile_op,
        project_with, project_with_segments,
    };
    use crate::project::{DrillCycle, OpSource};

    /// tnxu: a Profile op cutting the outline followed by a Drill op
    /// on the same source emits an `op_order_suspect` warning tagged
    /// `drill_after_profile`. The downstream Drill would be acting
    /// on a freed part — almost never intentional.
    #[test]
    fn op_order_drill_after_profile_emits_warning() {
        let tool = endmill(1, 3.0);
        // Profile op cuts the outside contour (parts the stock).
        let mut profile = profile_op(1, 1, ToolOffset::Outside);
        profile.params.step = Some(-1.0);
        profile.params.depth = -2.0;
        // Drill op on the same All source — would land on the loose part.
        let drill = drill_op(2, 1, DrillCycle::Simple { dwell_sec: 0.0 });
        let project = project_with(vec![profile, drill], vec![tool]);
        let mut warnings = Vec::new();
        push_op_order_warnings(&project, &mut warnings);
        let hit = warnings
            .iter()
            .find(|w| w.kind == "op_order_suspect")
            .expect("expected op_order_suspect");
        assert_eq!(hit.op_id, Some(2));
        assert!(
            hit.message.contains("drill_after_profile"),
            "expected drill_after_profile tag, got {}",
            hit.message
        );
    }

    /// Reverse order — Drill then Profile — is the SAFE order, so no
    /// warning. Same source, same tools, just swapped.
    #[test]
    fn op_order_drill_before_profile_no_warning() {
        let tool = endmill(1, 3.0);
        let drill = drill_op(1, 1, DrillCycle::Simple { dwell_sec: 0.0 });
        let mut profile = profile_op(2, 1, ToolOffset::Outside);
        profile.params.step = Some(-1.0);
        profile.params.depth = -2.0;
        let project = project_with(vec![drill, profile], vec![tool]);
        let mut warnings = Vec::new();
        push_op_order_warnings(&project, &mut warnings);
        assert!(
            warnings.iter().all(|w| w.kind != "op_order_suspect"),
            "no op_order_suspect expected in safe order, got {warnings:?}"
        );
    }

    /// Pocket-then-Pocket where the second op uses a LARGER tool than
    /// the first triggers the finish-before-rough heuristic. Same source.
    #[test]
    fn op_order_finish_before_rough_emits_warning() {
        let tool_small = endmill(1, 1.0);
        let tool_big = endmill(2, 6.0);
        let pocket_small = pocket_op(1, 1, OpSource::All);
        let pocket_big = pocket_op(2, 2, OpSource::All);
        let project = project_with(vec![pocket_small, pocket_big], vec![tool_small, tool_big]);
        let mut warnings = Vec::new();
        push_op_order_warnings(&project, &mut warnings);
        let hit = warnings
            .iter()
            .find(|w| w.kind == "op_order_suspect" && w.op_id == Some(1))
            .expect("expected op_order_suspect for the smaller-tool first op");
        assert!(
            hit.message.contains("finish-before-rough"),
            "message should mention finish-before-rough: {}",
            hit.message
        );
    }

    /// i5g4 MVP: geometry bbox that does NOT include the WCS origin
    /// (default 0,0) emits a `stock_origin_outside_geometry_bbox`
    /// warning. The canonical case is a DXF drawn off-origin —
    /// (100..200, 100..200) — with the machine zeroed at (0,0).
    #[test]
    fn stock_bbox_not_containing_origin_emits_warning() {
        let segs = closed_square_offset(100.0, 100.0, 100.0);
        let tool = endmill(1, 3.0);
        let mut profile = profile_op(1, 1, ToolOffset::Outside);
        profile.params.step = Some(-1.0);
        profile.params.depth = -1.0;
        let project = project_with_segments(segs, vec![profile], vec![tool]);
        let mut warnings = Vec::new();
        push_wcs_origin_warning(&project, &mut warnings);
        let hit = warnings
            .iter()
            .find(|w| w.kind == "stock_origin_outside_geometry_bbox")
            .expect("expected WCS origin warning when geometry doesn't include (0,0)");
        assert_eq!(hit.op_id, None, "WCS warning is project-wide, not per-op");
    }

    /// v0ez: a Cut segment whose endpoint leaves the machine work-area
    /// box emits exactly one project-wide `out_of_work_area` warning
    /// carrying the offending count + first gcode line. An in-bounds cut
    /// and an out-of-bounds RAPID are both ignored.
    #[test]
    fn work_area_scan_flags_out_of_envelope_cut() {
        use crate::gcode::preview::{MoveKind, Pose3, ToolpathSegment};
        let machine = crate::cam::setup::MachineConfig::default(); // 200×300×50
        let seg = |fx, fy, fz, tx, ty, tz, kind, line| ToolpathSegment {
            from: Pose3 { x: fx, y: fy, z: fz },
            to: Pose3 { x: tx, y: ty, z: tz },
            kind,
            gcode_line: line,
            op_id: 0,
        };
        let toolpath = vec![
            // In-bounds cut — fine.
            seg(0.0, 0.0, 0.0, 50.0, 50.0, -2.0, MoveKind::Cut, 10),
            // Cut that ends 25 mm past +X travel — out of envelope.
            seg(50.0, 50.0, -2.0, 225.0, 50.0, -2.0, MoveKind::Cut, 11),
            // Rapid well outside the box — excluded (park / clearance move).
            seg(225.0, 50.0, -2.0, 500.0, 500.0, 5.0, MoveKind::Rapid, 12),
        ];
        let mut warnings = Vec::new();
        push_work_area_warning(&toolpath, &machine, &mut warnings);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.kind == "out_of_work_area")
            .collect();
        assert_eq!(hits.len(), 1, "expected exactly one work-area warning: {warnings:?}");
        assert_eq!(hits[0].op_id, None, "work-area warning is project-wide");
        assert!(
            hits[0].message.contains("1 cut move") && hits[0].message.contains("gcode line 11"),
            "message should count one offending cut at line 11: {}",
            hits[0].message
        );
    }

    /// v0ez: a fully in-envelope toolpath produces no warning, and a
    /// zeroed work area (unset machine) is skipped rather than flagging
    /// every move.
    #[test]
    fn work_area_scan_silent_when_in_bounds_or_unset() {
        use crate::gcode::preview::{MoveKind, Pose3, ToolpathSegment};
        let in_bounds = vec![ToolpathSegment {
            from: Pose3 { x: 0.0, y: 0.0, z: 0.0 },
            to: Pose3 { x: 10.0, y: 10.0, z: -1.0 },
            kind: MoveKind::Cut,
            gcode_line: 5,
            op_id: 0,
        }];
        let mut warnings = Vec::new();
        push_work_area_warning(&in_bounds, &crate::cam::setup::MachineConfig::default(), &mut warnings);
        assert!(warnings.is_empty(), "in-bounds toolpath should not warn: {warnings:?}");

        // Same out-of-bounds cut but with a zeroed work area → skipped.
        let mut zeroed = crate::cam::setup::MachineConfig::default();
        zeroed.work_area = crate::cam::setup::AxisLimits { x: 0.0, y: 0.0, z: 0.0 };
        let wild = vec![ToolpathSegment {
            from: Pose3 { x: 0.0, y: 0.0, z: 0.0 },
            to: Pose3 { x: 9999.0, y: 9999.0, z: -9999.0 },
            kind: MoveKind::Cut,
            gcode_line: 7,
            op_id: 0,
        }];
        let mut w2 = Vec::new();
        push_work_area_warning(&wild, &zeroed, &mut w2);
        assert!(w2.is_empty(), "zeroed/unset work area should be skipped: {w2:?}");
    }

    /// Origin-containing geometry produces NO WCS warning. (0..20 square
    /// includes (0,0) at the corner, the slack accepts it.)
    #[test]
    fn stock_bbox_containing_origin_no_warning() {
        let segs = closed_square(20.0);
        let tool = endmill(1, 3.0);
        let mut profile = profile_op(1, 1, ToolOffset::Outside);
        profile.params.step = Some(-1.0);
        profile.params.depth = -1.0;
        let project = project_with_segments(segs, vec![profile], vec![tool]);
        let mut warnings = Vec::new();
        push_wcs_origin_warning(&project, &mut warnings);
        assert!(
            warnings
                .iter()
                .all(|w| w.kind != "stock_origin_outside_geometry_bbox"),
            "no WCS warning expected, got {warnings:?}"
        );
    }
}
