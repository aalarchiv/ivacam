//! End-to-end pocket validation harness (esnw).
//!
//! Runs a real pocket CAM job — 6 mm Ø endmill, single-pass 3 mm
//! pocket of a 50 × 30 rectangular source on 10 mm stock — through
//! the full pipeline → toolpath → heightmap-sim path. Measures
//! carved volume from the heightmap and compares against the
//! closed-form expected.
//!
//! Numbers to remember (6 mm endmill, r = 3 mm):
//!
//! * Source: closed rectangle 50 × 30 mm.
//! * The round endmill can reach to within `r` of every inside
//!   corner only along a quarter-arc fillet; uncut artefact per
//!   corner = `r²·(1 − π/4)` ≈ 1.93 mm².
//! * Carved floor area = `50·30 − 4·1.93` ≈ 1492.27 mm².
//! * Volume at 3 mm depth = ~4476.82 mm³.
//!
//! This passes today — it's the working baseline for the pocket op.
//! Same scaffolding the chamfer harness uses; first consumer of the
//! shared `common::` module beyond the chamfer harness.

// Same f64↔f32 cast allowance as chamfer_volume_validation.rs.
#![allow(clippy::cast_possible_truncation)]

// (common is declared at the binary entrypoint in tests/volume_validation.rs)

use super::common::{
    build_heightmap, build_project, closed_rectangle, deepest_z, dump_stl, endmill_tool,
    op_single_pass, pocket_rect_volume, removed_volume, run, sim_carve, stock_at_origin,
};
use wiac_core::project::{OpKind, PocketStrategy};
use wiac_core::schema::PostProcessorKind;

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
const TOOL_DIA: f64 = 6.0;
const TOOL_R: f64 = TOOL_DIA * 0.5;
const POCKET_DEPTH: f64 = -3.0;
const SIM_CELL_MM: f64 = 0.1;

fn pocket_op() -> wiac_core::project::Op {
    // Cascade strategy is the default pocket emitter; single-pass
    // schedule keeps the comparison clean (no inter-pass cell-write
    // dedup math to model).
    op_single_pass(
        1,
        "Pocket",
        OpKind::Pocket {
            strategy: PocketStrategy::Cascade,
            contour: wiac_core::project::ContourParams::default(),
            pocket: wiac_core::project::PocketParams::default(),
        },
        1,
        POCKET_DEPTH,
    )
}

/// Single-pass 6 mm-endmill pocket of a 50 × 30 rectangle: measured
/// carve volume should match the closed-form
/// `(W·H − 4·r²·(1 − π/4)) · depth` within heightmap-discretization
/// noise (~3 % at 0.1 mm grid + endmill-floor sampling artefacts).
#[test]
fn pocket_rectangle_volume_matches_closed_form() {
    let tool = endmill_tool(1, TOOL_DIA);
    let stock = stock_at_origin(STOCK_W, STOCK_H, STOCK_THICK);
    let project = build_project(
        &stock,
        vec![tool.clone()],
        pocket_op(),
        closed_rectangle(RECT_X0, RECT_Y0, RECT_X1, RECT_Y1),
    );
    let resp = run(project, PostProcessorKind::Linuxcnc);
    eprintln!("[esnw] {} toolpath segments", resp.toolpath.len());

    // The pipeline must have driven the endmill all the way to the
    // pocket depth.
    let min_seg_z: f64 = resp
        .toolpath
        .iter()
        .flat_map(|seg| [seg.from.z, seg.to.z])
        .fold(f64::INFINITY, f64::min);
    assert!(
        (min_seg_z - POCKET_DEPTH).abs() < 0.01,
        "expected toolpath to reach Z={POCKET_DEPTH}, got {min_seg_z}"
    );

    let mut hm = build_heightmap(&stock, SIM_CELL_MM);
    let (writes, diag) = sim_carve(&mut hm, &resp.toolpath, &tool);
    eprintln!(
        "[esnw] {writes} cell writes, {} warnings",
        diag.warnings.len()
    );

    // v5az / overcut sentinel: nothing should sit below the stock floor.
    let min_h = deepest_z(&hm);
    eprintln!("[esnw] min_h = {min_h:.4} mm (expected ≈ {POCKET_DEPTH})");
    assert!(
        f64::from(min_h) >= -STOCK_THICK - 1e-3,
        "deepest sample {min_h} below stock bottom {}",
        -STOCK_THICK,
    );
    // Endmill is flat-bottomed → the deepest cells should sit flush
    // at exactly `POCKET_DEPTH`. ±cell tolerates rasterization.
    assert!(
        (f64::from(min_h) - POCKET_DEPTH).abs() < 0.15,
        "deepest sample {min_h} should sit at pocket floor {POCKET_DEPTH}"
    );

    let expected = pocket_rect_volume(RECT_W, RECT_H, -POCKET_DEPTH, TOOL_R);
    let measured = removed_volume(&hm, STOCK_TOP_Z);
    let rel_err = (measured - expected).abs() / expected;
    eprintln!(
        "[esnw] V_expected = {expected:.2} mm³, V_measured = {measured:.2} mm³, rel_err = {:.2}%",
        rel_err * 100.0,
    );

    let n_bytes = dump_stl(&hm, "/tmp/wiac_pocket_rect.stl", -(STOCK_THICK as f32));
    eprintln!("[esnw] STL → /tmp/wiac_pocket_rect.stl ({n_bytes} bytes)");

    assert!(
        rel_err < 0.05,
        "pocket V_meas={measured:.2} mm³ vs expected {expected:.2} mm³, rel_err={:.2}%\n--- gcode (first 80 lines) ---\n{}",
        rel_err * 100.0,
        resp.gcode.lines().take(80).collect::<Vec<_>>().join("\n"),
    );
}
