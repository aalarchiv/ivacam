//! Tauri command handlers — the in-process equivalent of the HTTP endpoints
//! exposed by `ivac-server`. Frontend calls these via `invoke('name', args)`
//! when running inside the desktop app; the same `WiacClient` interface
//! abstracts over HTTP vs Tauri so component code is transport-agnostic.
//!
//! All three transports (HTTP / Tauri / WASM) hand off to
//! `ivac_core::pipeline::run_pipeline` for the actual CAM work; the only
//! per-transport code is request/response serialization.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Reference `Instant` captured once at first read. Lets us express
/// the previous close-request time as a u64 of milliseconds since
/// this reference — small enough for `AtomicU64` (covers 584M years),
/// monotonic, and 0 keeps its "never set" sentinel value.
pub fn process_start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use ivac_core::input::text::{
    render_text_api, render_text_layer_api, RenderTextLayerResponse, RenderTextRequest,
    RenderTextResponse,
};
use ivac_core::pipeline::{
    clear_pipeline_cache, generate_streaming, run_pipeline, CancelToken, PipelineEvent,
    PipelineRequest, PipelineResponse,
};
use ivac_core::project::TextLayer;
use ivac_core::{
    compute_helix_radius, Error as WiacError, HelixRadiusRequest, HelixRadiusResponse,
    ImportOptions, ImportOutput,
};

use crate::watcher::ProjectWatcher;

#[derive(Debug)]
pub struct AppState {
    pub watcher: Mutex<ProjectWatcher>,
    /// Set true by `confirm_close` once the frontend has either
    /// confirmed a dirty-discard or determined there is nothing to
    /// lose. The `CloseRequested` handler reads this flag to decide
    /// whether to prevent the close and bounce a prompt back to the
    /// UI, or let the window destroy normally (qjec).
    pub close_confirmed: AtomicBool,
    /// Milliseconds since `process_start()` of the previous
    /// `CloseRequested` we intercepted; `0` means "never". Lets a
    /// second OS-window close attempt within 3 seconds force-quit
    /// even if the frontend never responded to the first prompt —
    /// without this escape hatch a broken Svelte reactivity scheduler
    /// would trap the user with no way to quit short of `SIGKILL`.
    pub last_close_attempt_ms: AtomicU64,
}

/// Frontend calls this after the user clicks "Quit" on the in-app
/// close confirmation (or immediately if nothing is dirty). Flips
/// `close_confirmed` so the next `CloseRequested` event passes
/// through, then asks the window to close again.
// Tauri's command macro dispatches with an owned `AppHandle` — we use
// it only by reference here, but the signature is dictated by the
// `#[tauri::command]` calling contract.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn confirm_close(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    state.close_confirmed.store(true, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window("main") {
        window.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Frontend calls this from its global error handlers so uncaught JS
/// errors land on the desktop binary's stderr. Preferred over an
/// in-DOM banner for production diagnostics — a terminal user running
/// the AppImage gets the error directly, and stderr is what log
/// aggregators / journalctl already capture. The banner is reserved
/// for `IVAC_DEBUG=1` sessions.
//
// Tauri's command macro hands an owned `String` over the IPC bridge;
// taking `&str` here would force a re-borrow on every call. Per-call
// allow rather than relaxing the workspace lint.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn log_error(msg: String) {
    eprintln!("[ivac-frontend] {msg}");
}

/// Returns true when the user launched with `IVAC_DEBUG=1` (or any
/// truthy value). Gates the in-DOM error banner — production users
/// get clean UI, developers running with the flag see the banner on
/// top of everything for live diagnostics.
#[must_use]
#[tauri::command]
pub fn is_debug() -> bool {
    matches!(
        std::env::var("IVAC_DEBUG").as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES")
    )
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

/// eu2b: drop every entry from the process-global pipeline cache.
/// Frontend project-load / replace flows call this whenever the
/// machine config or tool library changes since the last load — the
/// cache key already encodes both fingerprints (so stale gcode can't
/// surface), but the entries from the previous project are dead
/// memory and would only get reclaimed by LRU eviction. Clearing on
/// project boundaries keeps the working set bounded by ops in the
/// CURRENT project.
#[tauri::command]
pub fn clear_pipeline_cache_cmd() {
    clear_pipeline_cache();
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
    tokio::task::spawn_blocking(move || ivac_core::input::import_path(&path, &opts))
        .await
        .map_err(|e| internal(format!("join error: {e}")))?
        .map_err(serialize_error)
}

#[tauri::command]
pub async fn generate(request: PipelineRequest) -> Result<PipelineResponse, String> {
    let project = request.project.clone();
    let result = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_pipeline(request, |_p, _f, _m| {})
        }))
    })
    .await
    .map_err(|e| internal(format!("join error: {e}")))?;
    match result {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => match e.to_structured(Some(&project)) {
            Some(structured) => Err(serialize_error(structured)),
            None => Err(internal("cancelled")),
        },
        Err(panic) => Err(serialize_error(
            WiacError::internal(format!("panic: {}", panic_message(&panic)))
                .with_hint("Please report this bug — see the toast for details."),
        )),
    }
}

fn token_registry() -> &'static Mutex<HashMap<u32, CancelToken>> {
    static REG: OnceLock<Mutex<HashMap<u32, CancelToken>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Pending generate runs whose worker thread is waiting for the FE's
/// `generate_streaming_ready_cmd` handshake. When the FE finishes
/// registering event listeners, it sends the signal here and the worker
/// proceeds. Without this gate, fast pipelines (empty / 1-op projects)
/// can emit `generate-result:<token>` before the FE's listener for that
/// event has finished registering, dropping the terminal event and
/// hanging the Generate UI.
fn ready_registry() -> &'static Mutex<HashMap<u32, std::sync::mpsc::Sender<()>>> {
    static REG: OnceLock<Mutex<HashMap<u32, std::sync::mpsc::Sender<()>>>> = OnceLock::new();
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

    // Per-token oneshot channel the worker thread blocks on until the FE
    // signals it's done registering listeners. See `ready_registry`.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    {
        if let Ok(mut reg) = ready_registry().lock() {
            reg.insert(token_id, ready_tx);
        }
    }

    let project = request.project.clone();
    std::thread::spawn(move || {
        // Wait for the FE handshake, capped so a buggy FE that forgets to
        // signal can't strand the worker forever. 2 s is generously more
        // than `listen()` round-trips ever take.
        let _ = ready_rx.recv_timeout(std::time::Duration::from_secs(2));
        let _ = ready_registry().lock().map(|mut m| m.remove(&token_id));
        let app_for_events = app.clone();
        let mut sink = move |ev: PipelineEvent| {
            let _ = app_for_events.emit(&event_channel, ev);
        };
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            generate_streaming(request, &cancel, &mut sink)
        }));
        let _ = token_registry().lock().map(|mut m| m.remove(&token_id));
        match res {
            Ok(Ok(resp)) => {
                let _ = app.emit(&result_channel, resp);
            }
            Ok(Err(ivac_core::pipeline::PipelineError::Cancelled)) => {
                let _ = app.emit(&cancelled_channel, token_id);
            }
            Ok(Err(e)) => {
                let payload = match e.to_structured(Some(&project)) {
                    Some(s) => serialize_error(s),
                    None => internal("cancelled"),
                };
                let _ = app.emit(&error_channel, payload);
            }
            Err(panic) => {
                let payload = serialize_error(
                    WiacError::internal(format!("panic: {}", panic_message(&panic)))
                        .with_hint("Please report this bug — see the toast for details."),
                );
                let _ = app.emit(&error_channel, payload);
            }
        }
    });

    Ok(GenerateStreamingResponse { token_id })
}

/// FE → backend handshake confirming all four `generate-{event,result,
/// cancelled,error}:<token>` listeners are registered. The worker
/// thread (spawned in `generate_streaming_cmd`) blocks on this signal
/// before it starts the pipeline, eliminating the race where a fast
/// pipeline finishes and emits its terminal event before the FE's
/// listener for that event has finished registering, which used to
/// leave the Generate UI stuck in "running" / "cancelling".
// `Result<(), String>` is the Tauri-command shape every other command in
// this file returns; keeping it uniform lets the frontend reach for the
// same error-handling helper regardless of which command failed, even
// though this particular path is currently infallible.
#[allow(clippy::unnecessary_wraps)]
#[tauri::command]
pub fn generate_streaming_ready_cmd(token_id: u32) -> Result<(), String> {
    if let Ok(mut reg) = ready_registry().lock() {
        if let Some(tx) = reg.remove(&token_id) {
            let _ = tx.send(());
        }
    }
    Ok(())
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
        .map_err(|e| internal(format!("join error: {e}")))?
        .map_err(serialize_error)
}

#[tauri::command]
pub async fn render_text_layer(layer: TextLayer) -> Result<RenderTextLayerResponse, String> {
    tokio::task::spawn_blocking(move || render_text_layer_api(&layer))
        .await
        .map_err(|e| internal(format!("join error: {e}")))?
        .map_err(serialize_error)
}

#[tauri::command]
pub async fn compute_helix_radius_cmd(
    req: HelixRadiusRequest,
) -> Result<HelixRadiusResponse, String> {
    tokio::task::spawn_blocking(move || compute_helix_radius(req))
        .await
        .map_err(|e| internal(format!("join error: {e}")))
}

/// Serialize a structured `ivac_core::Error` to JSON the frontend can
/// detect and parse via `tryParseStructuredError`. The string remains
/// the Tauri error type (per existing API), but its content is now JSON
/// for the frontend to introspect.
// Callers drop `err` after the JSON encoding; borrowing would force them
// to keep the structured error alive past the conversion just to satisfy
// clippy.
#[allow(clippy::needless_pass_by_value)]
fn serialize_error(err: WiacError) -> String {
    serde_json::to_string(&err).unwrap_or_else(|_| err.to_string())
}

fn internal(msg: impl Into<String>) -> String {
    serialize_error(WiacError::internal(msg))
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Resolve the absolute path to the workspace JSON, ensuring the parent
/// directory exists. The frontend never sees the path — it just reads /
/// writes opaque blobs through the two commands below.
fn workspace_path<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    let dir = dir.join("ivacam");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create_dir_all {}: {e}", dir.display()))?;
    Ok(dir.join("workspace.json"))
}

/// Read the workspace JSON file. Returns `Ok(None)` when the file does
/// not exist yet (first launch) — the frontend treats that as "use
/// defaults". Other I/O errors propagate as `Err` so the user sees
/// them in the toast.
#[tauri::command]
pub async fn read_workspace_file(app: AppHandle) -> Result<Option<String>, String> {
    let path = workspace_path(&app)?;
    match tokio::fs::read_to_string(&path).await {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Atomic write: stage to `<path>.tmp`, then rename. Avoids leaving a
/// half-written workspace.json behind on a crash mid-write.
#[tauri::command]
pub async fn write_workspace_file(app: AppHandle, json: String) -> Result<(), String> {
    let path = workspace_path(&app)?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, &json)
        .await
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|e| format!("rename {} → {}: {e}", tmp.display(), path.display()))?;
    Ok(())
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
