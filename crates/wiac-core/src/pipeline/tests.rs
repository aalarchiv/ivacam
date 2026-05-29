// 56a: extracted from pipeline.rs to keep the dispatcher file
// navigable. Loaded via `#[cfg(test)] mod tests;` in pipeline.rs;
// `super::*` still refers to the pipeline module.
//
// Test assertions like `assert_eq!(effective_step(&op, &tool).unwrap(), -0.5)`
// compare values that propagate through the pipeline by direct
// assignment from a literal — exact equality is the right test.
#![allow(clippy::float_cmp)]

use super::test_helpers::*;
use super::*;
use crate::cam::setup::{MachineConfig, ToolOffset};
use crate::geometry::Segment;
use crate::project::{
    FormProfileSample, Op, OpKind, OpParams, OpSource, SourceCombine, TextAlignment, TextLayer,
    TextLayerKind, ToolEntry, ToolKind,
};

/// f60x-C: a `ReliefMill` op with a brightness ramp + a ball-nose tool emits
/// a varying-Z surfacing toolpath end-to-end (no source geometry needed —
/// the surface comes from the project's relief source). A wrong tool kind
/// surfaces `tool_kind_mismatch`.
#[test]
fn pipeline_relief_mill_emits_varying_z_ballnose_surface() {
    use crate::cam::surface_mill::ScanDirection;
    use crate::geometry::Point2;
    use crate::project::ReliefSource;

    // 6x6 brightness ramp: dark (deep) at x=0 → bright (top) at x=max.
    let cols = 6u32;
    let rows = 6u32;
    let mut brightness = Vec::new();
    for _iy in 0..rows {
        for ix in 0..cols {
            brightness.push(ix as f32 / (cols as f32 - 1.0));
        }
    }
    let source = ReliefSource {
        id: 7,
        name: "ramp".into(),
        origin: Point2::new(0.0, 0.0),
        cell: 2.0,
        cols,
        rows,
        brightness,
    };
    let mut ball = endmill(1, 4.0);
    ball.kind = ToolKind::BallNose;
    ball.flute_length_mm = Some(20.0);
    let relief = |tool_id: u32| Op {
        id: 1,
        name: "Relief".into(),
        enabled: true,
        kind: OpKind::ReliefMill {
            source_id: 7,
            z_min_mm: -5.0,
            z_max_mm: 0.0,
            invert: false,
            scallop_height_mm: 0.0,
            stepover_mm: Some(2.0),
            scan_direction: ScanDirection::AlongX,
            along_step_mm: 1.0,
        },
        tool_id,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let project = Project {
        segments: Vec::new(),
        machine: MachineConfig::default(),
        tools: vec![ball],
        operations: vec![relief(1)],
        fixtures: Vec::default(),
        text_layers: Vec::new(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: vec![source.clone()],
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .expect("relief pipeline should run end-to-end");
    assert!(resp.gcode.contains("; OP 1"), "no op marker for relief op");
    // Cut segments exist and their Z varies (the ramp surface), staying
    // within the configured [-5, 0] range.
    let cut_zs: Vec<f64> = resp
        .toolpath
        .iter()
        .filter(|s| s.op_id == 1 && matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
        .map(|s| s.to.z)
        .collect();
    assert!(!cut_zs.is_empty(), "relief op emitted no cut moves");
    let zmin = cut_zs.iter().copied().fold(f64::INFINITY, f64::min);
    let zmax = cut_zs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    assert!(
        zmax - zmin > 0.5,
        "relief Z should vary across the ramp (got span {zmin}..{zmax})"
    );
    assert!(
        zmin >= -5.0 - 1e-6 && zmax <= 1e-6,
        "relief Z out of range: {zmin}..{zmax}"
    );
    assert!(
        !resp.warnings.iter().any(|w| w.kind == "tool_kind_mismatch"),
        "ball-nose relief should not warn tool_kind_mismatch: {:?}",
        resp.warnings
    );

    // Wrong tool kind → tool_kind_mismatch.
    let mut flat = endmill(2, 4.0); // Endmill, not BallNose
    flat.id = 2;
    let project2 = Project {
        segments: Vec::new(),
        machine: MachineConfig::default(),
        tools: vec![flat],
        operations: vec![relief(2)],
        fixtures: Vec::default(),
        text_layers: Vec::new(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: vec![source],
    };
    let resp2 = run_pipeline(
        PipelineRequest {
            project: project2,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .expect("relief pipeline (flat tool) should still run");
    assert!(
        resp2
            .warnings
            .iter()
            .any(|w| w.kind == "tool_kind_mismatch"),
        "non-ball-nose relief should warn tool_kind_mismatch: {:?}",
        resp2.warnings
    );
}

#[test]
fn pipeline_renders_text_layers_and_routes_via_synthetic_layer() {
    // Engrave op pointing at the synthetic `__text_1` layer.
    let engrave = Op {
        id: 1,
        name: "Engrave text".into(),
        enabled: true,
        kind: OpKind::Engrave {
            contour: crate::project::ContourParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::Layers {
            layers: vec!["__text_1".into()],
            combine: SourceCombine::default(),
        },
        params: OpParams::mill_default(),
        group: None,
    };
    let text_layer = TextLayer {
        id: 1,
        kind: TextLayerKind::Text,
        name: "Hello".into(),
        text: "A".into(),
        font_bytes: dejavu_font_bytes(),
        size_mm: 10.0,
        origin: (0.0, 0.0),
        rotation_deg: 0.0,
        letter_spacing_mm: 0.0,
        line_spacing_mm: 0.0,
        alignment: TextAlignment::Left,
        width_scale: 1.0,
    };
    let project = Project {
        segments: Vec::new(), // pipeline pre-pass appends the rendered text
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 1.0)],
        operations: vec![engrave],
        fixtures: Vec::default(),
        text_layers: vec![text_layer],
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .expect("pipeline should run text engraving end-to-end");
    // Pipeline emitted gcode and at least one cut move tagged to op #1.
    assert!(resp.gcode.contains("; OP 1"), "no op marker for text op");
    assert!(
        resp.toolpath.iter().any(|s| s.op_id == 1),
        "no cut segments emitted for the text op"
    );
}

#[test]
fn run_pipeline_emits_a_recognizable_program() {
    let resp = run_pipeline(
        PipelineRequest {
            project: project_with(
                vec![profile_op(1, 1, ToolOffset::Outside)],
                vec![endmill(1, 3.0)],
            ),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    assert!(resp.gcode.contains("G21"));
    assert!(resp.gcode.contains("G90"));
    assert!(!resp.toolpath.is_empty());
    assert_eq!(resp.stats.object_count, 1);
    assert_eq!(resp.stats.closed_object_count, 1);
    assert!(resp.stats.offset_count >= 1);
    assert!(resp.gcode.contains("; OP 1"));
    // Cut segments carry the op id; program-header rapids carry op_id=0.
    assert!(resp.toolpath.iter().any(|s| s.op_id == 1));
    assert!(resp
        .toolpath
        .iter()
        .filter(|s| s.op_id != 0)
        .all(|s| s.op_id == 1));
}

#[test]
fn run_pipeline_picks_grbl_when_requested() {
    let resp = run_pipeline(
        PipelineRequest {
            project: project_with(
                vec![profile_op(1, 1, ToolOffset::Outside)],
                vec![endmill(1, 3.0)],
            ),
            post_processor: Some(PostProcessorKind::Grbl),
        },
        |_, _, _| {},
    )
    .unwrap();
    assert!(!resp.gcode.is_empty());
}

#[test]
fn two_op_project_emits_two_distinct_op_blocks() {
    let project = project_with(
        vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 1, ToolOffset::Outside),
        ],
        vec![endmill(1, 3.0)],
    );
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    assert!(resp.gcode.contains("; OP 1"));
    assert!(resp.gcode.contains("; OP 2"));
    assert!(resp.toolpath.iter().any(|s| s.op_id == 1));
    assert!(resp.toolpath.iter().any(|s| s.op_id == 2));
}

#[test]
fn progress_callback_fires_each_phase() {
    let phases = std::cell::RefCell::new(Vec::<String>::new());
    let _ = run_pipeline(
        PipelineRequest {
            project: project_with(
                vec![profile_op(1, 1, ToolOffset::Outside)],
                vec![endmill(1, 3.0)],
            ),
            post_processor: None,
        },
        |phase, _f, _m| phases.borrow_mut().push(phase.to_string()),
    )
    .unwrap();
    let phases = phases.into_inner();
    for expected in ["import", "objects", "gcode", "preview", "done"] {
        assert!(
            phases.contains(&expected.to_string()),
            "missing {expected} in {phases:?}"
        );
    }
}

/// Post profile (rt1.15): a custom `program_start` template
/// replaces the `LinuxCNC` `(generated by …)` header, with token
/// substitution honoring the active tool and unit.
#[test]
fn post_profile_overrides_program_start_and_end() {
    use crate::gcode::post_profile::PostProfile;
    let machine = MachineConfig {
        post_profile: Some(PostProfile {
            name: "Test".into(),
            program_start: Some("; wiac <version>\n; tool <t> <n>".into()),
            program_end: Some("; bye\nM30".into()),
            ..Default::default()
        }),
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![{
            let mut t = endmill(7, 3.0);
            t.name = "3mm endmill".into();
            t
        }],
        operations: vec![profile_op(1, 7, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // Header has the custom prologue (multi-line via \n in
    // template) + the version + tool number / name tokens
    // substituted.
    assert!(
        resp.gcode.contains("; wiac "),
        "expected custom version prologue:\n{}",
        resp.gcode
    );
    assert!(
        resp.gcode.contains("; tool 7 3mm endmill"),
        "expected tool token substitution:\n{}",
        resp.gcode
    );
    assert!(
        resp.gcode.contains("; bye"),
        "expected custom footer:\n{}",
        resp.gcode
    );
    // Default LinuxCNC header is NOT emitted when a profile is set.
    assert!(
        !resp.gcode.contains("(generated by wiaConstructor)"),
        "default header leaked through with profile set:\n{}",
        resp.gcode
    );
}

/// Post profile (hev): per-axis config flips Z scale, renames Y to
/// V, disables I/J emission, and re-formats F with two decimals.
/// The emitted gcode reflects every knob.
#[test]
fn post_profile_axes_config_drives_axis_emission() {
    use crate::gcode::post_profile::{AxesConfig, AxisFormat, PostProfile};
    let mut axes = AxesConfig::default();
    axes.z.scale = -1.0; // flip Z-up to Z-down
    axes.y.name = "V".into(); // rotary swap
    axes.i.enabled = false;
    axes.j.enabled = false;
    axes.feed = AxisFormat {
        enabled: true,
        name: "F".into(),
        format: "%.2f".into(),
        scale: 1.0,
    };
    let machine = MachineConfig {
        post_profile: Some(PostProfile {
            name: "Test axes".into(),
            axes: Some(axes),
            ..Default::default()
        }),
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // Z is scaled by -1: the depth dives below zero in source units
    // (typically Z-2 or similar), so the emitted Z must be POSITIVE.
    let z_lines: Vec<&str> = resp
        .gcode
        .lines()
        .filter(|l| l.contains('Z') && (l.starts_with("G0") || l.starts_with("G1")))
        .collect();
    assert!(
        !z_lines.is_empty(),
        "expected some Z moves:\n{}",
        resp.gcode
    );
    assert!(
        z_lines
            .iter()
            .any(|l| l.contains("Z2.") || l.contains("Z3.") || l.contains("Z5.")),
        "expected at least one positive Z move after scale=-1 flip:\n{}",
        z_lines.join("\n")
    );
    // Y has been renamed to V. Some Y move should now show up as V.
    assert!(
        resp.gcode.contains(" V"),
        "expected renamed Y→V axis:\n{}",
        resp.gcode
    );
    assert!(
        !resp
            .gcode
            .lines()
            .any(|l| { (l.starts_with("G0") || l.starts_with("G1")) && l.contains(" Y") }),
        "Y should no longer be emitted on G0/G1:\n{}",
        resp.gcode
    );
    // Profile op walks a closed square — no arcs => no I/J in the
    // baseline. But the F line should use two decimals now.
    assert!(
        resp.gcode
            .lines()
            .any(|l| l.starts_with('F') && l.contains('.')),
        "feed line should now carry decimals from %.2f:\n{}",
        resp.gcode
    );
}

/// Post profile (hev): disabling Z entirely drops every Z word
/// from G0 / G1 moves — useful for laser controllers that don't
/// have a Z axis.
#[test]
fn post_profile_disabled_axis_drops_the_word() {
    use crate::gcode::post_profile::{AxesConfig, PostProfile};
    let mut axes = AxesConfig::default();
    axes.z.enabled = false;
    let machine = MachineConfig {
        post_profile: Some(PostProfile {
            name: "No Z".into(),
            axes: Some(axes),
            ..Default::default()
        }),
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // No G0/G1 line should mention Z when the axis is disabled.
    let bad: Vec<&str> = resp
        .gcode
        .lines()
        .filter(|l| (l.starts_with("G0 ") || l.starts_with("G1 ")) && l.contains('Z'))
        .collect();
    assert!(
        bad.is_empty(),
        "G0/G1 lines still carry Z words after disabling Z:\n{}",
        bad.join("\n")
    );
}

/// Post profile (hev): unset `axes` means baseline behavior — the
/// `LinuxCNC` `(generated by …)` header is gone (we set a custom
/// `program_start`) but coordinate emission stays exactly the same.
#[test]
fn post_profile_without_axes_keeps_legacy_output() {
    use crate::gcode::post_profile::PostProfile;
    let machine_with = MachineConfig {
        post_profile: Some(PostProfile {
            name: "Test".into(),
            program_start: Some("; header".into()),
            axes: None,
            ..Default::default()
        }),
        ..MachineConfig::default()
    };
    let machine_without = MachineConfig::default();
    let project = |m: crate::cam::setup::MachineConfig| Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine: m,
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp_a = run_pipeline(
        PipelineRequest {
            project: project(machine_with),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let resp_b = run_pipeline(
        PipelineRequest {
            project: project(machine_without),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // Skip the first two header lines so the program_start text
    // doesn't drown out the comparison; everything after must
    // match between the axes=None profile run and the no-profile
    // run.
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.starts_with("; header"))
            .filter(|l| !l.starts_with("(generated by wiaConstructor)"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        strip(&resp_a.gcode),
        strip(&resp_b.gcode),
        "axes=None should be a bit-identical no-op vs. no profile",
    );
}

/// New `ToolKind` variants (rt1.28 / z5yw): `BullNose` / Compression
/// / `FormProfile` all serialize + deserialize cleanly and carry
/// their geometry fields through round-trip. (T-slot was folded into
/// `FormProfile` — its undercut is a `(z, r)` profile now.)
#[test]
fn extended_tool_kinds_serde_round_trip() {
    for (kind, label) in [
        (ToolKind::BullNose, "bull_nose"),
        (ToolKind::Compression, "compression"),
        (ToolKind::FormProfile, "form_profile"),
    ] {
        let mut t = endmill(7, 6.0);
        t.kind = kind;
        t.corner_radius_mm = Some(0.5);
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains(label), "expected '{label}' in {json}");
        let back: ToolEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, kind);
        assert_eq!(back.corner_radius_mm, Some(0.5));
    }
}

/// Plot-mode Z (rt1.35): with `plot_mode_z` enabled, every Z value
/// in the gcode is one of {`fast_move_z`, `cut_depth`}. No
/// intermediate Z values from a step-down schedule.
#[test]
fn plot_mode_emits_only_two_z_values() {
    let machine = MachineConfig {
        plot_mode_z: true,
        ..MachineConfig::default()
    };
    let mut params = OpParams::mill_default();
    params.depth = -3.0; // would normally cascade through Z=-1, -2, -3
    params.start_depth = 0.0;
    params.fast_move_z = 5.0;
    params.step = Some(-1.0);
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 3.0)],
        operations: vec![Op {
            id: 1,
            name: "Laser cut".into(),
            enabled: true,
            kind: OpKind::Engrave {
                contour: crate::project::ContourParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params,
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let z_values: std::collections::HashSet<String> = resp
        .gcode
        .lines()
        .flat_map(|l| {
            l.split_whitespace()
                .filter_map(|t| t.strip_prefix('Z'))
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect();
    // Expect Z values only at {5, -3} (plus possibly 0 for the
    // pre-plunge "drop to z=0" line — that's still in the
    // emit_offset prelude before multi_pass takes over).
    let allowed = ["5", "-3", "0"];
    for z in &z_values {
        assert!(
            allowed.contains(&z.as_str()),
            "unexpected Z value {z} in plot mode:\n{}",
            resp.gcode
        );
    }
    // And the intermediate descent values must NOT appear.
    assert!(
        !z_values.contains("-1"),
        "Z=-1 leaked through plot mode:\n{}",
        resp.gcode
    );
    assert!(
        !z_values.contains("-2"),
        "Z=-2 leaked through plot mode:\n{}",
        resp.gcode
    );
}

/// Approach point serde round-trip (rt1.26).
#[test]
fn approach_point_serde_round_trip() {
    let contour = crate::project::ContourParams {
        approach_point: Some((3.5, -2.0)),
        ..crate::project::ContourParams::default()
    };
    let json = serde_json::to_string(&contour).unwrap();
    assert!(json.contains("approach_point"));
    let back: crate::project::ContourParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back.approach_point, Some((3.5, -2.0)));
    // Unset round-trips as absent.
    let none_contour = crate::project::ContourParams::default();
    let json_none = serde_json::to_string(&none_contour).unwrap();
    assert!(!json_none.contains("approach_point"));
}

/// Laser pierce time (rt1.29): a laser tool with
/// `laser_pierce_sec` set emits a `G4 P<sec>` dwell between
/// rapid-to-entry and plunge.
#[test]
fn laser_op_emits_pierce_dwell_before_cut() {
    let mut tool = endmill(1, 0.1);
    tool.kind = ToolKind::LaserBeam;
    tool.laser_pierce_sec = Some(0.3);
    let machine = MachineConfig {
        mode: crate::cam::setup::MachineMode::Laser,
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![tool],
        operations: vec![Op {
            id: 1,
            name: "Laser cut".into(),
            enabled: true,
            kind: OpKind::Engrave {
                contour: crate::project::ContourParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
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
        resp.gcode.contains("G4 P0.3"),
        "expected pierce dwell G4 P0.3 before cut:\n{}",
        resp.gcode
    );
}

/// Non-laser tools never get the pierce dwell even if
/// `laser_pierce_sec` is somehow set (e.g. legacy projects).
#[test]
fn non_laser_tool_ignores_pierce_field() {
    let mut tool = endmill(1, 3.0);
    // Endmill kind, but pierce field set (shouldn't fire). Use a
    // distinctive value (0.7s) that won't collide with the
    // toolchange envelope's spindle stop/start dwells (default
    // 0.5s) — we're asserting the pierce dwell specifically
    // doesn't appear, not anything else.
    tool.laser_pierce_sec = Some(0.7);
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![tool],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
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
        !resp.gcode.contains("G4 P0.7"),
        "endmill should ignore laser_pierce_sec:\n{}",
        resp.gcode
    );
}

/// Per-tool Z shift (rt1.30): when set on the first op's tool, a
/// `G92 Z<shift>` line follows `program_begin` to pin work-Z=0 to
/// the new tool's tip.
#[test]
fn first_tool_z_shift_emits_g92_after_program_begin() {
    let mut tool = endmill(1, 3.0);
    tool.z_shift_mm = Some(-0.5);
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![tool],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
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
        resp.gcode.contains("G92 Z-0.5"),
        "expected G92 Z-0.5 for tool z_shift:\n{}",
        resp.gcode
    );
}

/// Zero / unset `z_shift` emits no G92 (rt1.30 fallback).
#[test]
fn no_z_shift_emits_no_g92() {
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
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
        !resp.gcode.contains("G92 Z"),
        "no G92 Z expected when z_shift_mm is unset:\n{}",
        resp.gcode
    );
}

/// Comma decimal separator (rt1.36) makes the `LinuxCNC` post emit
/// `X1,5` instead of `X1.5`. Activated via `MachineConfig`.
#[test]
fn comma_decimal_separator_emits_commas_in_numbers() {
    let machine = MachineConfig {
        decimal_separator: ',',
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.5, 0.5),
        machine,
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // At least one coordinate with a fractional part survives in
    // the gcode (e.g. `X-1,5` from offsetting the 20-mm box).
    assert!(
        resp.gcode
            .lines()
            .any(|l| l.contains(',') && (l.starts_with("G0") || l.starts_with("G1"))),
        "expected at least one comma-decimal in a coordinate line:\n{}",
        resp.gcode
    );
    // No '.' inside coordinate words (allowing '.' in '; OP' lines
    // is fine since post.raw bypasses the formatter).
    for l in resp.gcode.lines() {
        assert!(
            !((l.starts_with("G0 ") || l.starts_with("G1 ")) && l.contains('.')),
            "decimal '.' leaked into a coordinate line under comma-mode: {l}"
        );
    }
}

/// Line numbering (rt1.36): when `MachineConfig.line_number_start` is
/// Some(10), every emitted line gets `N10`, `N20`, … prefix.
#[test]
fn line_numbering_prefixes_every_line() {
    let machine = MachineConfig {
        line_number_start: Some(10),
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let lines: Vec<&str> = resp.gcode.lines().collect();
    // First non-empty line should have N10; subsequent N20, N30, ...
    let mut expected = 10u32;
    let mut found_count = 0;
    for l in &lines {
        if l.is_empty() {
            continue;
        }
        assert!(
            l.starts_with(&format!("N{expected} ")),
            "expected line to start with 'N{expected} ', got: {l}\nFull:\n{}",
            resp.gcode
        );
        expected += 10;
        found_count += 1;
    }
    assert!(found_count > 3, "expected several numbered lines");
}

/// No numbering by default (rt1.36 fallback): lines do not get an
/// N-prefix when `MachineConfig.line_number_start` is None.
#[test]
fn no_line_numbering_by_default() {
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // No line should start with N\d+\s.
    for l in resp.gcode.lines() {
        assert!(
            !(l.starts_with('N') && l.chars().nth(1).is_some_and(|c| c.is_ascii_digit())),
            "unexpected N-prefix: {l}"
        );
    }
}

/// Chamfer op (rt1.18): walks the source contour at the computed
/// final Z from the V-bit cone math. A 60° V-bit + 1mm width
/// gives ~1.732 mm depth; the final lap of gcode must contain
/// Z-1.732. With default DPP -1.0 (`mill_default`), the descent
/// also passes through an intermediate stepdown — see
/// `chamfer_deep_chamfer_uses_multi_pass_stepdown` (00ia).
#[test]
fn chamfer_op_emits_constant_z_pass_at_computed_depth() {
    let vbit = vbit();
    let project = Project {
        segments: closed_square_offset(50.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![vbit],
        operations: vec![Op {
            id: 1,
            name: "Chamfer".into(),
            enabled: true,
            kind: OpKind::Chamfer {
                width_mm: 1.0,
                finish_pass: false,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // e63q: cone depth with the vbit's 0.1mm tip flat is
    // (1 - 0.05) / tan(30°) ≈ 1.6454; the gcode rounds to 4
    // decimals so we look for Z-1.6454. Pre-e63q the formula
    // ignored the tip flat and the test expected -1.732, which
    // over-cut by 0.087 mm.
    assert!(
        resp.gcode.contains("Z-1.6454") || resp.gcode.contains("Z-1.645"),
        "expected chamfer depth Z-1.6454 in gcode (e63q tip-flat correction):\n{}",
        resp.gcode
    );
}

/// Regression for the user's "chamfer depth seems added to the
/// previous op" report: a chamfer op that runs AFTER a deep profile
/// op must still cut at its OWN computed cone-tip Z, independent of
/// the prior op's depth. (`synthesize_op_setup` builds a fresh Setup per
/// op, but this proves no cross-op depth bleed end-to-end.) Profile
/// cuts to -5; chamfer width 1mm with the 60deg/0.1mm-tip vbit must
/// land at -1.6454, NOT -6.6454 (= -5 + -1.6454) or any deeper value.
#[test]
fn chamfer_after_deep_profile_keeps_own_depth() {
    let bit = vbit(); // id 1, VBit 60deg, tip 0.1
    let mut em = endmill(2, 6.0); // id 2, endmill for the profile
    em.id = 2;
    let mut profile_params = OpParams::mill_default();
    profile_params.depth = -5.0;
    profile_params.start_depth = 0.0;
    let project = Project {
        segments: closed_square_offset(50.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![bit, em],
        operations: vec![
            Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 2,
                finish_tool_id: None,
                source: OpSource::All,
                params: profile_params,
                group: None,
            },
            Op {
                id: 2,
                name: "Chamfer".into(),
                enabled: true,
                kind: OpKind::Chamfer {
                    width_mm: 1.0,
                    finish_pass: false,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                group: None,
            },
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // Isolate the chamfer op's gcode block (after the "; OP 2" marker).
    let cham = resp
        .gcode
        .split("; OP 2")
        .nth(1)
        .expect("chamfer op section present");
    // Collect every Z value emitted in the chamfer block.
    let zs: Vec<f64> = cham
        .lines()
        .flat_map(|l| l.split_whitespace())
        .filter_map(|t| t.strip_prefix('Z'))
        .filter_map(|v| v.parse::<f64>().ok())
        .collect();
    let deepest = zs.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(
            (deepest - (-1.6454)).abs() < 0.01,
            "chamfer should bottom at its own cone-tip Z -1.6454, got {deepest} (prior profile was -5). Z values: {zs:?}\n{cham}"
        );
    // The chamfer's toolpath segments must carry op_id == 2 so the 3D
    // driver's per-segment tool resolver feeds the v-bit (not a
    // fallback to tools[0] = the endmill, which would carve the
    // chamfer cylindrically — the reported symptom).
    let cham_cuts: Vec<_> = resp
        .toolpath
        .iter()
        .filter(|s| s.op_id == 2 && matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
        .collect();
    assert!(
        !cham_cuts.is_empty(),
        "chamfer toolpath cut segments must carry op_id == 2"
    );
    let tp_deepest = cham_cuts
        .iter()
        .map(|s| s.from.z.min(s.to.z))
        .fold(f64::INFINITY, f64::min);
    assert!(
        (tp_deepest - (-1.6454)).abs() < 0.01,
        "chamfer op_id=2 toolpath segments should bottom at -1.6454, got {tp_deepest}",
    );
}

/// Chamfer with `finish_pass=true` emits the source path twice —
/// once at rough feed, once tagged `is_finish` so the finish-set
/// feed wins. Verified by counting how many times the contour's
/// starting move appears (= number of passes through the path).
#[test]
fn chamfer_finish_pass_emits_second_pass_at_finish_feed() {
    let mut vbit = vbit();
    vbit.feed_rate = 1200;
    vbit.feed_rate_finish = Some(400);
    let project = Project {
        segments: closed_square_offset(50.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![vbit],
        operations: vec![Op {
            id: 1,
            name: "Chamfer".into(),
            enabled: true,
            kind: OpKind::Chamfer {
                width_mm: 1.0,
                finish_pass: true,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    assert!(resp.gcode.contains("F1200"), "rough feed missing");
    assert!(resp.gcode.contains("F400"), "finish feed missing");
}

/// Chamfer on a non-V-bit tool emits a warning so the user knows
/// the cone math is approximate.
#[test]
fn chamfer_with_non_vbit_warns() {
    let project = Project {
        segments: closed_square_offset(50.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![Op {
            id: 1,
            name: "Chamfer".into(),
            enabled: true,
            kind: OpKind::Chamfer {
                width_mm: 1.0,
                finish_pass: false,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .unwrap();
    assert!(resp.warnings.iter().any(|w| w.kind == "chamfer_non_vbit"));
}

/// 00ia: a chamfer whose final Z magnitude exceeds the per-pass
/// stepdown (DPP) must descend in multiple passes — the cutter
/// walks the source contour at each stepdown Z, deepening on each
/// lap. The pre-fix behavior forced a single full-depth plunge and
/// snapped V-bits. 60° V-bit + 2.5 mm width → Z ≈ -4.33 mm; with
/// default DPP -1.0 we expect intermediate Z-1, Z-2, Z-3 passes
/// plus a final Z-4.33 lap.
#[test]
fn chamfer_deep_chamfer_uses_multi_pass_stepdown() {
    let vbit = vbit();
    let project = Project {
        segments: closed_square_offset(50.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![vbit],
        operations: vec![Op {
            id: 1,
            name: "Chamfer".into(),
            enabled: true,
            kind: OpKind::Chamfer {
                width_mm: 2.5,
                finish_pass: false,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // e63q: final Z = -(2.5 - 0.05) / tan(30°) ≈ -4.2435 (the
    // vbit's 0.1mm tip flat subtracts tip_radius=0.05 before the
    // tan-division). Pre-e63q the formula gave -4.3301 — that
    // over-cut a 60° V-bit chamfer by 0.05/tan(30°) ≈ 0.087 mm.
    assert!(
        resp.gcode.contains("Z-4.2435") || resp.gcode.contains("Z-4.243"),
        "expected final chamfer depth Z-4.2435 in gcode (e63q tip-flat correction):\n{}",
        resp.gcode
    );
    // With DPP = -1.0 the schedule should include intermediate
    // stepdowns at Z-1, Z-2, Z-3 before the final Z-4.24 lap.
    // Counting the distinct intermediate Zs guards against a
    // regression to single-pass.
    for z in ["Z-1\n", "Z-2\n", "Z-3\n"] {
        assert!(
            resp.gcode.contains(z),
            "expected intermediate stepdown {z} in gcode:\n{}",
            resp.gcode
        );
    }
}

/// u1yq: when the user sets per-pass step LARGER (in magnitude) than the
/// chamfer cone-tip Z, the schedule must collapse to a single pass at the
/// chamfer Z — NEVER drive the cutter past it. Reported by a user who
/// picked a 60° vbit with width 1mm (depth ≈ -1.6454) and step = -3.0;
/// the cutter went deeper than the cone math.
#[test]
fn chamfer_step_larger_than_depth_clamps_to_chamfer_z() {
    let vbit = vbit();
    let mut params = OpParams::mill_default();
    // The bug-trigger: |step| > |chamfer_depth|.
    params.step = Some(-3.0);
    let project = Project {
        segments: closed_square_offset(50.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![vbit],
        operations: vec![Op {
            id: 1,
            name: "Chamfer".into(),
            enabled: true,
            kind: OpKind::Chamfer {
                width_mm: 1.0,
                finish_pass: false,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params,
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // Final Z must land at the cone math depth, not the user-set step.
    assert!(
        resp.gcode.contains("Z-1.6454") || resp.gcode.contains("Z-1.645"),
        "expected final chamfer depth Z-1.6454 in gcode:\n{}",
        resp.gcode
    );
    // No Z value should exceed (be deeper than) the chamfer depth.
    // Walk every Z<value> token; -1.65 and below would be the bug.
    for line in resp.gcode.lines() {
        if let Some(idx) = line.find("Z-") {
            let tail = &line[idx + 2..];
            let end = tail
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(tail.len());
            let z: f64 = tail[..end].parse().unwrap_or(0.0);
            assert!(
                z <= 1.6454 + 1e-3,
                "Z-{z} in `{line}` exceeds chamfer cone depth 1.6454 — overshoot bug:\n{}",
                resp.gcode
            );
        }
    }
}

/// uo1t: a chamfer width that exceeds the V-bit's cone span gets
/// clamped to (diameter - `tip_diameter`) / 2 so the shank never
/// engages stock. A 6.35 mm V-bit with 0.1 mm tip has cap 3.125
/// mm; requesting 10 mm should warn and emit Z computed from the
/// 3.125 mm clamp (3.125 / tan(30°) ≈ -5.413), not the raw
/// (catastrophic) -17.32 mm.
#[test]
fn chamfer_oversize_width_clamped_to_tool_reach() {
    let vbit = vbit();
    let project = Project {
        segments: closed_square_offset(50.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![vbit],
        operations: vec![Op {
            id: 1,
            name: "Chamfer".into(),
            enabled: true,
            kind: OpKind::Chamfer {
                width_mm: 10.0,
                finish_pass: false,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
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
        resp.warnings
            .iter()
            .any(|w| w.kind == "chamfer_width_clamped_to_reach"),
        "expected chamfer_width_clamped_to_reach warning, got: {:?}",
        resp.warnings.iter().map(|w| &w.kind).collect::<Vec<_>>()
    );
    // e63q: clamped final Z = -(3.125 - 0.05) / tan(30°) ≈ -5.326
    // (the vbit's 0.1mm tip flat subtracts tip_radius=0.05). Pre-
    // e63q the value was -5.4126 (raw 3.125 / tan(30°)).
    assert!(
        resp.gcode.contains("Z-5.326") || resp.gcode.contains("Z-5.3261"),
        "expected clamped final depth Z-5.326 in gcode (e63q tip-flat correction):\n{}",
        resp.gcode
    );
    // The unclamped depth (10 / tan(30°) ≈ -17.32) must NOT
    // appear — that's the catastrophic value uo1t was about.
    assert!(
        !resp.gcode.contains("Z-17."),
        "unclamped catastrophic depth leaked into gcode:\n{}",
        resp.gcode
    );
}

/// `Op.finish_tool_id` round-trips through serde and is
/// omitted from the wire payload when None.
#[test]
fn operation_finish_tool_id_serde_round_trip() {
    let mut op = pocket_op(1, 5, OpSource::All);
    op.finish_tool_id = Some(9);
    let json = serde_json::to_string(&op).unwrap();
    assert!(json.contains("finish_tool_id"));
    let back: Op = serde_json::from_str(&json).unwrap();
    assert_eq!(back.finish_tool_id, Some(9));

    let none_op = pocket_op(1, 5, OpSource::All);
    let json_none = serde_json::to_string(&none_op).unwrap();
    assert!(!json_none.contains("finish_tool_id"));
}

/// `PocketParams.finish_xy_allowance_mm` round-trips through
/// serde and omits the field when unset (rt1.24).
#[test]
fn finish_xy_allowance_serde_round_trip() {
    let pocket = crate::project::PocketParams {
        finish_xy_allowance_mm: Some(0.3),
        ..crate::project::PocketParams::default()
    };
    let json = serde_json::to_string(&pocket).unwrap();
    assert!(json.contains("finish_xy_allowance_mm"));
    let back: crate::project::PocketParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back.finish_xy_allowance_mm, Some(0.3));
    let none_pocket = crate::project::PocketParams::default();
    let json_none = serde_json::to_string(&none_pocket).unwrap();
    assert!(!json_none.contains("finish_xy_allowance_mm"));
}

/// Tool round-trips through serde with the new finish/drill fields
/// (rt1.27). Empty overrides serialize as omitted entries.
#[test]
fn tool_entry_serde_round_trip_with_finish_and_drill_overrides() {
    let mut t = endmill(1, 3.0);
    t.speed_finish = Some(12_000);
    t.feed_rate_finish = Some(400);
    t.plunge_rate_drill = Some(50);
    t.default_peck_step_mm = Some(1.5);
    let json = serde_json::to_string(&t).unwrap();
    let back: ToolEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.speed_finish, Some(12_000));
    assert_eq!(back.feed_rate_finish, Some(400));
    assert_eq!(back.plunge_rate_drill, Some(50));
    assert_eq!(back.default_peck_step_mm, Some(1.5));
    // Unset finish/drill overrides round-trip as None and don't
    // appear in the serialized form.
    assert!(back.speed_drill.is_none());
    assert!(!json.contains("speed_drill"));
}

// ─── Lead-in / lead-out (p31) ──────────────────────────────────────

#[test]
fn unknown_post_processor_is_a_deserialization_failure() {
    let raw = serde_json::json!({
        "project": {
            "segments": [],
            "machine": { "unit": "mm", "mode": "mill", "comments": true,
                         "arcs": true, "supports_toolchange": false },
            "tools": [],
            "operations": []
        },
        "post_processor": "robotic_arm"
    });
    let res: Result<PipelineRequest, _> = serde_json::from_value(raw);
    assert!(res.is_err());
}

#[test]
fn generate_streaming_emits_op_events_in_order() {
    let project = Project {
        segments: closed_square(20.0),
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 1, ToolOffset::Inside),
            profile_op(3, 1, ToolOffset::On),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let cancel = CancelToken::new();
    let mut events: Vec<PipelineEvent> = Vec::new();
    let resp = generate_streaming(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        &cancel,
        &mut |e| events.push(e),
    )
    .expect("streaming pipeline ran");

    let mut started: Vec<u32> = Vec::new();
    let mut completed: Vec<u32> = Vec::new();
    let mut done_count = 0usize;
    for ev in &events {
        match ev {
            PipelineEvent::OpStarted { op_id, .. } => started.push(*op_id),
            PipelineEvent::OpCompleted { op_id, .. } => completed.push(*op_id),
            PipelineEvent::Done { .. } => done_count += 1,
            PipelineEvent::Cancelled => panic!("unexpected Cancelled event"),
            PipelineEvent::OpProgress { .. } => {}
        }
    }
    assert_eq!(
        started,
        vec![1, 2, 3],
        "OpStarted fires once per op in order"
    );
    assert_eq!(
        completed,
        vec![1, 2, 3],
        "OpCompleted fires once per op in order"
    );
    assert_eq!(done_count, 1, "exactly one Done event at the end");
    assert!(!resp.gcode.is_empty());
}

#[test]
fn generate_streaming_done_event_carries_aggregated_stats() {
    let project = Project {
        segments: closed_square(20.0),
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let cancel = CancelToken::new();
    let mut last: Option<PipelineEvent> = None;
    let resp = generate_streaming(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        &cancel,
        &mut |e| last = Some(e),
    )
    .expect("streaming pipeline ran");
    match last {
        Some(PipelineEvent::Done {
            total_time_s,
            op_count,
        }) => {
            assert!((total_time_s - resp.time_estimate.total_s).abs() < 1e-9);
            assert_eq!(op_count, resp.stats.offset_count);
        }
        other => panic!("expected Done event last, got {other:?}"),
    }
}

#[test]
fn generate_streaming_cancellation() {
    // V-Carve a triangle on a background thread; from the main
    // thread set the cancel flag immediately. We expect the
    // streaming run to bail with Err(Cancelled) and emit a
    // Cancelled event within ≤200 ms.
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    let project = Project {
        segments: vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(20.0, 0.0), "0", 7),
            Segment::line(
                Point2::new(20.0, 0.0),
                Point2::new(10.0, 17.320_508),
                "0",
                7,
            ),
            Segment::line(Point2::new(10.0, 17.320_508), Point2::new(0.0, 0.0), "0", 7),
        ],
        machine: MachineConfig::default(),
        tools: vec![vbit()],
        operations: vec![Op {
            id: 9,
            name: "Carve".into(),
            enabled: true,
            kind: OpKind::VCarve {
                carve: crate::project::VCarveParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams {
                depth: -10.0,
                start_depth: 0.0,
                step: Some(-1.0),
                fast_move_z: 5.0,
                ..OpParams::default()
            },
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let cancel = CancelToken::new();
    let cancel_clone = cancel.clone();
    let events: Arc<Mutex<Vec<PipelineEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let request = PipelineRequest {
        project,
        post_processor: Some(PostProcessorKind::Linuxcnc),
    };
    cancel_clone.cancel();
    let start = Instant::now();
    let result = std::thread::spawn(move || {
        generate_streaming(request, &cancel_clone, &mut |e| {
            events_clone.lock().unwrap().push(e);
        })
    })
    .join()
    .expect("worker thread panicked");
    let elapsed = start.elapsed();
    assert!(
        matches!(result, Err(PipelineError::Cancelled)),
        "expected Err(Cancelled), got {result:?}"
    );
    assert!(
        elapsed < Duration::from_millis(200),
        "cancellation took too long: {elapsed:?}"
    );
    let evs = events.lock().unwrap();
    assert!(
        evs.iter().any(|e| matches!(e, PipelineEvent::Cancelled)),
        "expected a Cancelled event in stream, got {evs:?}"
    );
    assert!(
        !evs.iter().any(|e| matches!(e, PipelineEvent::Done { .. })),
        "should not emit Done after Cancelled",
    );
}

fn collect_cached_flags(project: Project) -> Vec<(u32, bool)> {
    let cancel = CancelToken::new();
    let mut flags: Vec<(u32, bool)> = Vec::new();
    let _ = generate_streaming(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        &cancel,
        &mut |e| {
            if let PipelineEvent::OpCompleted { op_id, cached } = e {
                flags.push((op_id, cached));
            }
        },
    )
    .expect("pipeline ran");
    flags
}

/// Generating twice with no edits should serve every op from cache
/// on the second run.
#[test]
fn regenerate_with_no_edits_hits_cache() {
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![endmill(91, 3.0)],
        operations: vec![Op {
            id: 91,
            name: "Profile cache test".into(),
            enabled: true,
            kind: OpKind::Profile {
                offset: ToolOffset::Outside,
                contour: crate::project::ContourParams::default(),
                profile: crate::project::ProfileParams::default(),
            },
            tool_id: 91,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    clear_pipeline_cache();
    let first = collect_cached_flags(project.clone());
    assert_eq!(first, vec![(91, false)], "first run misses cache");
    let second = collect_cached_flags(project);
    assert_eq!(second, vec![(91, true)], "second run hits cache");
}

/// Editing one op of many should miss only that op; the others
/// should still hit the cache.
#[test]
fn edit_one_op_misses_only_that() {
    // Five profile ops, distinct tool ids so each gets its own
    // cache slot regardless of segments (they all share the same
    // square geometry).
    let tools: Vec<ToolEntry> = (1..=5).map(|i| endmill(100 + i, 3.0)).collect();
    let ops: Vec<Op> = (1..=5)
        .map(|i| Op {
            id: 100 + i,
            name: format!("Profile {i}"),
            enabled: true,
            kind: OpKind::Profile {
                offset: ToolOffset::Outside,
                contour: crate::project::ContourParams::default(),
                profile: crate::project::ProfileParams::default(),
            },
            tool_id: 100 + i,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        })
        .collect();
    let mut project = Project {
        segments: closed_square_offset(30.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools,
        operations: ops,
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    clear_pipeline_cache();
    let first = collect_cached_flags(project.clone());
    assert!(
        first.iter().all(|(_, c)| !c),
        "first run should miss every op: {first:?}"
    );
    // Edit op 3's depth — only it should miss on the second run.
    project.operations[2].params.depth -= 0.1;
    let second = collect_cached_flags(project);
    let edited_id = 100 + 3;
    let expected: Vec<(u32, bool)> = (1..=5)
        .map(|i| (100 + i as u32, (100 + i) != edited_id))
        .collect();
    assert_eq!(second, expected, "only op {edited_id} should miss");
}

/// Cache hit must reproduce the same gcode + toolpath as a fresh
/// run. Asserted by clearing the cache, running once, then running
/// again with the cache primed.
#[test]
fn cache_hit_produces_identical_response() {
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![endmill(77, 3.0)],
        operations: vec![Op {
            id: 77,
            name: "Profile identity".into(),
            enabled: true,
            kind: OpKind::Profile {
                offset: ToolOffset::Outside,
                contour: crate::project::ContourParams::default(),
                profile: crate::project::ProfileParams::default(),
            },
            tool_id: 77,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    clear_pipeline_cache();
    let req = || PipelineRequest {
        project: project.clone(),
        post_processor: Some(PostProcessorKind::Linuxcnc),
    };
    let r1 = run_pipeline(req(), |_, _, _| {}).expect("first run");
    let r2 = run_pipeline(req(), |_, _, _| {}).expect("cached run");
    assert_eq!(r1.gcode, r2.gcode, "gcode must match across cache hit");
    assert_eq!(
        r1.toolpath.len(),
        r2.toolpath.len(),
        "toolpath segment count must match"
    );
    assert_eq!(r1.stats.offset_count, r2.stats.offset_count);
    assert_eq!(r1.stats.closed_object_count, r2.stats.closed_object_count);
}

#[test]
fn missing_tool_returns_structured_error() {
    let project = project_with(
        vec![profile_op(1, 99, ToolOffset::Outside)],
        vec![endmill(7, 3.0)],
    );
    let err = run_pipeline(
        PipelineRequest {
            project: project.clone(),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .expect_err("missing tool should fail");
    let structured = err
        .to_structured(Some(&project))
        .expect("UnknownTool should lift to a structured Error");
    assert_eq!(structured.kind, crate::errors::ErrorKind::Misconfigured);
    match structured.auto_fix {
        Some(crate::errors::AutoFix::AssignTool {
            op_id,
            suggested_tool_id,
        }) => {
            assert_eq!(op_id, 1);
            assert_eq!(suggested_tool_id, 7);
        }
        other => panic!("expected AssignTool auto_fix, got {other:?}"),
    }
    assert!(structured.recovery_hint.is_some());
}

#[test]
fn unsupported_op_kind_returns_structured_error() {
    let mut op = profile_op(1, 1, ToolOffset::Outside);
    op.kind = OpKind::Helix;
    let project = project_with(vec![op], vec![endmill(1, 3.0)]);
    let err = run_pipeline(
        PipelineRequest {
            project: project.clone(),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .expect_err("Thread op should fail");
    let structured = err
        .to_structured(Some(&project))
        .expect("UnimplementedKind should lift to a structured Error");
    assert_eq!(structured.kind, crate::errors::ErrorKind::Unsupported);
}

#[test]
fn cancelled_lifts_to_none() {
    let err = PipelineError::Cancelled;
    assert!(err.to_structured(None).is_none());
}

/// rt1.34 + yc2a: a Pause op emits M5 → M0 inline at its slot in
/// the op list. The cutter doesn't move and no source geometry is
/// touched. The comment carries the operator message. Post-pause
/// spindle restart is deferred to the next op's `spindle_on`
/// (via a `post.reset_state()`) so a CCW-tool program doesn't
/// get locked into M3 by a hardcoded raw line.
#[test]
fn pipeline_emits_m0_for_pause_op() {
    let pause = Op {
        id: 2,
        name: "Tool change".into(),
        enabled: true,
        kind: OpKind::Pause {
            message: "Swap to 1/8 endmill".into(),
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    // Real op in front of the Pause so the pipeline header machinery
    // resolves correctly (it picks the first enabled op's tool for
    // z_shift / etc.).
    let profile = Op {
        id: 1,
        name: "Profile".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile, pause],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    let gcode = &resp.gcode;
    // M0 line is present and ordered AFTER the profile cut, BEFORE
    // program end. Comment with the user's message is adjacent.
    assert!(gcode.contains("\nM0\n"), "expected M0 line; got:\n{gcode}");
    assert!(
        gcode.contains("Swap to 1/8 endmill"),
        "expected message comment; got:\n{gcode}",
    );
    // M5 (spindle off) precedes M0. yc2a: we DON'T assert M3 after
    // M0 — the post-pause spindle restart is driven by the next
    // op's tool.spindle_direction via the lazy `spindle_on`
    // dispatcher, not a hard-coded raw "M3". The Pause itself
    // emits only the bare M5 / message / M0 sequence and resets
    // the post's delta-encoder so the NEXT op's spindle command
    // re-emits.
    let m0_pos = gcode.find("\nM0\n").unwrap();
    let pre = &gcode[..m0_pos];
    let post_slice = &gcode[m0_pos..];
    assert!(pre.rfind("\nM5\n").is_some(), "expected M5 before M0");
    // The Pause op itself must NOT inject a bare M3 — only program
    // end (M5) or a subsequent cut op's M3/M4 may appear after M0.
    // Pause is the last op here, so the only post-M0 spindle line
    // is the program-end M5.
    let post_pause_lines: Vec<&str> = post_slice.lines().collect();
    let stray_m3 = post_pause_lines
        .iter()
        .find(|l| l.trim() == "M3" || l.trim_start().starts_with("M3 "));
    assert!(
        stray_m3.is_none(),
        "yc2a: Pause op must not emit a bare M3 after M0; got:\n{gcode}",
    );
}

/// rt1.34: Pause carries no tool reference, so the missing-tool
/// validation that would normally fail a 0-id lookup must skip it.
#[test]
fn pause_op_skips_tool_validation() {
    let pause = Op {
        id: 2,
        name: "Stop".into(),
        enabled: true,
        kind: OpKind::Pause {
            message: String::new(),
        },
        tool_id: 999, // would otherwise UnknownTool
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let project = crate::project::Project {
        segments: Vec::new(),
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![pause],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    );
    assert!(
        resp.is_ok(),
        "Pause op should not require a valid tool; got {resp:?}",
    );
}

/// rt1.9: `PocketStrategy::Zigzag` wire compatibility — `"zigzag"`
/// string still loads (legacy form, `angle_deg` defaults to 0), AND
/// a non-zero angle serialises as the tagged-object form.
#[test]
fn zigzag_strategy_legacy_string_round_trip() {
    // Legacy form: bare string deserialises with angle 0.
    let s: crate::project::PocketStrategy =
        serde_json::from_str("\"zigzag\"").expect("deserialize");
    match s {
        crate::project::PocketStrategy::Zigzag { angle_deg } => assert_eq!(angle_deg, 0.0),
        other => panic!("expected Zigzag, got {other:?}"),
    }
    // Re-serialise: angle 0 → bare string (compact).
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(json, "\"zigzag\"");
}

#[test]
fn zigzag_strategy_angled_round_trip() {
    let s = crate::project::PocketStrategy::Zigzag { angle_deg: 45.0 };
    let json = serde_json::to_string(&s).unwrap();
    assert!(
        json.contains("\"angle_deg\":45"),
        "expected angle_deg in tagged form; got {json}",
    );
    assert!(
        json.contains("\"kind\":\"zigzag\""),
        "expected kind:zigzag in tagged form; got {json}",
    );
    let back: crate::project::PocketStrategy = serde_json::from_str(&json).unwrap();
    match back {
        crate::project::PocketStrategy::Zigzag { angle_deg } => {
            assert!((angle_deg - 45.0).abs() < 1e-9);
        }
        other => panic!("expected Zigzag, got {other:?}"),
    }
}

/// 8n4k: Homing op emits a comment + `G28` and (by default) a rapid
/// retract to the op's safe Z. The cutter doesn't move along XY and
/// the program proceeds with the next op afterwards. The op carries
/// no tool / source — `tool_id = 0` is fine.
#[test]
fn pipeline_emits_g28_for_homing_op() {
    let homing = Op {
        id: 1,
        name: "Home".into(),
        enabled: true,
        kind: OpKind::Homing {
            retract_to_safe_z: true,
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: 0.0,
            start_depth: 0.0,
            step: None,
            fast_move_z: 7.5,
            ..OpParams::default()
        },
        group: None,
    };
    let profile = Op {
        id: 2,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![homing, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    let gcode = &resp.gcode;
    assert!(gcode.contains("\nG28\n"), "expected G28 line in:\n{gcode}");
    // Safe-Z retract should land at op.params.fast_move_z = 7.5.
    assert!(
        gcode.contains("G0 Z7.5") || gcode.contains("G0Z7.5"),
        "expected post-G28 retract to Z=7.5 in:\n{gcode}"
    );
    // Comment marker is present.
    assert!(
        gcode.contains("(homing)") || gcode.contains("; OP 1 (homing)"),
        "expected homing marker comment in:\n{gcode}"
    );
}

/// 8n4k: Homing with `retract_to_safe_z = false` emits ONLY `G28` —
/// no follow-up G0 Z line.
#[test]
fn pipeline_homing_without_retract_skips_safe_z_move() {
    let homing = Op {
        id: 1,
        name: "Home".into(),
        enabled: true,
        kind: OpKind::Homing {
            retract_to_safe_z: false,
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let profile = Op {
        id: 2,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![homing, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let mut project = project;
    project.machine.supports_toolchange = true;
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    let gcode = &resp.gcode;
    // Scope: just the homing op's own lines, cut off at the next op's
    // boundary toolchange envelope (`(toolchange: …)`) or `; OP 2`.
    // The boundary envelope emits its own safe-Z lift; not ours.
    let after_homing = gcode
        .split_once("; OP 1 (homing)")
        .expect("homing comment")
        .1;
    let block_end = after_homing
        .find("(toolchange:")
        .or_else(|| after_homing.find("; OP 2"))
        .unwrap_or(after_homing.len());
    let homing_block = &after_homing[..block_end];
    assert!(homing_block.contains("G28"), "G28 missing: {homing_block}");
    // Without retract_to_safe_z the only motion line should be G28.
    for line in homing_block.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with(';') || t == "G28" {
            continue;
        }
        panic!("unexpected line in homing-without-retract block: {line:?}");
    }
}

/// 8n4k: Probe op emits `G38.2 <axis><distance> F<feed>`.
#[test]
fn pipeline_emits_g38_2_for_probe_op() {
    let probe = Op {
        id: 1,
        name: "Probe Z".into(),
        enabled: true,
        kind: OpKind::Probe {
            axis: crate::project::ProbeAxis::Z,
            distance_mm: -15.0,
            feed_mm_min: 100,
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let profile = Op {
        id: 2,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![probe, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    let gcode = &resp.gcode;
    assert!(
        gcode.contains("G38.2 Z-15.0000 F100"),
        "expected G38.2 Z-15 F100 line in:\n{gcode}"
    );
    assert!(
        gcode.contains("(probe)"),
        "expected probe marker comment in:\n{gcode}"
    );
}

/// 8n4k: `CycleMarker` emits ONLY a wrapped comment line. No G-code
/// motion or modal change.
#[test]
fn pipeline_emits_comment_only_for_cycle_marker_op() {
    let marker = Op {
        id: 1,
        name: "Step 1 complete".into(),
        enabled: true,
        kind: OpKind::CycleMarker {
            label: "FINISHED ROUGHING — INSPECT NOW".into(),
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let profile = Op {
        id: 2,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![marker, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let mut project = project;
    project.machine.supports_toolchange = true;
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    let gcode = &resp.gcode;
    assert!(
        gcode.contains("; --- FINISHED ROUGHING — INSPECT NOW ---"),
        "expected wrapped marker line in:\n{gcode}"
    );
    // The marker op's own lines end at the next op's boundary
    // toolchange envelope or the next op's `; OP 2` marker. Inside
    // that scope nothing but comments may appear.
    let after_marker = gcode
        .split_once("; OP 1 (cycle marker)")
        .expect("marker comment")
        .1;
    let block_end = after_marker
        .find("(toolchange:")
        .or_else(|| after_marker.find("; OP 2"))
        .unwrap_or(after_marker.len());
    let marker_block = &after_marker[..block_end];
    for line in marker_block.lines() {
        let t = line.trim_start();
        assert!(
            t.starts_with(';') || t.is_empty(),
            "unexpected non-comment in cycle-marker block: {line:?}"
        );
    }
}

/// 8n4k: round-trip Homing / Probe / `CycleMarker` through serde JSON
/// at the `snake_case` `type` discriminator.
#[test]
fn building_block_ops_round_trip_through_serde() {
    let cases: Vec<(&str, OpKind)> = vec![
        (
            "\"homing\"",
            OpKind::Homing {
                retract_to_safe_z: true,
            },
        ),
        (
            "\"probe\"",
            OpKind::Probe {
                axis: crate::project::ProbeAxis::Z,
                distance_mm: -10.0,
                feed_mm_min: 80,
            },
        ),
        (
            "\"cycle_marker\"",
            OpKind::CycleMarker {
                label: "Halfway point".into(),
            },
        ),
    ];
    for (tag, kind) in cases {
        let json = serde_json::to_string(&kind).expect("serialize");
        assert!(json.contains(tag), "expected discriminator {tag} in {json}");
        let back: OpKind = serde_json::from_str(&json).expect("deserialize");
        // Round-trip equality check via re-serialize so we don't have
        // to derive PartialEq on OpKind (it's not).
        let back_json = serde_json::to_string(&back).unwrap();
        assert_eq!(json, back_json, "round-trip mismatch for {tag}");
    }
}

/// rxm9: `GcodeInclude` splices the op's `content` into the program
/// stream, substituting `{x}` / `{y}` / `{z}` / `{f}` / `{s}` /
/// `{safe_z}` against the post's live state. Run a Profile op first
/// so the post's `last_x` / `last_y` / `last_z` are non-None when
/// the include block executes.
#[test]
fn pipeline_emits_gcode_include_with_variable_expansion() {
    let profile = Op {
        id: 1,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let include = Op {
        id: 2,
        name: "Return home".into(),
        enabled: true,
        kind: OpKind::GcodeInclude {
            path: "/tmp/return_home.nc".into(),
            content: "G0 X{x} Y{y}\nG0 Z{safe_z}\nG0 X0 Y0\n".into(),
            verbose_unsim_warnings: false,
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            fast_move_z: 12.5,
            ..OpParams::mill_default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(8.0, 6.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile, include],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    let gcode = &resp.gcode;
    // Path is surfaced in the OP comment.
    assert!(
        gcode.contains("; OP 2 (gcode include: /tmp/return_home.nc)"),
        "expected include header line with path; got:\n{gcode}"
    );
    // Substituted X / Y come from the Profile op's last move. The
    // Profile-Outside with a 3 mm-Ø tool offsets the centerline by
    // r=1.5 mm from the source segment (0,0)→(8,6); the cut end lands
    // at the offset-segment's end (8.9, 4.8). That value is what the
    // post tracked in last_x / last_y at the boundary, and what the
    // include block sees.
    let line_with_xy = gcode
        .lines()
        .skip_while(|l| !l.contains("; OP 2 (gcode include"))
        .find(|l| l.starts_with("G0 X") && l.contains(" Y"))
        .expect("first XY line of include block present");
    assert!(
        line_with_xy.starts_with("G0 X8.9000 Y4.8000"),
        "expected G0 X8.9 Y4.8 from {{x}}/{{y}} (Profile-Outside end position), got `{line_with_xy}` in:\n{gcode}"
    );
    // `{safe_z}` resolves from the INCLUDE op's params.fast_move_z (12.5), not the previous op's.
    assert!(
        gcode.contains("G0 Z12.5000"),
        "expected expanded `G0 Z12.5` from {{safe_z}}; got:\n{gcode}"
    );
    // Verbatim line passes through unchanged.
    assert!(
        gcode.contains("G0 X0 Y0"),
        "expected verbatim `G0 X0 Y0`; got:\n{gcode}"
    );
    // yhen: the legacy blanket `gcode_include_not_simulated` warning
    // is gone. This body is 100 % G0, which the unified preview
    // interpreter already tessellates into ToolpathSegments — the
    // sim DOES model the included block. So no `_skipped` or
    // `_not_simulated` warning should fire for op 2.
    assert!(
        !resp.warnings
            .iter()
            .any(|w| w.kind == "gcode_include_not_simulated" && w.op_id == Some(2)),
        "yhen: legacy gcode_include_not_simulated warning must be gone; got {:?}",
        resp.warnings,
    );
    assert!(
        !resp.warnings
            .iter()
            .any(|w| w.kind == "gcode_include_lines_skipped" && w.op_id == Some(2)),
        "yhen: 100% G0 body should not produce a skipped-lines warning; got {:?}",
        resp.warnings,
    );
}

/// rxm9: unknown `{tokens}` in an include block pass through as
/// literal text AND surface a `gcode_include_unknown_variable`
/// warning per distinct name. The user spots typos without the
/// program shipping a half-substituted line.
#[test]
fn gcode_include_unknown_variable_warns_and_passes_through() {
    let include = Op {
        id: 1,
        name: "Bad macro".into(),
        enabled: true,
        kind: OpKind::GcodeInclude {
            path: String::new(),
            content: "G0 X{xx} Y{nope}\n".into(),
            verbose_unsim_warnings: false,
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let profile = Op {
        id: 2,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![include, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    let gcode = &resp.gcode;
    assert!(
        gcode.contains("G0 X{xx} Y{nope}"),
        "expected verbatim unknown-token line; got:\n{gcode}"
    );
    let unknown_warnings: Vec<&crate::pipeline::PipelineWarning> = resp
        .warnings
        .iter()
        .filter(|w| w.kind == "gcode_include_unknown_variable")
        .collect();
    assert_eq!(
        unknown_warnings.len(),
        2,
        "expected 2 unknown-variable warnings (one each for xx and nope); got {unknown_warnings:?}"
    );
}

/// rxm9: empty `content` emits a `gcode_include_empty` warning so
/// the user notices a forgotten file-pick instead of shipping a
/// silently no-op slot.
#[test]
fn gcode_include_empty_content_warns() {
    let include = Op {
        id: 1,
        name: "Forgot to pick a file".into(),
        enabled: true,
        kind: OpKind::GcodeInclude {
            path: String::new(),
            content: String::new(),
            verbose_unsim_warnings: false,
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let profile = Op {
        id: 2,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![include, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    assert!(
        resp.warnings
            .iter()
            .any(|w| w.kind == "gcode_include_empty"),
        "expected gcode_include_empty warning; got {:?}",
        resp.warnings,
    );
}

/// yhen: a body that mixes simulatable lines (G0/G1) with an
/// unsupported G-code (here G33 thread cutting) fires a counted
/// `gcode_include_lines_skipped` summary warning. The summary names
/// the FIRST skipped line so the user has a concrete starting point.
#[test]
fn gcode_include_mixed_body_emits_counted_skipped_summary() {
    let include = Op {
        id: 1,
        name: "Thread cycle".into(),
        enabled: true,
        kind: OpKind::GcodeInclude {
            path: "/tmp/thread.nc".into(),
            // 5 lines: 1 G0 (simulated), 1 comment (no-op),
            // 1 G33 (UNSIMULATED), 1 G1 (simulated), 1 M5 (no-op).
            content: "G0 X10 Y0\n; bore to size\nG33 X10 Z-5 P1.5\nG1 Z2\nM5\n"
                .into(),
            verbose_unsim_warnings: false,
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let profile = Op {
        id: 2,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![include, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    let skipped: Vec<&crate::pipeline::PipelineWarning> = resp
        .warnings
        .iter()
        .filter(|w| w.kind == "gcode_include_lines_skipped" && w.op_id == Some(1))
        .collect();
    assert_eq!(
        skipped.len(),
        1,
        "expected exactly one skipped-summary warning; got {skipped:?}"
    );
    let msg = &skipped[0].message;
    assert!(
        msg.contains("1 of 5"),
        "summary must count 1 skipped out of 5 total; got: {msg}"
    );
    assert!(
        msg.contains("line 3"),
        "summary must cite the 1-based line offset within the body (line 3 = G33); got: {msg}"
    );
    assert!(
        msg.contains("G33"),
        "summary must surface the offending text (G33...); got: {msg}"
    );
    assert!(
        msg.contains("unsupported G33"),
        "summary must surface the classifier's reason string; got: {msg}"
    );
    // And the legacy blanket warning must NOT fire — yhen replaces
    // it wholesale.
    assert!(
        !resp.warnings
            .iter()
            .any(|w| w.kind == "gcode_include_not_simulated"),
        "yhen: legacy `gcode_include_not_simulated` warning must be gone; got {:?}",
        resp.warnings,
    );
}

/// yhen: multi-axis A / B / C / U / V / W words land in
/// Unsimulated — the sim is 3-axis (XYZ) only. A 4-axis indexing
/// line (`G1 A90`) carved by the controller will not match the
/// sim's heightmap.
#[test]
fn gcode_include_multi_axis_line_classified_unsimulated() {
    let include = Op {
        id: 1,
        name: "4th-axis index".into(),
        enabled: true,
        kind: OpKind::GcodeInclude {
            path: String::new(),
            content: "G0 X0 Y0\nG1 A90 F500\nG0 X10\n".into(),
            verbose_unsim_warnings: false,
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let profile = Op {
        id: 2,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![include, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    let summary = resp
        .warnings
        .iter()
        .find(|w| w.kind == "gcode_include_lines_skipped" && w.op_id == Some(1))
        .expect("expected skipped-summary warning for the A-axis line");
    assert!(
        summary.message.contains("A-axis"),
        "expected `A-axis` reason; got: {}",
        summary.message
    );
}

/// yhen: a body that is 100 % comments / no-op lines (no movement,
/// no unsupported G-codes) emits NEITHER the legacy blanket warning
/// NOR a skipped-summary. It's the user's prerogative to ship a
/// comment-only or M-code-only block — the existing
/// `gcode_include_empty` check guards the truly-empty case.
#[test]
fn gcode_include_comment_only_body_emits_no_classification_warning() {
    let include = Op {
        id: 1,
        name: "Notes only".into(),
        enabled: true,
        kind: OpKind::GcodeInclude {
            path: String::new(),
            content: "; just a note\n( and another )\nM5\n".into(),
            verbose_unsim_warnings: false,
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let profile = Op {
        id: 2,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![include, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    for kind in ["gcode_include_not_simulated", "gcode_include_lines_skipped"] {
        assert!(
            !resp.warnings.iter().any(|w| w.kind == kind),
            "yhen: comment/M-code-only body must not warn `{kind}`; got {:?}",
            resp.warnings,
        );
    }
}

/// xi2g: when `verbose_unsim_warnings = true`, the classifier fans
/// out one `gcode_include_unsim_line` warning per skipped line in
/// addition to the `gcode_include_lines_skipped` summary. Off by
/// default (covered by `gcode_include_mixed_body_emits_counted_skipped_summary`
/// which asserts only the summary fires).
#[test]
fn gcode_include_verbose_mode_fans_out_per_line_warnings() {
    let include = Op {
        id: 1,
        name: "Exotic block".into(),
        enabled: true,
        kind: OpKind::GcodeInclude {
            path: "/tmp/exotic.nc".into(),
            // 4 lines: 1 G0 (simulated), 1 G33 (skipped), 1 G1 A90
            // (skipped — multi-axis), 1 G1 (simulated).
            content: "G0 X0 Y0\nG33 X10 Z-5 P1.5\nG1 A90 F500\nG1 Z2\n"
                .into(),
            verbose_unsim_warnings: true,
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let profile = Op {
        id: 2,
        name: "Cut".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![include, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    // Summary still fires (xi2g layers per-line warnings on top, doesn't replace).
    assert!(
        resp.warnings
            .iter()
            .any(|w| w.kind == "gcode_include_lines_skipped" && w.op_id == Some(1)),
        "verbose mode must still emit the summary warning"
    );
    let per_line: Vec<&crate::pipeline::PipelineWarning> = resp
        .warnings
        .iter()
        .filter(|w| w.kind == "gcode_include_unsim_line" && w.op_id == Some(1))
        .collect();
    assert_eq!(
        per_line.len(),
        2,
        "verbose mode must fan out one warning per skipped line; got {per_line:?}"
    );
    // First per-line warning cites the G33 on line 2.
    assert!(
        per_line[0].message.contains("line 2")
            && per_line[0].message.contains("G33")
            && per_line[0].message.contains("unsupported G33"),
        "first per-line warning should be the G33 on line 2 with reason; got: {}",
        per_line[0].message,
    );
    // Second per-line warning cites the A-axis on line 3.
    assert!(
        per_line[1].message.contains("line 3") && per_line[1].message.contains("A-axis"),
        "second per-line warning should be the A-axis on line 3; got: {}",
        per_line[1].message,
    );
}

/// rxm9: round-trip a `GcodeInclude` op through serde JSON.
#[test]
fn gcode_include_round_trips_through_serde() {
    let kind = OpKind::GcodeInclude {
        path: "/some/file.nc".into(),
        content: "G0 X{x}\n".into(),
        verbose_unsim_warnings: true,
    };
    let json = serde_json::to_string(&kind).expect("serialize");
    assert!(json.contains("\"gcode_include\""), "expected tag in {json}");
    let back: OpKind = serde_json::from_str(&json).expect("deserialize");
    let back_json = serde_json::to_string(&back).unwrap();
    assert_eq!(
        json, back_json,
        "round-trip mismatch: {json} vs {back_json}"
    );
}

/// dp6b: ops with `group` emit `; === GROUP: <name> ===` at every
/// group BOUNDARY. Two ops with the same group share one header;
/// ops without a group share none.
#[test]
fn pipeline_emits_group_boundary_markers() {
    let make_profile = |id: u32, name: &str, group: Option<&str>| Op {
        id,
        name: name.into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -1.0,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: group.map(str::to_string),
    };
    // rough, rough, finish, (no group), finish
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![
            make_profile(1, "rough A", Some("rough")),
            make_profile(2, "rough B", Some("rough")),
            make_profile(3, "finish A", Some("finish")),
            make_profile(4, "no group", None),
            make_profile(5, "finish C", Some("finish")),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    let gcode = &resp.gcode;
    // Header lines, in order:
    //   ; === GROUP: rough ===     (op 1)
    //   (op 2 shares rough — no header)
    //   ; === GROUP: finish ===    (op 3)
    //   ; === END GROUP ===        (op 4 leaves group)
    //   ; === GROUP: finish ===    (op 5 re-enters)
    let lines: Vec<&str> = gcode.lines().filter(|l| l.starts_with("; === ")).collect();
    assert_eq!(
        lines,
        vec![
            "; === GROUP: rough ===",
            "; === GROUP: finish ===",
            "; === END GROUP ===",
            "; === GROUP: finish ===",
        ],
        "unexpected group-boundary lines in:\n{gcode}"
    );
}

/// dp6b: a project with NO `group` field on any op emits ZERO
/// group markers — legacy projects stay byte-identical.
#[test]
fn pipeline_emits_no_group_markers_when_field_unset() {
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    assert!(
        !resp.gcode.contains("=== GROUP"),
        "expected NO group lines for groupless project; got:\n{}",
        resp.gcode,
    );
    assert!(
        !resp.gcode.contains("=== END GROUP"),
        "expected NO end-group lines for groupless project; got:\n{}",
        resp.gcode,
    );
}

/// dp6b: `Some("")` and `None` are equivalent — neither emits a
/// boundary on its own and both share the same "no group" state.
#[test]
fn pipeline_treats_empty_group_string_as_no_group() {
    let mk = |id: u32, group: Option<&str>| {
        let mut op = profile_op(id, 1, ToolOffset::Outside);
        op.name = format!("Op {id}");
        op.group = group.map(str::to_string);
        op
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![mk(1, Some("")), mk(2, None), mk(3, Some("rough"))],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    // Only the transition INTO "rough" should fire a header — the
    // Some("")→None transition is a no-group→no-group identity.
    let lines: Vec<&str> = resp
        .gcode
        .lines()
        .filter(|l| l.starts_with("; === "))
        .collect();
    assert_eq!(
        lines,
        vec!["; === GROUP: rough ==="],
        "Some(\"\") should be the same state as None — only the\nfinal `Some(\"rough\")` transition should fire a header; got:\n{}",
        resp.gcode,
    );
}

/// dp6b: an `Op` with a non-None group round-trips through serde JSON.
#[test]
fn op_group_round_trips_through_serde() {
    let mut op = Op::default();
    op.id = 7;
    op.group = Some("rough".into());
    let json = serde_json::to_string(&op).expect("serialize");
    assert!(
        json.contains("\"group\":\"rough\""),
        "expected group in {json}"
    );
    let back: Op = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.group.as_deref(), Some("rough"));
    // None defaults to None on round-trip.
    let mut op2 = Op::default();
    op2.id = 8;
    op2.group = None;
    let json2 = serde_json::to_string(&op2).expect("serialize");
    assert!(
        !json2.contains("group"),
        "expected NO group field in {json2}"
    );
    let back2: Op = serde_json::from_str(&json2).expect("deserialize");
    assert!(back2.group.is_none());
}

/// 4dxb: simulate the exact wire payload the frontend sends for a
/// program-only op — a Pause between two cutting ops, params bag
/// missing `depth`/`start_depth`/`fast_move_z` entirely (the FE
/// constructor doesn't set them; JSON.stringify drops the
/// undefined keys). Pre-fix this failed at deserialize with
/// `missing field depth`. Post-fix the universal scalars default to
/// 0.0 and the whole project decodes cleanly. Then run the pipeline
/// and assert the Pause still gets emitted (`; OP 2 (pause)` header)
/// between the two cutting op bodies.
///
/// Build the project programmatically so we don't have to hand-roll
/// the full JSON for every nested struct (MachineConfig, ToolEntry,
/// etc.). Then serialize → strip the Pause's depth fields → deserialize
/// to exercise the actual failure-mode wire shape.
#[test]
fn project_with_pause_between_cuts_decodes_and_runs() {
    let make_profile = |id: u32, name: &str, depth: f64| Op {
        id,
        name: name.into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: crate::project::ContourParams::default(),
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth,
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };
    let pause = Op {
        id: 2,
        name: "Pause".into(),
        enabled: true,
        kind: OpKind::Pause {
            message: "Flip stock".into(),
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let project = crate::project::Project {
        segments: vec![Segment::line(
            crate::geometry::Point2::new(0.0, 0.0),
            crate::geometry::Point2::new(10.0, 0.0),
            "0",
            7,
        )],
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![
            make_profile(1, "Cut 1", -1.0),
            pause,
            make_profile(3, "Cut 2", -2.0),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };

    // Round-trip through JSON, then surgically drop the three
    // universal scalars from the Pause's params bag — exactly what
    // `JSON.stringify` does when the FE op shape carries undefined.
    let mut as_value = serde_json::to_value(&project).expect("project serializes");
    let pause_params = as_value["operations"][1]["params"]
        .as_object_mut()
        .expect("params bag is an object");
    pause_params.remove("depth");
    pause_params.remove("start_depth");
    pause_params.remove("fast_move_z");

    // Pre-fix: this `from_value` panics with `missing field 'depth'`.
    // Post-fix (4dxb): the three scalars default to 0.0 and the
    // whole project decodes cleanly.
    let project: crate::project::Project = serde_json::from_value(as_value)
        .expect("4dxb: project must decode with Pause params bag missing depth scalars");
    // Cross-check: the Pause's depth defaulted to 0.0, the Cuts'
    // depths round-tripped untouched.
    assert_eq!(project.operations[0].params.depth, -1.0);
    assert_eq!(project.operations[1].params.depth, 0.0);
    assert_eq!(project.operations[1].params.start_depth, 0.0);
    assert_eq!(project.operations[1].params.fast_move_z, 0.0);
    assert_eq!(project.operations[2].params.depth, -2.0);

    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: None,
        },
        |_, _, _| {},
    )
    .expect("pipeline runs over a Pause-between-cuts project");
    let gcode = &resp.gcode;
    // The Pause op-header marker lands between the two cutting ops,
    // confirming the program-only op slotted in correctly.
    assert!(
        gcode.contains("; OP 2 (pause)"),
        "expected `; OP 2 (pause)` header between cutting ops; got:\n{gcode}"
    );
}

/// rt1.34: Pause op round-trips through serde JSON (`snake_case` tag).
#[test]
fn pause_op_round_trips_through_serde() {
    let pause = Op {
        id: 5,
        name: "Pause".into(),
        enabled: true,
        kind: OpKind::Pause {
            message: "Flip the stock".into(),
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let json = serde_json::to_string(&pause).expect("serialize");
    assert!(json.contains("\"pause\""), "expected pause tag in {json}");
    let back: Op = serde_json::from_str(&json).expect("deserialize");
    match back.kind {
        OpKind::Pause { message } => assert_eq!(message, "Flip the stock"),
        other => panic!("expected Pause, got {other:?}"),
    }
}

// ---- k2ew / y9ho / gd2x regression tests ----

/// k2ew: a two-op project with two different tools must emit T1
/// M6 for the first op and T2 M6 between ops on a toolchange-
/// capable machine. Pre-fix the program never asserted a tool —
/// only `dual_tool` / Stufenfase internal toolchanges fired M6.
#[test]
fn multi_op_different_tools_emit_m6_at_each_boundary() {
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 6.0), endmill(2, 3.0)],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 2, ToolOffset::Outside),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let t1_pos = resp.gcode.find("T1 M6").unwrap_or_else(|| {
        panic!(
            "expected T1 M6 for first op (k2ew first-op M6):\n{}",
            resp.gcode
        )
    });
    let t2_pos = resp
        .gcode
        .find("T2 M6")
        .unwrap_or_else(|| panic!("expected T2 M6 at op boundary (k2ew):\n{}", resp.gcode));
    assert!(t1_pos < t2_pos, "T1 M6 must precede T2 M6");
    let op1_pos = resp.gcode.find("; OP 1").expect("OP 1 marker");
    let op2_pos = resp.gcode.find("; OP 2").expect("OP 2 marker");
    assert!(t1_pos < op1_pos, "T1 M6 must precede OP 1 body");
    assert!(
        t2_pos > op1_pos && t2_pos < op2_pos,
        "T2 M6 must sit between OP 1 and OP 2"
    );
}

/// k2ew: two ops with the SAME tool — at most one M6 (the
/// program-start assertion). Pre-fix nothing fired; post-fix only
/// the first-op boundary fires.
#[test]
fn multi_op_same_tool_emits_at_most_one_m6() {
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 3.0)],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 1, ToolOffset::Outside),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let count = resp.gcode.matches(" M6").count();
    assert_eq!(
        count, 1,
        "expected exactly one M6 for same-tool two-op project, got {count}:\n{}",
        resp.gcode
    );
}

/// k2ew: `machine.supports_toolchange` == false suppresses M6
/// emission entirely. The Z shift still applies (it's a work-Z
/// origin, not a toolchange artifact).
#[test]
fn no_toolchange_machine_omits_m6() {
    let machine = MachineConfig {
        supports_toolchange: false,
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 6.0), endmill(2, 3.0)],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 2, ToolOffset::Outside),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
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
        "machines with supports_toolchange=false must not emit M6:\n{}",
        resp.gcode
    );
}
/// y9ho: rectangle tabs must drop from `tabs_z` back to `cut_z` at
/// the PLUNGE feedrate (`rate_v`), not the cut feedrate (`rate_h`).
/// Pre-fix the drop happened at `rate_h` — way too fast for safe
/// Z descent into residual stock.
#[test]
fn rectangle_tab_drop_uses_plunge_feedrate() {
    let mut tool = endmill(1, 3.0);
    // Make the rates distinguishable so a feed swap is visible in
    // the output. Cut feed >> plunge feed (the bug's failure mode).
    tool.feed_rate = 2000;
    tool.plunge_rate = 400;
    let mut params = OpParams::mill_default();
    params.depth = -3.0;
    params.step = Some(-1.5);
    params.start_depth = 0.0;
    params.fast_move_z = 5.0;
    let contour = crate::project::ContourParams {
        tabs: crate::cam::setup::TabsConfig {
            active: true,
            width: 5.0,
            height: 1.0,
            tab_type: crate::cam::setup::TabType::Rectangle,
            ramp_angle_deg: 30.0,
        },
        tab_mode: crate::project::TabPlacementMode::Auto { count: 1 },
        ..crate::project::ContourParams::default()
    };
    let op = Op {
        id: 1,
        name: "Profile with tab".into(),
        enabled: true,
        kind: OpKind::Profile {
            offset: ToolOffset::Outside,
            contour,
            profile: crate::project::ProfileParams::default(),
        },
        tool_id: 1,
        finish_tool_id: None,
        source: OpSource::All,
        params,
        group: None,
    };
    let project = Project {
        segments: closed_square_offset(40.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![tool],
        operations: vec![op],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // A tab block looks like:
    //   F2000  (cut feed in effect)
    //   G1 ... Z1            ; lift to tabs_z
    //   G1 X.. Y..           ; traverse
    //   F400  (plunge feed)  <-- y9ho fix
    //   G1 ... Z-1.5         ; drop back
    //   F2000                ; restore cut feed
    let lines: Vec<&str> = resp.gcode.lines().collect();
    // Find a line that lifts UP to a tab Z (positive Z post a cut
    // pass) — depth list ends at -3.0 so tab Z = -3 + 1 = -2 or
    // the tab Z for the first pass (-1.5 + 1 = -0.5). Just look
    // for the F400 → Z-drop pattern.
    let mut found_drop_at_plunge = false;
    for (i, l) in lines.iter().enumerate() {
        if !l.starts_with("F400") {
            continue;
        }
        // Next non-empty line should be a Z-drop (no X / Y).
        for next in lines.iter().skip(i + 1) {
            if next.is_empty() {
                continue;
            }
            let is_z_drop = next.starts_with("G1")
                && next.contains('Z')
                && !next.contains('X')
                && !next.contains('Y');
            if is_z_drop {
                found_drop_at_plunge = true;
            }
            break;
        }
        if found_drop_at_plunge {
            break;
        }
    }
    assert!(
            found_drop_at_plunge,
            "expected F400 (plunge feed) immediately before a Z-only G1 drop (tabs_z→cut_z); gcode:\n{}",
            resp.gcode
        );
    // After the drop, feedrate should be restored to the cut
    // value (F2000) before the next cut move.
    assert!(
        resp.gcode.matches("F2000").count() >= 2,
        "expected F2000 to appear at least twice (initial set + tab restore); gcode:\n{}",
        resp.gcode
    );
}

/// gd2x: laser pierce dwell must fire AFTER the plunge to z=0,
/// not before. Pre-fix the sequence was G0 X Y Z(fast) → G4 P →
/// G1 Z0 — the beam was defocused above stock during the dwell.
#[test]
fn laser_pierce_dwells_at_cut_z() {
    let mut tool = endmill(1, 0.1);
    tool.kind = ToolKind::LaserBeam;
    tool.laser_pierce_sec = Some(0.4);
    let machine = MachineConfig {
        mode: crate::cam::setup::MachineMode::Laser,
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![tool],
        operations: vec![Op {
            id: 1,
            name: "Laser cut".into(),
            enabled: true,
            kind: OpKind::Engrave {
                contour: crate::project::ContourParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // Locate the FIRST G1 Z0 (the plunge to cut height) and the
    // FIRST G4 P0.4 (the pierce dwell). The plunge must come
    // first; the dwell follows.
    let lines: Vec<&str> = resp.gcode.lines().collect();
    let plunge_idx = lines
        .iter()
        .position(|l| l.contains("G1") && l.contains("Z0"))
        .unwrap_or_else(|| panic!("expected a G1 ... Z0 plunge line in:\n{}", resp.gcode));
    let dwell_idx = lines
        .iter()
        .position(|l| l.contains("G4") && l.contains("P0.4"))
        .unwrap_or_else(|| panic!("expected a G4 P0.4 pierce dwell in:\n{}", resp.gcode));
    assert!(
        plunge_idx < dwell_idx,
        "G1 Z0 (idx {plunge_idx}) must precede G4 P0.4 (idx {dwell_idx}):\n{}",
        resp.gcode
    );
}

// ---- bd eaeq / m8sq / rwv8 / rfow: toolchange safety envelope ----

/// Helper: find the byte index of a substring, returning a clear
/// panic message when missing so the test failure shows the gcode.
fn must_find(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("expected `{needle}` in:\n{haystack}"))
}

/// Inter-op toolchange: the gcode between two different-tool ops
/// must emit the full M5 → safe-Z → M6 → M3 envelope, in order.
#[test]
fn multi_op_toolchange_envelope_has_m5_before_m6_and_m3_after() {
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 6.0), endmill(2, 3.0)],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 2, ToolOffset::Outside),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // Slice out the gcode between OP 1 and OP 2 — that's where
    // the inter-op envelope lives.
    let op1_pos = must_find(&resp.gcode, "; OP 1");
    let op2_pos = must_find(&resp.gcode, "; OP 2");
    let between = &resp.gcode[op1_pos..op2_pos];
    let m5_pos = must_find(between, "\nM5");
    let m6_pos = must_find(between, "T2 M6");
    // M3 for the second tool's RPM (18000 — endmill default).
    let m3_pos = must_find(between, "M3 S");
    assert!(
        m5_pos < m6_pos,
        "M5 must precede T2 M6 in inter-op envelope; got M5={m5_pos} M6={m6_pos}:\n{between}"
    );
    assert!(
        m6_pos < m3_pos,
        "M3 S<rpm> must follow T2 M6; got M6={m6_pos} M3={m3_pos}:\n{between}"
    );
}

/// Within-op dual-tool toolchange (rough → finish) must use the
/// full envelope, not just a bare T<n> M6.
#[test]
fn dual_tool_internal_change_uses_full_envelope() {
    let mut rough_tool = endmill(1, 6.0);
    rough_tool.speed = 20_000;
    let mut finish_tool = endmill(2, 3.0);
    finish_tool.speed = 24_000;

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
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // Locate the T2 M6 line and inspect the surrounding gcode.
    let m6_pos = must_find(&resp.gcode, "T2 M6");
    let before = &resp.gcode[..m6_pos];
    let after = &resp.gcode[m6_pos..];
    // M5 must appear in the "before" slice (the immediate
    // pre-M6 sequence), AND it must be the last M5 before the M6.
    let pre_m5 = before
        .rfind("\nM5")
        .unwrap_or_else(|| panic!("expected M5 before T2 M6:\n{}", resp.gcode));
    // Spindle-up at the finish tool's RPM must come after M6.
    assert!(
        after.contains("M3 S24000"),
        "expected M3 S24000 (finish tool spindle) after T2 M6:\n{}",
        resp.gcode
    );
    // Sanity: pre_m5 sits within the rough-pass tail (after a
    // safe-Z rapid retract). The G0 Z<fast> lift should come
    // BEFORE the M5.
    let lift_pos = before
        .rfind("G0 Z")
        .unwrap_or_else(|| panic!("expected G0 Z<fast> lift before M5:\n{}", resp.gcode));
    assert!(
        lift_pos < pre_m5,
        "safe-Z lift must precede M5 in envelope; lift={lift_pos} M5={pre_m5}"
    );
}

/// Drill → Stufenfase chamfer toolchange must use the full
/// envelope. Pre-fix it emitted `T<n> M6` immediately after the
/// drill block with the spindle still running.
#[test]
fn drill_stufenfase_change_uses_full_envelope() {
    let mut drill_bit = vbit();
    drill_bit.kind = ToolKind::Drill;
    drill_bit.diameter = 3.0;
    drill_bit.id = 1;
    drill_bit.speed = 8_000;
    let mut vbit_finish = vbit();
    vbit_finish.id = 2;
    vbit_finish.diameter = 6.35;
    vbit_finish.tip_angle_deg = 90.0;
    vbit_finish.speed = 22_000;
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    let center = crate::geometry::Point2::new(5.0, 7.0);
    let mut params = OpParams::mill_default();
    params.depth = -3.0;
    params.start_depth = 0.0;
    let project = Project {
        segments: closed_circle(center, 0.5),
        machine,
        tools: vec![drill_bit, vbit_finish],
        operations: vec![Op {
            id: 1,
            name: "Drill+stufenfase".into(),
            enabled: true,
            kind: OpKind::Drill {
                cycle: crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
                chamfer_after_width_mm: Some(0.5),
                pattern: None,
                spot_first: None,
            },
            tool_id: 1,
            finish_tool_id: Some(2),
            source: OpSource::All,
            params,
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let m6_pos = must_find(&resp.gcode, "T2 M6");
    let before = &resp.gcode[..m6_pos];
    let after = &resp.gcode[m6_pos..];
    assert!(
        before.rfind("\nM5").is_some(),
        "expected M5 before drill→chamfer T2 M6:\n{}",
        resp.gcode
    );
    assert!(
        after.contains("M3 S22000"),
        "expected M3 S22000 (chamfer tool spindle) after T2 M6:\n{}",
        resp.gcode
    );
}

/// 3lf0: with flood coolant active, the inter-op toolchange
/// envelope must turn coolant OFF (M9) BEFORE stopping the
/// spindle (M5) and performing the tool change (M6). Otherwise
/// water/mist sprays into the open spindle taper / collet while
/// the change happens — operator safety hazard and chuck
/// contamination; many auto-changers refuse to operate with
/// coolant active. The next op's `coolant_flood` call inside
/// `emit_offset` then re-engages M8 at the new tool's first cut.
#[test]
fn coolant_off_before_spindle_off_in_inter_op_toolchange() {
    let mut tool_a = endmill(1, 6.0);
    tool_a.coolant = crate::project::Coolant::Flood;
    let mut tool_b = endmill(2, 3.0);
    tool_b.coolant = crate::project::Coolant::Flood;
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![tool_a, tool_b],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 2, ToolOffset::Outside),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let op1_pos = must_find(&resp.gcode, "; OP 1");
    let op2_pos = must_find(&resp.gcode, "; OP 2");
    let between = &resp.gcode[op1_pos..op2_pos];
    // The first M8 fires inside OP 1's emit, so look for M9 in the
    // inter-op block followed by M5 followed by T2 M6.
    let m9_pos = between
        .find("\nM9")
        .unwrap_or_else(|| panic!("3lf0: missing M9 (coolant off) between ops:\n{between}"));
    let m5_pos = between
        .find("\nM5")
        .unwrap_or_else(|| panic!("3lf0: missing M5 between ops:\n{between}"));
    let m6_pos = must_find(between, "T2 M6");
    assert!(
            m9_pos < m5_pos,
            "3lf0: M9 (coolant off) must precede M5 (spindle stop) so the toolchange happens dry; got M9={m9_pos} M5={m5_pos}:\n{between}"
        );
    assert!(
        m5_pos < m6_pos,
        "3lf0: M5 must still precede T2 M6; got M5={m5_pos} M6={m6_pos}:\n{between}"
    );
}

/// 3lf0: first-tool path (program start) doesn't have prior coolant
/// active — the envelope must NOT emit a leading M9 there. Only
/// inter-op envelopes after a coolant-on op need the safeguard.
#[test]
fn first_tool_envelope_omits_leading_coolant_off() {
    let mut tool = endmill(1, 3.0);
    tool.coolant = crate::project::Coolant::Flood;
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![tool],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // The first `; OP 1` marker bounds the first-tool envelope —
    // anything before it that's an M9 would be a spurious leading
    // coolant-off (no prior op).
    let op1 = must_find(&resp.gcode, "; OP 1");
    let header = &resp.gcode[..op1];
    assert!(
        !header.contains("\nM9"),
        "3lf0: first-tool envelope must NOT emit M9 (no prior coolant to disable):\n{header}"
    );
}

/// Same-tool back-to-back ops must skip the envelope entirely
/// between them — no M5/M6/M3 between OP 1 and OP 2.
#[test]
fn same_tool_consecutive_ops_skip_envelope_entirely() {
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 3.0)],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 1, ToolOffset::Outside),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let op1 = must_find(&resp.gcode, "; OP 1");
    let op2 = must_find(&resp.gcode, "; OP 2");
    let between = &resp.gcode[op1..op2];
    assert!(
        !between.contains(" M6"),
        "expected no M6 between same-tool ops:\n{between}"
    );
    assert!(
        !between.contains("\nM5"),
        "expected no M5 between same-tool ops (the spindle keeps running):\n{between}"
    );
}

/// `machine.supports_toolchange == false` (hobby benchtop CNC):
/// instead of M6, emit M5 + program-pause (M0) + comment so the
/// operator hand-swaps the bit before pressing Cycle Start.
#[test]
fn non_toolchange_machine_pauses_for_manual_swap() {
    let machine = MachineConfig {
        supports_toolchange: false,
        ..MachineConfig::default()
    };
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 6.0), endmill(2, 3.0)],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 2, ToolOffset::Outside),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // No M6 (machine doesn't support it).
    assert!(
        !resp.gcode.contains(" M6"),
        "expected no M6 when supports_toolchange=false:\n{}",
        resp.gcode
    );
    // The inter-op section must have M5, the `pause: swap to tool 2`
    // comment, and an M0 program pause — IN THAT ORDER.
    let op1 = must_find(&resp.gcode, "; OP 1");
    let op2 = must_find(&resp.gcode, "; OP 2");
    let between = &resp.gcode[op1..op2];
    let m5 = must_find(between, "\nM5");
    let swap = must_find(between, "pause: swap to tool 2");
    let m0 = must_find(between, "\nM0");
    assert!(
        m5 < swap && swap < m0,
        "expected order M5 → pause comment → M0; got M5={m5} swap={swap} M0={m0}:\n{between}"
    );
}

/// w9hd: a project with `machine.unit = Inch` MUST scale every
/// emitted X/Y/Z by 1/25.4. The pipeline math runs in mm; the
/// post applies the boundary conversion. We assert that:
///   * `G20` (inch pragma) is present
///   * the 20 mm square's emitted X coords land near 0.787 in
///   * the depth Z of -1 mm lands near -0.039 in
///
/// The previous behaviour was a silent 25.4× over-cut.
#[test]
fn inch_units_emit_scaled_numbers() {
    let machine = MachineConfig {
        unit: crate::cam::setup::UnitSystem::Inch,
        ..MachineConfig::default()
    };
    let mut profile = profile_op(1, 1, ToolOffset::Outside);
    profile.params.step = Some(-1.0);
    profile.params.depth = -1.0;
    let project = Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
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
        resp.gcode.contains("G20"),
        "expected G20 (inch), got:\n{}",
        resp.gcode
    );
    assert!(!resp.gcode.contains("G21"));
    // 20 mm / 25.4 ≈ 0.7874 — appears formatted with up to 4 decimals.
    // Any line that contains an X value in the ballpark of 0.78 is a hit
    // (the offset cascade may emit slightly different numbers per pass,
    // but the LITERAL "20" mm value MUST NOT appear as an X word).
    assert!(
        !resp
            .gcode
            .lines()
            .any(|l| l.contains("X20") || l.contains("X20.0")),
        "raw 20 mm leaked into gcode despite Inch unit:\n{}",
        resp.gcode
    );
    assert!(
        resp.gcode.lines().any(|l| {
            (l.starts_with("G0") || l.starts_with("G1"))
                && (l.contains("X0.78") || l.contains("X0.79"))
        }),
        "expected an X value near 0.787 (20 mm in inches), got:\n{}",
        resp.gcode
    );
    // Z = -1 mm → -0.039 in. Confirm the depth got scaled.
    assert!(
        resp.gcode.lines().any(|l| l.contains("Z-0.03")),
        "expected a Z near -0.039 (-1 mm in inches), got:\n{}",
        resp.gcode
    );
}

// ----------------------------------------------------------------
// P0 pipeline-state regressions: zjgt + nguf + yc2a
// ----------------------------------------------------------------

/// zjgt: a tool with `spindle_direction == Ccw` must produce M4
/// (counter-clockwise) — not M3 — both at program-start spindle-up
/// AND at every toolchange envelope's post-M6 spindle-up.
///
/// Pre-fix `emit_toolchange_envelope` hardcoded `post.spindle_cw(...)`
/// which (a) wrote M3 directly into the gcode and (b) primed the
/// post's `last_speed` so the next cut's lazy `spindle_ccw(speed,...)`
/// saw the same speed and elided the M4 — silently flipping the
/// program from CCW to CW.
#[test]
fn toolchange_envelope_routes_ccw_tool_through_m4() {
    let mut t1 = endmill(1, 6.0);
    t1.spindle_direction = crate::project::SpindleDirection::Ccw;
    t1.speed = 12_000;
    let mut t2 = endmill(2, 3.0);
    t2.spindle_direction = crate::project::SpindleDirection::Ccw;
    t2.speed = 18_000;
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    let project = crate::project::Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![t1, t2],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 2, ToolOffset::Outside),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: Some(crate::pipeline::PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // CCW program must NOT emit a bare M3 (the W-flag is M4).
    // Allow incidental "M3" substrings inside other tokens; check
    // each whitespace-bounded line.
    for line in resp.gcode.lines() {
        let trimmed = line.trim();
        assert!(
            trimmed != "M3" && !trimmed.starts_with("M3 "),
            "zjgt: CCW tool program leaked an M3 line:\n{line}\nfull gcode:\n{}",
            resp.gcode
        );
    }
    // Both first-tool envelope and inter-op envelope must emit M4
    // at the correct RPM for the corresponding tool.
    assert!(
        resp.gcode.contains("M4 S12000"),
        "expected M4 S12000 for first CCW tool (rpm 12000):\n{}",
        resp.gcode
    );
    assert!(
        resp.gcode.contains("M4 S18000"),
        "expected M4 S18000 after T2 M6 for second CCW tool:\n{}",
        resp.gcode
    );
}

/// zjgt: same-direction (CW) tools still emit M3 — the dispatcher
/// must not regress the default path.
#[test]
fn toolchange_envelope_keeps_m3_for_default_cw_tool() {
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    let project = crate::project::Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 6.0), endmill(2, 3.0)],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 2, ToolOffset::Outside),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: Some(crate::pipeline::PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    assert!(
        !resp.gcode.lines().any(|l| {
            let t = l.trim();
            t == "M4" || t.starts_with("M4 ")
        }),
        "default CW project leaked an M4 line:\n{}",
        resp.gcode
    );
    assert!(
        resp.gcode.contains("M3 S"),
        "expected M3 S<rpm> for default CW tool:\n{}",
        resp.gcode
    );
}

/// nguf: a dual-tool op that declares a `finish_tool_id` but
/// whose driver doesn't actually emit the internal swap must NOT
/// bias `prev_tool_id` to the finish id. The next op asking for
/// the rough tool would then see "tool changes — skip" and run
/// with the wrong T number still in the spindle.
///
/// Uses a Pocket on a wide-enough geometry to actually swap to T2
/// for the wall ring — exercises the `real_swap == true` branch,
/// where post-fix we DO bias `prev_tool_id` to 2 (so OP 2 on T1
/// re-emits T1 M6). The companion `..._drill_chamfer_skip` test
/// exercises the `real_swap == false` branch.
#[test]
fn prev_tool_id_stays_unchanged_when_dual_tool_skips_finish() {
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    // Build a Pocket op pointing at finish_tool 2 but geometry
    // small enough that the cascade likely produces only rough
    // offsets (no `is_finish` ring). Even if it does produce one,
    // we explicitly assert behavior conditional on whether a real
    // toolchange envelope showed up.
    let pocket = Op {
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
        group: None,
    };
    // Second op on the same rough tool 1. The bug: it would skip
    // its M6 envelope because prev_tool_id was biased to 2.
    let follow_up = profile_op(2, 1, ToolOffset::Outside);
    let project = crate::project::Project {
        segments: closed_square_offset(50.0, 0.0, 0.0),
        machine,
        tools: vec![endmill(1, 6.0), endmill(2, 3.0)],
        operations: vec![pocket, follow_up],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: Some(crate::pipeline::PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // Slice the gcode between the END of OP 1's body and the END
    // of OP 2's body. That's the window where the per-op
    // toolchange envelope for OP 2 (if any) sits.
    let op1 = must_find(&resp.gcode, "; OP 1");
    let op2 = must_find(&resp.gcode, "; OP 2");
    let after_op1_block_end = op2; // OP 2 marker is end of OP 1
    let after_op2 = &resp.gcode[op2..];
    let next_op = after_op2.find("\n; OP ").map_or(after_op2.len(), |i| i);
    let around_op2 = &resp.gcode[op2..op2 + next_op];
    // The inter-op envelope (if any) for OP 2 lives between
    // `; OP 1`'s last line and `; OP 2`. We need to inspect that
    // region — it's where the bug's M6 would show up.
    let inter_op_window = &resp.gcode[op1..after_op1_block_end];
    // Determine whether the Pocket actually emitted an internal
    // dual-tool envelope (rough→finish). Scan only the body
    // (before any potential inter-op envelope tail).
    //
    // The rough→finish envelope is annotated with the
    // `; toolchange: finish pass with tool 2` comment from
    // dual_tool.rs, which is distinct from the run_per_op's
    // `toolchange: T? (...) for op ...` comment.
    let real_swap = inter_op_window.contains("toolchange: finish pass with tool");
    if real_swap {
        // Post-fix: when the cascade DID emit a real T2 swap
        // inside OP 1, prev_tool_id MUST be biased to 2 so that
        // OP 2 (on tool 1) emits its own T1 M6 envelope to swap
        // back. Failure here means a real swap was lost.
        assert!(
            inter_op_window.contains("T1 M6"),
            "nguf: cascade swapped to T2 inside OP 1; OP 2 on \
                 tool 1 must emit T1 M6 to swap back. Got:\n{}",
            resp.gcode
        );
    } else {
        // Bug condition: dual_tool fell through to single-emit
        // (no finish ring). Pre-fix prev_tool_id was still
        // Some(2). Post-fix: prev_tool_id stays at 1, so OP 2's
        // tool-change check sees no change and skips the envelope
        // entirely. The window between OP 1 and OP 2 must NOT
        // contain any M6 line.
        for line in inter_op_window.lines() {
            let t = line.trim();
            assert!(
                !t.contains(" M6") && t != "M6",
                "nguf: dual_tool didn't actually swap to T2, but \
                     an M6 line appeared between OP 1 and OP 2 — \
                     pre-fix bug would emit T?M6 here. Line:\n{line}\nfull gcode:\n{}",
                resp.gcode
            );
        }
        // Sanity: also no T2-related M6 inside OP 2's block.
        assert!(
            !around_op2.contains("T2 M6"),
            "nguf: OP 2 unexpectedly emitted T2 M6 even though \
                 OP 1's dual_tool did not swap; full gcode:\n{}",
            resp.gcode
        );
    }
}

/// nguf companion: an op declaring `finish_tool_id = Some(2)` whose
/// driver decides NOT to emit the internal swap (e.g. Drill op
/// without `chamfer_after_width_mm`) must leave `prev_tool_id`
/// at the rough tool, so the next op on the same rough tool
/// correctly skips its own M6 envelope.
///
/// Pre-fix: `prev_tool_id = Some(finish_id = 2)` always at end of
/// op, so the next op on tool 1 saw `prev_tool_id != Some(1)`,
/// emitted a T1 M6 envelope, and the user got a wasteful M6 — or
/// worse, the controller's tool-table told it T2 was loaded so
/// the wrong tool geometry / offset got applied to the next cut.
#[test]
fn prev_tool_id_unchanged_after_drill_skips_chamfer_swap() {
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    // Drill op on tool 1 declares finish_tool_id = Some(2) but
    // no chamfer_after_width_mm → run_drill returns Ok(false).
    let drill = Op {
        id: 1,
        name: "Drill".into(),
        enabled: true,
        kind: OpKind::Drill {
            cycle: crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
            chamfer_after_width_mm: None,
            pattern: None,
            spot_first: None,
        },
        tool_id: 1,
        finish_tool_id: Some(2),
        source: OpSource::All,
        params: {
            let mut p = OpParams::mill_default();
            p.depth = -3.0;
            p.start_depth = 0.0;
            p.fast_move_z = 5.0;
            p
        },
        group: None,
    };
    let follow_up = profile_op(2, 1, ToolOffset::Outside);
    let project = crate::project::Project {
        segments: {
            let mut s = closed_square_offset(50.0, 0.0, 0.0);
            // Add a small drill target circle inside the square.
            s.extend(super::test_helpers::closed_circle(
                crate::geometry::Point2::new(25.0, 25.0),
                0.5,
            ));
            s
        },
        machine,
        tools: vec![endmill(1, 3.0), endmill(2, 1.5)],
        operations: vec![drill, follow_up],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: Some(crate::pipeline::PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // The drill op declared finish_tool_id=Some(2) but didn't ask
    // for chamfer, so the driver MUST NOT have emitted an internal
    // T2 M6. Anywhere.
    let op1 = must_find(&resp.gcode, "; OP 1");
    let op2 = must_find(&resp.gcode, "; OP 2");
    let between = &resp.gcode[op1..op2];
    assert!(
        !between.contains("T2 M6"),
        "nguf: drill without chamfer should not emit T2 M6 between \
             OP 1 and OP 2; got:\n{between}\nfull:\n{}",
        resp.gcode
    );
    // The follow-up Profile op is on tool 1, same as the drill's
    // rough tool. Post-fix prev_tool_id == 1 at end of OP 1, so
    // OP 2's envelope is elided. Pre-fix, prev_tool_id was 2
    // (finish_id bias), so OP 2 emitted a spurious T1 M6.
    assert!(
        !between.contains("T1 M6"),
        "nguf: pre-fix bug — OP 2 on the same rough tool emitted \
             T1 M6 because prev_tool_id was pessimistically biased to \
             finish_id (2). Post-fix expects no envelope between \
             same-tool ops. Got:\n{between}\nfull:\n{}",
        resp.gcode
    );
    // And no stray M5 (the envelope's spindle-stop) either.
    assert!(
        !between.lines().any(|l| l.trim() == "M5"),
        "nguf: pre-fix bug — OP 2's spurious envelope emitted M5 \
             between same-tool ops. Got:\n{between}\nfull:\n{}",
        resp.gcode
    );
}

/// yc2a: a Pause op in a CCW-tool program must not lock the
/// post-pause spindle into M3. The pause emits only `M5 / message
/// / M0` plus a `reset_state()` so the NEXT op's lazy `spindle_on`
/// (driven by its tool's `spindle_direction`) re-emits M3 OR M4
/// at the correct speed and direction.
#[test]
fn pause_op_does_not_lock_spindle_direction() {
    let mut ccw_tool = endmill(1, 3.0);
    ccw_tool.spindle_direction = crate::project::SpindleDirection::Ccw;
    ccw_tool.speed = 15_000;
    let profile_before = profile_op(1, 1, ToolOffset::Outside);
    let pause = Op {
        id: 2,
        name: "Pause".into(),
        enabled: true,
        kind: OpKind::Pause {
            message: "manual flip".into(),
        },
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };
    let profile_after = profile_op(3, 1, ToolOffset::Outside);
    let project = crate::project::Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![ccw_tool],
        operations: vec![profile_before, pause, profile_after],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: Some(crate::pipeline::PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    // No stray M3 lines anywhere — the whole program is CCW.
    for line in resp.gcode.lines() {
        let trimmed = line.trim();
        assert!(
            trimmed != "M3" && !trimmed.starts_with("M3 "),
            "yc2a: CCW + Pause program leaked an M3 line:\n{line}\nfull gcode:\n{}",
            resp.gcode
        );
    }
    // The pause sequence emits exactly M5 → comment → M0; no raw
    // M3 follows.
    let m0_pos = resp
        .gcode
        .find("\nM0\n")
        .unwrap_or_else(|| panic!("expected M0 in pause output:\n{}", resp.gcode));
    let after_m0 = &resp.gcode[m0_pos..];
    // Find the next op's `; OP 3` header so we can slice the
    // post-pause window without including OP 3's gcode.
    let op3 = after_m0
        .find("; OP 3")
        .unwrap_or_else(|| panic!("expected OP 3 after pause:\n{}", resp.gcode));
    let immediate = &after_m0[..op3];
    for line in immediate.lines() {
        let t = line.trim();
        assert!(
            t != "M3" && !t.starts_with("M3 "),
            "yc2a: Pause op injected a raw M3 between M0 and OP 3:\n{immediate}"
        );
    }
    // After the pause, OP 3's spindle-on must re-emit M4 with the
    // CCW tool's RPM (last_speed was cleared by reset_state, so
    // the lazy `spindle_ccw(15000, ...)` actually emits).
    let op3_start = resp
        .gcode
        .find("; OP 3")
        .unwrap_or_else(|| panic!("expected OP 3 marker:\n{}", resp.gcode));
    let op3_block = &resp.gcode[op3_start..];
    assert!(
        op3_block.contains("M4 S15000"),
        "yc2a: expected M4 S15000 to re-emit after pause for CCW tool:\n{}",
        resp.gcode
    );
}

/// lx1u: a laser-mode multi-tool project must NOT emit M3 / M4 in
/// the toolchange envelope. Pre-fix the envelope always called
/// `crate::gcode::spindle_on(...)` (and `spindle_off()` for the
/// stop side), which on GRBL laser turns the beam steady-on at
/// the clamped-min RPM during a toolchange — silent fire-mode
/// flip (pulse → steady-on at min power) and a real safety hazard.
///
/// Per-cut laser firing is still handled by `emit_*_block`'s
/// `cut_tool_on` (20y5); the envelope is only there to manage the
/// SPINDLE, which laser mode doesn't have. Gate it on
/// `MachineMode::Mill`.
#[test]
fn laser_mode_toolchange_envelope_emits_no_spindle_commands() {
    use crate::cam::setup::MachineMode;
    let mut t1 = endmill(1, 6.0);
    t1.kind = ToolKind::LaserBeam;
    t1.speed = 1000;
    let mut t2 = endmill(2, 3.0);
    t2.kind = ToolKind::LaserBeam;
    t2.speed = 500;
    let machine = MachineConfig {
        supports_toolchange: true,
        mode: MachineMode::Laser,
        ..MachineConfig::default()
    };
    let project = crate::project::Project {
        segments: closed_square_offset(20.0, 0.0, 0.0),
        machine,
        tools: vec![t1, t2],
        operations: vec![
            profile_op(1, 1, ToolOffset::Outside),
            profile_op(2, 2, ToolOffset::Outside),
        ],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        crate::pipeline::PipelineRequest {
            project,
            post_processor: Some(crate::pipeline::PostProcessorKind::Grbl),
        },
        |_, _, _| {},
    )
    .unwrap();
    // The envelope must not leak ANY spindle directive — neither
    // M3 / M4 (spindle-up) nor a bare M5 in the toolchange context.
    // Per-cut laser-on / -off is handled by cut_tool_on (20y5)
    // and lives INSIDE each op's body, not in the envelope.
    //
    // GRBL's laser post emits `M5` only at cut-leave (laser_off) or
    // program_end; neither path runs inside the envelope after the
    // lx1u gate. Scan the gcode for any M3 / M4 line — none must
    // appear, because the per-cut path goes through `laser_on(S)`
    // which uses M3 too — wait, that's the same opcode. Refine the
    // assertion: any M3/M4 line that appears between the safe-Z
    // lift and the T<n> M6 of a toolchange envelope is the bug.
    //
    // For GRBL with laser tools, `laser_on(power)` does emit
    // `M3 S<power>` (laser firing rides on the spindle opcode). So
    // the simpler check is: scan the gcode for occurrences of
    // `M3 S<clamped_min>` between consecutive `T<n> M6` lines.
    // The clamped-min is whatever the post's silent clamp produced
    // for speed=0 — we don't depend on the value, just that the
    // ENVELOPE itself doesn't introduce a spurious M3/M4 at the
    // tool's commanded speed.
    //
    // GRBL doesn't emit `T<n> M6` literally — it emits a
    // `; toolchange: T2 (...)` comment and skips the M6. Use the
    // toolchange-comment marker to bracket the envelope.
    //
    // Between OP 1's last cut and the "; toolchange:" marker the
    // emitter runs `cut_tool_off` (laser_off → M5) plus the
    // envelope's safe-Z lift. The M5 is *correct* and part of the
    // per-cut path (20y5), not the envelope. The bug we're testing
    // for is the envelope adding a SECOND M5 (post.spindle_off)
    // and an M3 S<clamped_min> (post.spindle_on) which silently
    // fired the laser at min power during the toolchange.
    //
    // Count: post-fix the entire OP1-end → OP2-start window has
    // EXACTLY ONE M5 line (the cut_tool_off) and ZERO M3/M4 lines.
    // Pre-fix it had at least TWO M5 lines and ONE M3 S<n> line.
    let op1_end = resp
        .gcode
        .find("; OP 2")
        .unwrap_or_else(|| panic!("expected ; OP 2 marker in:\n{}", resp.gcode));
    let prefix = &resp.gcode[..op1_end];
    let last_cut = prefix
        .rfind("G1 X")
        .or_else(|| prefix.rfind("G1 Y"))
        .or_else(|| prefix.rfind("G2"))
        .or_else(|| prefix.rfind("G3"))
        .unwrap_or_else(|| panic!("expected at least one OP 1 cut line in:\n{}", resp.gcode));
    let envelope_window = &resp.gcode[last_cut..op1_end];
    let mut m5_count = 0;
    let mut m3_count = 0;
    let mut m4_count = 0;
    for line in envelope_window.lines() {
        let t = line.trim();
        if t == "M5" {
            m5_count += 1;
        }
        if t.starts_with("M3") && !t.starts_with("M30") {
            m3_count += 1;
        }
        if t.starts_with("M4") {
            m4_count += 1;
        }
    }
    assert_eq!(
            m5_count, 1,
            "lx1u: laser-mode envelope leaked extra M5 (pre-fix had spindle_off + laser_off):\n{envelope_window}\nfull:\n{}",
            resp.gcode
        );
    assert_eq!(
            m3_count, 0,
            "lx1u: laser-mode envelope leaked M3 (firing laser at clamped-min during toolchange):\n{envelope_window}\nfull:\n{}",
            resp.gcode
        );
    assert_eq!(
        m4_count, 0,
        "lx1u: laser-mode envelope leaked M4:\n{envelope_window}\nfull:\n{}",
        resp.gcode
    );
}

/// v0ez: the work-area envelope guard is enforced pipeline-side, so a
/// program whose cuts leave the machine travel box surfaces an
/// `out_of_work_area` warning on EVERY transport (here via the bare
/// `run_pipeline` core entry, no frontend involved). A 20 mm square
/// placed at X 250..270 sits entirely past the default 200 mm X
/// travel.
#[test]
fn run_pipeline_flags_cuts_outside_work_area() {
    let project = Project {
        segments: closed_square_offset(20.0, 250.0, 0.0),
        machine: MachineConfig::default(), // 200×300×50
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .expect("pipeline should run");
    assert!(
        resp.warnings.iter().any(|w| w.kind == "out_of_work_area"),
        "expected core-side out_of_work_area warning for cuts at X>200; got {:?}",
        resp.warnings.iter().map(|w| &w.kind).collect::<Vec<_>>(),
    );
}

/// v0ez companion: an in-envelope program must NOT raise the
/// work-area warning — the guard is silent when the cuts stay inside
/// the travel box. Placed at (50, 50) so the Outside profile's
/// tool-radius offset (≈ -1.5 mm at the corner) still lands well
/// inside [0, travel] on every axis.
#[test]
fn run_pipeline_no_work_area_warning_when_in_bounds() {
    let project = Project {
        segments: closed_square_offset(20.0, 50.0, 50.0),
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![profile_op(1, 1, ToolOffset::Outside)],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    };
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .expect("pipeline should run");
    assert!(
        !resp.warnings.iter().any(|w| w.kind == "out_of_work_area"),
        "in-bounds program should not raise out_of_work_area; got {:?}",
        resp.warnings.iter().map(|w| &w.kind).collect::<Vec<_>>(),
    );
}

/// 3g6u: a T-slot op cuts the undercut as a SINGLE pass at the floor
/// Z — it must NOT cascade through intermediate depth levels the way
/// a Profile/Pocket would (that head-at-every-depth cascade is the
/// bug this op kind fixes). Mirrors the plot-mode Engrave test.
fn tslot_tool(id: u32, head_dia: f64, neck_dia: f64) -> ToolEntry {
    use crate::project::FormProfileSample;
    let mut t = endmill(id, head_dia);
    // z5yw: a T-slot is a FormProfile now — a wide cutting disk at
    // the tip narrowing to the neck above.
    t.kind = ToolKind::FormProfile;
    t.form_profile_mm = vec![
        FormProfileSample {
            z_mm: 0.0,
            r_mm: head_dia / 2.0,
        },
        FormProfileSample {
            z_mm: 3.0,
            r_mm: head_dia / 2.0,
        },
        FormProfileSample {
            z_mm: 3.0,
            r_mm: neck_dia / 2.0,
        },
        FormProfileSample {
            z_mm: 8.0,
            r_mm: neck_dia / 2.0,
        },
    ];
    t
}

// `depth` is parametrized so each test builds a DISTINCT project —
// run_pipeline shares a process-global op cache (GLOBAL_CACHE), so two
// identical projects would collide and the second would hit the cache
// (skipping build_op_offsets + its warnings). A unique depth per test
// keeps every call a cache miss.
fn tslot_project(tool: ToolEntry, depth: f64) -> Project {
    let mut params = OpParams::mill_default();
    params.depth = depth; // a Profile/Pocket would cascade -1, -2, …
    params.start_depth = 0.0;
    params.fast_move_z = 5.0;
    params.step = Some(-1.0);
    Project {
        segments: closed_square_offset(20.0, 30.0, 30.0),
        machine: MachineConfig::default(),
        tools: vec![tool],
        operations: vec![Op {
            id: 1,
            name: "T-slot".into(),
            enabled: true,
            kind: OpKind::TSlot {
                contour: crate::project::ContourParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params,
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    }
}

#[test]
fn tslot_op_emits_single_floor_z_pass_not_a_depth_cascade() {
    let resp = run_pipeline(
        PipelineRequest {
            project: tslot_project(tslot_tool(1, 12.0, 6.0), -3.0),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    assert!(
        resp.gcode.contains("; OP 1"),
        "missing op marker:\n{}",
        resp.gcode
    );
    let z_values: std::collections::HashSet<String> = resp
        .gcode
        .lines()
        .flat_map(|l| {
            l.split_whitespace()
                .filter_map(|t| t.strip_prefix('Z'))
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect();
    // Only the safe Z (5), the surface pre-plunge (0), and the single
    // floor Z (-3) may appear — never the cascade levels -1 / -2.
    for z in &z_values {
        assert!(
            ["5", "-3", "0"].contains(&z.as_str()),
            "T-slot emitted an unexpected Z {z} (depth cascade leaked?):\n{}",
            resp.gcode
        );
    }
    assert!(
        z_values.contains("-3"),
        "missing floor-Z undercut pass:\n{}",
        resp.gcode
    );
    assert!(
        !z_values.contains("-1") && !z_values.contains("-2"),
        "T-slot cascaded through intermediate depths (the bug):\n{}",
        resp.gcode
    );
}

/// 3g6u: a T-slot op always surfaces the stem-slot prerequisite note
/// (a T-slot cutter can't mill the narrow stem itself).
#[test]
fn tslot_op_emits_stem_slot_prerequisite_warning() {
    let resp = run_pipeline(
        PipelineRequest {
            project: tslot_project(tslot_tool(1, 12.0, 6.0), -7.0),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let hit = resp
        .warnings
        .iter()
        .find(|w| w.kind == "tslot_requires_stem_slot")
        .expect("expected tslot_requires_stem_slot warning");
    assert!(
        hit.message.contains("6.00 mm") && hit.message.contains("stem slot"),
        "warning should name the neck width + stem-slot prerequisite: {}",
        hit.message
    );
}

/// 3g6u: a T-slot op with a non-T-slot cutter warns `tool_kind_mismatch`
/// (no undercut head ⇒ it would just cut a plain centerline groove).
#[test]
fn tslot_op_with_plain_endmill_warns_kind_mismatch() {
    let resp = run_pipeline(
        PipelineRequest {
            project: tslot_project(endmill(1, 6.0), -5.0),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    assert!(
        resp.warnings.iter().any(|w| w.kind == "tool_kind_mismatch"),
        "expected tool_kind_mismatch for an endmill on a T-slot op; got {:?}",
        resp.warnings.iter().map(|w| &w.kind).collect::<Vec<_>>(),
    );
}

/// my03: per-op planning warnings must survive a pipeline CACHE HIT.
/// `run_pipeline` uses a process-global op cache, so a second identical
/// Generate serves the op from cache (skipping `build_op_offsets` / the
/// driver / `synthesize_op_setup`). Before the fix the warnings those
/// produce — including the 94sf-critical `tool_kind_mismatch` — were
/// dropped on the hit. We run the SAME mis-tooled project twice and
/// assert the warning is present BOTH times.
#[test]
fn per_op_warnings_resurface_on_cache_hit() {
    // Endmill on a T-slot op → tool_kind_mismatch (critical) plus the
    // tslot_requires_stem_slot note. A unique depth + geometry keeps
    // this project off every other test's cache key, so the first call
    // is a guaranteed miss (populates the global cache) and the second
    // a hit — no need to clear the shared cache (which would disturb
    // parallel tests). Even if the entry were evicted between the two
    // calls, the recompute would still raise the warning, so the test
    // never false-fails; it only fails if the hit path drops it.
    let mk = || PipelineRequest {
        project: tslot_project(endmill(1, 6.0), -2.71),
        post_processor: Some(PostProcessorKind::Linuxcnc),
    };
    let kinds = |r: &crate::pipeline::PipelineResponse| -> Vec<String> {
        r.warnings.iter().map(|w| w.kind.clone()).collect()
    };
    let first = run_pipeline(mk(), |_, _, _| {}).unwrap();
    assert!(
        first
            .warnings
            .iter()
            .any(|w| w.kind == "tool_kind_mismatch"),
        "first (cache-miss) run should raise tool_kind_mismatch; got {:?}",
        kinds(&first),
    );
    // Second run is served from the cache for this op.
    let second = run_pipeline(mk(), |_, _, _| {}).unwrap();
    assert!(
        second
            .warnings
            .iter()
            .any(|w| w.kind == "tool_kind_mismatch"),
        "cache HIT dropped the per-op critical warning (my03); got {:?}",
        kinds(&second),
    );
    // The non-critical prerequisite note must ride along too.
    assert!(
        second
            .warnings
            .iter()
            .any(|w| w.kind == "tslot_requires_stem_slot"),
        "cache HIT dropped tslot_requires_stem_slot; got {:?}",
        kinds(&second),
    );
}

// ───────────────────────────── b7qz: dovetail op ──────────────────
/// A dovetail bit (`FormProfile`, widest at the bottom face). The
/// form-profile samples run tip → top; the narrowest sample is the
/// neck, which sets the roughing-channel width the op warns about.
fn dovetail_tool(id: u32, dia: f64) -> ToolEntry {
    let mut t = endmill(id, dia);
    t.kind = ToolKind::FormProfile;
    // Widest (dia/2) at the tip, narrowing to dia/4 at the neck.
    t.form_profile_mm = vec![
        FormProfileSample {
            z_mm: 0.0,
            r_mm: dia / 2.0,
        },
        FormProfileSample {
            z_mm: 9.5,
            r_mm: dia / 4.0,
        },
    ];
    t
}

// `depth` parametrized so each test builds a DISTINCT project — see
// `tslot_project` for why (the process-global op cache).
fn dovetail_project(tool: ToolEntry, depth: f64) -> Project {
    let mut params = OpParams::mill_default();
    params.depth = depth; // a Profile/Pocket would cascade -1, -2, …
    params.start_depth = 0.0;
    params.fast_move_z = 5.0;
    params.step = Some(-1.0);
    Project {
        segments: closed_square_offset(20.0, 30.0, 30.0),
        machine: MachineConfig::default(),
        tools: vec![tool],
        operations: vec![Op {
            id: 1,
            name: "Dovetail".into(),
            enabled: true,
            kind: OpKind::Dovetail {
                contour: crate::project::ContourParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params,
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    }
}

/// b7qz: a dovetail op cuts the undercut as a SINGLE pass at the floor
/// Z — it must NOT cascade through intermediate depths the way a
/// Profile/Pocket would (the flank-at-every-depth cascade is the bug
/// this op kind fixes). Mirrors the T-slot sibling test.
#[test]
fn dovetail_op_emits_single_floor_z_pass_not_a_depth_cascade() {
    let resp = run_pipeline(
        PipelineRequest {
            project: dovetail_project(dovetail_tool(1, 12.0), -3.0),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    assert!(
        resp.gcode.contains("; OP 1"),
        "missing op marker:\n{}",
        resp.gcode
    );
    let z_values: std::collections::HashSet<String> = resp
        .gcode
        .lines()
        .flat_map(|l| {
            l.split_whitespace()
                .filter_map(|t| t.strip_prefix('Z'))
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect();
    for z in &z_values {
        assert!(
            ["5", "-3", "0"].contains(&z.as_str()),
            "dovetail emitted an unexpected Z {z} (depth cascade leaked?):\n{}",
            resp.gcode
        );
    }
    assert!(
        z_values.contains("-3"),
        "missing floor-Z undercut pass:\n{}",
        resp.gcode
    );
    assert!(
        !z_values.contains("-1") && !z_values.contains("-2"),
        "dovetail cascaded through intermediate depths (the bug):\n{}",
        resp.gcode
    );
}

/// b7qz: a dovetail op always surfaces the roughing-channel
/// prerequisite note (the angled flank can't be plunged through
/// solid stock), naming the profile's narrowest width.
#[test]
fn dovetail_op_emits_rough_channel_prerequisite_warning() {
    let resp = run_pipeline(
        PipelineRequest {
            project: dovetail_project(dovetail_tool(1, 12.0), -7.0),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let hit = resp
        .warnings
        .iter()
        .find(|w| w.kind == "dovetail_requires_rough_channel")
        .expect("expected dovetail_requires_rough_channel warning");
    // Narrowest sample r = 12/4 = 3.0 ⇒ width 6.00 mm.
    assert!(
        hit.message.contains("6.00 mm") && hit.message.contains("channel"),
        "warning should name the narrowest width + roughing-channel prerequisite: {}",
        hit.message
    );
}

/// b7qz: a dovetail op with a non-FormProfile cutter warns
/// `tool_kind_mismatch` (straight walls ⇒ no angled undercut flanks).
#[test]
fn dovetail_op_with_plain_endmill_warns_kind_mismatch() {
    let resp = run_pipeline(
        PipelineRequest {
            project: dovetail_project(endmill(1, 6.0), -5.0),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    assert!(
        resp.warnings.iter().any(|w| w.kind == "tool_kind_mismatch"),
        "expected tool_kind_mismatch for an endmill on a dovetail op; got {:?}",
        resp.warnings.iter().map(|w| &w.kind).collect::<Vec<_>>(),
    );
}

/// ldu2: a Profile on a circle that arrives TESSELLATED (many short
/// LINE segments, as SVG/DXF imports usually do) must not explode. The
/// source line run is arc-fit back into true arcs before the offset
/// cascade, so cavalier offsets a clean arc and the emitter produces a
/// handful of G2/G3 — not one move per tessellation segment plus a
/// per-vertex round-join arc. Before the fix this emitted thousands of
/// lines; here we assert it stays tiny and uses arcs.
fn tessellated_circle(cx: f64, cy: f64, r: f64, n: usize) -> Vec<Segment> {
    (0..n)
        .map(|i| {
            let a0 = std::f64::consts::TAU * (i as f64) / (n as f64);
            let a1 = std::f64::consts::TAU * ((i + 1) as f64) / (n as f64);
            Segment::line(
                crate::geometry::Point2::new(cx + r * a0.cos(), cy + r * a0.sin()),
                crate::geometry::Point2::new(cx + r * a1.cos(), cy + r * a1.sin()),
                "0",
                7,
            )
        })
        .collect()
}

fn tessellated_circle_profile_project(arcs: bool) -> Project {
    let mut params = OpParams::mill_default();
    params.depth = -2.0;
    params.start_depth = 0.0;
    params.step = Some(-1.0);
    let machine = MachineConfig {
        arcs,
        ..MachineConfig::default()
    };
    Project {
        segments: tessellated_circle(20.0, 20.0, 10.0, 180),
        machine,
        tools: vec![endmill(1, 3.0)],
        operations: vec![Op {
            id: 1,
            name: "Profile".into(),
            enabled: true,
            kind: OpKind::Profile {
                offset: ToolOffset::Outside,
                contour: crate::project::ContourParams::default(),
                profile: crate::project::ProfileParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params,
            group: None,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: crate::project::WorkOffset::default(),
        stock: None,
        relief_sources: Vec::new(),
    }
}

#[test]
fn ldu2_tessellated_circle_profile_collapses_to_arcs() {
    let resp = run_pipeline(
        PipelineRequest {
            project: tessellated_circle_profile_project(true),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .unwrap();
    let lines = resp.gcode.lines().count();
    let arc_moves = resp
        .gcode
        .lines()
        .filter(|l| l.starts_with("G2 ") || l.starts_with("G3 "))
        .count();
    // A 180-segment circle Profile (2 passes) collapses to a few arcs
    // per pass. Pre-fix this was ~2000+ lines; a generous ceiling that
    // still fails hard on the explosion:
    assert!(
        lines < 100,
        "tessellated-circle Profile should collapse (got {lines} gcode lines):\n{}",
        resp.gcode
    );
    assert!(
        arc_moves >= 2,
        "expected the fitted offset to emit G2/G3 arcs, got {arc_moves}:\n{}",
        resp.gcode
    );
}

/// ldu2 companion: the source fit is gated on `machine.arcs` (the same
/// flag as the emit-time fitter). With arcs off, the user opted into
/// pure-line output, so the tessellated circle is NOT collapsed and the
/// program stays large; with arcs on it collapses. Asserting the gap
/// (rather than brittle absolute counts) documents that the gate works.
#[test]
fn ldu2_source_fit_is_gated_on_arcs_flag() {
    let count = |arcs: bool| {
        run_pipeline(
            PipelineRequest {
                project: tessellated_circle_profile_project(arcs),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap()
        .gcode
        .lines()
        .count()
    };
    let on = count(true);
    let off = count(false);
    assert!(
        off > on * 4,
        "arcs=off (no source fit) should be far larger than arcs=on (collapsed): off={off} on={on}",
    );
}
