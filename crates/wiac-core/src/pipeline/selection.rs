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
                // alwa: 1-based ids — id=0 is invalid (and flagged separately
                // by validate_op_source_objects). saturating_sub(1) would
                // silently fold id=0 into objects[0], so two ops with
                // ids=[0,X] and [0,Y] would smuggle object 0's segments
                // into both — spurious cache misses + future correctness
                // bugs. Use checked_sub to skip id=0 cleanly, matching
                // ordered_selection above.
                let Some(idx) = (id as usize).checked_sub(1) else {
                    continue;
                };
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
        // alwa: 1-based ids — id=0 is invalid and reported as missing.
        // `checked_sub(1)` returns None for id=0 so we don't have to
        // special-case the saturating-collapse-to-objects[0] foot-gun.
        let missing = match (id as usize).checked_sub(1) {
            None => true,
            Some(idx) => objects.get(idx).is_none(),
        };
        if missing {
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

/// dcna: sibling of `validate_op_source_objects` for `OpSource::Layers`.
/// A user typo in a layer name (e.g. "TEXTT" instead of "TEXT") used to
/// produce a silent no-op: `resolve_op_segments` returned an empty Vec,
/// the op emitted nothing, no diagnostic surfaced. Now we check each
/// requested layer against the segment pool's layer set and emit
///   * `op_source_missing_layer` per layer not present in the segment
///     set (covers both user-imported layers and synthetic
///     `__text_<id>` layers — text rendering pre-populates
///     `project.segments` before this validator runs), and
///   * `op_source_empty` (critical) when every requested layer is
///     missing.
///
/// Cheap — single pass over segments to collect the layer set, then a
/// linear walk over the requested layers. Called from the per-op loop
/// before `resolve_op_segments` so warnings ride along even on a cache
/// hit.
pub(in crate::pipeline) fn validate_op_source_layers(
    op: &Op,
    segments: &[Segment],
    warnings: &mut Vec<PipelineWarning>,
) {
    let OpSource::Layers { layers, .. } = &op.source else {
        return;
    };
    // De-dupe by &str so the membership lookup is O(L) per requested
    // layer rather than O(segments.len()).
    let mut present: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for s in segments {
        present.insert(s.layer.as_ref());
    }
    let mut survivors = 0usize;
    for layer in layers {
        if present.contains(layer.as_str()) {
            survivors += 1;
        } else {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "op_source_missing_layer".into(),
                message: format!(
                    "op '{}': source references layer '{}' which is not present in the project's segment pool (typo, deleted import, or removed text layer). The layer is silently dropped from this op's selection.",
                    op.name, layer
                ),
            });
        }
    }
    if survivors == 0 && !layers.is_empty() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "op_source_empty".into(),
            message: format!(
                "op '{}': every layer in the source ({} entries) is missing from the project — the op will produce no toolpath. Re-pick the source or remove the op.",
                op.name,
                layers.len()
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

    // ─── alwa: id=0 must NOT silently map to objects[0] ─────────────

    /// alwa: `OpSource::Objects { ids: vec![0, X] }` used to silently
    /// pull objects[0]'s segments because `saturating_sub(1)` collapses
    /// 0 → 0. Now `resolve_op_segments` uses `checked_sub` and drops
    /// the id=0 entry cleanly, producing the same segment set as if
    /// id=0 hadn't been listed at all.
    #[test]
    fn resolve_op_segments_id_zero_drops_object_zero() {
        use crate::cam::VcObject;
        use crate::geometry::{Point2, Segment};

        // Two objects with distinguishable segments.
        let seg_a = Segment::line(Point2::new(0.0, 0.0), Point2::new(1.0, 0.0), "A", 1);
        let seg_b = Segment::line(Point2::new(0.0, 0.0), Point2::new(0.0, 1.0), "B", 2);
        let obj_a = VcObject::new(vec![seg_a.clone()], false);
        let obj_b = VcObject::new(vec![seg_b.clone()], false);
        let objects = vec![obj_a, obj_b];

        let mut op_id0 = profile_op(1, 1, ToolOffset::Outside);
        op_id0.source = OpSource::Objects {
            ids: vec![0, 2], // id=0 invalid (should be skipped), id=2 → obj_b
            combine: SourceCombine::Auto,
        };
        let segs_id0 = resolve_op_segments(&op_id0, &[], &objects);
        // Only obj_b's segments should be present; obj_a (index 0) must
        // NOT be smuggled in through saturating_sub(1)=0.
        assert_eq!(
            segs_id0.len(),
            1,
            "id=0 must be skipped, not folded into objects[0]; got {} segs",
            segs_id0.len()
        );
        assert_eq!(
            segs_id0[0].layer.as_ref(),
            "B",
            "id=0 silently pulled objects[0] (layer A) instead of being skipped"
        );

        // Sanity: two ops differing only in their id-zero ghost
        // ([0, 2] vs [2]) now produce the same segment set, so the
        // cache key collapses and the entries hit. Spurious cache
        // misses from id=0 ghosts are eliminated.
        let mut op_no_id0 = op_id0.clone();
        op_no_id0.source = OpSource::Objects {
            ids: vec![2],
            combine: SourceCombine::Auto,
        };
        let segs_no_id0 = resolve_op_segments(&op_no_id0, &[], &objects);
        assert_eq!(
            segs_id0, segs_no_id0,
            "OpSource::Objects ids=[0,2] and [2] must resolve to the same segments after the id=0 guard"
        );
    }

    /// alwa: `validate_op_source_objects` still flags id=0 as missing
    /// (parity with the resolve-side guard) — same warning kind as a
    /// stale 1-based id.
    #[test]
    fn validate_op_source_id_zero_flags_as_missing() {
        let mut op = profile_op(3, 1, ToolOffset::Outside);
        op.source = OpSource::Objects {
            ids: vec![0],
            combine: SourceCombine::Auto,
        };
        let mut warnings = Vec::new();
        validate_op_source_objects(&op, &[], &mut warnings);
        assert!(
            warnings.iter().any(|w| w.kind == "op_source_missing_object"
                && w.message.contains(" 0 ")),
            "id=0 must surface as op_source_missing_object, got {:?}",
            warnings
        );
    }

    // ─── dcna: OpSource::Layers missing-layer validation ────────────

    /// dcna: a typo in a layer name used to silently produce zero
    /// segments and no warning. Now `validate_op_source_layers` emits
    /// `op_source_missing_layer` per unknown layer, plus
    /// `op_source_empty` when no requested layer matches.
    #[test]
    fn validate_op_source_layers_missing_warns() {
        use crate::geometry::{Point2, Segment};
        let segs = vec![Segment::line(
            Point2::new(0.0, 0.0),
            Point2::new(1.0, 0.0),
            "TEXT", // real layer
            7,
        )];
        let mut op = profile_op(5, 1, ToolOffset::Outside);
        op.source = OpSource::Layers {
            layers: vec!["TEXT".into(), "TEXTT".into()], // second is a typo
            combine: SourceCombine::Auto,
        };
        let mut warnings = Vec::new();
        validate_op_source_layers(&op, &segs, &mut warnings);
        assert!(
            warnings.iter().any(|w| w.kind == "op_source_missing_layer"
                && w.op_id == Some(5)
                && w.message.contains("TEXTT")),
            "expected op_source_missing_layer for typo 'TEXTT', got {:?}",
            warnings
        );
        // One layer matches, so no op_source_empty.
        assert!(
            !warnings.iter().any(|w| w.kind == "op_source_empty"),
            "op_source_empty must NOT fire when at least one layer matches"
        );
    }

    /// dcna: when EVERY requested layer is missing, both
    /// `op_source_missing_layer` (per layer) and the critical
    /// `op_source_empty` warning fire.
    #[test]
    fn validate_op_source_layers_all_missing_emits_empty() {
        use crate::geometry::{Point2, Segment};
        let segs = vec![Segment::line(
            Point2::new(0.0, 0.0),
            Point2::new(1.0, 0.0),
            "REAL",
            7,
        )];
        let mut op = profile_op(8, 1, ToolOffset::Outside);
        op.source = OpSource::Layers {
            layers: vec!["GHOST".into()],
            combine: SourceCombine::Auto,
        };
        let mut warnings = Vec::new();
        validate_op_source_layers(&op, &segs, &mut warnings);
        assert!(
            warnings.iter().any(|w| w.kind == "op_source_missing_layer"),
            "expected op_source_missing_layer for 'GHOST', got {:?}",
            warnings
        );
        assert!(
            warnings.iter().any(|w| w.kind == "op_source_empty"),
            "expected op_source_empty when every layer is missing, got {:?}",
            warnings
        );
    }

    /// dcna: `OpSource::All` / `OpSource::Objects` never trigger the
    /// layer validator.
    #[test]
    fn validate_op_source_layers_skips_non_layers_sources() {
        let op = profile_op(1, 1, ToolOffset::Outside);
        let mut warnings = Vec::new();
        validate_op_source_layers(&op, &[], &mut warnings);
        assert!(
            warnings.is_empty(),
            "OpSource::All must not trigger layer validation"
        );
    }

    /// dcna: end-to-end through `run_pipeline` — a typoed layer name
    /// surfaces both `op_source_missing_layer` and `op_source_empty`
    /// in the response warnings.
    #[test]
    fn validate_op_source_layers_run_pipeline_smoke() {
        use crate::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
        let tool = endmill(1, 3.0);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.source = OpSource::Layers {
            layers: vec!["NOPE".into()],
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
            resp.warnings.iter().any(|w| w.kind == "op_source_missing_layer"),
            "expected op_source_missing_layer from run_pipeline, got {:?}",
            resp.warnings
        );
        assert!(
            resp.warnings.iter().any(|w| w.kind == "op_source_empty"),
            "expected op_source_empty from run_pipeline, got {:?}",
            resp.warnings
        );
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
