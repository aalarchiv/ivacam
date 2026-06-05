//! cargo-xtask: project-wide dev workflows.
//!
//! Run with `cargo xtask <task>`. Mirrors what CI runs so you can verify
//! locally before pushing.

// # CAM/sim pedantic-lint exemptions
// xtask shells out via `Command::status()`; the integer cast is the
// POSIX-standard `status.code()` mapping into `u8` for ExitCode.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::env;
use std::process::{Command, ExitCode, Stdio};

fn main() -> ExitCode {
    let task = env::args().nth(1);
    let result = match task.as_deref() {
        Some("test") => cargo(&["test", "--workspace", "--all-features"]),
        Some("fmt") => cargo(&["fmt", "--all"]),
        Some("fmt-check") => cargo(&["fmt", "--all", "--", "--check"]),
        Some("clippy") => cargo(&[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]),
        Some("wasm") => wasm_pack(),
        Some("frontend-build") => pnpm(&["run", "build"]),
        Some("frontend-test") => pnpm(&["run", "test"]),
        Some("frontend-check") => pnpm(&["run", "check"]),
        Some("frontend-lint") => pnpm(&["run", "lint"]),
        Some("schema") => schema(false),
        Some("schema-check") => schema(true),
        Some("ci") => ci_all(),
        Some(unknown) => {
            eprintln!("xtask: unknown task '{unknown}'");
            usage();
            return ExitCode::from(2);
        }
        None => {
            usage();
            return ExitCode::from(2);
        }
    };
    result
}

fn usage() {
    eprintln!(
        "usage: cargo xtask <task>\n\
         tasks:\n\
           test            run cargo test --workspace --all-features\n\
           fmt             run cargo fmt --all\n\
           fmt-check       run cargo fmt --all -- --check\n\
           clippy          run cargo clippy -D warnings\n\
           wasm            wasm-pack build crates/ivac-wasm --target web --release\n\
           frontend-build  pnpm run build (in frontend/)\n\
           frontend-test   pnpm run test\n\
           frontend-check  pnpm run check (svelte-check + tsc)\n\
           frontend-lint   pnpm run lint (prettier --check)\n\
           schema          regenerate schema/openapi.yaml's components/schemas\n\
           schema-check    fail if schema/openapi.yaml is out of date\n\
           ci              run everything CI runs"
    );
}

fn cargo(args: &[&str]) -> ExitCode {
    run(Command::new("cargo").args(args))
}

fn pnpm(args: &[&str]) -> ExitCode {
    let mut cmd = Command::new("pnpm");
    cmd.current_dir("frontend").args(args);
    run(&mut cmd)
}

fn wasm_pack() -> ExitCode {
    run(Command::new("wasm-pack").args([
        "build",
        "crates/ivac-wasm",
        "--target",
        "web",
        "--release",
    ]))
}

type CiStep = (&'static str, fn() -> ExitCode);

fn ci_all() -> ExitCode {
    let steps: &[CiStep] = &[
        ("cargo fmt --check", || {
            cargo(&["fmt", "--all", "--", "--check"])
        }),
        ("cargo clippy", || {
            cargo(&[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ])
        }),
        ("cargo test", || {
            cargo(&["test", "--workspace", "--all-features"])
        }),
        // Pairs with the frontend codegen drift guard below: the Rust
        // schema and the checked-in OpenAPI YAML must agree (ci.yml runs
        // this in the rust job).
        ("xtask schema-check", || schema(true)),
        ("frontend lint", || pnpm(&["run", "lint"])),
        ("frontend check", || pnpm(&["run", "check"])),
        ("frontend test", || pnpm(&["run", "test"])),
        ("frontend build", || pnpm(&["run", "build"])),
        // Regenerate generated.ts from the YAML and fail on any diff — the
        // ts client must track the schema (ci.yml's "codegen drift guard").
        ("codegen drift", codegen_drift),
        // wasm-pack is an optional local tool; skip loudly rather than hard-
        // fail when it's absent (ci.yml installs it, pre-release.sh skips it).
        ("wasm-pack build (web)", wasm_or_skip),
    ];
    for (label, step) in steps {
        eprintln!("\n==> {label}");
        let code = step();
        if code != ExitCode::SUCCESS {
            return code;
        }
    }
    ExitCode::SUCCESS
}

/// Regenerate `generated.ts` from `schema/openapi.yaml` and fail if the
/// checked-in file drifts. Mirrors ci.yml's frontend codegen drift guard.
fn codegen_drift() -> ExitCode {
    let gen = pnpm(&["run", "codegen"]);
    if gen != ExitCode::SUCCESS {
        return gen;
    }
    run(Command::new("git").args([
        "diff",
        "--exit-code",
        "--",
        "frontend/src/lib/api/generated.ts",
    ]))
}

/// Run wasm-pack if available; otherwise skip with a clear notice so the
/// gate is never silently dropped.
fn wasm_or_skip() -> ExitCode {
    if binary_available("wasm-pack") {
        wasm_pack()
    } else {
        eprintln!("  skipped: wasm-pack not on PATH (`cargo install wasm-pack` to enable)");
        ExitCode::SUCCESS
    }
}

fn binary_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn schema(check_only: bool) -> ExitCode {
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let yaml_path = workspace_root.join("schema/openapi.yaml");
    let on_disk = match std::fs::read_to_string(&yaml_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("read {}: {e}", yaml_path.display());
            return ExitCode::from(1);
        }
    };
    let mut doc: serde_yaml::Value = match serde_yaml::from_str(&on_disk) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("parse {}: {e}", yaml_path.display());
            return ExitCode::from(1);
        }
    };
    let derived = ivac_core::schema::components_schemas();
    let derived_yaml = serde_yaml::to_value(&derived).unwrap();
    let components = doc
        .get_mut("components")
        .and_then(|c| c.as_mapping_mut())
        .expect("components: missing in OpenAPI YAML");
    components.insert(serde_yaml::Value::String("schemas".into()), derived_yaml);

    let serialized = serde_yaml::to_string(&doc).unwrap();
    if check_only {
        if serialized.trim() == on_disk.trim() {
            eprintln!("schema/openapi.yaml is up to date");
            ExitCode::SUCCESS
        } else {
            eprintln!(
                "schema/openapi.yaml drift detected. Run `cargo xtask schema` to regenerate."
            );
            ExitCode::from(1)
        }
    } else if let Err(e) = std::fs::write(&yaml_path, serialized) {
        eprintln!("write {}: {e}", yaml_path.display());
        ExitCode::from(1)
    } else {
        eprintln!("regenerated {}", yaml_path.display());
        ExitCode::SUCCESS
    }
}

fn run(cmd: &mut Command) -> ExitCode {
    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to run {:?}: {e}", cmd.get_program());
            return ExitCode::from(1);
        }
    };
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(status.code().unwrap_or(1) as u8)
    }
}
