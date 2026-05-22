//! Op-source selection helpers (56a, split from pipeline.rs). One place
//! to map between an op's `OpSource` ({ All, Layers, Objects } variant)
//! and the chained-object set produced by `segments2objects`. Used by
//! `offset_builder`, `setup_resolver`, `tabs`, `warnings`, and `frame`.
//!
//! These functions are silent about IDs / layers that fall outside the
//! current `objects` set — the chained set may have been replaced by a
//! prior op's pattern expansion or frame synthesis, and an op pointing
//! at a now-gone object should produce no segments rather than panic.

use crate::cam::VcObject;
use crate::geometry::Segment;
use crate::project::{Op, OpSource, SourceCombine};

use super::PipelineWarning;

/// Slice the project's segments down to the subset this op consumes.
/// Used by the cache key — hashing the relevant segments only keeps the
/// hit rate up when the user adds unrelated geometry on a different
/// layer or another object that this op never touches.
///
/// `objects` is the current chained-object set (which the per-op loop
/// may have expanded with patterns or frame synthesis); for
/// `OpSource::Objects { ids }` we walk only the segments owned by the
/// selected objects in id order so adding an unrelated object that
/// falls outside the current `objects` set is silently skipped (e.g.
/// after a prior op's pattern expansion replaced the chained set — the
/// resulting empty segment list still hashes deterministically).
pub(in crate::pipeline) fn resolve_op_segments(op: &Op, all: &[Segment], objects: &[VcObject]) -> Vec<Segment> {
    match &op.source {
        OpSource::All => all.to_vec(),
        OpSource::Layers { layers, .. } => all
            .iter()
            .filter(|s| layers.iter().any(|l| l.as_str() == s.layer.as_ref()))
            .cloned()
            .collect(),
        OpSource::Objects { ids, .. } => {
            let mut out = Vec::new();
            for &id in ids {
                let idx = (id as usize).saturating_sub(1);
                if let Some(obj) = objects.get(idx) {
                    out.extend(obj.segments.iter().cloned());
                }
            }
            out
        }
    }
}

/// Walk the op's source in user-specified order and return the matching
/// object indices. Used by non-Auto combine modes — Difference in
/// particular is order-sensitive ("first selected minus the rest"), so
/// we cannot iterate the unordered `selected_set` there.
pub(in crate::pipeline) fn ordered_selection(op: &Op, objects: &[VcObject]) -> Vec<usize> {
    match &op.source {
        OpSource::All => (0..objects.len()).collect(),
        OpSource::Layers { layers, .. } => objects
            .iter()
            .enumerate()
            .filter(|(_, obj)| layers.iter().any(|l| l.as_str() == obj.layer.as_ref()))
            .map(|(i, _)| i)
            .collect(),
        OpSource::Objects { ids, .. } => ids
            .iter()
            .filter_map(|id| {
                let idx = (*id as usize).checked_sub(1)?;
                objects.get(idx).map(|_| idx)
            })
            .collect(),
    }
}

/// Pull the `SourceCombine` mode out of an op's source.
///
/// `OpSource::All` always reports `Auto` — by design. "All objects" has
/// no UI affordance for a combine selector, so the pipeline treats it
/// as "let each op kind decide". Pocket then falls through to its
/// containment-aware per-object loop (outer carves + inner holes);
/// Profile / Engrave / `DragKnife` emit one path per selected object.
/// Layers / Objects sources carry an explicit `combine` field and that
/// value is honored verbatim — including `Auto`, which means the same
/// per-op-kind dispatch path.
pub(in crate::pipeline) fn source_combine_mode(op: &Op) -> SourceCombine {
    match &op.source {
        OpSource::All => SourceCombine::Auto,
        OpSource::Layers { combine, .. } | OpSource::Objects { combine, .. } => *combine,
    }
}

/// 7l0a: surface `OpSource::Objects { ids }` entries that point at IDs no
/// longer present in the current `objects` slice. The chained-object set
/// changes between import and emit (pattern expansion, frame synthesis,
/// user deletion of the source object after creating the op), so an
/// otherwise-silent "empty selection" can hide a real misconfig. Emits
///   * `op_source_missing_object` per missing id, and
///   * `op_source_empty` (critical) when the filtered set is empty after
///     all missing ids are dropped.
///
/// Cheap to run — single linear walk over `ids`. Called from the per-op
/// loop before [`resolve_op_segments`] runs so the warnings ride along
/// with the rest of the op's diagnostics.
pub(in crate::pipeline) fn validate_op_source_objects(
    op: &Op,
    objects: &[VcObject],
    warnings: &mut Vec<PipelineWarning>,
) {
    let OpSource::Objects { ids, .. } = &op.source else {
        return;
    };
    let mut survivors = 0usize;
    for &id in ids {
        let idx = (id as usize).saturating_sub(1);
        // `saturating_sub` collapses id=0 to idx=0 too; treat the 0-id case
        // as missing since 1-based ids should never be 0 in well-formed
        // data.
        if id == 0 || objects.get(idx).is_none() {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "op_source_missing_object".into(),
                message: format!(
                    "op '{}': source references object id {} which is not in the current chained-object set (deleted or replaced by pattern/frame expansion). The id is silently dropped from this op's selection.",
                    op.name, id
                ),
            });
        } else {
            survivors += 1;
        }
    }
    if survivors == 0 && !ids.is_empty() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "op_source_empty".into(),
            message: format!(
                "op '{}': every object id in the source ({} entries) is missing from the current chained-object set — the op will produce no toolpath. Re-pick the source or remove the op.",
                op.name,
                ids.len()
            ),
        });
    }
}

pub(in crate::pipeline) fn op_includes_object(op: &Op, obj: &VcObject, idx: usize) -> bool {
    match &op.source {
        OpSource::All => true,
        OpSource::Layers { layers, .. } => layers.iter().any(|l| l.as_str() == obj.layer.as_ref()),
        // OpSource::Objects ids are 1-based, matching the
        // ImportOutput.objects[i] mapping the frontend uses for
        // selection.
        OpSource::Objects { ids, .. } => {
            let chain_id = (idx as u32) + 1;
            ids.contains(&chain_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::setup::ToolOffset;
    use crate::pipeline::test_helpers::{endmill, profile_op, project_with};
    use crate::project::SourceCombine;

    /// 7l0a: an OpSource::Objects id that doesn't map to any current
    /// VcObject emits `op_source_missing_object` AND, when every id is
    /// missing, an `op_source_empty` critical warning.
    #[test]
    fn validate_op_source_missing_id_warns() {
        let _tool = endmill(1, 3.0);
        let mut op = profile_op(7, 1, ToolOffset::Outside);
        op.source = OpSource::Objects {
            ids: vec![1, 42], // id=42 has no backing object
            combine: SourceCombine::Auto,
        };
        // Empty objects slice (real chain after a previous op blew it
        // away) — both ids are missing.
        let mut warnings = Vec::new();
        validate_op_source_objects(&op, &[], &mut warnings);
        assert!(
            warnings.iter().any(|w| w.kind == "op_source_missing_object"
                && w.op_id == Some(7)
                && w.message.contains("42")),
            "expected op_source_missing_object warning for id 42, got {:?}",
            warnings
        );
        assert!(
            warnings.iter().any(|w| w.kind == "op_source_empty"),
            "expected op_source_empty when EVERY id is missing, got {:?}",
            warnings
        );
    }

    /// 7l0a: OpSource::All is never an "objects" source — no warnings
    /// are emitted regardless of the objects slice.
    #[test]
    fn validate_op_source_all_emits_no_warning() {
        let op = profile_op(1, 1, ToolOffset::Outside);
        let mut warnings = Vec::new();
        validate_op_source_objects(&op, &[], &mut warnings);
        assert!(warnings.is_empty(), "OpSource::All should not warn");
    }

    /// 7l0a: project_with builds a project of given ops + tools so a
    /// quick `run_pipeline` smoke test exercises the wiring without
    /// crashing.
    #[test]
    fn validate_op_source_run_pipeline_smoke() {
        use crate::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
        let tool = endmill(1, 3.0);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.source = OpSource::Objects {
            ids: vec![99], // no matching object
            combine: SourceCombine::Auto,
        };
        op.params.step = Some(-1.0);
        op.params.depth = -1.0;
        let resp = run_pipeline(
            PipelineRequest {
                project: project_with(vec![op], vec![tool]),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.warnings.iter().any(|w| w.kind == "op_source_empty"),
            "expected op_source_empty from run_pipeline, got {:?}",
            resp.warnings
        );
    }
}
