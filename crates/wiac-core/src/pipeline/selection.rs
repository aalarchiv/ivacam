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
