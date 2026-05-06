# Contributing to wiaConstructor

## License

By contributing you agree your work is licensed under GPL-3.0-or-later. This
matches the upstream viaConstructor license. Do not include code under
incompatible licenses without prior discussion.

If you port code from the original Python viaConstructor or another
GPL-compatible source, preserve the original copyright notice in the file
header.

## Repository layout

```
crates/
  wiac-core/      DXF/SVG import, CAM math, gcode generation (lib)
  wiac-cli/       headless converter binary
  wiac-server/    axum HTTP server binary
  wiac-tauri/     Tauri desktop shell binary
  wiac-wasm/      wasm-bindgen browser bindings (cdylib)
xtask/            cargo-xtask for dev workflows
frontend/         Svelte + Vite + TypeScript web UI
schema/           OpenAPI / JSON Schema source-of-truth contracts
tests/            integration corpus + bench baselines
refs/             upstream viaConstructor + dxf-rs (read-only references,
                  gitignored — clone yourself per the comment in .gitignore)
```

## Issue tracker

This project uses [`bd` (beads)](https://github.com/steveyegge/beads) for issue
tracking. Beads files live in `.beads/`.

```bash
bd ready              # what's available to start
bd show <id>          # full issue details
bd update <id> --claim
bd close <id>
```

Open an issue before non-trivial work; reference the issue ID in commits and
PRs.

## Toolchain

- Rust: pinned via `rust-toolchain.toml` (currently 1.88.0)
- Node: LTS 20+
- pnpm 10+ (lockfile committed)
- `wasm-pack` 0.14+ (for the WASM crate; `cargo install wasm-pack --locked`)
- `tauri-cli` 2.x (for desktop bundles; `cargo install tauri-cli --version "^2" --locked`)
- `cargo-deny` 0.19+ (for the licenses / advisories check)

## Development

```bash
cargo test                      # all Rust crates
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cd frontend && pnpm install && pnpm test && pnpm build
cargo xtask test-all            # end-to-end (runs both)
```

Pre-commit hooks (rustfmt, clippy, eslint, prettier) are wired via
`.pre-commit-config.yaml`. Install with `pre-commit install`.

## Pull requests

- Branch off `main`. Keep PRs scoped to a single bd issue when practical.
- CI must pass: cargo test (workspace), clippy, fmt, cargo-deny, frontend
  lint+check+build, wasm-pack build.
- Geometry / gcode changes should add or update a unit test in
  `crates/wiac-core/src/cam/` or `crates/wiac-core/src/gcode.rs`. The
  workspace-wide smoke test (`crates/wiac-core/tests/golden_corpus.rs`)
  walks every fixture under `refs/viaconstructor/tests/data/*.dxf` and
  asserts a non-empty linuxcnc program comes out — keep it green.
- Conventional commit messages preferred (`feat:`, `fix:`, `refactor:`, …)
  and reference the bd issue ID where relevant.

## Reporting bugs

Open a `bd` issue (`--type=bug`) with: input file (DXF / SVG / project,
if distributable), expected output, actual output, platform, build mode
(server / desktop / browser-WASM).
