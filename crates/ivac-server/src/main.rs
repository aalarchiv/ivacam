//! `ivac-server` — axum HTTP server exposing the JSON contract from
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
use axum::http::{HeaderValue, Method};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use ivac_core::input::text::{render_text_api, RenderTextRequest, RenderTextResponse};
use ivac_core::pipeline::{
    generate_streaming, run_pipeline, CancelToken, PipelineError, PipelineEvent, PipelineRequest,
    PipelineResponse,
};
use ivac_core::{compute_helix_radius, HelixRadiusRequest, HelixRadiusResponse};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ivac_server=info,tower_http=info".into()),
        )
        .init();

    let host = std::env::var("IVAC_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("IVAC_PORT")
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
        .route("/helix-radius", post(helix_radius_handler))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::new(AppState::default()));

    let addr = format!("{host}:{port}").parse::<SocketAddr>()?;
    tracing::info!("ivac-server listening on http://{addr}");
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

/// Build a CORS layer from `IVAC_CORS_ORIGINS` (comma-separated).
///
/// - unset or empty: localhost-only allow-list (dev default)
/// - `*` or `any`: permissive (origin: any). Methods/headers stay restricted
///   to what the JSON API actually uses.
/// - otherwise: exact origin match against the supplied list.
fn build_cors_layer() -> CorsLayer {
    let methods = [Method::GET, Method::POST, Method::OPTIONS];
    let headers = [
        axum::http::header::CONTENT_TYPE,
        axum::http::header::ACCEPT,
        axum::http::header::AUTHORIZATION,
    ];
    let raw = std::env::var("IVAC_CORS_ORIGINS").unwrap_or_default();
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
                "IVAC_CORS_ORIGINS contained no valid entries; falling back to localhost defaults"
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
    segments: &'a [ivac_core::Segment],
    layers: &'a [ivac_core::Layer],
    bbox: &'a ivac_core::BBox,
    unit_scale: f64,
    warnings: &'a [String],
}

// `GenerateRequest` and `GenerateResponse` types live in
// `ivac_core::pipeline` so all three transports (HTTP, Tauri, WASM) share
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
                .map(str::to_ascii_lowercase)
        })
        .unwrap_or_else(|| "dxf".into());

    // Persist to tempfile to use the path-based importer.
    let tmp = tempfile_path(&suffix);
    tokio::fs::write(&tmp, &bytes).await?;
    let opts = ivac_core::ImportOptions::default();
    let result = tokio::task::spawn_blocking(move || ivac_core::input::import_path(&tmp, &opts))
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
/// by the `AddTextDialog`; the WASM and Tauri transports expose the same
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

/// Helix auto-fit preview — runs the same inscribed-circle search the
/// generator does at run time, so the `OpPropertiesPanel` can show the
/// detected radius before the user clicks Generate.
async fn helix_radius_handler(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<HelixRadiusRequest>,
) -> Result<Json<HelixRadiusResponse>, AppError> {
    let resp = tokio::task::spawn_blocking(move || compute_helix_radius(req))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(resp))
}

/// SSE variant: emits a `token` event with the cancellation handle the
/// client posts to `/generate/cancel/<token>`, followed by per-op
/// `PipelineEvent`s, and finally a `result` (or `cancelled` / `error`)
/// frame. Frontend reads via `fetch` + a hand-rolled SSE parser
/// because `EventSource` is GET-only.
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
                // Stream the full structured `ivac_core::Error` so the frontend
                // sees the same shape on SSE that it sees on /generate (luf1).
                // The HTTP status of the SSE response itself is already 200
                // (sent at stream open); the per-event payload carries the
                // error semantics.
                let app_err = AppError::from(err);
                send(
                    SseEvent::default()
                        .event("error")
                        .json_data(&app_err.inner)
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
    Ok(Json(
        serde_json::json!({ "ok": false, "reason": "unknown_token" }),
    ))
}

// ─── error type ────────────────────────────────────────────────────────────

/// HTTP error wrapper around the cross-transport `ivac_core::Error`.
///
/// The full structured form (`kind` / `message` / `recovery_hint` / `auto_fix`
/// / `span`) is serialized as the JSON response body so HTTP clients see the
/// same shape the Tauri and WASM transports surface (luf1). The HTTP status
/// code is derived from `ivac_core::ErrorKind` so the response also remains
/// REST-ful.
#[derive(Debug)]
struct AppError {
    status: StatusCode,
    inner: ivac_core::Error,
}

impl AppError {
    fn from_core(inner: ivac_core::Error) -> Self {
        Self {
            status: status_for_kind(inner.kind),
            inner,
        }
    }
    fn bad_request(msg: impl Into<String>) -> Self {
        Self::from_core(ivac_core::Error::bad_input(msg))
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self::from_core(ivac_core::Error::internal(msg))
    }
}

fn status_for_kind(kind: ivac_core::ErrorKind) -> StatusCode {
    use ivac_core::ErrorKind;
    match kind {
        ErrorKind::BadInput | ErrorKind::Unsupported | ErrorKind::Misconfigured => {
            StatusCode::BAD_REQUEST
        }
        ErrorKind::Limit => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorKind::Io => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(self.inner)).into_response()
    }
}

impl From<ivac_core::Error> for AppError {
    fn from(e: ivac_core::Error) -> Self {
        Self::from_core(e)
    }
}

impl From<ivac_core::pipeline::PipelineError> for AppError {
    fn from(e: ivac_core::pipeline::PipelineError) -> Self {
        use ivac_core::pipeline::PipelineError;
        match e {
            // Client's cancel arrived after the response stream began (or the
            // synchronous /generate handler raced cancellation). 408 keeps the
            // legacy mapping; the body still carries the uniform structured
            // shape so frontend error parsing remains a single code path.
            PipelineError::Cancelled => Self {
                status: StatusCode::REQUEST_TIMEOUT,
                inner: ivac_core::Error::internal(e.to_string()),
            },
            // Anything else routes through the structured-error converter so
            // recovery hints + auto-fix survive into the HTTP body.
            other => {
                let inner = other
                    .to_structured(None)
                    .unwrap_or_else(|| ivac_core::Error::misconfigured(other.to_string()));
                Self::from_core(inner)
            }
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::from_core(ivac_core::Error::io(e.to_string()))
    }
}

impl From<axum::extract::multipart::MultipartError> for AppError {
    fn from(e: axum::extract::multipart::MultipartError) -> Self {
        Self::bad_request(e.to_string())
    }
}

fn tempfile_path(suffix: &str) -> PathBuf {
    let mut name = format!("ivac-{}.{}", uuid_like(), suffix);
    name.retain(|c| !c.is_whitespace());
    std::env::temp_dir().join(name)
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use ivac_core::{AutoFix, Error as WiacError, ErrorKind, SourceSpan};

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn http_error_response_carries_full_structured_shape() {
        // The whole point of luf1: a `ivac_core::Error` with recovery hint
        // + auto-fix + source span must survive the HTTP round-trip with
        // every field intact, not collapse to `{error: "msg"}`.
        let inner = WiacError::misconfigured("op 2 references missing tool 9")
            .with_hint("Pick a tool from the library.")
            .with_auto_fix(AutoFix::AssignTool {
                op_id: 2,
                suggested_tool_id: 1,
            })
            .with_span(SourceSpan {
                file: "test.dxf".into(),
                line: 12,
                column: 3,
            });
        let app_err = AppError::from(inner.clone());
        assert_eq!(app_err.status, StatusCode::BAD_REQUEST);

        let resp = app_err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        let parsed: WiacError = serde_json::from_value(body).unwrap();
        assert_eq!(parsed, inner);
    }

    #[test]
    fn status_for_kind_matches_legacy_mapping() {
        assert_eq!(
            status_for_kind(ErrorKind::BadInput),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for_kind(ErrorKind::Misconfigured),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for_kind(ErrorKind::Unsupported),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for_kind(ErrorKind::Limit),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            status_for_kind(ErrorKind::Io),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            status_for_kind(ErrorKind::Internal),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn bad_request_helper_synthesizes_structured_error() {
        // Plain `bad_request("...")` (used for multipart / missing-field
        // failures) still emits a structured body so the frontend always
        // sees `{kind, message}` regardless of which path raised the error.
        let app_err = AppError::bad_request("file field missing");
        assert_eq!(app_err.status, StatusCode::BAD_REQUEST);
        assert_eq!(app_err.inner.kind, ErrorKind::BadInput);
        assert_eq!(app_err.inner.message, "file field missing");
    }

    #[test]
    fn internal_helper_synthesizes_structured_error() {
        let app_err = AppError::internal("join error");
        assert_eq!(app_err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(app_err.inner.kind, ErrorKind::Internal);
    }

    #[test]
    fn pipeline_unknown_tool_promotes_auto_fix_into_http_body() {
        // PipelineError::UnknownTool carries enough context to produce a
        // structured Error with AutoFix::AssignTool. Verify the From<> impl
        // routes through to_structured() instead of stringifying.
        let pe = ivac_core::pipeline::PipelineError::UnknownTool(2, 99);
        let app_err = AppError::from(pe);
        assert_eq!(app_err.status, StatusCode::BAD_REQUEST);
        assert_eq!(app_err.inner.kind, ErrorKind::Misconfigured);
        assert!(app_err.inner.recovery_hint.is_some());
    }

    #[test]
    fn pipeline_cancelled_maps_to_408_with_structured_body() {
        let pe = ivac_core::pipeline::PipelineError::Cancelled;
        let app_err = AppError::from(pe);
        assert_eq!(app_err.status, StatusCode::REQUEST_TIMEOUT);
        // Still a structured body; clients can distinguish via the 408
        // status without parsing the message text.
        assert_eq!(app_err.inner.kind, ErrorKind::Internal);
    }
}
