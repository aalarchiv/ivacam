## ivaCAM — common tasks. `just --list` to enumerate.

# Run everything CI runs (mirrors xtask::ci_all).
ci:
    cargo xtask ci

# Run all Rust tests.
test:
    cargo test --workspace --all-features

# Run cargo + frontend builds.
build:
    cargo build --workspace --release
    cd frontend && pnpm install --frozen-lockfile && pnpm run build

# Start the Rust axum server on port 8766.
serve:
    cargo run --release -p ivac-server

# Start the Vite dev server.
dev-frontend:
    cd frontend && pnpm dev

# Format everything.
fmt:
    cargo fmt --all
    cd frontend && pnpm exec prettier --write .

# Lint everything.
lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cd frontend && pnpm run lint

# Regenerate the JSON contract: schema/openapi.yaml from Rust JsonSchema-deriving
# types + the frontend TypeScript types from the YAML. Run this after touching
# any pub type that derives JsonSchema in ivac-core (project.rs, pipeline.rs,
# errors.rs, gcode.rs, …). Both files are checked into git.
regen-schema:
    cargo xtask schema
    cd frontend && pnpm run codegen

# Verify the JSON contract is in sync with the Rust types. Returns non-zero
# if regenerating would change schema/openapi.yaml or frontend/src/lib/api/generated.ts.
# Wired into the pre-commit hook so drift is caught locally, not on CI.
check-schema:
    cargo xtask schema-check
    cd frontend && pnpm run codegen && git diff --exit-code -- src/lib/api/generated.ts
