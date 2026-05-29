//! v5az programmatic repro: chamfer cone tip rendered/carved BELOW
//! surrounding pocket floor.
//!
//! Replicates the user's screenshot scenario:
//! * A pocket op clears the rectangle to Z = -2 mm with an endmill.
//! * A chamfer op then walks the same closed rectangle with a Kegel
//!   (cone) tool, 45° tip angle, width 0.5 mm.
//!
//! Closed-form expectations:
//! * `chamfer_depth_capped(0.5, 45°, 6 mm, 0)` → `effective_width` = 0.5
//!   (well under the 3 mm reach cap), cone-tip Z =
//!   `-0.5 / tan(22.5°) ≈ -1.2071 mm`.
//! * Cells INSIDE the pocket already sit at Z = -2 from the pocket
//!   op. The chamfer's cone tip at Z ≈ -1.21 is SHALLOWER than that,
//!   so a correct sim should leave them at -2 — `sweep_segment`
//!   lowers cells, never raises them.
//! * Cells OUTSIDE the rectangle (in unmilled stock at Z = 0) are
//!   what the chamfer actually carves: a V-bevel from Z = 0 at the
//!   rim down to ≈ -1.21 at the source line.
//!
//! v5az's screenshot showed the cone tip carved BELOW the pocket
//! floor — i.e. cells with Z < -2 — which is exactly the assertion
//! this test runs. Before oulh's fix the open-polyline cascade left
//! a deep cone-tip pit at the path endpoint; after the fix the
//! closed-rectangle path no longer hits that bug. This test pins
//! the result so any future regression that re-deepens the pocket
//! floor under the chamfer fires the assertion.
//!
//! Dumps `/tmp/wiac_v5az_chamfer_after_pocket.stl` so the resulting
//! mesh can be loaded into `FreeCAD` / `MeshLab` for eye-checking.

#![allow(clippy::cast_possible_truncation)]

mod common;

use common::{
    build_heightmap, closed_rectangle, deepest_z, dump_stl, endmill_tool, op_single_pass,
    removed_volume, run, stock_at_origin, vbit_tool,
};
use wiac_core::cam::chamfer::chamfer_depth_capped;
use wiac_core::cam::setup::{MachineConfig, ToolOffset};
use wiac_core::pipeline::{run_pipeline, PipelineRequest};
use wiac_core::project::{
    ContourParams, Op, OpKind, OpParams, OpSource, PocketParams, PocketStrategy, ProfileParams,
    Project, ToolKind, WorkOffset,
};
use wiac_core::schema::PostProcessorKind;
use wiac_core::sim::diagnostics::SimDiagnostics;
use wiac_core::sim::heightmap::ToolProfile;
use wiac_core::sim::sweep::sweep_range;

const STOCK_W: f64 = 80.0;
const STOCK_H: f64 = 60.0;
const STOCK_THICK: f64 = 10.0;
const STOCK_TOP_Z: f32 = 0.0;
const RECT_X0: f64 = 15.0;
const RECT_Y0: f64 = 15.0;
const RECT_X1: f64 = 65.0;
const RECT_Y1: f64 = 45.0;
const POCKET_TOOL_DIA: f64 = 6.0;
const POCKET_DEPTH: f64 = -2.0;
const CHAMFER_TOOL_DIA: f64 = 6.0;
const CHAMFER_TIP_ANGLE_DEG: f64 = 45.0; // very pointy cone
const CHAMFER_WIDTH: f64 = 0.5;
const SIM_CELL_MM: f64 = 0.1;

/// Build a Kegel (cone) tool from the v-bit builder — `from_tool`
/// maps Kegel → `ToolProfile::VBit` so the sim behavior is
/// identical to a V-bit of the same geometry.
fn cone_tool(id: u32, diameter_mm: f64, tip_angle_deg: f64) -> wiac_core::project::ToolEntry {
    let mut t = vbit_tool(id, diameter_mm, tip_angle_deg, 0.0);
    t.kind = ToolKind::Kegel;
    t.name = format!("{diameter_mm}mm {tip_angle_deg}° cone");
    t
}

/// Closed-loop chamfer of a rectangle outline AFTER an endmill
/// pocket has cleared the interior to Z = -2 mm. The chamfer's
/// cone-tip Z (≈ -1.21 mm) sits well above the pocket floor, so the
/// post-chamfer heightmap must not have any cell below -2 mm. Any
/// dip below -2 is the v5az "cone tip below surrounding floor" bug.
#[test]
fn chamfer_after_pocket_does_not_dip_below_pocket_floor() {
    // ── Cone math sanity ─────────────────────────────────────────────
    let sol = chamfer_depth_capped(CHAMFER_WIDTH, CHAMFER_TIP_ANGLE_DEG, CHAMFER_TOOL_DIA, 0.0);
    eprintln!(
        "[v5az] chamfer: effective_width={:.4}, z={:.4}, cap={:.4}",
        sol.effective_width_mm, sol.z, sol.width_cap_mm,
    );
    assert!(!sol.clamped_to_reach, "width 0.5 mm should fit a 6 mm cone");
    assert!(
        (sol.z - (-1.2071)).abs() < 1e-3,
        "cone-tip Z should be ≈ -1.2071, got {}",
        sol.z,
    );

    // ── Build the two-op project ─────────────────────────────────────
    let endmill = endmill_tool(1, POCKET_TOOL_DIA);
    let cone = cone_tool(2, CHAMFER_TOOL_DIA, CHAMFER_TIP_ANGLE_DEG);
    let stock = stock_at_origin(STOCK_W, STOCK_H, STOCK_THICK);

    let pocket = op_single_pass(
        1,
        "Pocket",
        OpKind::Pocket {
            strategy: PocketStrategy::Cascade,
            contour: ContourParams::default(),
            pocket: PocketParams::default(),
        },
        1,
        POCKET_DEPTH,
    );
    // Use step = -1 mm so the cascade emits a TWO-pass schedule
    // ([-1, -1.2071]) — this is what the user almost certainly had
    // when they took the v5az screenshot (the mill_default step).
    // Pinning the multi-pass path catches any cascade-induced
    // overshoot that a single-pass test would silently miss.
    let chamfer = Op {
        id: 2,
        name: "Chamfer".into(),
        enabled: true,
        kind: OpKind::Chamfer {
            width_mm: CHAMFER_WIDTH,
            finish_pass: false,
        },
        tool_id: 2,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams {
            depth: -2.0, // ignored — setup_resolver forces depth = sol.z
            start_depth: 0.0,
            step: Some(-1.0),
            fast_move_z: 5.0,
            ..OpParams::default()
        },
        group: None,
    };

    let project = Project {
        segments: closed_rectangle(RECT_X0, RECT_Y0, RECT_X1, RECT_Y1),
        machine: MachineConfig::default(),
        tools: vec![endmill.clone(), cone.clone()],
        operations: vec![pocket, chamfer],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: WorkOffset::default(),
        stock: Some(stock.clone()),
        relief_sources: Vec::new(),
    };

    let resp = run_pipeline(
        PipelineRequest {
            project: project.clone(),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        },
        |_, _, _| {},
    )
    .expect("pipeline runs");
    eprintln!(
        "[v5az] pipeline: {} toolpath segments, {} warnings",
        resp.toolpath.len(),
        resp.warnings.len(),
    );

    // ── Per-op sim — pocket first, then chamfer on the same hm ───────
    let mut hm = build_heightmap(&stock, SIM_CELL_MM);

    // Find the boundary between op 1 (pocket) and op 2 (chamfer)
    // segments. The toolpath stream carries op_id on each segment via
    // ToolpathSegment.op_id; group by that.
    let pocket_segs: Vec<_> = resp
        .toolpath
        .iter()
        .filter(|s| s.op_id == 1)
        .cloned()
        .collect();
    let chamfer_segs: Vec<_> = resp
        .toolpath
        .iter()
        .filter(|s| s.op_id == 2)
        .cloned()
        .collect();
    eprintln!(
        "[v5az] pocket has {} segments; chamfer has {}",
        pocket_segs.len(),
        chamfer_segs.len(),
    );
    assert!(!pocket_segs.is_empty(), "pocket op produced no segments");
    assert!(!chamfer_segs.is_empty(), "chamfer op produced no segments");

    // Sweep pocket with the endmill profile.
    let pocket_profile = ToolProfile::from_tool(&endmill);
    let mut diag1 = SimDiagnostics::default();
    let pocket_writes = sweep_range(
        &mut hm,
        &pocket_segs,
        0,
        pocket_segs.len(),
        &pocket_profile,
        &[],
        None,
        &mut diag1,
    );
    let after_pocket_min = deepest_z(&hm);
    let after_pocket_vol = removed_volume(&hm, STOCK_TOP_Z);
    eprintln!(
        "[v5az] after pocket: min_h={after_pocket_min:.4} mm, V={after_pocket_vol:.2} mm³, writes={pocket_writes}",
    );
    assert!(
        f64::from(after_pocket_min) <= POCKET_DEPTH + 0.05,
        "pocket should reach Z={POCKET_DEPTH}, got {after_pocket_min}",
    );
    assert!(
        f64::from(after_pocket_min) >= POCKET_DEPTH - 0.05,
        "pocket should NOT cut deeper than Z={POCKET_DEPTH}, got {after_pocket_min}",
    );

    // Sweep chamfer with the cone profile on the SAME heightmap.
    let chamfer_profile = ToolProfile::from_tool(&cone);
    let mut diag2 = SimDiagnostics::default();
    let chamfer_writes = sweep_range(
        &mut hm,
        &chamfer_segs,
        0,
        chamfer_segs.len(),
        &chamfer_profile,
        &[],
        None,
        &mut diag2,
    );
    let after_chamfer_min = deepest_z(&hm);
    let after_chamfer_vol = removed_volume(&hm, STOCK_TOP_Z);
    eprintln!(
        "[v5az] after chamfer: min_h={after_chamfer_min:.4} mm, V={after_chamfer_vol:.2} mm³, writes={chamfer_writes}",
    );

    // STL out for visual inspection (the v5az repro path).
    let stl_bytes = dump_stl(
        &hm,
        "/tmp/wiac_v5az_chamfer_after_pocket.stl",
        -(STOCK_THICK as f32),
    );
    eprintln!("[v5az] STL → /tmp/wiac_v5az_chamfer_after_pocket.stl ({stl_bytes} bytes)",);

    // ── The v5az assertion ───────────────────────────────────────────
    // The chamfer's cone tip sits at Z ≈ -1.21 (the cone math), which
    // is SHALLOWER than the pocket floor at Z = -2. Since
    // `sweep_segment` only LOWERS cells, the chamfer must not deepen
    // any cell already at -2 — `min(-2, -1.21+something_positive)`
    // stays at -2 everywhere inside the pocket.
    assert!(
        f64::from(after_chamfer_min) >= f64::from(after_pocket_min) - 1e-3,
        "v5az: chamfer carved DEEPER than the pocket floor ({after_chamfer_min} mm < {after_pocket_min} mm). \
         The chamfer's cone tip is supposed to sit at Z ≈ {:.4} mm — ABOVE the pocket floor — \
         and the sim's sweep_segment only LOWERS cells. Anything deeper is the v5az bug.",
        sol.z,
    );
    // Stronger: nothing should sit below -2 anywhere. (The above check
    // would silently pass if the pocket itself carved below -2; this
    // assertion pins the absolute floor.)
    assert!(
        f64::from(after_chamfer_min) >= POCKET_DEPTH - 1e-3,
        "v5az: heightmap deepest sample {after_chamfer_min} mm is below the pocket depth {POCKET_DEPTH} mm",
    );

    // Sanity: the chamfer DID carve something (it should produce
    // OUTSIDE-the-rectangle V-bevels, increasing total volume).
    let chamfer_delta = after_chamfer_vol - after_pocket_vol;
    eprintln!("[v5az] chamfer added {chamfer_delta:.2} mm³ of carve");
    assert!(
        chamfer_delta > 5.0,
        "chamfer should add ≥ 5 mm³ of V-bevel carve outside the rectangle; got {chamfer_delta}",
    );
}

// ─────────────────────────────────────────────────────────────────────
// Helper: also exercise `OpKind::Profile` with the same after-pocket
// pattern, as a sanity check that the sim machinery doesn't mis-write
// when ANY op chains after a Pocket. Keeps the test focused on
// chamfer-specific behavior above.
// ─────────────────────────────────────────────────────────────────────

/// A regular Profile-Outside op after the same pocket must NOT
/// deepen the pocket floor either. Pins the broader invariant: any
/// op whose toolpath stays above the existing carve depth leaves
/// the existing carve untouched.
#[test]
fn profile_after_pocket_does_not_dip_below_pocket_floor() {
    let endmill1 = endmill_tool(1, POCKET_TOOL_DIA);
    let endmill2 = endmill_tool(2, 3.0); // smaller mill for profile
    let stock = stock_at_origin(STOCK_W, STOCK_H, STOCK_THICK);

    let pocket = op_single_pass(
        1,
        "Pocket",
        OpKind::Pocket {
            strategy: PocketStrategy::Cascade,
            contour: ContourParams::default(),
            pocket: PocketParams::default(),
        },
        1,
        POCKET_DEPTH,
    );
    // Profile cuts shallower than the pocket — outside the rectangle
    // it carves a -0.5mm-deep band; inside, the pocket already cleared
    // to -2, so the profile must leave that alone.
    let profile = op_single_pass(
        2,
        "Profile",
        OpKind::Profile {
            offset: ToolOffset::Outside,
            contour: ContourParams::default(),
            profile: ProfileParams::default(),
        },
        2,
        -0.5,
    );

    let project = Project {
        segments: closed_rectangle(RECT_X0, RECT_Y0, RECT_X1, RECT_Y1),
        machine: MachineConfig::default(),
        tools: vec![endmill1.clone(), endmill2.clone()],
        operations: vec![pocket, profile],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: WorkOffset::default(),
        stock: Some(stock.clone()),
        relief_sources: Vec::new(),
    };
    let resp = run(project, PostProcessorKind::Linuxcnc);

    let mut hm = build_heightmap(&stock, SIM_CELL_MM);
    let p1: Vec<_> = resp
        .toolpath
        .iter()
        .filter(|s| s.op_id == 1)
        .cloned()
        .collect();
    let p2: Vec<_> = resp
        .toolpath
        .iter()
        .filter(|s| s.op_id == 2)
        .cloned()
        .collect();
    let prof1 = ToolProfile::from_tool(&endmill1);
    let prof2 = ToolProfile::from_tool(&endmill2);
    let mut d = SimDiagnostics::default();
    sweep_range(&mut hm, &p1, 0, p1.len(), &prof1, &[], None, &mut d);
    let after_pocket_min = deepest_z(&hm);
    sweep_range(&mut hm, &p2, 0, p2.len(), &prof2, &[], None, &mut d);
    let after_profile_min = deepest_z(&hm);

    eprintln!(
        "[v5az/profile] after pocket min={after_pocket_min:.4}, after profile min={after_profile_min:.4}",
    );
    assert!(
        f64::from(after_profile_min) >= f64::from(after_pocket_min) - 1e-3,
        "profile after pocket must not deepen the pocket floor: pocket={after_pocket_min}, after profile={after_profile_min}",
    );
}
