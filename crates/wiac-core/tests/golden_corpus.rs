//! Smoke test that walks every DXF in `refs/viaconstructor/tests/data/` and
//! exercises the full Rust pipeline (import → chain → parallel offset →
//! gcode emit). Asserts the program is non-empty and carries the expected
//! mm/absolute-mode prologue. The Python-oracle parity harness was retired
//! along with the `FastAPI` bridge — coverage now sits at "doesn't panic on
//! the corpus, emits sane gcode for an outside cut".

use std::fs;
use std::path::{Path, PathBuf};

use wiac_core::cam::chaining::{classify_containment, segments_to_objects};
use wiac_core::cam::offsets::{parallel_offset_object, PolylineOffset};
use wiac_core::cam::setup::{Setup, ToolOffset};
use wiac_core::gcode::{emit_polylines, linuxcnc};
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
        // 1zya: refs/ is optional dev scaffolding, so a checkout without it
        // must not fail. Announce the skip clearly (visible under
        // `cargo test -- --nocapture`) so a green run isn't mistaken for
        // actual corpus coverage — the real correctness load lives in
        // tests/volume_validation, not here.
        eprintln!(
            "golden_corpus: SKIPPED (no corpus) — {} not present; \
             this smoke test exercised 0 DXFs. Correctness coverage is in \
             tests/volume_validation.",
            dxf_dir.display()
        );
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
