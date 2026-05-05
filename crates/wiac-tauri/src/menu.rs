//! Native menu bar — File / Edit / View / Help with the platform-conventional
//! accelerators. Most items emit `tauri::Emitter` events the frontend listens
//! for (so the same UI controls in the toolbar do the work). The `Quit`
//! item exits the app outright.

use tauri::menu::{
    AboutMetadataBuilder, Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};

pub fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    // ── File ──
    let open_file = MenuItemBuilder::with_id("file:open", "Open File…")
        .accelerator("CmdOrCtrl+O")
        .build(app)?;
    let open_project = MenuItemBuilder::with_id("file:open_project", "Open Project…")
        .accelerator("CmdOrCtrl+Shift+O")
        .build(app)?;
    let save_project = MenuItemBuilder::with_id("file:save_project", "Save Project…")
        .accelerator("CmdOrCtrl+S")
        .build(app)?;
    let export_gcode = MenuItemBuilder::with_id("file:export_gcode", "Export G-code…")
        .accelerator("CmdOrCtrl+E")
        .build(app)?;
    let quit = PredefinedMenuItem::quit(app, None)?;
    let file = SubmenuBuilder::new(app, "File")
        .item(&open_file)
        .item(&open_project)
        .item(&save_project)
        .separator()
        .item(&export_gcode)
        .separator()
        .item(&quit)
        .build()?;

    // ── Edit ── (uses platform predefined items so cut/copy/paste work in
    // text inputs — Tauri 2 wires these up against the focused webview).
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    // ── View ──
    let view_2d = MenuItemBuilder::with_id("view:2d", "2D View")
        .accelerator("CmdOrCtrl+1")
        .build(app)?;
    let view_3d = MenuItemBuilder::with_id("view:3d", "3D View")
        .accelerator("CmdOrCtrl+2")
        .build(app)?;
    let toggle_tabs = MenuItemBuilder::with_id("view:toggle_tabs", "Tab Placement Mode")
        .accelerator("CmdOrCtrl+T")
        .build(app)?;
    let view = SubmenuBuilder::new(app, "View")
        .item(&view_2d)
        .item(&view_3d)
        .separator()
        .item(&toggle_tabs)
        .separator()
        .item(&PredefinedMenuItem::fullscreen(app, None)?)
        .build()?;

    // ── Help ──
    let metadata = AboutMetadataBuilder::new()
        .name(Some("wiaConstructor"))
        .version(Some(env!("CARGO_PKG_VERSION")))
        .copyright(Some("GPL-3.0-or-later"))
        .website(Some("https://github.com/wiaconstructor/wiaconstructor"))
        .build();
    let help = SubmenuBuilder::new(app, "Help")
        .item(&PredefinedMenuItem::about(app, Some("About"), Some(metadata))?)
        .build()?;

    MenuBuilder::new(app)
        .item(&file)
        .item(&edit)
        .item(&view)
        .item(&help)
        .build()
}

/// Route menu events to the frontend. Each id matches the `id` argument of
/// the `MenuItemBuilder` calls above; the frontend listens for
/// `app:menu` events with `{action: <id>}` and reacts accordingly.
pub fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.emit("app:menu", id);
    }
}
