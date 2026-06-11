#!/usr/bin/env bash
#
# Local pre-release ritual (bb8q).
#
# This repo has no git remote, so the ci.yml in .github/workflows/ never
# fires. This script is the local stand-in: run it before tagging a
# release (or before handing an AppImage to a tester) and only ship if
# every step is green. It mirrors ci.yml step-for-step, just on whatever
# OS the dev is on (vs ci.yml's ubuntu/macos/windows matrix).
#
# Usage:  scripts/pre-release.sh
# Exit:   0 on full pass, 1 on the first failure.
#
# Optional gates that need extra tooling (wasm-pack, cargo-deny) are
# skipped with a note if the binary isn't on PATH; install them if you
# care about parity with the CI yml for a release.

# Note: NO `set -e`. We want every gate to run even when an earlier one
# fails — a release checklist is more useful when it reports the full
# pass/fail matrix in one pass than when it bails at the first red. The
# final exit status reflects whether any gate failed.
set -uo pipefail

cd "$(dirname "$0")/.."

# ANSI colours; degrade gracefully on dumb terminals.
if [[ -t 1 ]]; then
  G=$'\033[1;32m'; R=$'\033[1;31m'; Y=$'\033[1;33m'; D=$'\033[2m'; N=$'\033[0m'
else
  G=""; R=""; Y=""; D=""; N=""
fi

steps=()
fail=0

step() {
  local name="$1"; shift
  printf "%s━━ %s%s\n" "$D" "$name" "$N"
  if "$@"; then
    steps+=("${G}✓${N} $name")
  else
    steps+=("${R}✗${N} $name")
    fail=1
  fi
}

skip() {
  local name="$1" reason="$2"
  printf "%s━━ %s (skipped: %s)%s\n" "$D" "$name" "$reason" "$N"
  steps+=("${Y}-${N} $name ${D}(skipped: $reason)${N}")
}

#───────────── Rust workspace ─────────────

step "cargo fmt --check"      cargo fmt --all -- --check
# ivr0: re-enable clippy::pedantic explicitly here. Workspace lints
# leave it off for the inner dev loop (so daily `cargo clippy` runs in
# ~30 s instead of 3 min); the release gate restores the strict walk
# so nothing slips out.
step "cargo clippy -Dwarnings (with pedantic)" \
  cargo clippy --workspace --all-targets -- -W clippy::pedantic -D warnings
# waud: prefer cargo-nextest when installed (parallel test-binary
# execution, ~30-60 % faster wall time on multi-binary workspaces).
# Falls through to plain `cargo test` when nextest isn't on PATH, so
# a developer without it installed isn't blocked.
if command -v cargo-nextest >/dev/null 2>&1; then
  step "cargo nextest run (ci profile)" \
    cargo nextest run --workspace --all-features --profile ci --run-ignored all
else
  step "cargo test --workspace"  cargo test --workspace --all-features
fi
step "xtask schema-check"      cargo run --quiet -p xtask -- schema-check

#───────────── Frontend ─────────────
#
# pushd/popd, NOT a (cd subshell): the subshell variant orphans every
# `steps+=` append (the array lives in the parent), so the final summary
# would silently lose every frontend step entry.

pushd frontend >/dev/null

step "pnpm install"            pnpm install --frozen-lockfile

# Codegen drift guard: regenerate generated.ts from the YAML; the diff
# must be empty. Pairs with the rust xtask schema-check above.
step "codegen drift"           bash -c '
  pnpm run codegen >/dev/null
  git diff --exit-code -- src/lib/api/generated.ts
'

step "pnpm run lint"           pnpm run lint
step "pnpm run check"          pnpm run check
step "pnpm run test"           pnpm run test
step "pnpm run build"          pnpm run build

popd >/dev/null

#───────────── Optional gates ─────────────

if command -v wasm-pack >/dev/null 2>&1; then
  step "wasm-pack build (web)"   wasm-pack build crates/ivac-wasm --target web --release
else
  skip "wasm-pack build (web)"   "wasm-pack not on PATH"
fi

if command -v cargo-deny >/dev/null 2>&1; then
  step "cargo-deny check"        cargo deny check bans licenses sources advisories
else
  skip "cargo-deny check"        "cargo-deny not on PATH"
fi

# Runtime UI smoke — boots the built frontend in headless Chromium and
# clicks through the main tabs + tool table. The only gate that catches
# render-time-only regressions (e.g. the d41i effect-loop freeze).
if command -v chromium-browser >/dev/null 2>&1 || command -v chromium >/dev/null 2>&1 || command -v google-chrome >/dev/null 2>&1; then
  step "ui smoke (headless)"     scripts/ui-smoke.sh
else
  skip "ui smoke (headless)"     "no chromium on PATH"
fi

#───────────── Summary ─────────────

echo
echo "${D}━━━ summary ━━━${N}"
for s in "${steps[@]}"; do echo "  $s"; done
echo

if (( fail == 0 )); then
  echo "${G}all gates green — ok to release${N}"
  exit 0
else
  echo "${R}pre-release: failures above. Fix before tagging / shipping.${N}"
  exit 1
fi
