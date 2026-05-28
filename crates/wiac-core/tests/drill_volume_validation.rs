//! End-to-end Drill validation harness (me1m).
//!
//! Runs a real drill CAM job — 5 mm Ø drill, three point sources, 5 mm
//! through-hole into 10 mm stock — and asserts the carved volume equals
//! `3 · π · r² · depth` within tolerance.
//!
//! Numbers to remember (5 mm 118° drill, r = 2.5 mm, hole depth 5 mm):
//!
//! * The pipeline plunges past the shoulder depth by
//!   `r / tan(118° / 2) ≈ 1.50 mm` so the cone tip clears the hole
//!   bottom — standard CAM convention. The sim carves a flat
//!   cylinder to that plane.
//! * Per-hole carved cylinder = `π · 2.5² · (5 + 1.50)` ≈ 127.69 mm³.
//! * 3 holes ≈ 383.06 mm³ total.
//!
//! Catches: tip-cone math, per-hole positioning, plunge depth,
//! drill-tool sim profile (flat-bottom cylinder regardless of tip
//! angle), and the v5az floor sentinel.

#![allow(clippy::cast_possible_truncation)]

mod common;

use common::{
    build_heightmap, build_project, deepest_z, drill_tool, drill_volume, dump_stl, op_single_pass,
    points, removed_volume, run, sim_carve, stock_at_origin,
};
use wiac_core::project::{DrillCycle, OpKind};
use wiac_core::schema::PostProcessorKind;

const STOCK_W: f64 = 80.0;
const STOCK_H: f64 = 60.0;
const STOCK_THICK: f64 = 10.0;
const STOCK_TOP_Z: f32 = 0.0;
const TOOL_DIA: f64 = 5.0;
const TOOL_R: f64 = TOOL_DIA * 0.5;
const TIP_ANGLE_DEG: f64 = 118.0; // standard twist drill
const DRILL_DEPTH: f64 = -5.0;
const SIM_CELL_MM: f64 = 0.1;

const HOLES: [(f64, f64); 3] = [(20.0, 30.0), (40.0, 30.0), (60.0, 30.0)];

fn drill_op() -> wiac_core::project::Op {
    op_single_pass(
        1,
        "Drill",
        OpKind::Drill {
            cycle: DrillCycle::Simple { dwell_sec: 0.0 },
            chamfer_after_width_mm: None,
            pattern: None,
            spot_first: None,
        },
        1,
        DRILL_DEPTH,
    )
}

/// 5 mm drill at three points → carved volume must match
/// `3 · π · r² · depth = 294.52 mm³` within heightmap-rasterization
/// noise (~3 % at 0.1 mm grid against a circular hole boundary).
#[test]
fn drill_three_holes_volume_matches_closed_form() {
    let tool = drill_tool(1, TOOL_DIA, TIP_ANGLE_DEG);
    let stock = stock_at_origin(STOCK_W, STOCK_H, STOCK_THICK);
    let project = build_project(&stock, vec![tool.clone()], drill_op(), points(&HOLES));
    let resp = run(project, PostProcessorKind::Linuxcnc);
    eprintln!("[me1m/drill] {} toolpath segments", resp.toolpath.len());

    // Standard CAM: tip cone clears the requested shoulder depth, so
    // the tool tip plunges past `DRILL_DEPTH` by `r / tan(angle/2)`.
    let tip_extra = TOOL_R / (TIP_ANGLE_DEG * 0.5).to_radians().tan();
    let expected_tip_z = DRILL_DEPTH - tip_extra;
    let min_seg_z: f64 = resp
        .toolpath
        .iter()
        .flat_map(|seg| [seg.from.z, seg.to.z])
        .fold(f64::INFINITY, f64::min);
    assert!(
        (min_seg_z - expected_tip_z).abs() < 0.01,
        "expected toolpath to reach tip Z={expected_tip_z}, got {min_seg_z}"
    );

    let mut hm = build_heightmap(&stock, SIM_CELL_MM);
    let (writes, diag) = sim_carve(&mut hm, &resp.toolpath, &tool);
    eprintln!(
        "[me1m/drill] {writes} cell writes, {} warnings",
        diag.warnings.len()
    );

    let min_h = deepest_z(&hm);
    eprintln!("[me1m/drill] min_h = {min_h:.4} mm (expected ≈ {expected_tip_z:.4})");
    assert!(
        f64::from(min_h) >= -STOCK_THICK - 1e-3,
        "stock-floor sentinel (v5az): {min_h} below stock bottom {}",
        -STOCK_THICK,
    );
    assert!(
        (f64::from(min_h) - expected_tip_z).abs() < 0.15,
        "deepest sample {min_h} should sit at tip Z {expected_tip_z}"
    );

    let expected = drill_volume(HOLES.len(), TOOL_R, -DRILL_DEPTH, TIP_ANGLE_DEG);
    let measured = removed_volume(&hm, STOCK_TOP_Z);
    let rel_err = (measured - expected).abs() / expected;
    eprintln!(
        "[me1m/drill] V_expected = {expected:.2} mm³, V_measured = {measured:.2} mm³, rel_err = {:.2}%",
        rel_err * 100.0,
    );

    let n_bytes = dump_stl(&hm, "/tmp/wiac_drill_three.stl", -(STOCK_THICK as f32));
    eprintln!("[me1m/drill] STL → /tmp/wiac_drill_three.stl ({n_bytes} bytes)");

    assert!(
        rel_err < 0.03,
        "drill V_meas={measured:.2} mm³ vs expected {expected:.2} mm³, rel_err={:.2}%",
        rel_err * 100.0,
    );
}
