//! cargo-xtask: project-wide dev workflows.
//!
//! Run with `cargo xtask <task>`. Mirrors what CI runs so you can verify
//! locally before pushing.

use std::env;
use std::process::{Command, ExitCode};

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
           wasm            wasm-pack build crates/wiac-wasm --target web --release\n\
           frontend-build  pnpm run build (in frontend/)\n\
           frontend-test   pnpm run test\n\
           frontend-check  pnpm run check (svelte-check + tsc)\n\
           frontend-lint   pnpm run lint (prettier --check)\n\
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
        "crates/wiac-wasm",
        "--target",
        "web",
        "--release",
    ]))
}

fn ci_all() -> ExitCode {
    let steps: &[(&str, fn() -> ExitCode)] = &[
        ("cargo fmt --check", || cargo(&["fmt", "--all", "--", "--check"])),
        ("cargo clippy", || {
            cargo(&["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])
        }),
        ("cargo test", || cargo(&["test", "--workspace", "--all-features"])),
        ("frontend lint", || pnpm(&["run", "lint"])),
        ("frontend check", || pnpm(&["run", "check"])),
        ("frontend test", || pnpm(&["run", "test"])),
        ("frontend build", || pnpm(&["run", "build"])),
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
