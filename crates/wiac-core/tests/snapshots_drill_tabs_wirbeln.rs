//! 020s: Golden snapshot coverage for drill / tabs / wirbeln gcode.
//!
//! Pins the exact emitted gcode for four representative pipeline
//! configurations so future refactors can't silently change drill /
//! tab-handling / face-mill-overlay output (the P0 plunge-feed
//! regression filed against the May audit wouldn't have been caught
//! by current `assert!(contains("G81"))`-style tests).
//!
//! The fixtures cover (audit brief 020s):
//!  1. Drill + Stufenfase + ToolChange — verifies G81/G82 + the
//!     toolchange envelope (M5/M6/M3) + the rim chamfer revolution.
//!  2. Profile with Rectangle tabs + helix entry — verifies the
//!     tab lift sequence and the helical plunge.
//!  3. Profile with Ramp tabs — verifies the trapezoidal Z profile
//!     over the tab footprint.
//!  4. Wirbeln tool walking a closed contour — verifies the face-
//!     mill helical-spiral overlay (7z7w: misnamed historically as
//!     "wirbeln") produces dense G1 stamps with the cutter
//!     centerline displaced.
//!
//! **Updating the baselines:** when an intentional change shifts the
//! emitted gcode, set `WIAC_UPDATE_SNAPSHOTS=1` in the env and re-run
//! `cargo test -p wiac-core --test snapshots_drill_tabs_wirbeln`; the
//! test prints the new baseline string and fails so you can paste it
//! into the const below. Without the env var the test does a
//! byte-equal compare and fails on any drift.

use wiac_core::cam::setup::{MachineConfig, PlungeStrategy, TabType, TabsConfig, ToolOffset};
use wiac_core::geometry::{Point2, Segment, SegmentKind};
use wiac_core::pipeline::{run_pipeline, PipelineRequest, PostProcessorKind};
use wiac_core::project::{
    ContourParams, DrillCycle, Op, OpKind, OpParams, OpSource, PocketStrategy, ProfileParams,
    Project, TabPlacementMode, ToolEntry, ToolKind, WorkOffset,
};

/// Compare `actual` to `expected`. When `WIAC_UPDATE_SNAPSHOTS=1`,
/// print a code-paste-ready string literal of the actual output and
/// always fail so the developer can update the baseline. Otherwise
/// fall back to a strict byte-equal compare.
fn assert_snapshot(name: &str, actual: &str, expected: &str) {
    if std::env::var("WIAC_UPDATE_SNAPSHOTS").is_ok() {
        eprintln!("=== UPDATE SNAPSHOT [{name}] ===");
        eprintln!("let expected = \"\\");
        for line in actual.lines() {
            // Print each line with embedded quote/backslash escaping.
            let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
            eprintln!("{escaped}\\n\\");
        }
        eprintln!("\";");
        panic!("WIAC_UPDATE_SNAPSHOTS set — rerun without the env var after pasting the new baseline.");
    }
    assert_eq!(
        actual, expected,
        "[{name}] snapshot drift; rerun with WIAC_UPDATE_SNAPSHOTS=1 to refresh"
    );
}

fn closed_circle(center: Point2, radius: f64) -> Vec<Segment> {
    let p_right = Point2::new(center.x + radius, center.y);
    let p_left = Point2::new(center.x - radius, center.y);
    vec![
        Segment {
            kind: SegmentKind::Circle,
            start: p_right,
            end: p_left,
            bulge: 1.0,
            center: Some(center),
            layer: "0".into(),
            color: 7,
        },
        Segment {
            kind: SegmentKind::Circle,
            start: p_left,
            end: p_right,
            bulge: 1.0,
            center: Some(center),
            layer: "0".into(),
            color: 7,
        },
    ]
}

fn closed_square(side: f64, ox: f64, oy: f64) -> Vec<Segment> {
    vec![
        Segment::line(Point2::new(ox, oy), Point2::new(ox + side, oy), "0", 7),
        Segment::line(
            Point2::new(ox + side, oy),
            Point2::new(ox + side, oy + side),
            "0",
            7,
        ),
        Segment::line(
            Point2::new(ox + side, oy + side),
            Point2::new(ox, oy + side),
            "0",
            7,
        ),
        Segment::line(Point2::new(ox, oy + side), Point2::new(ox, oy), "0", 7),
    ]
}

fn endmill(id: u32, diameter: f64) -> ToolEntry {
    let mut t = ToolEntry::default();
    t.id = id;
    t.name = format!("{diameter:.1}mm endmill");
    t.kind = ToolKind::Endmill;
    t.diameter = diameter;
    t.tip_angle_deg = 60.0;
    t.flutes = 2;
    t.speed = 18_000;
    t.plunge_rate = 100;
    t.feed_rate = 800;
    t.pause = 1;
    t
}

fn vbit(id: u32, diameter: f64, tip_angle_deg: f64) -> ToolEntry {
    let mut t = ToolEntry::default();
    t.id = id;
    t.name = "V-bit".into();
    t.kind = ToolKind::VBit;
    t.diameter = diameter;
    t.tip_diameter = Some(0.1);
    t.tip_angle_deg = tip_angle_deg;
    t.flutes = 2;
    t.speed = 18_000;
    t.plunge_rate = 200;
    t.feed_rate = 1200;
    t.pause = 1;
    t
}

fn run_to_gcode(project: Project) -> String {
    let resp = run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .expect("pipeline ran");
    // Strip the project-wide preamble for stability — the (generated by
    // …) header carries the wiac version string which moves on every
    // release. Keep everything from the first `; OP` marker onward.
    if let Some(idx) = resp.gcode.find("; OP ") {
        resp.gcode[idx..].to_string()
    } else {
        resp.gcode
    }
}

/// 020s.a: drill + stufenfase rim chamfer + distinct finish tool.
/// Pins the G82 / G80 / M6 toolchange / rim G1 revolution sequence.
#[test]
fn snapshot_drill_with_stufenfase_and_toolchange() {
    let center = Point2::new(5.0, 7.0);
    let drill = endmill(1, 3.0);
    let mut finisher = vbit(2, 6.35, 90.0);
    finisher.flutes = 2;
    let machine = MachineConfig {
        supports_toolchange: true,
        ..MachineConfig::default()
    };
    let mut params = OpParams::mill_default();
    params.depth = -3.0;
    params.start_depth = 0.0;
    params.fast_move_z = 5.0;
    let project = Project {
        segments: closed_circle(center, 0.5),
        machine,
        tools: vec![drill, finisher],
        operations: vec![Op {
            id: 1,
            name: "DrillStufenfase".into(),
            enabled: true,
            kind: OpKind::Drill {
                cycle: DrillCycle::Simple { dwell_sec: 0.0 },
                chamfer_after_width_mm: Some(0.5),
                pattern: None,
            },
            tool_id: 1,
            finish_tool_id: Some(2),
            source: OpSource::All,
            params,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: WorkOffset::default(),
    };
    let actual = run_to_gcode(project);
    // Structural pin — the brief asks for a snapshot covering the op
    // block, not every line. We pin the characteristic markers + a
    // hash of the rest so byte-level shifts in formatting are
    // caught without making the test brittle to comment additions.
    assert!(actual.contains("; OP 1"), "missing op marker:\n{actual}");
    assert!(actual.contains("G81"), "missing G81 drill:\n{actual}");
    assert!(actual.contains("G80"), "missing G80 cancel:\n{actual}");
    assert!(actual.contains("T2 M6"), "missing toolchange:\n{actual}");
    assert!(
        actual.contains("M5") && actual.contains("M3"),
        "missing toolchange spindle envelope:\n{actual}",
    );
    assert!(
        actual.lines().any(|l| l.starts_with("G1") && l.contains("Z-")),
        "missing rim chamfer G1 Z move:\n{actual}",
    );
    // Full-snapshot pin: capture a stable digest of the gcode so a
    // single-line change anywhere in the op surfaces as a diff.
    let digest = seahash_hex(&actual);
    assert_snapshot(
        "drill_stufenfase_toolchange",
        &digest,
        EXPECTED_DRILL_STUFENFASE_TOOLCHANGE_DIGEST,
    );
}

/// 020s.b: profile with Rectangle tabs + helix entry.
/// Pins the helix plunge + tab-lift sequence.
#[test]
fn snapshot_profile_rectangle_tabs_and_helix_entry() {
    let mut params = OpParams::mill_default();
    params.depth = -2.0;
    params.step = Some(-2.0);
    params.start_depth = 0.0;
    params.fast_move_z = 5.0;
    params.plunge = PlungeStrategy::Helix {
        angle_deg: 5.0,
        radius_mm: None,
    };
    let mut contour = ContourParams::default();
    contour.tabs = TabsConfig {
        active: true,
        width: 4.0,
        height: 1.0,
        tab_type: TabType::Rectangle,
        ramp_angle_deg: 30.0,
    };
    contour.tab_mode = TabPlacementMode::Auto { count: 2 };
    let project = Project {
        segments: closed_square(20.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![Op {
            id: 1,
            name: "ProfileTabsHelix".into(),
            enabled: true,
            kind: OpKind::Profile {
                offset: ToolOffset::Outside,
                contour,
                profile: ProfileParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: WorkOffset::default(),
    };
    let actual = run_to_gcode(project);
    assert!(actual.contains("; OP 1"), "missing op marker:\n{actual}");
    // Helix entry → at least one G2/G3 arc on the plunge before the
    // first straight cut.
    assert!(
        actual.lines().any(|l| l.starts_with("G2 ") || l.starts_with("G3 ")),
        "missing helix entry arc:\n{actual}",
    );
    // Rectangle tab → cutter lifts to tab Z then drops back.
    // The op's tab_z = depth + height = -2 + 1 = -1, which renders
    // as "Z-1" in the gcode.
    assert!(
        actual.lines().filter(|l| l.contains("Z-1")).count() >= 1,
        "expected tab lift to Z-1:\n{actual}",
    );
    let digest = seahash_hex(&actual);
    assert_snapshot(
        "profile_rectangle_tabs_helix",
        &digest,
        EXPECTED_PROFILE_RECTANGLE_TABS_HELIX_DIGEST,
    );
}

/// 020s.c: profile with Ramp tabs.
/// Pins the trapezoid Z profile (ramp-up, plateau, ramp-down).
#[test]
fn snapshot_profile_ramp_tabs() {
    let mut params = OpParams::mill_default();
    params.depth = -2.0;
    params.step = Some(-2.0);
    params.start_depth = 0.0;
    params.fast_move_z = 5.0;
    let mut contour = ContourParams::default();
    contour.tabs = TabsConfig {
        active: true,
        width: 4.0,
        height: 1.0,
        tab_type: TabType::Ramp,
        ramp_angle_deg: 30.0,
    };
    contour.tab_mode = TabPlacementMode::Auto { count: 2 };
    let project = Project {
        segments: closed_square(20.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![endmill(1, 3.0)],
        operations: vec![Op {
            id: 1,
            name: "ProfileRampTabs".into(),
            enabled: true,
            kind: OpKind::Profile {
                offset: ToolOffset::Outside,
                contour,
                profile: ProfileParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: WorkOffset::default(),
    };
    let actual = run_to_gcode(project);
    assert!(actual.contains("; OP 1"), "missing op marker:\n{actual}");
    // Ramp tab uses G1 with XY + Z together (ramp up) — not just a
    // bare Z step.
    let xyz_moves = actual
        .lines()
        .filter(|l| l.starts_with("G1") && l.contains('Z') && (l.contains('X') || l.contains('Y')))
        .count();
    assert!(
        xyz_moves >= 2,
        "expected ≥2 ramp G1 X*Y*Z* moves; got {xyz_moves}:\n{actual}",
    );
    let digest = seahash_hex(&actual);
    assert_snapshot(
        "profile_ramp_tabs",
        &digest,
        EXPECTED_PROFILE_RAMP_TABS_DIGEST,
    );
}

/// 020s.d: face-mill overlay (historical name "wirbeln") on a closed
/// contour. Pins the dense G1 stamp output that the helical-spiral
/// overlay produces.
#[test]
fn snapshot_wirbeln_walks_closed_contour() {
    let mut tool = endmill(1, 3.0);
    tool.wirbeln = true;
    tool.wirbeln_extra_width_mm = Some(2.0);
    tool.wirbeln_stepover_mm = Some(2.0);
    tool.wirbeln_osc_mm = Some(0.0);
    let mut params = OpParams::mill_default();
    params.depth = -1.0;
    params.step = Some(-1.0);
    params.start_depth = 0.0;
    params.fast_move_z = 5.0;
    let project = Project {
        segments: closed_square(10.0, 0.0, 0.0),
        machine: MachineConfig::default(),
        tools: vec![tool],
        operations: vec![Op {
            id: 1,
            name: "WirbelnPocket".into(),
            enabled: true,
            kind: OpKind::Pocket {
                strategy: PocketStrategy::Cascade,
                contour: ContourParams::default(),
                pocket: wiac_core::project::PocketParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params,
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: WorkOffset::default(),
    };
    let actual = run_to_gcode(project);
    assert!(actual.contains("; OP 1"), "missing op marker:\n{actual}");
    // Wirbeln overlay emits many short G1 stamps along the chord —
    // verify the density (a non-wirbeln pocket would emit ~20 G1
    // moves; wirbeln produces hundreds).
    let g1_count = actual.lines().filter(|l| l.starts_with("G1 ")).count();
    assert!(
        g1_count >= 50,
        "expected ≥50 G1 stamps from the face-mill overlay; got {g1_count}",
    );
    let digest = seahash_hex(&actual);
    assert_snapshot(
        "wirbeln_closed_contour",
        &digest,
        EXPECTED_WIRBELN_CLOSED_CONTOUR_DIGEST,
    );
}

// ---------- baselines ----------
//
// Recorded digests; refresh with WIAC_UPDATE_SNAPSHOTS=1 when an
// intentional change shifts emitted gcode. The first run after this
// test was added captured these values from the live pipeline; each
// reflects the canonical output for the corresponding fixture.

const EXPECTED_DRILL_STUFENFASE_TOOLCHANGE_DIGEST: &str = "d3cf8dc400b85360";
const EXPECTED_PROFILE_RECTANGLE_TABS_HELIX_DIGEST: &str = "a8b7e7c57a3930a2";
const EXPECTED_PROFILE_RAMP_TABS_DIGEST: &str = "e27f51a03b597954";
const EXPECTED_WIRBELN_CLOSED_CONTOUR_DIGEST: &str = "0af2d9bbca3fb0f4";

/// Hex digest of `seahash` over the gcode bytes. Stable across
/// platforms; small enough to paste into a `const`.
fn seahash_hex(s: &str) -> String {
    use std::hash::Hasher;
    let mut h = seahash::SeaHasher::new();
    h.write(s.as_bytes());
    format!("{:016x}", h.finish())
}
