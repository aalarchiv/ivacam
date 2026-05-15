//! Per-op pipeline warning helpers. Each runs against a single
//! operation + its inputs and pushes [`PipelineWarning`] entries onto
//! the caller's vector. Sanity warnings (`push_tool_fit_kind_warnings`,
//! `push_trochoidal_warnings`, `push_ramp_with_arcs_warning`) fire
//! before the offset cascade runs; size-fit warnings
//! (`push_tool_fit_size_warning`) fire after, because they need the
//! emitted offset list.

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::Setup;
use crate::cam::VcObject;
use crate::project::{Operation, OperationKind, PocketStrategy, Project};

use super::{op_includes_object, PipelineWarning};

/// Surface the v1 limitation that the ramp-pass emitter treats
/// boundary-crossing arcs as regular segments (instant Z descent at
/// the arc's start), not as ramped sections. Users with ramp plunge
/// on an arc-heavy source need to know the cutter dives at the arc
/// instead of sloping through it (audit 8so).
pub(super) fn push_ramp_with_arcs_warning(
    op: &Operation,
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

pub(super) fn push_trochoidal_warnings(op: &Operation, warnings: &mut Vec<PipelineWarning>) {
    if !matches!(
        op.kind,
        OperationKind::Pocket {
            strategy: PocketStrategy::Trochoidal { .. }
        }
    ) {
        return;
    }
    if op.params.tabs.active {
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
    op: &Operation,
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
        (OperationKind::Pocket { .. }, ToolKind::Drill) => Some("pocket op assigned a drill bit"),
        (OperationKind::Pocket { .. }, ToolKind::DragKnife) => {
            Some("pocket op assigned a drag knife (cut path won't carve area)")
        }
        (OperationKind::Profile { .. }, ToolKind::Drill) => Some("profile op assigned a drill bit"),
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
    op: &Operation,
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
    let needs_offset = match op.kind {
        OperationKind::Pocket { .. } => true,
        OperationKind::Profile {
            offset: crate::cam::setup::ToolOffset::Outside,
        }
        | OperationKind::Profile {
            offset: crate::cam::setup::ToolOffset::Inside,
        } => true,
        _ => false,
    };
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
    if matches!(op.kind, OperationKind::Pocket { .. })
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
