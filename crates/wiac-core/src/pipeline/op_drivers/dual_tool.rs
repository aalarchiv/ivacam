//! Dual-tool finish dispatch (rt1.33).
//!
//! Run from [`super::run_standard_op`] for any non-Drill op kind:
//! if the op declares a finish tool AND the offsets cascade produced
//! at least one ring tagged `is_finish`, split at that boundary and
//! emit `rough → M6 toolchange → finish`. Otherwise fall through to
//! a single [`emit_polylines_block`] call with the op's primary
//! setup.
//!
//! All Profile / Pocket / Engrave / Chamfer / `DragKnife` ops share
//! this code path — the only per-kind behaviour is whether the
//! offsets cascade decided to emit a finish ring. The driver itself
//! is kind-agnostic.

use crate::cam::offsets::PolylineOffset;
use crate::cam::setup::Setup;
use crate::gcode::{emit_polylines_block, PostProcessor};
use crate::geometry::Point2;
use crate::pipeline::{emit_toolchange_envelope, synthesize_finish_setup, PipelineError, PipelineWarning};
use crate::project::{Op, Project};

/// Returns `true` when the driver actually emitted an internal
/// rough→finish toolchange envelope. Used by `run_per_op` to decide
/// whether to bias `prev_tool_id` to `finish_tool_id` for the next
/// op's M6 decision (nguf).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_dual_tool_or_single<P: PostProcessor>(
    op: &Op,
    project: &Project,
    setup: &Setup,
    offsets: &[PolylineOffset],
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<bool, PipelineError> {
    let dual = synthesize_finish_setup(op, project, warnings)?;
    let has_finish_offsets = offsets.iter().any(|o| o.is_finish);
    let Some(finish_setup) = dual.filter(|_| has_finish_offsets) else {
        // Plain single-tool single-emit path — the common case for
        // Profile / Pocket / Engrave / etc. without a finish ring.
        // nguf: includes the dual-tool-declared-but-no-finish-offsets
        // fall-through, which previously left `prev_tool_id` biased
        // to the finish id even though no swap was emitted.
        emit_polylines_block(setup, offsets, post, last_pos);
        return Ok(false);
    };

    let (rough_offsets, finish_offsets): (Vec<_>, Vec<_>) =
        offsets.iter().cloned().partition(|o| !o.is_finish);
    if !rough_offsets.is_empty() {
        emit_polylines_block(setup, &rough_offsets, post, last_pos);
    }
    // Toolchange + comment. post.tool() emits T<n> M6 for posts that
    // support it; no-op posts (GRBL) skip silently. Surface a
    // pipeline warning when the machine isn't toolchange-capable so
    // the user spots the manual-intervention requirement.
    if !project.machine.supports_toolchange {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "dual_tool_no_toolchange".into(),
            message: format!(
                "op '{}' uses a dual-tool setup (rough + finish) but the machine config has supports_toolchange=false; the gcode will assume a manual tool change.",
                op.name
            ),
        });
    }
    post.raw(&format!(
        "; toolchange: finish pass with tool {}",
        finish_setup.tool.number
    ));
    // bd eaeq/m8sq: wrap the rough→finish M6 in the safety envelope
    // (safe-Z → M5+dwell → M6 → z-shift → M3+dwell). The helper picks
    // up the finish tool's Z shift, spindle speed, and warm-up pause
    // automatically — the previous code emitted only `T<n> M6`
    // followed by an optional G92 Z shift, leaving the spindle still
    // running through the change.
    let finish_tool = op
        .finish_tool_id
        .and_then(|id| project.tools.iter().find(|t| t.id == id));
    emit_toolchange_envelope(
        post,
        &project.machine,
        setup,
        finish_tool,
        finish_setup.tool.number,
        false,
        // liyy: the finish block emits at the resolved finish RPM; spin
        // the envelope up to that directly so the post doesn't emit a
        // transient M3 at the rough speed first.
        Some(finish_setup.tool.speed),
    );
    if !finish_offsets.is_empty() {
        emit_polylines_block(&finish_setup, &finish_offsets, post, last_pos);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use crate::cam::setup::MachineConfig;
    use crate::pipeline::test_helpers::{closed_square_offset, endmill, pocket_op};
    use crate::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
    use crate::project::{Op, OpKind, OpParams, OpSource, Project};

    /// Dual-tool Pocket op (rt1.33): when `finish_tool_id` is set to a
    /// different tool, the gcode contains a `T<n> M6` toolchange and
    /// uses the finish tool's feed for the wall ring.
    #[test]
    fn dual_tool_pocket_emits_toolchange_and_uses_finish_tool_feed() {
        let mut rough_tool = endmill(1, 6.0);
        rough_tool.feed_rate = 1500;
        rough_tool.speed = 20_000;
        let mut finish_tool = endmill(2, 3.0);
        finish_tool.feed_rate = 600;
        finish_tool.speed = 24_000;
        finish_tool.feed_rate_finish = Some(300);

        let machine = MachineConfig {
            supports_toolchange: true,
            ..MachineConfig::default()
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine,
            tools: vec![rough_tool, finish_tool],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: Some(2),
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("T2 M6"),
            "expected toolchange T2 M6 for finish pass:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("F1500"),
            "expected rough feed 1500:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("F300"),
            "expected finish feed 300 (finish tool's feed_rate_finish):\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("S24000"),
            "expected finish tool spindle 24000:\n{}",
            resp.gcode
        );
    }

    /// Dual-tool Pocket op without a distinct finish tool
    /// (`finish_tool_id` == `tool_id`) — no toolchange emitted.
    #[test]
    fn dual_tool_same_id_skips_toolchange() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: Some(1),
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            !resp.gcode.contains(" M6"),
            "expected no toolchange when finish_tool_id == tool_id:\n{}",
            resp.gcode
        );
    }

    /// Dual-tool Pocket op without `finish_tool_id` (None) — legacy
    /// single-tool behavior: no toolchange.
    #[test]
    fn dual_tool_none_uses_single_tool() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pocket_op(1, 1, OpSource::All)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(!resp.gcode.contains(" M6"));
    }

    /// Dual-tool Pocket (rt1.33) with `z_shift` on the finish tool:
    /// after the M6 we emit the finish tool's G92 Z shift.
    #[test]
    fn dual_tool_finish_tool_z_shift_emits_g92_after_m6() {
        let rough_tool = endmill(1, 6.0);
        let mut finish_tool = endmill(2, 3.0);
        finish_tool.z_shift_mm = Some(1.25);
        let machine = MachineConfig {
            supports_toolchange: true,
            ..MachineConfig::default()
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine,
            tools: vec![rough_tool, finish_tool],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                    contour: crate::project::ContourParams::default(),
                    pocket: crate::project::PocketParams::default(),
                },
                tool_id: 1,
                finish_tool_id: Some(2),
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
            work_offset: crate::project::WorkOffset::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.gcode.contains("T2 M6"), "toolchange missing");
        let m6_idx = resp.gcode.find("T2 M6").unwrap();
        let after = &resp.gcode[m6_idx..];
        assert!(
            after.contains("G92 Z1.25"),
            "expected G92 Z1.25 AFTER T2 M6:\n{}",
            resp.gcode
        );
    }
}
