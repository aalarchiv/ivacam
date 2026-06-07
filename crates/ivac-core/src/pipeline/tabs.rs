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
use crate::project::Op;

use super::{op_includes_object, PipelineWarning};

/// 8rik: minimum spacing factor between auto-placed tabs. If the
/// contour's perimeter divided by `count` is shallower than
/// `tab_width * SHORT_CONTOUR_SPACING_FACTOR`, the auto count is
/// clamped down so each tab gets at least ~`0.5×tab_width` of cut
/// material between it and the next.
const SHORT_CONTOUR_SPACING_FACTOR: f64 = 1.5;

/// eylk: in Mixed mode, manual placements within this fraction of the
/// auto spacing (`1 / auto_count`) of an auto-position are treated as
/// the SAME tab. The manual placement wins (user intent overrides) and
/// the colliding auto position is dropped.
const MIXED_MERGE_FRACTION_OF_SPACING: f64 = 0.25;

/// Resolve an op's tab placements + auto-spacing into a per-object
/// `TabPoint` map for `attach_tabs_to_offsets` (rt1.10). Manual
/// placements walk `cam/tabs::polyline_at_t`; auto placements use
/// evenly spaced parameters over each closed source object's chain.
///
/// 8rik / a7rq / eylk: auto-count is clamped down when the perimeter
/// can't fit N tab footprints at the configured `tab.width` (warning
/// surfaced); closed-contour auto tabs are phase-shifted by half a
/// spacing so they don't sit on the start vertex; Mixed-mode merges
/// any manual placement that lands within `1/auto_count *
/// MIXED_MERGE_FRACTION_OF_SPACING` of an auto position (manual wins).
// juvx: walk over (object, mode) crosses every TabPlacementMode arm
// inline; splitting per mode would duplicate the warning-push +
// dedupe-merge logic across branches.
#[allow(clippy::too_many_lines)]
pub(super) fn build_op_tabs_by_object(
    op: &Op,
    objects: &[VcObject],
    warnings: &mut Vec<PipelineWarning>,
) -> HashMap<usize, Vec<TabPoint>> {
    use crate::cam::segments_to_points;
    use crate::cam::tabs::{
        auto_tab_ts, polyline_arc_lengths, polyline_at_t, resolve_tab_placements,
    };
    use crate::project::TabPlacementMode;

    // kbx5 step 2: tabs come from ContourParams (Profile / Pocket /
    // Engrave / DragKnife); other kinds have no tabs.
    let Some(contour) = op.contour_params() else {
        return HashMap::new();
    };
    // a7rq: closed-contour auto-tabs are phase-shifted by
    // `tab_width / 2 + epsilon` (computed per object below) so the
    // first tab lands mid-segment instead of on the start vertex; the
    // factor of `0.5 / count` we add by default ALSO keeps tabs off
    // corners on simple n-gons. Combined with the perimeter-aware
    // clamp below, the auto layout is always corner-safe.
    let tab_width = contour.tabs.width.max(0.0);
    let mut out: HashMap<usize, Vec<TabPoint>> = match contour.tab_mode {
        TabPlacementMode::Off => return HashMap::new(),
        TabPlacementMode::Manual => resolve_tab_placements(&contour.tab_placements, objects, 6),
        TabPlacementMode::Auto { .. } | TabPlacementMode::Mixed { .. } => HashMap::new(),
    };
    // Auto + Mixed: add evenly-spaced tabs on every selected closed
    // object.
    let auto_count = match contour.tab_mode {
        TabPlacementMode::Auto { count } | TabPlacementMode::Mixed { auto_count: count } => count,
        _ => 0,
    };
    if auto_count > 0 {
        for (idx, obj) in objects.iter().enumerate() {
            if !op_includes_object(op, obj, idx) {
                continue;
            }
            let pts = segments_to_points(&obj.segments, 6);
            if pts.len() < 2 {
                continue;
            }
            // 8rik: clamp auto count by perimeter / (tab_width * 1.5)
            // so adjacent tabs have ≥ 0.5×tab_width of cut material
            // between them. Open contours use the un-closed arc length;
            // closed contours include the closing segment.
            let (_, open_total) = polyline_arc_lengths(&pts);
            let perimeter = if obj.closed {
                open_total + pts.last().unwrap().distance(pts[0])
            } else {
                open_total
            };
            let max_fit = if tab_width > 1e-9 && perimeter > 1e-9 {
                (perimeter / (tab_width * SHORT_CONTOUR_SPACING_FACTOR)).floor() as u32
            } else {
                auto_count
            };
            // Always allow at least one tab (the user explicitly asked
            // for tabs; refusing to place any defeats the purpose).
            let effective_count = auto_count.min(max_fit.max(1));
            if effective_count < auto_count {
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "tabs_count_clamped_short_contour".into(),
                    message: format!(
                        "Tabs on op '{}' object #{}: perimeter {:.2} mm too short for {} tabs at width {:.2} mm; reduced to {}. Each tab now has at least {:.2} mm of cut between it and the next.",
                        op.name,
                        idx + 1,
                        perimeter,
                        auto_count,
                        tab_width,
                        effective_count,
                        tab_width * 0.5,
                    ),
                });
            }
            let mut auto_ts = if obj.closed {
                auto_tab_ts(effective_count, true)
            } else {
                auto_tab_ts(effective_count, false)
            };
            // a7rq: phase-shift closed-contour auto-tabs by
            // (tab_width / 2 + epsilon) of arc length (normalized to
            // perimeter) so the first tab lands flat on a segment, not
            // on the start vertex. Open contours already inset their
            // endpoints; skip there. epsilon=1e-6 of perimeter keeps a
            // zero-tab-width op landing identically to legacy.
            if obj.closed && perimeter > 1e-9 && tab_width > 1e-9 {
                let shift_arc = tab_width * 0.5 + 1e-6;
                let shift_t = (shift_arc / perimeter).min(0.49);
                for t in &mut auto_ts {
                    *t = (*t + shift_t).rem_euclid(1.0);
                }
            }
            // eylk: Mixed mode — dedupe manual placements vs auto
            // positions on the same object. A manual placement within
            // `(1 / auto_count) * MIXED_MERGE_FRACTION_OF_SPACING` of
            // an auto position drops the auto and keeps manual.
            let manual_ts_for_obj: Vec<f64> =
                if matches!(contour.tab_mode, TabPlacementMode::Mixed { .. }) {
                    contour
                        .tab_placements
                        .iter()
                        .filter(|tp| (tp.object_id as usize).checked_sub(1) == Some(idx))
                        .map(|tp| tp.t.rem_euclid(1.0))
                        .collect()
                } else {
                    Vec::new()
                };
            if !manual_ts_for_obj.is_empty() && effective_count > 0 {
                let spacing = 1.0 / f64::from(effective_count);
                let merge_tol = spacing * MIXED_MERGE_FRACTION_OF_SPACING;
                auto_ts.retain(|&auto_t| {
                    !manual_ts_for_obj.iter().any(|&m| {
                        let diff = (auto_t - m).abs();
                        let wrapped = (1.0 - diff).abs();
                        diff.min(wrapped) < merge_tol
                    })
                });
            }
            for t in auto_ts {
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
    // For Mixed, also include manual placements (Manual was handled
    // above; Mixed enters this branch with no manual entries yet).
    if matches!(contour.tab_mode, TabPlacementMode::Mixed { .. }) {
        for (k, v) in resolve_tab_placements(&contour.tab_placements, objects, 6) {
            out.entry(k).or_default().extend(v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::VcObject;
    use crate::geometry::{Point2, Segment};
    use crate::project::{
        ContourParams, Op, OpKind, OpParams, OpSource, ProfileParams, TabPlacement,
        TabPlacementMode,
    };
    use crate::project::{TabsConfig, ToolOffset};

    fn closed_square_segments(side: f64) -> Vec<Segment> {
        vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(side, 0.0), "0", 7),
            Segment::line(Point2::new(side, 0.0), Point2::new(side, side), "0", 7),
            Segment::line(Point2::new(side, side), Point2::new(0.0, side), "0", 7),
            Segment::line(Point2::new(0.0, side), Point2::new(0.0, 0.0), "0", 7),
        ]
    }

    fn make_op(contour: ContourParams) -> Op {
        Op {
            id: 1,
            name: "Profile".into(),
            enabled: true,
            kind: OpKind::Profile {
                offset: ToolOffset::Outside,
                contour,
                profile: ProfileParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
            pin_order: false,
        }
    }

    fn make_object(side: f64) -> VcObject {
        VcObject::new(closed_square_segments(side), true)
    }

    /// 8rik: 4 tabs at width=10 on a 5mm-perimeter (per side, so 20mm
    /// total) square forces a clamp. 20 / (10 * 1.5) = 1.33 → floor = 1
    /// tab. A warning is surfaced.
    #[test]
    fn auto_count_clamped_on_short_contour() {
        let side = 5.0; // perimeter = 20mm
        let contour = ContourParams {
            tabs: TabsConfig {
                active: true,
                width: 10.0,
                ..TabsConfig::default()
            },
            tab_mode: TabPlacementMode::Auto { count: 4 },
            ..ContourParams::default()
        };
        let op = make_op(contour);
        let objects = vec![make_object(side)];
        let mut warnings = Vec::new();
        let out = build_op_tabs_by_object(&op, &objects, &mut warnings);
        assert_eq!(out.get(&0).map(Vec::len), Some(1));
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == "tabs_count_clamped_short_contour"),
            "expected tabs_count_clamped_short_contour warning, got {:?}",
            warnings.iter().map(|w| &w.kind).collect::<Vec<_>>(),
        );
    }

    /// 8rik: a roomy contour (40mm side, 160mm perimeter) at width=10
    /// fits 4 tabs (160 / 15 = 10.6 ≥ 4) and emits no clamp warning.
    #[test]
    fn auto_count_passes_through_when_perimeter_fits() {
        let contour = ContourParams {
            tabs: TabsConfig {
                active: true,
                width: 10.0,
                ..TabsConfig::default()
            },
            tab_mode: TabPlacementMode::Auto { count: 4 },
            ..ContourParams::default()
        };
        let op = make_op(contour);
        let objects = vec![make_object(40.0)];
        let mut warnings = Vec::new();
        let out = build_op_tabs_by_object(&op, &objects, &mut warnings);
        assert_eq!(out.get(&0).map(Vec::len), Some(4));
        assert!(!warnings
            .iter()
            .any(|w| w.kind == "tabs_count_clamped_short_contour"));
    }

    /// a7rq: closed-contour auto tabs are phase-shifted by
    /// `tab_width/2` + ε of arc length so the first tab doesn't sit on
    /// the start vertex (which for a square IS a 90° corner). With
    /// side=40, perimeter=160, `tab_width=10` → phase shift ≈ 5/160 =
    /// 0.03125; first tab t = 0 + 0.03125 = 0.03125, world point
    /// (5.+ε, 0) — well off the corner.
    #[test]
    fn auto_tabs_on_closed_contour_skip_start_vertex() {
        let side = 40.0;
        let contour = ContourParams {
            tabs: TabsConfig {
                active: true,
                width: 10.0,
                ..TabsConfig::default()
            },
            tab_mode: TabPlacementMode::Auto { count: 4 },
            ..ContourParams::default()
        };
        let op = make_op(contour);
        let objects = vec![make_object(side)];
        let mut warnings = Vec::new();
        let out = build_op_tabs_by_object(&op, &objects, &mut warnings);
        let pts = out.get(&0).expect("expected tabs on object 0");
        assert_eq!(pts.len(), 4);
        // None of the tabs should land at the four corners (0,0), (40,0), (40,40), (0,40).
        let corners = [(0.0, 0.0), (40.0, 0.0), (40.0, 40.0), (0.0, 40.0)];
        for tp in pts {
            for &(cx, cy) in &corners {
                let d = ((tp.x - cx).powi(2) + (tp.y - cy).powi(2)).sqrt();
                assert!(
                    d > 1.0,
                    "tab at ({:.3},{:.3}) too close to corner ({},{}): d={:.3}",
                    tp.x,
                    tp.y,
                    cx,
                    cy,
                    d,
                );
            }
        }
    }

    /// eylk: Mixed mode with `auto_count=4` (t = 0, 0.25, 0.5, 0.75 pre-
    /// shift) and a manual placement at t=0.26 (on `object_id=1`) must
    /// dedupe down to 4 tabs total, not 5: the manual displaces the
    /// nearby auto position.
    #[test]
    fn mixed_mode_dedupes_nearby_manual_placement() {
        let side = 40.0;
        let mut contour = ContourParams {
            tabs: TabsConfig {
                active: true,
                width: 1.0, // tiny width so shift is negligible
                ..TabsConfig::default()
            },
            tab_mode: TabPlacementMode::Mixed { auto_count: 4 },
            ..ContourParams::default()
        };
        contour.tab_placements.push(TabPlacement {
            object_id: 1,
            t: 0.26,
            width_override_mm: None,
            height_override_mm: None,
        });
        let op = make_op(contour);
        let objects = vec![make_object(side)];
        let mut warnings = Vec::new();
        let out = build_op_tabs_by_object(&op, &objects, &mut warnings);
        let pts = out.get(&0).expect("expected tabs on object 0");
        assert_eq!(
            pts.len(),
            4,
            "expected 4 tabs after dedupe; got {} ({:?})",
            pts.len(),
            pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
        );
    }

    /// eylk: Mixed mode where the manual placement is FAR from any
    /// auto position (e.g. t=0.4 with `auto_count=4` → nearest is 0.5,
    /// diff=0.1 > 0.25*0.25=0.0625) keeps both → 5 tabs total.
    #[test]
    fn mixed_mode_keeps_separate_manual_placement() {
        let side = 40.0;
        let mut contour = ContourParams {
            tabs: TabsConfig {
                active: true,
                width: 1.0,
                ..TabsConfig::default()
            },
            tab_mode: TabPlacementMode::Mixed { auto_count: 4 },
            ..ContourParams::default()
        };
        contour.tab_placements.push(TabPlacement {
            object_id: 1,
            t: 0.4,
            width_override_mm: None,
            height_override_mm: None,
        });
        let op = make_op(contour);
        let objects = vec![make_object(side)];
        let mut warnings = Vec::new();
        let out = build_op_tabs_by_object(&op, &objects, &mut warnings);
        let pts = out.get(&0).expect("expected tabs on object 0");
        assert_eq!(pts.len(), 5);
    }
}
