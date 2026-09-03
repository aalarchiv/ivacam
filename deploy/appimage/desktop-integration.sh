#!/usr/bin/env bash
# Sourced by the AppImage's AppRun (see scripts/postprocess-appimage.sh, which
# copies this file into the AppDir and adds the `source` line).
#
# WHY: Cinnamon, GNOME Shell and most other panels do NOT take a window's
# taskbar icon from _NET_WM_ICON. They match the window's WM_CLASS
# ("ivac-desktop") against the installed .desktop entries and then resolve
# that entry's `Icon=` name through the icon theme. An AppImage installs
# nothing, so the match fails and the panel shows its generic
# application-x-executable tile (on Mint-Y: a grey gear on white) — which is
# exactly the "no icon in the taskbar" bug. Shipping a bigger _NET_WM_ICON
# does not help; the panel never looks at it.
#
# So mirror what the .deb does, but into the user's own data dir: drop the
# icons into ~/.local/share/icons/hicolor and write a .desktop entry whose
# Exec points back at this AppImage. That fixes the panel icon, the Alt-Tab
# icon and the application menu entry in one go.
#
# Everything here is best-effort: the AppRun runs under `set -e`, so a
# failure must never take the app down with it. Set IVACAM_NO_DESKTOP_INTEGRATION=1
# (or the conventional NO_DESKTOP_INTEGRATION=1) to skip it entirely.

_ivacam_integrate_desktop() {
    [ -n "${IVACAM_NO_DESKTOP_INTEGRATION:-}" ] && return 0
    [ -n "${NO_DESKTOP_INTEGRATION:-}" ] && return 0

    # $APPIMAGE is set by the AppImage runtime only. When it is empty we are
    # running from an extracted AppDir, and an Exec= line pointing into a temp
    # directory would just rot — skip.
    local target="${APPIMAGE:-}"
    [ -n "$target" ] || return 0
    [ -n "${HOME:-}" ] || return 0

    local appdir="${APPDIR:-${this_dir:-}}"
    [ -d "$appdir" ] || return 0

    # AppImageLauncher / Gearlever already own the integration on this system.
    [ -e "$HOME/.local/share/applications/appimagekit_"*"ivaCAM.desktop" ] 2>/dev/null && return 0

    local data="${XDG_DATA_HOME:-$HOME/.local/share}"

    local src size dest
    for src in "$appdir"/usr/share/icons/hicolor/*/apps/ivac-desktop.png; do
        [ -e "$src" ] || continue
        size=$(basename "$(dirname "$(dirname "$src")")")
        # Skip the bogus "256x256@2"-style buckets the bundler emits; they are
        # not directories the icon theme spec defines.
        case "$size" in *@*) continue ;; esac
        dest="$data/icons/hicolor/$size/apps/ivac-desktop.png"
        mkdir -p "$(dirname "$dest")" || continue
        cmp -s "$src" "$dest" || cp -f "$src" "$dest" || true
    done

    local entry="$data/applications/ivaCAM.desktop"
    local source_entry="$appdir/usr/share/applications/ivaCAM.desktop"
    [ -e "$source_entry" ] || return 0
    mkdir -p "$(dirname "$entry")" || return 0

    local generated
    generated=$(sed \
        -e "s|^Exec=.*|Exec=\"$target\" %U|" \
        -e "s|^TryExec=.*|TryExec=$target|" \
        "$source_entry") || return 0
    generated="$generated
X-AppImage-Path=$target"

    # Rewrite only on change, so a launch does not needlessly touch the file
    # (and make every desktop environment re-scan on every start).
    if [ "$(cat "$entry" 2>/dev/null)" != "$generated" ]; then
        printf '%s\n' "$generated" > "$entry" || return 0
        chmod +x "$entry" 2>/dev/null || true
        command -v update-desktop-database >/dev/null 2>&1 &&
            update-desktop-database "$data/applications" >/dev/null 2>&1
        command -v gtk-update-icon-cache >/dev/null 2>&1 &&
            gtk-update-icon-cache -tq "$data/icons/hicolor" >/dev/null 2>&1
    fi
    return 0
}

_ivacam_integrate_desktop >/dev/null 2>&1 || true
