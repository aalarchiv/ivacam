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
tests/            integration / golden-file corpus
refs/             upstream viaConstructor + dxf-rs (read-only references)
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

- Rust: pinned via `rust-toolchain.toml` (stable; current MSRV documented in
  the workspace `Cargo.toml`)
- Node: LTS (`.nvmrc`)
- Tauri 2 (for desktop builds)
- `wasm-pack` (for the WASM crate)

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
- CI must pass: cargo test, clippy, fmt, frontend lint+test+build, parity tests
  against the Python reference (`bd-0zz` epic) once that harness lands.
- Add or update tests for behavior changes. Geometry/gcode changes need a
  golden-file entry.
- Conventional commit messages preferred (`feat:`, `fix:`, `refactor:`, …) and
  reference the bd issue ID, e.g. `feat(core): port LWPOLYLINE bulge expansion (wiaconstructor-av1.6)`.

## Reporting bugs

Open a `bd` issue (`--type=bug`) with: viaConstructor input file (if
distributable), expected output, actual output, platform, build mode (server /
desktop / WASM).
