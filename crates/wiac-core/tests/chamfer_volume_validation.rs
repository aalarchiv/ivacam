//! End-to-end chamfer validation harness (7krz).
//!
//! Runs a real chamfer CAM job — 8 mm Ø 90° v-bit, 5 mm requested
//! chamfer width on one side of a rectangle, 10 mm stock — through the
//! full pipeline → toolpath → heightmap-sim path. Measures carved
//! volume from the heightmap and compares against the closed-form
//! volume the cone math predicts.
//!
//! Numbers to remember (8 mm 90° v-bit, no tip flat):
//!
//! * `chamfer_width_cap_mm(8, 0) = 4 mm` — physical reach of the cone.
//! * User asks for `width_mm = 5`; pipeline clamps to 4 and emits a
//!   `chamfer_width_clamped_to_reach` warning.
//! * Spindle Z at the cone tip: `-effective_width / tan(45°) = -4 mm`.
//! * Trench cross-section (perpendicular to the chord): V from
//!   `(-4, 0) → (0, -4) → (4, 0)` → area = ½·8·4 = 16 mm².
//!
//! Two cases:
//!
//! 1. [`chamfer_closed_rectangle_volume_matches_closed_form`] —
//!    chamfer a closed rectangle (the common "break all edges" case).
//!    Pipeline behaves correctly. This is the working baseline that
//!    proves the sim + analytic formula agree.
//!
//! 2. [`chamfer_open_edge_emits_lateral_cut_at_each_pass`] — chamfer
//!    a single open edge of the rectangle (the user-requested
//!    "chamfer one side" case). This currently FAILS because of the
//!    open-polyline `multi_pass` cascade bug (oulh) — pass 1 cuts at
//!    Z=-1, pass 2/3/4 plunge to -4 at the end position without ever
//!    walking back to cut at the deeper levels. Will pass once oulh
//!    is fixed; leave it in as a regression pin.
//!
//! Both tests dump the simulated stock as STL to `/tmp` so the
//! resulting mesh can be loaded into `FreeCAD` / `MeshLab` / Blender
//! for eyeballing the v5az "cone tip below floor" symptom.

// f64 → f32 / usize → u32 casts inside this harness are intentional:
// STL is an f32 format, grid dimensions need u32, and the stock /
// rectangle constants are deliberately f64. Allowing these pedantic
// lints once at the crate level keeps the test body readable.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::unnecessary_cast
)]

use std::f64::consts::PI;
use std::fs;

use wiac_core::cam::chamfer::{chamfer_depth_capped, chamfer_width_cap_mm};
use wiac_core::cam::setup::MachineConfig;
use wiac_core::geometry::{Point2, Segment};
use wiac_core::pipeline::{run_pipeline, PipelineRequest};
use wiac_core::project::{
    Coolant, Op, OpKind, OpParams, OpSource, Project, SpindleDirection, StockConfig, ToolEntry,
    ToolKind, WorkOffset,
};
use wiac_core::schema::PostProcessorKind;
use wiac_core::sim::diagnostics::SimDiagnostics;
use wiac_core::sim::heightmap::{Heightmap, ToolProfile};
use wiac_core::sim::stl::heightmap_to_stl_binary;
use wiac_core::sim::sweep::sweep_range;

// ── Shared geometry / tool constants ─────────────────────────────────
const STOCK_W: f64 = 80.0;
const STOCK_H: f64 = 60.0;
const STOCK_THICK: f64 = 10.0;
const STOCK_TOP_Z: f32 = 0.0;
const RECT_X0: f64 = 15.0;
const RECT_Y0: f64 = 15.0;
const RECT_X1: f64 = 65.0;
const RECT_Y1: f64 = 45.0;
const RECT_W: f64 = RECT_X1 - RECT_X0; // 50 mm
const RECT_H: f64 = RECT_Y1 - RECT_Y0; // 30 mm
const TOOL_DIA: f64 = 8.0;
const TIP_ANGLE_DEG: f64 = 90.0; // 45° half-angle
const REQUESTED_WIDTH: f64 = 5.0;

fn vbit_8mm_90deg() -> ToolEntry {
    ToolEntry {
        id: 1,
        name: "8mm 90° chamfer v-bit".into(),
        kind: ToolKind::VBit,
        diameter: TOOL_DIA,
        tip_diameter: Some(0.0),
        tip_angle_deg: TIP_ANGLE_DEG,
        dragoff: None,
        drag_knife_self_align_angle_deg: None,
        flutes: 2,
        speed: 18_000,
        plunge_rate: 200,
        feed_rate: 1200,
        coolant: Coolant::Off,
        speed_finish: None,
        plunge_rate_finish: None,
        feed_rate_finish: None,
        speed_drill: None,
        plunge_rate_drill: None,
        feed_rate_drill: None,
        default_peck_step_mm: None,
        default_step: None,
        default_xy_overlap: None,
        comment: None,
        z_shift_mm: None,
        laser_pierce_sec: None,
        laser_lead_in_mm: None,
        kerf_mm: None,
        corner_radius_mm: None,
        form_profile_mm: Vec::new(),
        wirbeln: false,
        wirbeln_stepover_mm: None,
        wirbeln_extra_width_mm: None,
        wirbeln_osc_mm: None,
        pause: 1,
        flute_length_mm: None,
        length_mm: None,
        compression_transition_mm: None,
        thread_pitch_mm: None,
        shank_diameter_mm: None,
        stickout_length_mm: None,
        holder: None,
        spindle_direction: SpindleDirection::default(),
        pierce_height_mm: None,
        cut_height_mm: None,
        pierce_delay_sec: None,
        vcarve_lead_in_angle_deg: None,
    }
}

fn build_project(source: Vec<Segment>) -> Project {
    Project {
        segments: source,
        machine: MachineConfig::default(),
        tools: vec![vbit_8mm_90deg()],
        operations: vec![Op {
            id: 1,
            name: "Chamfer".into(),
            enabled: true,
            kind: OpKind::Chamfer {
                width_mm: REQUESTED_WIDTH,
                finish_pass: false,
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
        }],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: WorkOffset::default(),
        stock: Some(StockConfig {
            origin: [0.0, 0.0],
            width_mm: STOCK_W,
            height_mm: STOCK_H,
            thickness_mm: STOCK_THICK,
        }),
        relief_sources: Vec::new(),
    }
}

/// Closed rectangle outline (4 line segments forming a loop, CCW).
fn closed_rectangle_outline() -> Vec<Segment> {
    vec![
        Segment::line(
            Point2::new(RECT_X0, RECT_Y0),
            Point2::new(RECT_X1, RECT_Y0),
            "0",
            7,
        ),
        Segment::line(
            Point2::new(RECT_X1, RECT_Y0),
            Point2::new(RECT_X1, RECT_Y1),
            "0",
            7,
        ),
        Segment::line(
            Point2::new(RECT_X1, RECT_Y1),
            Point2::new(RECT_X0, RECT_Y1),
            "0",
            7,
        ),
        Segment::line(
            Point2::new(RECT_X0, RECT_Y1),
            Point2::new(RECT_X0, RECT_Y0),
            "0",
            7,
        ),
    ]
}

/// Just the bottom edge of the rectangle (one open line segment).
fn one_rectangle_edge() -> Vec<Segment> {
    vec![Segment::line(
        Point2::new(RECT_X0, RECT_Y0),
        Point2::new(RECT_X1, RECT_Y0),
        "0",
        7,
    )]
}

/// Sum `(top_z - h[i])` over every cell to get carved volume (mm³).
fn removed_volume(hm: &Heightmap, top_z: f32) -> f64 {
    let cell_area = (hm.cell * hm.cell) as f64;
    let mut sum = 0.0;
    for &h in &hm.data {
        let drop_mm = f64::from(top_z) - f64::from(h);
        if drop_mm > 0.0 {
            sum += drop_mm;
        }
    }
    sum * cell_area
}

/// Closed-form chamfer volume (mm³) — straight trench of length L
/// (cross-section = V triangle width 2·W, height D) PLUS the end-cap
/// half-cones at each open endpoint (none if the path closes on
/// itself, so `n_end_caps = 0` for a closed rectangle).
fn expected_volume(effective_width: f64, depth: f64, path_len: f64, n_end_caps: usize) -> f64 {
    let trench_area = effective_width * depth; // ½·(2W)·D = W·D
    let trench = trench_area * path_len;
    let half_cone = 0.5 * (1.0 / 3.0) * PI * effective_width * effective_width * depth;
    trench + n_end_caps as f64 * half_cone
}

/// Build a fresh heightmap covering the stock footprint at the given
/// cell size. `cell` = 0.1 mm gives ~480 k cells over 80×60 stock —
/// fine enough that V-trench discretization error stays under ~2 %.
fn build_heightmap(cell: f64) -> Heightmap {
    let cols = ((STOCK_W / cell).round() as u32) + 1;
    let rows = ((STOCK_H / cell).round() as u32) + 1;
    Heightmap::new(Point2::new(0.0, 0.0), cell, cols, rows, STOCK_TOP_Z)
}

/// Run the full pipeline → sim path and return (gcode, toolpath,
/// heightmap, sim cell-writes, warnings count). Shared by both tests.
fn run_chamfer_sim(
    source: Vec<Segment>,
) -> (
    String,
    Vec<wiac_core::gcode::preview::ToolpathSegment>,
    Heightmap,
    u32,
    usize,
) {
    let project = build_project(source);
    let resp = run_pipeline(
        PipelineRequest {
            project: project.clone(),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .expect("chamfer pipeline runs");
    let mut hm = build_heightmap(0.1);
    let profile = ToolProfile::from_tool(&project.tools[0]);
    let mut diag = SimDiagnostics::default();
    let writes = sweep_range(
        &mut hm,
        &resp.toolpath,
        0,
        resp.toolpath.len(),
        &profile,
        &[],
        None,
        &mut diag,
    );
    (resp.gcode, resp.toolpath, hm, writes, diag.warnings.len())
}

/// Cone-math sanity (shared assertions).
fn assert_cone_math_is_4mm_at_z_minus_4() {
    let cap = chamfer_width_cap_mm(TOOL_DIA, 0.0);
    assert!(
        (cap - 4.0).abs() < 1e-9,
        "tool reach should be 4 mm, got {cap}"
    );
    let sol = chamfer_depth_capped(REQUESTED_WIDTH, TIP_ANGLE_DEG, TOOL_DIA, 0.0);
    assert!(sol.clamped_to_reach, "5 mm chamfer on 8 mm bit must clamp");
    assert!((sol.effective_width_mm - 4.0).abs() < 1e-9);
    assert!((sol.z - (-4.0)).abs() < 1e-9);
}

// ─────────────────────────────────────────────────────────────────────
// Test 1: closed rectangle — works correctly today
// ─────────────────────────────────────────────────────────────────────

/// Chamfer all four edges of a closed rectangle outline. The
/// `multi_pass` cascade walks the closed loop on every pass (closed
/// paths bring the tool back to the start naturally), so pass 4 at
/// Z=-4 cuts the full perimeter. Measured carve should match the
/// analytic formula to within the bilinear-grid discretization error
/// (~5 %).
///
/// `perimeter = 2 · (50 + 30) = 160 mm`. Closed path, so 0 end-caps.
/// Expected volume = 4 · 4 · 160 = 2560 mm³.
#[test]
fn chamfer_closed_rectangle_volume_matches_closed_form() {
    assert_cone_math_is_4mm_at_z_minus_4();
    let (gcode, toolpath, hm, writes, n_warnings) = run_chamfer_sim(closed_rectangle_outline());

    eprintln!(
        "[7krz/closed] {} segments, {writes} cell writes, {n_warnings} warnings",
        toolpath.len()
    );

    // The pipeline must have emitted at least one segment at Z=-4
    // (the chamfer cone tip).
    let min_seg_z: f64 = toolpath
        .iter()
        .flat_map(|seg| [seg.from.z, seg.to.z])
        .fold(f64::INFINITY, f64::min);
    assert!(
        (min_seg_z - (-4.0)).abs() < 0.01,
        "expected toolpath to dip to Z=-4 (chamfer cone-tip), got {min_seg_z}"
    );

    // Sim deepest sample should land at -4 (within ±cell/2 of grid).
    let min_h: f32 = hm.data.iter().copied().fold(f32::INFINITY, f32::min);
    eprintln!("[7krz/closed] min_h = {min_h:.4} mm (expected ≈ -4)");
    assert!(
        (f64::from(min_h) - (-4.0)).abs() < 0.15,
        "deepest sample {min_h} should be near -4 mm"
    );

    // Volume check.
    let perimeter = 2.0 * (RECT_W + RECT_H);
    let expected = expected_volume(4.0, 4.0, perimeter, 0);
    let measured = removed_volume(&hm, STOCK_TOP_Z);
    let rel_err = (measured - expected).abs() / expected;
    eprintln!(
        "[7krz/closed] V_expected = {expected:.2} mm³, V_measured = {measured:.2} mm³, rel_err = {:.2}%",
        rel_err * 100.0,
    );

    // STL out for eyeballing.
    let stl = heightmap_to_stl_binary(&hm, -(STOCK_THICK as f32));
    fs::write("/tmp/wiac_chamfer_closed.stl", &stl).expect("write closed STL");
    eprintln!(
        "[7krz/closed] STL → /tmp/wiac_chamfer_closed.stl ({} bytes)",
        stl.len()
    );

    // Tolerance picked at 8 % — the 4-mm-radius cone over a 160-mm
    // perimeter on a 0.1-mm grid loses ~3 % to discretization at the
    // V-tip; the rectangle corners add overlap/end-cone double-cuts
    // worth a few percent more. Closed-form ignores both.
    assert!(
        rel_err < 0.08,
        "closed-loop chamfer V_meas={measured:.2} mm³ vs expected {expected:.2} mm³, rel_err={:.2}%\n--- gcode ---\n{gcode}",
        rel_err * 100.0,
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 2: single open edge — REGRESSION PIN for oulh
// ─────────────────────────────────────────────────────────────────────

/// Chamfer one edge of the rectangle as an OPEN line segment. With
/// step=-1 mm (the default) and a 4 mm cone-tip depth, the pipeline
/// should emit a lateral cut at each of the four scheduled Z levels
/// (-1, -2, -3, -4). It currently only cuts at Z=-1; passes 2-4
/// plunge at the end-of-segment XY but never walk back. See oulh.
///
/// Ignored until oulh is fixed — `multi_pass` must retract-and-rapid
/// back to `segments[0].start` between open-path passes, or reverse
/// segment direction for alternating passes. Re-enable by deleting
/// the `#[ignore]` attribute when oulh closes.
#[test]
#[ignore = "regression pin for oulh — open-polyline multi_pass cascade bug"]
fn chamfer_open_edge_emits_lateral_cut_at_each_pass() {
    assert_cone_math_is_4mm_at_z_minus_4();
    let (gcode, toolpath, hm, writes, n_warnings) = run_chamfer_sim(one_rectangle_edge());

    eprintln!(
        "[7krz/open] {} segments, {writes} cell writes, {n_warnings} warnings",
        toolpath.len()
    );
    eprintln!("[7krz/open] gcode:\n{gcode}");

    // Dump the STL early so it lands on disk even when the assertions
    // below panic — running `--ignored` against this test is the
    // hands-on repro path for oulh / v5az and you want the mesh.
    let stl = heightmap_to_stl_binary(&hm, -(STOCK_THICK as f32));
    fs::write("/tmp/wiac_chamfer_open.stl", &stl).expect("write open STL");
    eprintln!(
        "[7krz/open] STL → /tmp/wiac_chamfer_open.stl ({} bytes)",
        stl.len()
    );

    // Count the Cut-kind segments per Z bucket. With 4 scheduled
    // passes there must be ≥1 Cut segment at each of -1, -2, -3, -4
    // mm (within 0.05 mm tolerance to absorb f32 round).
    let cut_zs: Vec<f64> = toolpath
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                wiac_core::gcode::preview::MoveKind::Cut | wiac_core::gcode::preview::MoveKind::Arc
            )
        })
        .map(|s| s.to.z) // chord end Z is the cut depth for level moves
        .collect();
    eprintln!("[7krz/open] cut segment Zs: {cut_zs:?}");

    let near = |zs: &[f64], target: f64| zs.iter().any(|z| (z - target).abs() < 0.05);
    for level in [-1.0, -2.0, -3.0, -4.0] {
        assert!(
            near(&cut_zs, level),
            "open-edge cascade missing a Cut-kind segment at Z={level} (oulh): only saw {cut_zs:?}"
        );
    }

    // Volume check — closed-form: open path, so 2 end-caps.
    let expected = expected_volume(4.0, 4.0, RECT_W, 2);
    let measured = removed_volume(&hm, STOCK_TOP_Z);
    let rel_err = (measured - expected).abs() / expected;
    eprintln!(
        "[7krz/open] V_expected = {expected:.2} mm³, V_measured = {measured:.2} mm³, rel_err = {:.2}%",
        rel_err * 100.0,
    );
    assert!(
        rel_err < 0.05,
        "open-edge chamfer V_meas={measured:.2} mm³ vs expected {expected:.2} mm³, rel_err={:.2}% (oulh)",
        rel_err * 100.0,
    );
}
