#!/usr/bin/env bash
# Stage built packages into ./dist-test/ — the ONE directory testers get
# handed (it's gitignored for exactly this; see .gitignore).
#
# WHY: the build tools each drop their output in a tool-specific tree
#   - Android APKs: crates/ivac-tauri/gen/android/app/build/outputs/apk/<flavor>/debug/app-<flavor>-debug.apk
#   - AppImage:     target/release/bundle/appimage/ivaCAM_<ver>_amd64.AppImage
# Copying them out by hand drifts (stale files, ad-hoc names). This script
# is the single source of truth for "where do shippable packages live and
# what are they called": always ./dist-test/, always
#   ivaCAM-<ver>-debug-<abi>.apk   /   ivaCAM-<ver>-amd64.AppImage
# with <ver> read from tauri.conf.json so the name can't drift from the build.
#
# Android is built split-per-ABI (cargo tauri android build --split-per-abi
# --target aarch64 armv7 i686 x86_64). Tauri names the gradle flavors by Rust
# target short name (arm64/arm/x86/x86_64); we map each to its Android ABI
# (arm64-v8a/armeabi-v7a/x86/x86_64) for the shipped filename.
#
# Usage:
#   scripts/stage-artifacts.sh [android|appimage|all]   (default: all present)
# Stages only the artifacts that exist; warns about the ones that don't so
# you know to build them first. Does NOT build — run the build, then this.
set -euo pipefail
cd "$(dirname "$0")/.."

DEST=dist-test
CONF=crates/ivac-tauri/tauri.conf.json

VER=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$CONF" | head -1)
[[ -n "$VER" ]] || { echo "stage-artifacts: could not read version from $CONF" >&2; exit 1; }

APK_BASE=crates/ivac-tauri/gen/android/app/build/outputs/apk
# Tauri gradle flavor → Android ABI (the name testers recognize).
ABIS=("arm64:arm64-v8a" "arm:armeabi-v7a" "x86:x86" "x86_64:x86_64")
APPIMAGE_SRC="target/release/bundle/appimage/ivaCAM_${VER}_amd64.AppImage"
APPIMAGE_DST="$DEST/ivaCAM-$VER-amd64.AppImage"

what=${1:-all}
mkdir -p "$DEST"
staged=0

stage() { # src dst label
  if [[ -f "$1" ]]; then
    cp -f "$1" "$2"
    echo "staged $3 → $2 ($(du -h "$2" | cut -f1))"
    staged=$((staged + 1))
  else
    echo "stage-artifacts: $3 not found at $1 — build it first" >&2
  fi
}

stage_android() {
  local found=0
  for pair in "${ABIS[@]}"; do
    local flavor="${pair%%:*}" abi="${pair##*:}"
    local src="$APK_BASE/$flavor/debug/app-$flavor-debug.apk"
    if [[ -f "$src" ]]; then
      stage "$src" "$DEST/ivaCAM-$VER-debug-$abi.apk" "APK ($abi)"
      found=1
    fi
  done
  if [[ "$found" -eq 0 ]]; then
    echo "stage-artifacts: no per-ABI APKs under $APK_BASE/<flavor>/debug/ — build them first" >&2
    echo "  cargo tauri android build --debug --apk --split-per-abi --target aarch64 armv7 i686 x86_64" >&2
  fi
}

case "$what" in
  android)  stage_android ;;
  appimage) stage "$APPIMAGE_SRC" "$APPIMAGE_DST" "AppImage" ;;
  all)
    stage_android
    stage "$APPIMAGE_SRC" "$APPIMAGE_DST" "AppImage"
    ;;
  *) echo "usage: $0 [android|appimage|all]" >&2; exit 2 ;;
esac

[[ "$staged" -gt 0 ]] || { echo "stage-artifacts: nothing staged" >&2; exit 1; }
