//! End-to-end Profile-Outside validation harness (me1m).
//!
//! Runs a real outside-profile CAM job — 6 mm Ø endmill, single-pass
//! 3 mm cut around a closed circle of radius 20 mm on 10 mm stock —
//! through the full pipeline → toolpath → heightmap-sim path.
//! Measures the carved annular volume and compares against the
//! closed-form expected.
//!
//! Numbers to remember (6 mm endmill, R = 20 mm source radius):
//!
//! * Tool centerline rides at `R + r = 23 mm` for `Outside` offset.
//! * Carved annulus runs from `R = 20` (source edge) to `R + 2r = 26`.
//! * Annular area = `π · (26² − 20²)` ≈ 866.96 mm².
//! * Volume at 3 mm depth ≈ 2600.88 mm³.
//!
//! Picked a CIRCLE source so the closed-form has no rounded-corner
//! ambiguity — the math is exact. Future Profile-on-rectangle tests
//! can pin down corner-handling semantics separately.

// Same f64↔f32 cast allowance as the other harness tests.
#![allow(clippy::cast_possible_truncation)]

mod common;

use common::{
    build_heightmap, build_project, closed_circle, deepest_z, dump_stl, endmill_tool,
    op_single_pass, profile_outside_circle_volume, removed_volume, run, sim_carve, stock_at_origin,
};
use wiac_core::cam::setup::ToolOffset;
use wiac_core::project::{ContourParams, OpKind, ProfileParams};
use wiac_core::schema::PostProcessorKind;

const STOCK_W: f64 = 80.0;
const STOCK_H: f64 = 70.0;
const STOCK_THICK: f64 = 10.0;
const STOCK_TOP_Z: f32 = 0.0;
const CIRCLE_CX: f64 = 40.0;
const CIRCLE_CY: f64 = 35.0;
const CIRCLE_R: f64 = 20.0;
const TOOL_DIA: f64 = 6.0;
const TOOL_R: f64 = TOOL_DIA * 0.5;
const PROFILE_DEPTH: f64 = -3.0;
const SIM_CELL_MM: f64 = 0.1;

fn profile_outside_op() -> wiac_core::project::Op {
    op_single_pass(
        1,
        "Profile-Outside",
        OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: ContourParams::default(),
            profile: ProfileParams::default(),
        },
        1,
        PROFILE_DEPTH,
    )
}

/// Single-pass 6 mm-endmill outside profile around a 20 mm-radius
/// circle: measured carve must match the closed-form annulus
/// `π · ((R + 2r)² − R²) · depth` within heightmap-discretization
/// noise. Endmill is flat-bottomed so the floor is exact and most
/// error comes from rasterizing the circular boundary at 0.1 mm.
#[test]
fn profile_outside_circle_volume_matches_closed_form() {
    let tool = endmill_tool(1, TOOL_DIA);
    let stock = stock_at_origin(STOCK_W, STOCK_H, STOCK_THICK);
    let project = build_project(
        &stock,
        vec![tool.clone()],
        profile_outside_op(),
        closed_circle(CIRCLE_CX, CIRCLE_CY, CIRCLE_R),
    );
    let resp = run(project, PostProcessorKind::Linuxcnc);
    eprintln!("[me1m/profile] {} toolpath segments", resp.toolpath.len());

    // Pipeline must drive the endmill to the profile depth.
    let min_seg_z: f64 = resp
        .toolpath
        .iter()
        .flat_map(|seg| [seg.from.z, seg.to.z])
        .fold(f64::INFINITY, f64::min);
    assert!(
        (min_seg_z - PROFILE_DEPTH).abs() < 0.01,
        "expected toolpath to reach Z={PROFILE_DEPTH}, got {min_seg_z}"
    );

    let mut hm = build_heightmap(&stock, SIM_CELL_MM);
    let (writes, diag) = sim_carve(&mut hm, &resp.toolpath, &tool);
    eprintln!(
        "[me1m/profile] {writes} cell writes, {} warnings",
        diag.warnings.len()
    );

    let min_h = deepest_z(&hm);
    eprintln!("[me1m/profile] min_h = {min_h:.4} mm (expected ≈ {PROFILE_DEPTH})");
    assert!(
        f64::from(min_h) >= -STOCK_THICK - 1e-3,
        "stock-floor sentinel (v5az): {min_h} below stock bottom {}",
        -STOCK_THICK,
    );
    assert!(
        (f64::from(min_h) - PROFILE_DEPTH).abs() < 0.15,
        "deepest sample {min_h} should sit at profile floor {PROFILE_DEPTH}"
    );

    let expected = profile_outside_circle_volume(CIRCLE_R, TOOL_R, -PROFILE_DEPTH);
    let measured = removed_volume(&hm, STOCK_TOP_Z);
    let rel_err = (measured - expected).abs() / expected;
    eprintln!(
        "[me1m/profile] V_expected = {expected:.2} mm³, V_measured = {measured:.2} mm³, rel_err = {:.2}%",
        rel_err * 100.0,
    );

    let n_bytes = dump_stl(
        &hm,
        "/tmp/wiac_profile_outside_circle.stl",
        -(STOCK_THICK as f32),
    );
    eprintln!("[me1m/profile] STL → /tmp/wiac_profile_outside_circle.stl ({n_bytes} bytes)");

    assert!(
        rel_err < 0.03,
        "profile-outside V_meas={measured:.2} mm³ vs expected {expected:.2} mm³, rel_err={:.2}%",
        rel_err * 100.0,
    );
}
