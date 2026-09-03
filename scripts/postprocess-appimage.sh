#!/usr/bin/env bash
# Post-process the built AppImage and re-pack it.
#
# Two fixes, both applied to the AppDir before repacking:
#
# 1. DROP THE BUNDLED GStreamer *AND* glib CORE.
#    linuxdeploy bundles the build machine's GStreamer core libs
#    (libgstreamer-1.0 & friends) as WebKit transitive deps even though
#    the app uses no media. Shipping them shadows the host's GStreamer,
#    so the host's plugins get loaded against OUR core — absent or
#    version-mismatched plugins then print "GStreamer element appsink
#    not found" on every launch.
#
#    Removing GStreamer alone is NOT enough, and shipped a v0.5.0
#    AppImage that could not start on any distro newer than the build
#    machine: the AppDir still carried the builder's glib (2.72 from
#    ubuntu-22.04), the host's GStreamer is linked against the host's
#    glib, and on e.g. Ubuntu 24.04 / Linux Mint 22 that combination
#    dies at startup with
#
#      ivac-desktop: symbol lookup error:
#      /lib/x86_64-linux-gnu/libgstvideo-1.0.so.0:
#      undefined symbol: g_once_init_leave_pointer
#
#    (g_once_init_leave_pointer is glib >= 2.80.) So the glib core goes
#    too: with neither bundled, glib and GStreamer both come from the
#    host and the stack is genuinely version-consistent. Every
#    webkit2gtk package hard-depends on both, so they are always there.
#    Cost: the AppImage now needs a host glib at least as new as the
#    builder's (ubuntu-22.04 => glib 2.72), which the bundled GTK 3.24.33
#    already required in practice.
#
# 2. NORMALIZE THE HICOLOR ICON DIRECTORIES.
#    Tauri's bundler writes the 256x256 icon (icons/128x128@2x.png) to
#    hicolor/256x256@2/, which is not a directory name the freedesktop
#    icon theme spec knows — desktop environments ignore it, leaving the
#    app with only the 32x32 icon to scale up. Copy it into the plain
#    256x256/ bucket (the original name stays, which costs nothing) and
#    refresh the AppImage's top-level .DirIcon from the largest icon
#    available.
#
# 3. INSTALL THE DESKTOP-INTEGRATION AppRun HOOK.
#    Panels (Cinnamon, GNOME Shell, ...) resolve a window's taskbar icon by
#    matching WM_CLASS against installed .desktop entries, not from
#    _NET_WM_ICON — so an un-integrated AppImage always shows the generic
#    application-x-executable tile, which is the "no icon in the taskbar"
#    bug. deploy/appimage/desktop-integration.sh registers the icons and a
#    .desktop entry under ~/.local/share on first run; see that file for the
#    full reasoning.
#
# Usage: scripts/postprocess-appimage.sh
#   (run after `cargo tauri build --bundles appimage`)
set -euo pipefail
cd "$(dirname "$0")/.."

BUNDLE_DIR=target/release/bundle/appimage
APPDIR="$BUNDLE_DIR/ivaCAM.AppDir"
PACKER="$HOME/.cache/tauri/linuxdeploy-plugin-appimage.AppImage"

[[ -d "$APPDIR" ]] || { echo "postprocess-appimage: $APPDIR missing — build first" >&2; exit 1; }
[[ -x "$PACKER" ]] || { echo "postprocess-appimage: $PACKER missing (tauri cache)" >&2; exit 1; }

shopt -s nullglob

# -- 1. host-provided multimedia + glib core --
removed=0
for f in "$APPDIR"/usr/lib/libgst*-1.0.so* \
         "$APPDIR"/usr/lib/libglib-2.0.so* \
         "$APPDIR"/usr/lib/libgobject-2.0.so* \
         "$APPDIR"/usr/lib/libgio-2.0.so* \
         "$APPDIR"/usr/lib/libgmodule-2.0.so*; do
  rm -f "$f"
  removed=$((removed + 1))
done
# Plugin dirs only exist when bundleMediaFramework was on; clear any
# stragglers so a config flip can't half-ship the framework.
rm -rf "$APPDIR"/usr/lib/gstreamer-1.0 "$APPDIR"/usr/lib/gstreamer1.0
rm -f "$APPDIR"/apprun-hooks/linuxdeploy-plugin-gstreamer.sh
echo "postprocess-appimage: removed $removed GStreamer/glib core libs"

# -- 2. icon theme directories --
HICOLOR="$APPDIR/usr/share/icons/hicolor"
if [[ -d "$HICOLOR" ]]; then
  for bogus in "$HICOLOR"/*@2; do
    [[ -d "$bogus" ]] || continue
    proper="${bogus%@2}"
    mkdir -p "$proper/apps"
    for icon in "$bogus"/apps/*; do
      cp -f "$icon" "$proper/apps/"
      echo "postprocess-appimage: $(basename "$icon") -> $(basename "$proper")/apps/"
    done
  done
  # AppImage thumbnailers and AppImageLauncher read .DirIcon; point it at
  # the largest square icon we ship instead of whatever landed there.
  best=""
  best_px=0
  for d in "$HICOLOR"/*/apps; do
    px=${d#"$HICOLOR"/}
    px=${px%%x*}
    [[ $px =~ ^[0-9]+$ ]] || continue
    for icon in "$d"/*.png; do
      if (( px > best_px )); then best_px=$px; best=$icon; fi
    done
  done
  if [[ -n "$best" ]]; then
    rm -f "$APPDIR/.DirIcon"
    cp -f "$best" "$APPDIR/.DirIcon"
    echo "postprocess-appimage: .DirIcon <- ${best_px}x${best_px}"
  fi
fi

# -- 3. desktop integration hook --
# linuxdeploy's generated AppRun sources each hook by name (it does not glob),
# so the source line has to be added explicitly. It goes directly before the
# final exec, so the hook sees the environment the GTK hook set up ($APPDIR).
# `|| true`: AppRun runs under `set -e`, and a hostile desktop environment
# must never stop the app from launching.
HOOK_SRC=deploy/appimage/desktop-integration.sh
HOOK_NAME=ivacam-desktop-integration.sh
if [[ -f "$HOOK_SRC" ]]; then
  mkdir -p "$APPDIR/apprun-hooks"
  cp -f "$HOOK_SRC" "$APPDIR/apprun-hooks/$HOOK_NAME"
  chmod +x "$APPDIR/apprun-hooks/$HOOK_NAME"
  if grep -q "$HOOK_NAME" "$APPDIR/AppRun"; then
    echo "postprocess-appimage: AppRun already sources $HOOK_NAME"
  else
    awk -v hook="$HOOK_NAME" '
      /^exec "\$this_dir"\/AppRun\.wrapped/ && !inserted {
        print "source \"$this_dir\"/apprun-hooks/\"" hook "\" || true"
        print ""
        inserted = 1
      }
      { print }
    ' "$APPDIR/AppRun" > "$APPDIR/AppRun.new"
    mv "$APPDIR/AppRun.new" "$APPDIR/AppRun"
    chmod +x "$APPDIR/AppRun"
    grep -q "$HOOK_NAME" "$APPDIR/AppRun" || {
      echo "postprocess-appimage: FAILED to hook AppRun (layout changed?)" >&2
      exit 1
    }
    echo "postprocess-appimage: AppRun now sources $HOOK_NAME"
  fi
else
  echo "postprocess-appimage: $HOOK_SRC missing - no desktop integration" >&2
  exit 1
fi

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
ls -lh "$orig" | awk '{print "postprocess-appimage: " $5 " " $9}'
