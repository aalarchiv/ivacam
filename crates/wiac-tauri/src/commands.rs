//! Tauri command handlers — the in-process equivalent of the HTTP endpoints
//! exposed by `wiac-server`. Frontend calls these via `invoke('name', args)`
//! when running inside the desktop app; the same `WiacClient` interface
//! abstracts over HTTP vs Tauri so component code is transport-agnostic.
//!
//! All three transports (HTTP / Tauri / WASM) hand off to
//! `wiac_core::pipeline::run_pipeline` for the actual CAM work; the only
//! per-transport code is request/response serialization.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use wiac_core::input::text::{render_text_api, RenderTextRequest, RenderTextResponse};
use wiac_core::pipeline::{
    generate_streaming, run_pipeline, CancelToken, PipelineEvent, PipelineRequest,
    PipelineResponse,
};
use wiac_core::{ImportOptions, ImportOutput};

use crate::watcher::ProjectWatcher;

#[derive(Debug)]
pub struct AppState {
    pub watcher: Mutex<ProjectWatcher>,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Serialize)]
pub struct VersionResponse {
    pub version: &'static str,
    pub transport: &'static str,
    pub git_sha: Option<&'static str>,
}

#[tauri::command]
pub fn healthz() -> HealthResponse {
    HealthResponse { ok: true }
}

#[tauri::command]
pub fn version() -> VersionResponse {
    VersionResponse {
        version: env!("CARGO_PKG_VERSION"),
        transport: "tauri",
        git_sha: option_env!("GIT_SHA"),
    }
}

/// Import a DXF/SVG/HPGL file by path. Counterpart of the HTTP `/import`
/// multipart endpoint; the desktop shell can hand a real OS path so we
/// avoid an upload round-trip.
#[tauri::command]
pub async fn import_path(path: String) -> Result<ImportOutput, String> {
    let path = PathBuf::from(path);
    let opts = ImportOptions::default();
    tokio::task::spawn_blocking(move || wiac_core::input::import_path(&path, &opts))
        .await
        .map_err(|e| format!("join error: {e}"))?
        .map_err(|e| format!("{e}"))
}

#[tauri::command]
pub async fn generate(request: PipelineRequest) -> Result<PipelineResponse, String> {
    tokio::task::spawn_blocking(move || run_pipeline(request, |_p, _f, _m| {}))
        .await
        .map_err(|e| format!("join error: {e}"))?
        .map_err(|e| e.to_string())
}

fn token_registry() -> &'static Mutex<HashMap<u32, CancelToken>> {
    static REG: OnceLock<Mutex<HashMap<u32, CancelToken>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_token_id() -> u32 {
    static COUNTER: AtomicU32 = AtomicU32::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[derive(Serialize, Clone)]
pub struct GenerateStreamingResponse {
    pub token_id: u32,
}

/// Streaming generate: returns immediately with a `token_id` the caller
/// can hand to `cancel_generate`. Pipeline events are pushed via the
/// `generate-event:<token_id>` Tauri event; the final result lands on
/// `generate-result:<token_id>` (or `generate-error:<token_id>` on
/// failure / `generate-cancelled:<token_id>` on cancellation).
#[tauri::command]
pub async fn generate_streaming_cmd(
    app: AppHandle,
    request: PipelineRequest,
) -> Result<GenerateStreamingResponse, String> {
    let token_id = next_token_id();
    let cancel = CancelToken::new();
    token_registry()
        .lock()
        .map_err(|e| format!("token registry poisoned: {e}"))?
        .insert(token_id, cancel.clone());
    let event_channel = format!("generate-event:{token_id}");
    let result_channel = format!("generate-result:{token_id}");
    let error_channel = format!("generate-error:{token_id}");
    let cancelled_channel = format!("generate-cancelled:{token_id}");

    std::thread::spawn(move || {
        let app_for_events = app.clone();
        let mut sink = move |ev: PipelineEvent| {
            let _ = app_for_events.emit(&event_channel, ev);
        };
        let res = generate_streaming(request, &cancel, &mut sink);
        let _ = token_registry().lock().map(|mut m| m.remove(&token_id));
        match res {
            Ok(resp) => {
                let _ = app.emit(&result_channel, resp);
            }
            Err(wiac_core::pipeline::PipelineError::Cancelled) => {
                let _ = app.emit(&cancelled_channel, token_id);
            }
            Err(e) => {
                let _ = app.emit(&error_channel, e.to_string());
            }
        }
    });

    Ok(GenerateStreamingResponse { token_id })
}

/// Flip the cancel flag for a previously-issued token. The streaming
/// worker thread consults the flag at coarse granularity inside the
/// pipeline. No-op if the token id is unknown (already completed).
#[tauri::command]
pub fn cancel_generate(token_id: u32) -> Result<(), String> {
    let reg = token_registry()
        .lock()
        .map_err(|e| format!("token registry poisoned: {e}"))?;
    if let Some(token) = reg.get(&token_id) {
        token.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn render_text(request: RenderTextRequest) -> Result<RenderTextResponse, String> {
    tokio::task::spawn_blocking(move || render_text_api(&request))
        .await
        .map_err(|e| format!("join error: {e}"))?
        .map_err(|e| e.to_string())
}

/// Replace the active source-file watch set. Subsequent modifications
/// to any of these paths emit `source-file-changed` events the frontend
/// listens for. Called on project load / when sources change.
#[tauri::command]
pub async fn watch_source_paths(
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    state
        .watcher
        .lock()
        .map_err(|e| format!("watcher mutex poisoned: {e}"))?
        .set_paths(paths)
}

/// Drop every watch slot. Called on project close.
#[tauri::command]
pub async fn unwatch_all(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state
        .watcher
        .lock()
        .map_err(|e| format!("watcher mutex poisoned: {e}"))?
        .unwatch_all()
}

