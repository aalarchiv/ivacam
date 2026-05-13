//! wiaConstructor desktop: Tauri 2 shell wrapping the Svelte frontend.
//!
//! The frontend is built by Vite (see `tauri.conf.json::build`), and Tauri
//! serves it via the asset protocol. Native commands live in `commands.rs`
//! and mirror the JSON contract the HTTP / WASM transports already speak.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod watcher;

use std::sync::Mutex;

use tauri::{Emitter, Manager};

use commands::AppState;
use watcher::ProjectWatcher;

fn main() {
    if let Err(err) = run() {
        eprintln!("wiac-desktop: fatal: {err:?}");
        std::process::exit(1);
    }
}

fn run() -> tauri::Result<()> {
    let log_plugin = tauri_plugin_log::Builder::new()
        .level(log::LevelFilter::Info)
        .target(tauri_plugin_log::Target::new(
            tauri_plugin_log::TargetKind::LogDir { file_name: None },
        ))
        .target(tauri_plugin_log::Target::new(
            tauri_plugin_log::TargetKind::Stdout,
        ))
        .max_file_size(2 * 1024 * 1024)
        .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepAll)
        .build();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(log_plugin)
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(AppState {
                watcher: Mutex::new(ProjectWatcher::new(handle.clone())),
            });
            // No native window menu — the Svelte UI owns the menubar so the
            // user sees one consistent set of File / Edit / View / Tools /
            // Help dropdowns instead of duplicates above and below.

            // OS file-association launches deliver the path as argv. Forward
            // it to the frontend so the same import flow runs as if the user
            // had picked from the dialog.
            if let Some(arg) = std::env::args().nth(1) {
                let p = std::path::PathBuf::from(&arg);
                if p.is_file() {
                    if let Some(window) = app.get_webview_window("main") {
                        let path_str = arg.clone();
                        // Wait for the frontend to settle before emitting.
                        let window = window.clone();
                        tauri::async_runtime::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                            let _ = window.emit("app:open_path", path_str);
                        });
                    }
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::healthz,
            commands::version,
            commands::import_path,
            commands::generate,
            commands::generate_streaming_cmd,
            commands::generate_streaming_ready_cmd,
            commands::cancel_generate,
            commands::render_text,
            commands::compute_helix_radius_cmd,
            commands::read_workspace_file,
            commands::write_workspace_file,
            commands::watch_source_paths,
            commands::unwatch_all,
        ])
        .run(tauri::generate_context!())
}
