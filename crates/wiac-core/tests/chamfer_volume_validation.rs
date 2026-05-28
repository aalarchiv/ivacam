//! End-to-end chamfer validation harness (7krz; refactored onto
//! `common::` scaffolding by esnw).
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
//!    a single open edge (the user-requested "chamfer one side"
//!    case). FAILS because of the open-polyline `multi_pass` cascade
//!    bug (oulh) — pass 1 cuts at Z=-1, pass 2/3/4 plunge to -4 at
//!    the end position without ever walking back. Will pass once
//!    oulh is fixed; left in as a regression pin.

// Test crosses f64 (geometry) ↔ f32 (heightmap/STL) at a few cast
// sites; allowed at file scope same as the common harness.
#![allow(clippy::cast_possible_truncation)]

mod common;

use common::{
    build_heightmap, build_project, chamfer_volume, closed_rectangle, deepest_z, dump_stl, line,
    op_single_pass, removed_volume, run, sim_carve, stock_at_origin, vbit_tool,
};
use wiac_core::cam::chamfer::{chamfer_depth_capped, chamfer_width_cap_mm};
use wiac_core::project::OpKind;
use wiac_core::schema::PostProcessorKind;

// ── Geometry / tool constants ─────────────────────────────────────────
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
const SIM_CELL_MM: f64 = 0.1;

/// Build the standard chamfer op used by both tests below.
fn chamfer_op() -> wiac_core::project::Op {
    let mut op = op_single_pass(
        1,
        "Chamfer",
        OpKind::Chamfer {
            width_mm: REQUESTED_WIDTH,
            finish_pass: false,
        },
        1,
        -2.0, // setup_resolver overrides this to the cone-tip Z
    );
    // Step is intentionally smaller than the cone-tip depth so the
    // multi-pass cascade triggers — the chamfer comment in
    // setup_resolver explicitly says step-down is kept "so a V-bit
    // ramps in gently".
    op.params.step = Some(-1.0);
    op
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
/// Z=-4 cuts the full perimeter. Measured carve matches the analytic
/// formula within the bilinear-grid discretization error (~5 %).
///
/// `perimeter = 2 · (50 + 30) = 160 mm`. Closed path → 0 end-caps.
/// Expected volume = 4 · 4 · 160 = 2560 mm³.
#[test]
fn chamfer_closed_rectangle_volume_matches_closed_form() {
    assert_cone_math_is_4mm_at_z_minus_4();
    let tool = vbit_tool(1, TOOL_DIA, TIP_ANGLE_DEG, 0.0);
    let stock = stock_at_origin(STOCK_W, STOCK_H, STOCK_THICK);
    let project = build_project(
        &stock,
        vec![tool.clone()],
        chamfer_op(),
        closed_rectangle(RECT_X0, RECT_Y0, RECT_X1, RECT_Y1),
    );
    let resp = run(project, PostProcessorKind::Linuxcnc);
    eprintln!("[7krz/closed] {} toolpath segments", resp.toolpath.len());

    // The pipeline must have emitted at least one segment at Z=-4.
    let min_seg_z: f64 = resp
        .toolpath
        .iter()
        .flat_map(|seg| [seg.from.z, seg.to.z])
        .fold(f64::INFINITY, f64::min);
    assert!(
        (min_seg_z - (-4.0)).abs() < 0.01,
        "expected toolpath to dip to Z=-4 (chamfer cone-tip), got {min_seg_z}"
    );

    let mut hm = build_heightmap(&stock, SIM_CELL_MM);
    let (writes, diag) = sim_carve(&mut hm, &resp.toolpath, &tool);
    eprintln!(
        "[7krz/closed] {writes} cell writes, {} warnings",
        diag.warnings.len()
    );

    let min_h = deepest_z(&hm);
    eprintln!("[7krz/closed] min_h = {min_h:.4} mm (expected ≈ -4)");
    assert!(
        (f64::from(min_h) - (-4.0)).abs() < 0.15,
        "deepest sample {min_h} should be near -4 mm"
    );
    assert!(
        f64::from(min_h) >= -STOCK_THICK - 1e-3,
        "stock-floor sentinel (v5az): {min_h} below stock bottom {}",
        -STOCK_THICK,
    );

    let perimeter = 2.0 * (RECT_W + RECT_H);
    let expected = chamfer_volume(4.0, 4.0, perimeter, 0);
    let measured = removed_volume(&hm, STOCK_TOP_Z);
    let rel_err = (measured - expected).abs() / expected;
    eprintln!(
        "[7krz/closed] V_expected = {expected:.2} mm³, V_measured = {measured:.2} mm³, rel_err = {:.2}%",
        rel_err * 100.0,
    );

    let n_bytes = dump_stl(&hm, "/tmp/wiac_chamfer_closed.stl", -(STOCK_THICK as f32));
    eprintln!("[7krz/closed] STL → /tmp/wiac_chamfer_closed.stl ({n_bytes} bytes)");

    assert!(
        rel_err < 0.08,
        "closed-loop chamfer V_meas={measured:.2} mm³ vs expected {expected:.2} mm³, rel_err={:.2}%\n--- gcode ---\n{}",
        rel_err * 100.0,
        resp.gcode,
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 2: single open edge — REGRESSION PIN for oulh
// ─────────────────────────────────────────────────────────────────────

/// Chamfer one edge of the rectangle as an OPEN line segment. With
/// `step = -1 mm` (the default) and a 4 mm cone-tip depth, the
/// pipeline emits a lateral cut at each of the four scheduled Z
/// levels (-1, -2, -3, -4). Before oulh's fix, only the first pass
/// at Z=-1 cut laterally; passes 2-4 plunged at the segment's
/// trailing endpoint without ever walking back. The fix has
/// `multi_pass` alternate walk direction between passes for open
/// polylines, so the trailing endpoint becomes the next pass's
/// starting point — no XY retract / rapid needed.
#[test]
fn chamfer_open_edge_emits_lateral_cut_at_each_pass() {
    assert_cone_math_is_4mm_at_z_minus_4();
    let tool = vbit_tool(1, TOOL_DIA, TIP_ANGLE_DEG, 0.0);
    let stock = stock_at_origin(STOCK_W, STOCK_H, STOCK_THICK);
    let project = build_project(
        &stock,
        vec![tool.clone()],
        chamfer_op(),
        line((RECT_X0, RECT_Y0), (RECT_X1, RECT_Y0)),
    );
    let resp = run(project, PostProcessorKind::Linuxcnc);
    eprintln!("[7krz/open] {} toolpath segments", resp.toolpath.len());
    eprintln!("[7krz/open] gcode:\n{}", resp.gcode);

    let mut hm = build_heightmap(&stock, SIM_CELL_MM);
    let (writes, diag) = sim_carve(&mut hm, &resp.toolpath, &tool);
    eprintln!(
        "[7krz/open] {writes} cell writes, {} warnings",
        diag.warnings.len()
    );

    // Dump the STL early so the mesh lands on disk even when the
    // assertions below panic — `--ignored` against this test is the
    // hands-on repro path for oulh / v5az.
    let n_bytes = dump_stl(&hm, "/tmp/wiac_chamfer_open.stl", -(STOCK_THICK as f32));
    eprintln!("[7krz/open] STL → /tmp/wiac_chamfer_open.stl ({n_bytes} bytes)");

    // Count Cut-kind segments per Z bucket. With 4 scheduled passes
    // there must be ≥1 Cut segment at each of -1, -2, -3, -4 mm
    // (±0.05 mm to absorb f32 round).
    let cut_zs: Vec<f64> = resp
        .toolpath
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                wiac_core::gcode::preview::MoveKind::Cut | wiac_core::gcode::preview::MoveKind::Arc
            )
        })
        .map(|s| s.to.z)
        .collect();
    eprintln!("[7krz/open] cut segment Zs: {cut_zs:?}");

    let near = |zs: &[f64], target: f64| zs.iter().any(|z| (z - target).abs() < 0.05);
    for level in [-1.0, -2.0, -3.0, -4.0] {
        assert!(
            near(&cut_zs, level),
            "open-edge cascade missing a Cut-kind segment at Z={level} (oulh): only saw {cut_zs:?}"
        );
    }

    let expected = chamfer_volume(4.0, 4.0, RECT_W, 2);
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
