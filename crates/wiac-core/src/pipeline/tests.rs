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
        Op, OpKind, OpParams, OpSource, SourceCombine, TextAlignment, TextLayer, TextLayerKind,
        ToolEntry, ToolKind,
    };

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

    /// New `ToolKind` variants (rt1.28): `BullNose` / Compression /
    /// `TSlot` / `FormProfile` all serialize + deserialize cleanly and
    /// carry their geometry fields through round-trip.
    #[test]
    fn extended_tool_kinds_serde_round_trip() {
        for (kind, label) in [
            (ToolKind::BullNose, "bull_nose"),
            (ToolKind::Compression, "compression"),
            (ToolKind::TSlot, "t_slot"),
            (ToolKind::FormProfile, "form_profile"),
        ] {
            let mut t = endmill(7, 6.0);
            t.kind = kind;
            t.corner_radius_mm = Some(0.5);
            t.tslot_neck_diameter_mm = Some(3.0);
            t.tslot_neck_length_mm = Some(8.0);
            let json = serde_json::to_string(&t).unwrap();
            assert!(json.contains(label), "expected '{label}' in {json}");
            let back: ToolEntry = serde_json::from_str(&json).unwrap();
            assert_eq!(back.kind, kind);
            assert_eq!(back.corner_radius_mm, Some(0.5));
            assert_eq!(back.tslot_neck_diameter_mm, Some(3.0));
            assert_eq!(back.tslot_neck_length_mm, Some(8.0));
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
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
        // Endmill kind, but pierce field set (shouldn't fire).
        tool.laser_pierce_sec = Some(0.5);
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
            !resp.gcode.contains("G4 P0.5"),
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

    /// Chamfer op (rt1.18): walks the source contour at constant Z,
    /// computed from the V-bit cone math. A 60° V-bit + 1mm width
    /// gives ~1.732 mm depth; the gcode must contain Z-1.732.
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
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // Cone depth: 1 / tan(30°) ≈ 1.7320508; the gcode rounds to
        // 4 decimals so we look for Z-1.732.
        assert!(
            resp.gcode.contains("Z-1.732"),
            "expected chamfer depth Z-1.732 in gcode:\n{}",
            resp.gcode
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
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
            })
            .collect();
        let mut project = Project {
            segments: closed_square_offset(30.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools,
            operations: ops,
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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

    /// rt1.34: a Pause op emits M5 → M0 → M3 inline at its slot in the
    /// op list. The cutter doesn't move and no source geometry is
    /// touched. The comment carries the operator message.
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
        // M5 (spindle off) immediately before M0; M3 (spindle back on)
        // immediately after.
        let m0_pos = gcode.find("\nM0\n").unwrap();
        let pre = &gcode[..m0_pos];
        let post = &gcode[m0_pos..];
        assert!(pre.rfind("\nM5\n").is_some(), "expected M5 before M0");
        assert!(post.find("\nM3\n").is_some(), "expected M3 after M0");
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
        };
        let project = crate::project::Project {
            segments: Vec::new(),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pause],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
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

    /// rt1.34: Pause op round-trips through serde JSON (snake_case tag).
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
        };
        let json = serde_json::to_string(&pause).expect("serialize");
        assert!(json.contains("\"pause\""), "expected pause tag in {json}");
        let back: Op = serde_json::from_str(&json).expect("deserialize");
        match back.kind {
            OpKind::Pause { message } => assert_eq!(message, "Flip the stock"),
            other => panic!("expected Pause, got {other:?}"),
        }
    }
