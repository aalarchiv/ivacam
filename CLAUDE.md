# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**This repo now has a git remote** — `origin` →
`git@github.com:aalarchiv/ivacam.git`. Commits land on `main` locally
and must be pushed at session end. (Earlier guidance described the repo
as local-only; that is no longer true now that the remote exists.)

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** — Create bd issues for anything
   that needs follow-up
2. **Run quality gates** (if code changed) — Tests, linters, builds
3. **Update issue status** — Close finished work, update in-progress
   items via `bd close <id>` / `bd update <id> --claim`
4. **Commit locally** — Every logical change should land as its own
   commit on `main`. NEVER stop with uncommitted work in the working
   tree (that's the failure mode ivac-5kcj documented).
5. **Push** — `git pull --rebase && git push` to `origin/main`.
   Confirm with a fresh `git fetch` that local HEAD == `origin/main`.
6. **Verify** — `git status` shows clean working tree; `bd list
   --status=in_progress` is empty or accurately reflects active work
7. **Hand off** — Provide a brief context summary for the next session

### Pre-release ritual (bb8q)

Run [`scripts/pre-release.sh`](./scripts/pre-release.sh) before tagging a
release or handing an AppImage to a tester. It mirrors `ci.yml`
step-for-step: fmt, clippy `-D warnings`, `cargo test --workspace`,
xtask schema-check, codegen drift guard, then frontend lint / check /
test / build. Optional `wasm-pack` and `cargo-deny` gates fire if
those binaries are on `$PATH`. Fail-fast — only ship when every gate
reports green. This is the local stand-in for CI, not a per-commit
hook (the routine session-completion "run quality gates" step above
is the lighter check).
<!-- END BEADS INTEGRATION (block edited locally — ivac-uqvd; bd init may regenerate, re-apply the push-at-session-end workflow if so) -->


## Build & Test

Full setup + per-transport build instructions live in
[`BUILDING.md`](./docs/BUILDING.md). Quick reference:

```bash
# Rust workspace (ivac-core + transports)
cargo build --workspace
cargo test --workspace --tests   # full Rust unit + integration suite
cargo clippy --workspace --no-deps --release

# Frontend (Svelte 5 + Vite)
cd frontend
pnpm install
pnpm exec svelte-check         # type-check (0 errors/warnings expected)
pnpm test --run                # vitest suite
pnpm dev                       # dev server on http://localhost:5173

# Desktop bundle (Tauri 2)
cargo tauri build --bundles appimage
scripts/strip-appimage-media.sh   # drop bundled GStreamer core (see docs/BUILDING.md)
```

## Architecture Overview

See [`ARCHITECTURE.md`](./docs/ARCHITECTURE.md) for the full picture (crate
layout, data flow, the schema seam, key patterns, and anti-patterns).
In short: `ivac-core` holds all CAM/geometry/sim math; the `ivac-cli` /
`-server` / `-tauri` / `-wasm` crates are thin transports over it; the
Svelte frontend in `frontend/` talks to whichever transport is active
through a generated TypeScript client (`schema/openapi.yaml` →
`frontend/src/lib/api/generated.ts`).

## Conventions & Patterns

See the "Key patterns" and "Anti-patterns" sections of
[`ARCHITECTURE.md`](./docs/ARCHITECTURE.md), and [`CONTRIBUTING.md`](./docs/CONTRIBUTING.md)
for the end-to-end recipes (new op kind, new post-processor). Highlights:

- Frontend state lives in `$state`-class slices under
  `frontend/src/lib/state/`; mutations route through the command bus
  for undo.
- Never hand-edit `frontend/src/lib/api/generated.ts` — change the Rust
  type and regenerate the schema.
- Push post-processor dialect differences into trait methods, not inline
  branches in the emit shells.
- Tests co-locate with the code they cover; pure logic is extracted into
  plain `.ts` / module files so it's testable without the rune runtime.
