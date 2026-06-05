//! ivaCAM desktop: Tauri 2 shell wrapping the Svelte frontend.
//!
//! The frontend is built by Vite (see `tauri.conf.json::build`), and Tauri
//! serves it via the asset protocol. Native commands live in `commands.rs`
//! and mirror the JSON contract the HTTP / WASM transports already speak.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod watcher;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Force-close window: a second `CloseRequested` within this many
/// milliseconds bypasses the in-app confirmation. Lets the user
/// escape a frontend whose reactivity scheduler is stuck.
const FORCE_CLOSE_WINDOW_MS: u64 = 3_000;

use tauri::{Emitter, Manager};

use commands::AppState;
use watcher::ProjectWatcher;

fn main() {
    if let Err(err) = run() {
        eprintln!("ivac-desktop: fatal: {err:?}");
        std::process::exit(1);
    }
}

/// Linux only: find every `WebKit` helper process spawned by this
/// `ivac-desktop` instance — the renderer (`WebKitWebProcess`), the
/// network process, and the GPU process — and SIGKILL them.
///
/// Mesa's `libgallium` registers an atexit destructor that, when run
/// from inside the `WebKit` renderer's normal `exit()` path on Arch /
/// recent Mesa, double-frees an internal allocation; glibc's malloc
/// heap-corruption detector catches it and SIGABRTs (bd issue 2re4).
///
/// Killing the children with SIGKILL while we're still alive means the
/// renderer dies via signal before its `main()` can return, so
/// `exit()` and the broken atexit chain never run. The kernel reclaims
/// the renderer's memory cleanly; on the main-process side `WebKit`
/// has already handed the `GtkWindow` back by the time `ExitRequested`
/// fires, so there's nothing live left to coordinate with the
/// renderer.
///
/// We filter by `PPid == our_pid` so a SIGKILL never escapes this
/// instance — even if another ivac-desktop happens to be running, its
/// renderer children stay untouched.
#[cfg(target_os = "linux")]
fn kill_webkit_children() {
    use std::fs;
    let our_pid = std::process::id();
    let Ok(entries) = fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(pid) = name.parse::<i32>() else {
            continue;
        };
        if pid <= 1 {
            continue;
        }

        let status_path = entry.path().join("status");
        let Ok(status) = fs::read_to_string(&status_path) else {
            continue;
        };
        let is_our_child = status.lines().any(|line| {
            line.strip_prefix("PPid:")
                .and_then(|rest| rest.trim().parse::<u32>().ok())
                == Some(our_pid)
        });
        if !is_our_child {
            continue;
        }

        let cmdline_path = entry.path().join("cmdline");
        let Ok(cmdline) = fs::read_to_string(&cmdline_path) else {
            continue;
        };
        let is_webkit_helper = cmdline.contains("WebKitWebProcess")
            || cmdline.contains("WebKitNetworkProcess")
            || cmdline.contains("WebKitGPUProcess");
        if !is_webkit_helper {
            continue;
        }

        // SAFETY: signalling a pid we just read from /proc; if the
        // process has already been reaped between the readdir and here,
        // `kill` returns ESRCH which is benign. PIDs are not reused
        // until we wait() on them (or they're reaped by init after we
        // exit), so there's no PID-recycle window.
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
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
                close_confirmed: AtomicBool::new(false),
                last_close_attempt_ms: AtomicU64::new(0),
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
            commands::render_text_layer,
            commands::compute_helix_radius_cmd,
            commands::read_workspace_file,
            commands::write_workspace_file,
            commands::watch_source_paths,
            commands::unwatch_all,
            commands::confirm_close,
            commands::log_error,
            commands::is_debug,
            commands::clear_pipeline_cache_cmd,
        ])
        // qjec: intercept window close so the user can confirm
        // discarding unsaved work. First CloseRequested call emits an
        // event to the frontend and prevents close; the frontend
        // responds by either invoking `confirm_close` (which flips the
        // flag and re-issues close) or doing nothing (keep editing).
        //
        // Escape hatch: a second OS-close attempt within 3 seconds
        // force-closes even without `confirm_close`. This catches the
        // case where the Svelte reactivity scheduler is dead (a thrown
        // effect can silently abort it) — the close event listener
        // fires but the prompt UI never paints, so the user has no way
        // to confirm. Force-closing on double-attempt keeps the user
        // from being trapped.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.app_handle().state::<AppState>();
                if state.close_confirmed.load(Ordering::SeqCst) {
                    return;
                }
                // `now_ms` is non-zero after the first close attempt
                // because process_start() is captured at app launch.
                // `0` is the sentinel for "no previous attempt".
                let now_ms = u64::try_from(commands::process_start().elapsed().as_millis())
                    .unwrap_or(u64::MAX);
                let prev_ms = state.last_close_attempt_ms.swap(now_ms, Ordering::SeqCst);
                let within_window =
                    prev_ms != 0 && now_ms.saturating_sub(prev_ms) <= FORCE_CLOSE_WINDOW_MS;
                if within_window {
                    state.close_confirmed.store(true, Ordering::SeqCst);
                    return;
                }
                api.prevent_close();
                let _ = window.emit("app:close_requested", ());
            }
        })
        .build(tauri::generate_context!())?
        .run(|_app, event| {
            // 2re4: SIGKILL WebKit's helper processes the moment we
            // know the app is about to exit, so the renderer dies via
            // signal instead of returning from `main()` and tripping
            // Mesa's broken atexit destructor. ExitRequested fires
            // after the last window has confirmed close (any
            // save-prompt or close-prevention has already resolved),
            // before WebKit's main-side teardown begins.
            #[cfg(target_os = "linux")]
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                kill_webkit_children();
            }
            // `event` is consumed only by the Linux teardown above; keep the
            // binding live on other targets so `clippy -D warnings` (run on
            // the win/mac CI lanes) doesn't trip on an unused variable.
            #[cfg(not(target_os = "linux"))]
            let _ = &event;
        });
    Ok(())
}
