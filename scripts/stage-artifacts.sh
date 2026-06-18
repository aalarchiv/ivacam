#!/usr/bin/env bash
# Stage built packages into ./dist-test/ — the ONE directory testers get
# handed (it's gitignored for exactly this; see .gitignore).
#
# WHY: the build tools each drop their output in a tool-specific tree
#   - Android APK: crates/ivac-tauri/gen/android/.../debug/app-universal-debug.apk
#   - AppImage:    target/release/bundle/appimage/ivaCAM_<ver>_amd64.AppImage
# Copying them out by hand drifts (stale files, ad-hoc names). This script
# is the single source of truth for "where do shippable packages live and
# what are they called": always ./dist-test/, always
#   ivaCAM-<ver>-debug-arm64.apk   /   ivaCAM-<ver>-amd64.AppImage
# with <ver> read from tauri.conf.json so the name can't drift from the build.
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

APK_SRC=crates/ivac-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
APK_DST="$DEST/ivaCAM-$VER-debug-arm64.apk"
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

case "$what" in
  android)  stage "$APK_SRC" "$APK_DST" "APK" ;;
  appimage) stage "$APPIMAGE_SRC" "$APPIMAGE_DST" "AppImage" ;;
  all)
    stage "$APK_SRC" "$APK_DST" "APK"
    stage "$APPIMAGE_SRC" "$APPIMAGE_DST" "AppImage"
    ;;
  *) echo "usage: $0 [android|appimage|all]" >&2; exit 2 ;;
esac

[[ "$staged" -gt 0 ]] || { echo "stage-artifacts: nothing staged" >&2; exit 1; }
