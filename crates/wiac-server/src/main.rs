//! `wiac-server` — axum HTTP server exposing the JSON contract from
//! `schema/openapi.yaml`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde::Serialize;
use tokio::net::TcpListener;
use axum::http::{HeaderValue, Method};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use wiac_core::input::text::{render_text_api, RenderTextRequest, RenderTextResponse};
use wiac_core::pipeline::{
    generate_streaming, run_pipeline, CancelToken, PipelineError, PipelineEvent, PipelineRequest,
    PipelineResponse,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wiac_server=info,tower_http=info".into()),
        )
        .init();

    let host = std::env::var("WIAC_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("WIAC_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8766);

    let cors = build_cors_layer();

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(version))
        .route("/import", post(import))
        .route("/generate", post(generate))
        .route("/generate/stream", post(generate_stream))
        .route("/generate/cancel/:token_id", post(generate_cancel))
        .route("/text", post(render_text_handler))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::new(AppState::default()));

    let addr = format!("{host}:{port}").parse::<SocketAddr>()?;
    tracing::info!("wiac-server listening on http://{addr}");
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

/// Build a CORS layer from `WIAC_CORS_ORIGINS` (comma-separated).
///
/// - unset or empty: localhost-only allow-list (dev default)
/// - `*` or `any`:   permissive (origin: any). Methods/headers stay restricted to
///                   what the JSON API actually uses.
/// - otherwise:      exact origin match against the supplied list.
fn build_cors_layer() -> CorsLayer {
    let methods = [Method::GET, Method::POST, Method::OPTIONS];
    let headers = [
        axum::http::header::CONTENT_TYPE,
        axum::http::header::ACCEPT,
        axum::http::header::AUTHORIZATION,
    ];
    let raw = std::env::var("WIAC_CORS_ORIGINS").unwrap_or_default();
    let trimmed = raw.trim();
    let origin = if trimmed.is_empty() {
        let defaults: Vec<HeaderValue> = [
            "http://localhost:5173",
            "http://127.0.0.1:5173",
            "http://localhost:1420",
            "http://127.0.0.1:1420",
            "tauri://localhost",
        ]
        .into_iter()
        .filter_map(|s| HeaderValue::from_str(s).ok())
        .collect();
        AllowOrigin::list(defaults)
    } else if trimmed == "*" || trimmed.eq_ignore_ascii_case("any") {
        AllowOrigin::any()
    } else {
        let list: Vec<HeaderValue> = trimmed
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|s| HeaderValue::from_str(s).ok())
            .collect();
        if list.is_empty() {
            tracing::warn!(
                "WIAC_CORS_ORIGINS contained no valid entries; falling back to localhost defaults"
            );
            AllowOrigin::list(
                ["http://localhost:5173", "http://127.0.0.1:5173"]
                    .into_iter()
                    .filter_map(|s| HeaderValue::from_str(s).ok())
                    .collect::<Vec<_>>(),
            )
        } else {
            AllowOrigin::list(list)
        }
    };
    CorsLayer::new()
        .allow_methods(methods)
        .allow_headers(headers)
        .allow_origin(origin)
}

#[derive(Default)]
struct AppState {
    cancel_tokens: Mutex<HashMap<u32, CancelToken>>,
}

static TOKEN_COUNTER: AtomicU32 = AtomicU32::new(1);

fn next_token_id() -> u32 {
    TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed)
}

// ─── DTOs ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Serialize)]
struct VersionResponse {
    version: &'static str,
    transport: &'static str,
    capabilities: Vec<&'static str>,
}

#[derive(Serialize)]
struct ImportResponse<'a> {
    filename: &'a str,
    format: &'a str,
    segments: &'a [wiac_core::Segment],
    layers: &'a [wiac_core::Layer],
    bbox: &'a wiac_core::BBox,
    unit_scale: f64,
    warnings: &'a [String],
}

// `GenerateRequest` and `GenerateResponse` types live in
// `wiac_core::pipeline` so all three transports (HTTP, Tauri, WASM) share
// the same shape. Tabs are keyed by **imported** segment index.
type GenerateRequest = PipelineRequest;
type GenerateResponse = PipelineResponse;

// ─── handlers ──────────────────────────────────────────────────────────────

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION"),
        transport: "rust-server",
        capabilities: vec![
            "import-dxf",
            "generate-gcode",
            "post-linuxcnc",
            "post-grbl",
            "post-hpgl",
        ],
    })
}


async fn import(
    State(_state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut filename = String::new();
    let mut bytes: Vec<u8> = Vec::new();
    let mut format_hint: Option<String> = None;
    while let Some(field) = multipart.next_field().await? {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            filename = field.file_name().unwrap_or("").to_string();
            bytes = field.bytes().await?.to_vec();
        } else if name == "format" {
            format_hint = Some(field.text().await?);
        }
    }
    if bytes.is_empty() {
        return Err(AppError::bad_request("file field missing or empty"));
    }
    let suffix = format_hint
        .clone()
        .or_else(|| {
            std::path::Path::new(&filename)
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
        })
        .unwrap_or_else(|| "dxf".into());

    // Persist to tempfile to use the path-based importer.
    let tmp = tempfile_path(&suffix)?;
    tokio::fs::write(&tmp, &bytes).await?;
    let opts = wiac_core::ImportOptions::default();
    let result = tokio::task::spawn_blocking(move || {
        wiac_core::input::import_path(&tmp, &opts)
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))??;

    let resp = ImportResponse {
        filename: &result.filename,
        format: &result.format,
        segments: &result.segments,
        layers: &result.layers,
        bbox: &result.bbox,
        unit_scale: result.unit_scale,
        warnings: &result.warnings,
    };
    Ok(Json(serde_json::to_value(resp).unwrap()))
}

async fn generate(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<GenerateRequest>,
) -> Result<Json<GenerateResponse>, AppError> {
    run_pipeline(req, |_phase, _fraction, _msg| {})
        .map(Json)
        .map_err(AppError::from)
}

/// Render a TTF font + string → segments. Cross-transport entry point used
/// by the AddTextDialog; the WASM and Tauri transports expose the same
/// shape so the frontend's `WiacClient.renderText` is transport-agnostic.
async fn render_text_handler(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<RenderTextRequest>,
) -> Result<Json<RenderTextResponse>, AppError> {
    tokio::task::spawn_blocking(move || render_text_api(&req))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .map(Json)
        .map_err(AppError::from)
}

/// SSE variant: emits a `token` event with the cancellation handle the
/// client posts to `/generate/cancel/<token>`, followed by per-op
/// `PipelineEvent`s, and finally a `result` (or `cancelled` / `error`)
/// frame. Frontend reads via `fetch` + a hand-rolled SSE parser
/// because EventSource is GET-only.
async fn generate_stream(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GenerateRequest>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<SseEvent>(64);
    let token_id = next_token_id();
    let cancel = CancelToken::new();
    if let Ok(mut map) = state.cancel_tokens.lock() {
        map.insert(token_id, cancel.clone());
    }
    let state_for_worker = Arc::clone(&state);

    let _ = tx
        .send(
            SseEvent::default()
                .event("token")
                .json_data(serde_json::json!({ "token_id": token_id }))
                .expect("token payload"),
        )
        .await;

    tokio::task::spawn_blocking(move || {
        let send = |ev: SseEvent| {
            let _ = tx.blocking_send(ev);
        };
        let mut sink = |pe: PipelineEvent| {
            send(
                SseEvent::default()
                    .event("pipeline")
                    .json_data(&pe)
                    .expect("pipeline payload"),
            );
        };
        let outcome = generate_streaming(req, &cancel, &mut sink);
        if let Ok(mut map) = state_for_worker.cancel_tokens.lock() {
            map.remove(&token_id);
        }
        match outcome {
            Ok(resp) => send(
                SseEvent::default()
                    .event("result")
                    .json_data(&resp)
                    .expect("result payload"),
            ),
            Err(PipelineError::Cancelled) => send(
                SseEvent::default()
                    .event("cancelled")
                    .json_data(serde_json::json!({ "token_id": token_id }))
                    .expect("cancelled payload"),
            ),
            Err(err) => {
                let app_err = AppError::from(err);
                send(
                    SseEvent::default()
                        .event("error")
                        .json_data(serde_json::json!({
                            "status": app_err.status.as_u16(),
                            "message": app_err.message,
                        }))
                        .expect("error payload"),
                );
            }
        }
        // tx drops here → stream completes.
    });

    let stream = ReceiverStream::new(rx).map(Ok);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn generate_cancel(
    State(state): State<Arc<AppState>>,
    Path(token_id): Path<u32>,
) -> Result<Json<serde_json::Value>, AppError> {
    if let Ok(map) = state.cancel_tokens.lock() {
        if let Some(token) = map.get(&token_id) {
            token.cancel();
            return Ok(Json(serde_json::json!({ "ok": true })));
        }
    }
    Ok(Json(serde_json::json!({ "ok": false, "reason": "unknown_token" })))
}

// ─── error type ────────────────────────────────────────────────────────────

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.message });
        (self.status, Json(body)).into_response()
    }
}

impl From<wiac_core::Error> for AppError {
    fn from(e: wiac_core::Error) -> Self {
        use wiac_core::ErrorKind;
        let status = match e.kind {
            ErrorKind::BadInput | ErrorKind::Unsupported | ErrorKind::Misconfigured => {
                StatusCode::BAD_REQUEST
            }
            ErrorKind::Limit => StatusCode::PAYLOAD_TOO_LARGE,
            ErrorKind::Io => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            message: e.to_string(),
        }
    }
}

impl From<wiac_core::pipeline::PipelineError> for AppError {
    fn from(e: wiac_core::pipeline::PipelineError) -> Self {
        use wiac_core::pipeline::PipelineError;
        match e {
            PipelineError::Cancelled => Self {
                status: StatusCode::REQUEST_TIMEOUT,
                message: e.to_string(),
            },
            other => Self::bad_request(other.to_string()),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::internal(e.to_string())
    }
}

impl From<axum::extract::multipart::MultipartError> for AppError {
    fn from(e: axum::extract::multipart::MultipartError) -> Self {
        Self::bad_request(e.to_string())
    }
}

fn tempfile_path(suffix: &str) -> Result<PathBuf, AppError> {
    let mut name = format!("wiac-{}.{}", uuid_like(), suffix);
    name.retain(|c| !c.is_whitespace());
    Ok(std::env::temp_dir().join(name))
}

fn uuid_like() -> String {
    // No external uuid dep; this is good enough for unique tempfiles.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{nanos:x}-{pid:x}")
}
