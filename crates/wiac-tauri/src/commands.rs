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
use wiac_core::{Error as WiacError, ImportOptions, ImportOutput};

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

    let project = request.project.clone();
    std::thread::spawn(move || {
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
            Ok(Err(wiac_core::pipeline::PipelineError::Cancelled)) => {
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

/// Serialize a structured `wiac_core::Error` to JSON the frontend can
/// detect and parse via `tryParseStructuredError`. The string remains
/// the Tauri error type (per existing API), but its content is now JSON
/// for the frontend to introspect.
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

