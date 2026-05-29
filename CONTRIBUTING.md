# Contributing to wiaConstructor

## License

By contributing you agree your work is licensed under GPL-3.0-or-later. This
matches the upstream viaConstructor license. Do not include code under
incompatible licenses without prior discussion.

If you port code from the original Python viaConstructor or another
GPL-compatible source, preserve the original copyright notice in the file
header.

## Read this first

[`ARCHITECTURE.md`](./ARCHITECTURE.md) is the 2-page map: layer diagram,
data flow (one user click traced end-to-end), the named patterns this
codebase reaches for, and the anti-patterns to avoid. Skim it before
touching multiple layers — most "where do I even start" friction is
answered there.

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

Pre-commit hooks (rustfmt, clippy, eslint, prettier, schema-check) are
wired via `.pre-commit-config.yaml`. Install with `pre-commit install`.

### JSON contract sync

`schema/openapi.yaml` and `frontend/src/lib/api/generated.ts` are
derived: the YAML's `components.schemas` block comes from the Rust
`#[derive(JsonSchema)]` types in `wiac-core` (regenerated via
`cargo xtask schema`), and `generated.ts` is the TypeScript output of
`pnpm run codegen` against the YAML. Both files are checked in so
downstream builds don't depend on the toolchain.

After touching any pub JsonSchema-deriving type in `wiac-core`:

```bash
cargo xtask schema && (cd frontend && pnpm run codegen)
# or, with `just`:
just regen-schema
```

The pre-commit hooks `xtask-schema-check` and `frontend-codegen-check`
catch drift locally; CI runs the same checks.

## Extension recipes

These are the two most common starter tasks. Both touch Rust + the
frontend + the JSON contract — the checklists exist so you don't ship a
half-wired change.

### Adding a new operation kind

An "operation kind" is one row in the `OpKindPicker` (Profile / Pocket /
Drill / Engrave / V-Carve / …). Pattern of an existing simple kind
(Engrave) to mirror:

1. **Rust enum variant** — `crates/wiac-core/src/project/op.rs`, the
   `OpKind` enum (around `pub enum OpKind {`). Add a variant. If the kind
   carries per-kind data, embed it in the variant (see
   `Thread { pitch_mm, internal, climb }`). If not, a unit variant like
   `Engrave` is fine. If the kind needs bulk out-of-band data (a grid, an
   image), store it at PROJECT level referenced by id — see
   `Project.relief_sources` / `ReliefSource` (f60x) — not inline in the op,
   which gets cloned and hashed.
2. **Fix the exhaustive matches** — adding a variant breaks a few
   `match`es that have no `_` arm; the compiler points at each. Known
   ones: `pipeline_cache.rs::hash_operation_kind` (assign the next free
   discriminant and hash every field that changes the output),
   `cam/offsets.rs::context_for`, and `pipeline/offset_builder.rs`'s
   per-kind `match` (specialty kinds that bypass the cascade go in the
   `Skip` / no-op arm). The frontend has matching exhaustive maps — see
   step 5.
3. **Pipeline dispatch** — `crates/wiac-core/src/pipeline.rs`. Either:
   - Let it route through the standard offset-cascade path (no edit
     needed) if your kind cuts along an offset of the source path
     (Profile / Engrave / DragKnife behave this way), **or**
   - Add a special-case driver to `crates/wiac-core/src/pipeline/op_drivers.rs`
     and dispatch from `run_per_op` (see `run_vcarve_op`, `run_thread_op`,
     `run_relief_op`). Specialty drivers emit XYZ blocks via
     `emit_vcarve_block` and add a `*_would_emit` Level-1 gate.
   Either way the per-op output is CACHED: any project-level data your kind
   reads (a relief source) must fold into `op_cache_key_with_finish` like
   `text_layers` / `relief_sources` do, and a `hash_tool` / op-shape change
   means bumping `PIPELINE_VERSION` and re-pinning the `stable_hash_regression`
   test.
4. **Frontend type** — `frontend/src/lib/state/op_types.ts`. Add the
   string to the `OpKind` union, add a per-kind interface that extends
   `OpBase`, add the variant to the `OpEntry` discriminated union, and
   update `isPathOp` / similar predicates if applicable. Then mirror it
   through the seam: `project-types.ts` (`prettyOpKind`, plus any
   project-level collection like `reliefSources`), `op_tool_constraint.ts`
   (`expectedToolKinds`), the `addOperation` factory + save/load in
   `state/project.svelte.ts`, and the wire mapping in
   `api/build-project.ts` (`FlatOp` fields, `WireOpKind` variant,
   `buildOpKind` case). `svelte-check` flags every exhaustive map you miss.
5. **Picker metadata** — `frontend/src/lib/components/OpKindPicker.svelte`.
   Add entries to `KIND_LABEL`, `KIND_ICON`, `ALL_PICKER_KINDS`,
   `PICKER_HELP`, and `OP_REQUIRES`. Each is a `Record<OpKind, …>` so the
   compiler flags the missing entry.
6. **Properties panel routing** — `frontend/src/lib/components/OpPropertiesPanel.svelte`.
   Add the kind to the appropriate `{#if op.kind === '…' || …}` block
   so the right sections render (or a dedicated `{:else if}` branch when
   the kind doesn't use the shared source/geometry UI — see `relief_mill`).
   If the kind needs bespoke fields, create
   `frontend/src/lib/components/op_properties/<Kind>Section.svelte`
   (mirror `VCarveSection.svelte` / `ReliefMillSection.svelte`) and render it.
7. **Schema regen** — `cargo xtask schema && (cd frontend && pnpm run codegen)`.
   CI's codegen drift guard fails if the checked-in `generated.ts` differs
   from a fresh run (it stays raw `openapi-typescript` output, not
   prettier-formatted).
8. **Tests** — add a unit test in `crates/wiac-core/src/pipeline/tests.rs`
   (search for `#[test]` near the bottom) that emits a tiny program
   with one op of the new kind. The corpus smoke test
   (`crates/wiac-core/tests/golden_corpus.rs`) doesn't exercise new
   kinds directly but must stay green.

### Adding a new G-code post-processor

A post-processor is a dialect of G-code emission (LinuxCNC / GRBL /
HPGL today). Mirror the simplest existing one (GRBL):

1. **New post file** — copy `crates/wiac-core/src/gcode/grbl.rs` to
   `crates/wiac-core/src/gcode/<name>.rs`. Adjust the `Post::new()`
   defaults and override `PostProcessor` trait methods as needed (see
   `gcode.rs:24` for the trait). Most posts only differ in headers /
   spindle / canned-cycle support.
2. **Register it** — declare the module in `crates/wiac-core/src/gcode.rs`
   (e.g. `pub mod <name>;`) and re-export `<name>::Post` if appropriate.
3. **Pipeline enum** — add a variant to `PostProcessorKind` in
   `crates/wiac-core/src/pipeline.rs`. Add the dispatch arm in
   `run_pipeline` (search for `PostProcessorKind::Linuxcnc => …`).
4. **CLI flag** — `crates/wiac-cli/src/main.rs`. Add the option name to
   the help text and the match arm that picks the impl.
5. **Frontend dropdown** — `frontend/src/lib/components/GenerateBar.svelte`.
   Extend the `PostId` union, update `coercePost`, and add an
   `<option>` element with its label.
6. **Schema regen** — `cargo xtask schema && (cd frontend && pnpm run codegen)`.
7. **Tests** — at minimum, a unit test in your new `<name>.rs` that
   verifies a one-line program round-trips through `Post::header` +
   `Post::move_to` + `Post::footer`. The corpus smoke test runs the
   default (LinuxCNC) post only; if you want the new post in CI, add
   it to the golden corpus parametrisation.

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
