#!/usr/bin/env bash
# Strip the GStreamer core libraries out of the built AppImage and
# re-pack it.
#
# WHY: linuxdeploy bundles the build machine's GStreamer CORE libs
# (libgstreamer-1.0 & friends) as WebKit transitive deps even though
# the app uses no media. At runtime they shadow the host's copies, so
# the host's WebKit ends up loading OUR core against the HOST's
# plugins — absent or version-mismatched plugins then print
# "GStreamer element appsink not found" on every launch. Removing the
# bundled core lets the loader fall back to the host's own GStreamer
# (a hard dependency of every webkit2gtk package, so it's always
# there), giving a version-consistent stack and a smaller AppImage.
# Hosts that genuinely lack the plugin packages may still see the
# warning — accepted trade-off vs. shipping a +14 MB media framework
# the app never uses (see docs/BUILDING.md).
#
# Usage: scripts/strip-appimage-media.sh
#   (run after `cargo tauri build --bundles appimage`)
set -euo pipefail
cd "$(dirname "$0")/.."

BUNDLE_DIR=target/release/bundle/appimage
APPDIR="$BUNDLE_DIR/ivaCAM.AppDir"
PACKER="$HOME/.cache/tauri/linuxdeploy-plugin-appimage.AppImage"

[[ -d "$APPDIR" ]] || { echo "strip-appimage-media: $APPDIR missing — build first" >&2; exit 1; }
[[ -x "$PACKER" ]] || { echo "strip-appimage-media: $PACKER missing (tauri cache)" >&2; exit 1; }

shopt -s nullglob
removed=0
for f in "$APPDIR"/usr/lib/libgst*-1.0.so*; do
  rm -f "$f"
  removed=$((removed + 1))
done
# Plugin dirs only exist when bundleMediaFramework was on; clear any
# stragglers so a config flip can't half-ship the framework.
rm -rf "$APPDIR"/usr/lib/gstreamer-1.0 "$APPDIR"/usr/lib/gstreamer1.0
rm -f "$APPDIR"/apprun-hooks/linuxdeploy-plugin-gstreamer.sh
echo "strip-appimage-media: removed $removed GStreamer core libs"

# Re-pack, REPLACING the original artifact. The packer drops
# `ivaCAM-x86_64.AppImage` in $PWD; APPIMAGE_EXTRACT_AND_RUN works
# around missing FUSE (headless CI / containers).
orig=$(ls "$BUNDLE_DIR"/ivaCAM_*.AppImage 2>/dev/null | head -1)
[[ -n "$orig" ]] || orig="$BUNDLE_DIR/ivaCAM_amd64.AppImage"
(
  cd "$BUNDLE_DIR"
  APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$PACKER" --appdir "$(basename "$APPDIR")" >/dev/null 2>&1
)
mv "$BUNDLE_DIR/ivaCAM-x86_64.AppImage" "$orig"
ls -lh "$orig" | awk '{print "strip-appimage-media: " $5 " " $9}'
