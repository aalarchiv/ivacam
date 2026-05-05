//! End-to-end pipeline benchmarks: DXF import → CAM → gcode emit.
//!
//! Run with `cargo bench -p wiac-core` from the repo root. The fixture set
//! lives in `refs/viaconstructor/tests/data/`; we silently skip files that
//! aren't present so the bench can run in a stripped-down checkout.
//!
//! For numbers worth comparing across runs use a fixed self-hosted runner
//! (cloud CI varies too much). Record results in tests/bench-baseline.md.

use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use wiac_core::cam::chaining::{classify_containment, segments_to_objects};
use wiac_core::cam::offsets::{parallel_offset_object, PolylineOffset};
use wiac_core::cam::setup::{Setup, ToolOffset};
use wiac_core::gcode::{emit_polylines, linuxcnc};
use wiac_core::ImportOptions;

/// Walk up from CWD until we hit the repo root (Cargo.toml + crates/).
fn workspace_root() -> PathBuf {
    let mut p = std::env::current_dir().unwrap();
    loop {
        if p.join("Cargo.toml").is_file() && p.join("crates").is_dir() {
            return p;
        }
        if !p.pop() {
            break;
        }
    }
    std::env::current_dir().unwrap()
}

fn run_pipeline(dxf: &PathBuf) -> usize {
    let import = match wiac_core::input::import_path(dxf, &ImportOptions::default()) {
        Ok(out) => out,
        Err(_) => return 0,
    };
    if import.segments.is_empty() {
        return 0;
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

    let g = emit_polylines(&setup, &offsets, &mut linuxcnc::Post::new());
    g.len()
}

fn bench_pipeline(c: &mut Criterion) {
    let root = workspace_root();
    let data_dir = root
        .join("refs")
        .join("viaconstructor")
        .join("tests")
        .join("data");
    let names = ["simple", "check", "nest", "colors", "all"];
    let mut group = c.benchmark_group("pipeline");
    for name in names {
        let path = data_dir.join(format!("{name}.dxf"));
        if !path.is_file() {
            eprintln!("skipping {} (not present)", path.display());
            continue;
        }
        group.bench_with_input(BenchmarkId::from_parameter(name), &path, |b, p| {
            b.iter(|| run_pipeline(p));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
