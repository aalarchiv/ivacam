//! Tauri command handlers — the in-process equivalent of the HTTP endpoints
//! exposed by `wiac-server`. Frontend calls these via `invoke('name', args)`
//! when running inside the desktop app; the same `WiacClient` interface
//! abstracts over HTTP vs Tauri so component code is transport-agnostic.
//!
//! All three transports (HTTP / Tauri / WASM) hand off to
//! `wiac_core::pipeline::run_pipeline` for the actual CAM work; the only
//! per-transport code is request/response serialization.

use std::path::PathBuf;

use serde::Serialize;

use wiac_core::cam::setup::Setup;
use wiac_core::pipeline::{run_pipeline, PipelineRequest, PipelineResponse};
use wiac_core::{ImportOptions, ImportOutput};

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

#[derive(Serialize)]
pub struct DefaultsResponse {
    pub setup: Setup,
    pub schema: serde_json::Value,
    pub definitions: serde_json::Value,
}

#[tauri::command]
pub fn defaults() -> Result<DefaultsResponse, String> {
    let components = wiac_core::schema::components_schemas();
    let setup_schema = components
        .get("Setup")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok(DefaultsResponse {
        setup: Setup::default(),
        schema: setup_schema,
        definitions: components,
    })
}
