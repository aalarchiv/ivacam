## wiaConstructor — common tasks. `just --list` to enumerate.

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

# Start the Stage-1 Python bridge on port 8765.
bridge:
    cd bridge && WIAC_HOST=0.0.0.0 .venv/bin/python -m wiac_bridge

# Start the Rust axum server on port 8766.
serve:
    cargo run --release -p wiac-server

# Start the Vite dev server.
dev-frontend:
    cd frontend && pnpm dev

# Re-generate gcode reference files using upstream Python viaConstructor.
refresh-golden:
    bridge/.venv/bin/python tests/golden/refresh.py

# Format everything.
fmt:
    cargo fmt --all
    cd frontend && pnpm exec prettier --write .

# Lint everything.
lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cd frontend && pnpm run lint
