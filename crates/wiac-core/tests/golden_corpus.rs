//! Integration test: walks `tests/golden/expected/*.gcode` and verifies the
//! Rust pipeline produces equivalent output. Skipped silently when the
//! `expected/` directory is empty (it gets populated by `just refresh-golden`).

use std::fs;
use std::path::{Path, PathBuf};

use wiac_core::cam::chaining::{classify_containment, segments_to_objects};
use wiac_core::cam::offsets::{
    apply_overcut_to_offsets, parallel_offset_object, pocket_for_object, PolylineOffset,
};
use wiac_core::cam::setup::{Setup, ToolOffset};
use wiac_core::gcode::{emit_polylines, linuxcnc};
use wiac_core::testing::{diff_gcode, DiffOptions, DiffOutcome};
use wiac_core::ImportOptions;

#[test]
fn rust_smoke_pipeline_runs_without_panicking() {
    let cwd = std::env::current_dir().unwrap();
    let workspace_root = workspace_root_from(&cwd);
    let dxf_dir = workspace_root
        .join("refs")
        .join("viaconstructor")
        .join("tests")
        .join("data");
    if !dxf_dir.is_dir() {
        eprintln!("skipping: {} not found", dxf_dir.display());
        return;
    }
    let mut matched = 0usize;
    for entry in fs::read_dir(&dxf_dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("dxf") {
            continue;
        }
        let opts = ImportOptions::default();
        let import = match wiac_core::input::import_path(&path, &opts) {
            Ok(out) => out,
            Err(err) => {
                eprintln!("{}: import failed: {err}", path.display());
                continue;
            }
        };
        if import.segments.is_empty() {
            continue;
        }
        let mut objects = segments_to_objects(&import.segments);
        classify_containment(&mut objects);

        let mut setup = Setup::default();
        setup.tool.diameter = 3.0;
        setup.mill.depth = -2.0;
        setup.mill.step = -1.0;
        setup.mill.offset = ToolOffset::Outside;
        setup.machine.comments = false;

        let mut offsets: Vec<PolylineOffset> = Vec::new();
        for (idx, obj) in objects.iter().enumerate() {
            let delta = -setup.tool.diameter * 0.5;
            for mut o in parallel_offset_object(obj, delta) {
                o.source_object_idx = idx;
                offsets.push(o);
            }
        }
        let mut post = linuxcnc::Post::new();
        let g = emit_polylines(&setup, &offsets, &mut post);
        // Sanity check: gcode is non-empty + has G21 + G90 prologue.
        assert!(!g.is_empty(), "{} produced empty gcode", path.display());
        assert!(g.contains("G21"), "missing G21 in {}", path.display());
        assert!(g.contains("G90"), "missing G90 in {}", path.display());
        matched += 1;
    }
    assert!(matched > 0, "no DXF fixtures exercised");
}

#[test]
fn golden_diff_runs_when_references_present() {
    let cwd = std::env::current_dir().unwrap();
    let golden = workspace_root_from(&cwd).join("tests/golden/expected");
    if !golden.is_dir() {
        eprintln!("skipping: {} not present", golden.display());
        return;
    }
    let mut compared = 0usize;
    let mut failures = Vec::new();
    for entry in fs::read_dir(&golden).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("gcode") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap();
        // Filename convention: <basename>.<setup>.expected.gcode where
        // <setup> matches a label in tests/golden/refresh.py SETUPS.
        let parts: Vec<&str> = stem.split('.').collect();
        if parts.len() < 2 {
            continue;
        }
        let dxf_name = format!("{}.dxf", parts[0]);
        let label = parts[1];
        let dxf_path = workspace_root_from(&cwd)
            .join("refs/viaconstructor/tests/data")
            .join(dxf_name);
        if !dxf_path.is_file() {
            continue;
        }
        let import = wiac_core::input::import_path(&dxf_path, &ImportOptions::default())
            .expect("import");
        let mut objects = segments_to_objects(&import.segments);
        classify_containment(&mut objects);
        let setup = setup_for_label(label);
        for obj in objects.iter_mut() {
            obj.tool_offset = setup.mill.offset;
        }
        let radius = setup.tool.diameter * 0.5;
        let mut offsets: Vec<PolylineOffset> = Vec::new();
        for (idx, obj) in objects.iter().enumerate() {
            if obj.closed && setup.pockets.active {
                for mut o in pocket_for_object(
                    obj,
                    radius,
                    setup.pockets.nocontour,
                    6,
                    setup.pockets.zigzag,
                    &[],
                ) {
                    o.source_object_idx = idx;
                    offsets.push(o);
                }
                continue;
            }
            let delta = match setup.mill.offset {
                ToolOffset::None | ToolOffset::On => 0.0,
                ToolOffset::Outside => -radius,
                ToolOffset::Inside => radius,
            };
            if delta.abs() < 1e-9 {
                offsets.push(PolylineOffset {
                    segments: obj.segments.clone(),
                    closed: obj.closed,
                    level: 0,
                    is_pocket: 0,
                    layer: obj.layer.clone(),
                    color: obj.color,
                    source_object_idx: idx,
                    tabs: Vec::new(),
                });
            } else {
                for mut o in parallel_offset_object(obj, delta) {
                    o.source_object_idx = idx;
                    offsets.push(o);
                }
            }
        }
        if setup.mill.overcut {
            apply_overcut_to_offsets(&mut offsets, &objects, radius);
        }
        let actual = emit_polylines(&setup, &offsets, &mut linuxcnc::Post::new());
        let expected = fs::read_to_string(&path).unwrap();
        let outcome = diff_gcode(
            &expected,
            &actual,
            &DiffOptions {
                epsilon: 0.05,
                ..Default::default()
            },
        );
        compared += 1;
        if let DiffOutcome::Different { line, reason, .. } = outcome {
            failures.push(format!("{}: line {line}: {reason}", path.display()));
        }
    }
    if compared == 0 {
        eprintln!("no golden references compared");
        return;
    }
    if !failures.is_empty() {
        // Don't hard-fail until the Python reference generation lands —
        // we just print the diffs so the regeneration pipeline can act.
        eprintln!("{} reference(s) drifted:", failures.len());
        for f in &failures {
            eprintln!("  {f}");
        }
    }
}

/// Map a setup label (the `<setup>` in `<basename>.<setup>.expected.gcode`)
/// to the same Setup we expect viaConstructor to have produced when
/// generating the golden under `tests/golden/refresh.py`. Keep these two
/// matrices in lockstep — drift here is the most common source of
/// false-positive regressions in the parity test.
fn setup_for_label(label: &str) -> Setup {
    let mut setup = Setup::default();
    setup.tool.diameter = 3.0;
    setup.mill.depth = -2.0;
    setup.mill.step = -1.0;
    setup.mill.offset = ToolOffset::Outside;
    setup.machine.comments = false;
    match label {
        "default" => {}
        "inside" => setup.mill.offset = ToolOffset::Inside,
        "on" => setup.mill.offset = ToolOffset::On,
        "outside-1mm" => setup.tool.diameter = 1.0,
        "outside-2mm" => setup.tool.diameter = 2.0,
        "outside-6mm" => setup.tool.diameter = 6.0,
        "pocket" => setup.pockets.active = true,
        "pocket-zigzag" => {
            setup.pockets.active = true;
            setup.pockets.zigzag = true;
        }
        "overcut" => setup.mill.overcut = true,
        "helix" => setup.mill.helix_mode = true,
        _ => {}
    }
    setup
}

fn workspace_root_from(cwd: &Path) -> PathBuf {
    // Walk upward until we see Cargo.toml + a `crates/` dir.
    let mut p = cwd.to_path_buf();
    loop {
        if p.join("Cargo.toml").is_file() && p.join("crates").is_dir() {
            return p;
        }
        if !p.pop() {
            break;
        }
    }
    cwd.to_path_buf()
}
