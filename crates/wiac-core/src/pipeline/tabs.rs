//! Per-op tab placement resolver. Walks an op's `tab_mode` (Off /
//! Manual / Auto / Mixed) and produces a per-object map of
//! [`TabPoint`]s that the downstream `attach_tabs_to_offsets` pass
//! consumes verbatim. Manual placements come from
//! [`crate::cam::tabs::resolve_tab_placements`]; Auto / Mixed counts
//! generate evenly-spaced parameters via
//! [`crate::cam::tabs::auto_tab_ts`].

use std::collections::HashMap;

use crate::cam::offsets::TabPoint;
use crate::cam::VcObject;
use crate::project::Operation;

use super::op_includes_object;

/// Resolve an op's tab placements + auto-spacing into a per-object
/// `TabPoint` map for `attach_tabs_to_offsets` (rt1.10). Manual
/// placements walk `cam/tabs::polyline_at_t`; auto placements use
/// evenly spaced parameters over each closed source object's chain.
pub(super) fn build_op_tabs_by_object(
    op: &Operation,
    objects: &[VcObject],
) -> HashMap<usize, Vec<TabPoint>> {
    use crate::cam::segments_to_points;
    use crate::cam::tabs::{auto_tab_ts, polyline_at_t, resolve_tab_placements};
    use crate::project::TabPlacementMode;

    let mut out: HashMap<usize, Vec<TabPoint>> = match op.params.tab_mode {
        TabPlacementMode::Off => return HashMap::new(),
        TabPlacementMode::Manual => resolve_tab_placements(&op.params.tab_placements, objects, 6),
        TabPlacementMode::Auto { .. } | TabPlacementMode::Mixed { .. } => HashMap::new(),
    };
    // Auto + Mixed: add evenly-spaced tabs on every selected closed
    // object.
    if let TabPlacementMode::Auto { count } | TabPlacementMode::Mixed { auto_count: count } =
        op.params.tab_mode
    {
        if count > 0 {
            let auto_ts = auto_tab_ts(count, true);
            let auto_ts_open = auto_tab_ts(count, false);
            for (idx, obj) in objects.iter().enumerate() {
                if !op_includes_object(op, obj, idx) {
                    continue;
                }
                let pts = segments_to_points(&obj.segments, 6);
                if pts.len() < 2 {
                    continue;
                }
                let ts = if obj.closed { &auto_ts } else { &auto_ts_open };
                for &t in ts {
                    let (p, _) = polyline_at_t(&pts, t, obj.closed);
                    out.entry(idx).or_default().push(TabPoint {
                        x: p.x,
                        y: p.y,
                        width_override_mm: None,
                        height_override_mm: None,
                    });
                }
            }
        }
    }
    // For Mixed, also include manual placements (Manual was handled
    // above; Mixed enters this branch with no manual entries yet).
    if matches!(op.params.tab_mode, TabPlacementMode::Mixed { .. }) {
        for (k, v) in resolve_tab_placements(&op.params.tab_placements, objects, 6) {
            out.entry(k).or_default().extend(v);
        }
    }
    out
}
