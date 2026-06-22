#!/usr/bin/env bash
# Bump the project version from its single source of truth.
#
# The version's single source of truth is `[workspace.package].version` in the
# root Cargo.toml. This script writes that one value and propagates it to the
# few places that can't read it directly, so there's nothing to hand-edit and
# nothing to drift:
#   - all Rust crates       inherit it via `version.workspace = true`
#   - schema/openapi.yaml    `info.version` is rewritten by `xtask schema`
#                            (the schema-check CI guard enforces it)
#   - tauri.conf.json        carries an explicit `version` — Tauri can't resolve
#                            `version.workspace = true`, so this script writes it
#                            and `xtask version-check` (CI) fails on drift
#   - frontend/package.json  has no version field (private, unpublished)
#
# Android note: the APK's version lives in the git-ignored
# gen/android/app/tauri.properties, which only `cargo tauri android init`
# regenerates — re-run it after a bump before building the release APK
# (see docs/BUILDING.md#versioning).
#
# Usage:  scripts/bump-version.sh 0.3.0
# Then review `git diff`, commit, and tag `v<version>`.
set -euo pipefail

new="${1:-}"
if [[ ! "$new" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: $0 <major.minor.patch>   (e.g. 0.3.0)" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

old="$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' Cargo.toml)"
if [[ "$old" == "$new" ]]; then
  echo "version is already $new — nothing to do"
  exit 0
fi
echo "bumping $old -> $new"

# 1. The single source: [workspace.package].version (scoped to that block).
sed -i '/^\[workspace.package\]/,/^\[/ s/^version = ".*"/version = "'"$new"'"/' Cargo.toml

# 2. tauri.conf.json: explicit copy Tauri reads (it can't resolve workspace
#    inheritance). Guarded by `xtask version-check`.
sed -i 's/^\(  "version": "\).*\("\,\)$/\1'"$new"'\2/' crates/ivac-tauri/tauri.conf.json

# 3. Cargo.lock: refresh the workspace members' version entries only.
cargo update --workspace >/dev/null

# 4. schema/openapi.yaml: rewrite info.version from the crate version.
cargo run -q -p xtask -- schema

# 5. Verify the tauri.conf.json copy matches (catches a botched sed early).
cargo run -q -p xtask -- version-check

# 6. frontend/src/lib/api/generated.ts: keep the TS client in lockstep with the
#    schema (version-independent, but leaves a fully-regenerated tree).
( cd frontend && pnpm run -s codegen )

echo
echo "done. Derived files regenerated. Next:"
echo "  git diff                       # review"
echo "  git commit -am 'chore: release v$new'"
echo "  git tag v$new && git push --follow-tags   # triggers CI release"
