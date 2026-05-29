//! dpah (4re9-d): closed-form volume-validation for a `GcodeInclude`
//! op. Proves end-to-end that the heightmap-side sim DOES model the
//! included block when its content is all supported G-codes — i.e.
//! the yhen claim "the unified preview::interpret_with_index pass
//! already tessellates G0/G1/G2/G3 + canned cycles into
//! ToolpathSegments that the sim already sweeps" is testable, not
//! just plausible.
//!
//! Fixture: a project with a single `GcodeInclude` op whose body
//! is a hand-rolled rapid-plunge-cut-retract sequence carving a
//! straight slot with a 4 mm flat endmill at Z = -1 over a 20 mm
//! horizontal line. Closed-form: the swept footprint is a "stadium"
//! (a 20 mm × 4 mm rectangle bookended by two half-disks of
//! radius 2 mm = one full disk), extruded to depth 1 mm:
//!
//!     V = (length × diameter + π · r²) × depth
//!       = (20 × 4 + π · 4) × 1
//!       ≈ 92.566 mm³
//!
//! Sim measurement: build the heightmap, sweep the pipeline's
//! interpreted toolpath into it, sum (top_z − h) over every cell.
//! Assert within 2 % of the closed-form expected — same tolerance
//! band the chamfer/profile/drill tests use.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]

use std::f64::consts::PI;

use wiac_core::cam::setup::MachineConfig;
use wiac_core::project::{Op, OpKind, OpParams, OpSource, Project, WorkOffset};
use wiac_core::schema::PostProcessorKind;

use super::common::{
    build_heightmap, dump_stl, endmill_tool, removed_volume, run, sim_carve, stock_at_origin,
};

/// 4mm flat endmill carving a 20 mm horizontal line at Z = -1, all
/// authored by hand in the GcodeInclude body. The pipeline emits
/// the body verbatim; preview::interpret reads it back and produces
/// ToolpathSegments; the sim sweeps them; we measure the carved
/// volume and compare against the closed-form stadium-prism.
#[test]
fn gcode_include_g1_slot_volume_matches_closed_form() {
    let stock = stock_at_origin(50.0, 30.0, 5.0);
    let tool = endmill_tool(1, 4.0);
    let tool_radius = tool.diameter * 0.5;

    // y = 15 keeps the swept stadium clear of the stock edges
    // (tool radius 2 mm; stock height 30 mm; stadium half-width 2 mm
    // → margins of 13 mm above/below — heightmap edge effects are
    // a non-factor).
    let x0 = 15.0;
    let x1 = 35.0;
    let cut_y = 15.0;
    let cut_z = -1.0;

    // Authored G-code: rapid above, traverse to start, plunge, cut,
    // retract. Every line is in the supported set (G0/G1) so the
    // yhen classifier returns 100 % Simulated and no skipped-summary
    // warning fires.
    let body = format!(
        "G0 Z5\nG0 X{x0} Y{cut_y}\nG1 Z{cut_z} F200\nG1 X{x1} Y{cut_y} F1200\nG0 Z5\n"
    );

    let include = Op {
        id: 1,
        name: "Hand-rolled slot".into(),
        enabled: true,
        kind: OpKind::GcodeInclude {
            path: "/tmp/slot.nc".into(),
            content: body,
            verbose_unsim_warnings: false,
        },
        // Program-only ops carry no tool — the tools[] still has the
        // 4 mm endmill because the SIM needs a tool profile for the
        // sweep. tool_id: 0 is the standard for program-only.
        tool_id: 0,
        finish_tool_id: None,
        source: OpSource::All,
        params: OpParams::mill_default(),
        group: None,
    };

    let project = Project {
        segments: Vec::new(),
        machine: MachineConfig::default(),
        tools: vec![tool.clone()],
        operations: vec![include],
        fixtures: Vec::new(),
        text_layers: Vec::new(),
        work_offset: WorkOffset::default(),
        stock: Some(stock.clone()),
        relief_sources: Vec::new(),
    };

    let resp = run(project, PostProcessorKind::Grbl);

    // yhen contract: a 100 %-simulated body emits NO skipped-summary
    // and NO legacy gcode_include_not_simulated warning. If THIS
    // assertion ever flips false, the classifier has regressed and
    // we're back to the pre-yhen lie.
    assert!(
        !resp.warnings.iter().any(|w| w.kind == "gcode_include_lines_skipped"
            || w.kind == "gcode_include_not_simulated"),
        "yhen: 100% G0/G1 body must produce no skipped-summary or legacy warning; got {:?}",
        resp.warnings,
    );

    // Toolpath should contain the rapid Z, rapid XY, plunge, cut,
    // retract — at minimum the cut segment whose YZ live at
    // (cut_y, cut_z).
    assert!(
        !resp.toolpath.is_empty(),
        "expected the pipeline to interpret the hand-rolled body into toolpath segments; got 0"
    );
    let has_cut = resp.toolpath.iter().any(|s| {
        (s.from.z - cut_z).abs() < 1e-6
            && (s.to.z - cut_z).abs() < 1e-6
            && (s.from.x - x0).abs() < 1e-6
            && (s.to.x - x1).abs() < 1e-6
            && (s.from.y - cut_y).abs() < 1e-6
            && (s.to.y - cut_y).abs() < 1e-6
    });
    assert!(
        has_cut,
        "expected a (x0,y,z)→(x1,y,z) cut segment from the hand-rolled body; toolpath={:?}",
        resp.toolpath,
    );

    // 0.1 mm cells gives ~150_000 samples over 50×30 mm — plenty of
    // resolution for the swept-stadium footprint at 4 mm diameter,
    // and matches what the other volume-validation tests use.
    let mut hm = build_heightmap(&stock, 0.1);
    let (writes, _diag) = sim_carve(&mut hm, &resp.toolpath, &tool);
    assert!(
        writes > 0,
        "sim_carve must touch the heightmap; got 0 cell writes"
    );

    // Closed-form: stadium-prism volume.
    // V = (length · diameter + π · r²) · depth
    let length = x1 - x0;
    let depth = -cut_z;
    let expected = (length * tool.diameter + PI * tool_radius * tool_radius) * depth;

    let measured = removed_volume(&hm, 0.0);

    // Diagnostic STL — only useful when the assertion fails and the
    // tester wants to eyeball the carved shape.
    let stl_path = "/tmp/dpah_gcode_include_slot.stl";
    let _ = dump_stl(&hm, stl_path, -(stock.thickness_mm as f32));

    let pct_err = (measured - expected).abs() / expected * 100.0;
    assert!(
        pct_err < 2.0,
        "gcode_include slot carved volume {measured:.3} mm³ vs expected {expected:.3} mm³ \
         (err {pct_err:.2}%) — see {stl_path} for inspection"
    );
}
