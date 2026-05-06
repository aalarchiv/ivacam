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

# Start the Rust axum server on port 8766.
serve:
    cargo run --release -p wiac-server

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
