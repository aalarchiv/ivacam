//! wiaConstructor desktop: Tauri 2 shell wrapping the Svelte frontend.
//!
//! The frontend is built by Vite (see `tauri.conf.json::build`), and Tauri
//! serves it via the asset protocol. Native commands live in `commands.rs`
//! and mirror the JSON contract the HTTP / WASM transports already speak.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

fn main() {
    if let Err(err) = run() {
        eprintln!("wiac-desktop: fatal: {err:?}");
        std::process::exit(1);
    }
}

fn run() -> tauri::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            commands::healthz,
            commands::version,
            commands::import_path,
            commands::generate,
            commands::defaults,
        ])
        .run(tauri::generate_context!())
}
