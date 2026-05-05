//! `wiac-server` — axum HTTP server exposing the JSON contract from
//! `schema/openapi.yaml`. Drop-in replacement for the Stage-1 Python
//! FastAPI bridge.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use wiac_core::cam::chaining::{classify_containment, segments_to_objects};
use wiac_core::cam::offsets::{parallel_offset_object, pocket_for_object, PolylineOffset};
use wiac_core::cam::setup::{Setup, ToolOffset};
use wiac_core::gcode::{emit_polylines, grbl, hpgl, linuxcnc, preview};

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

    let cors = CorsLayer::new()
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_origin(Any);

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(version))
        .route("/import", post(import))
        .route("/generate", post(generate))
        .route("/defaults", get(defaults))
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

#[derive(Default)]
struct AppState {}

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

#[derive(Deserialize)]
struct GenerateRequest {
    segments: Vec<wiac_core::Segment>,
    #[serde(default)]
    setup: Option<Setup>,
    #[serde(default)]
    post_processor: Option<String>,
}

#[derive(Serialize)]
struct GenerateResponse {
    gcode: String,
    toolpath: Vec<preview::ToolpathSegment>,
    stats: GenerateStats,
}

#[derive(Serialize, Default)]
struct GenerateStats {
    object_count: usize,
    closed_object_count: usize,
    offset_count: usize,
}

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

async fn defaults() -> Json<serde_json::Value> {
    let setup = Setup::default();
    let components = wiac_core::schema::components_schemas();
    Json(serde_json::json!({
        "setup": setup,
        // The frontend renders a form from `schema` (the Setup type's JSON
        // Schema) using `definitions` to resolve $refs. Refs are written as
        // OpenAPI's `#/components/schemas/X` form so the frontend can use
        // the same lookup logic against the full OpenAPI doc.
        "schema": components.get("Setup").cloned().unwrap_or(serde_json::Value::Null),
        "definitions": components,
    }))
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
    let setup = req.setup.unwrap_or_default();
    let mut objects = segments_to_objects(&req.segments);
    classify_containment(&mut objects);
    for obj in &mut objects {
        obj.tool_offset = setup.mill.offset;
    }
    let radius = setup.tool.diameter * 0.5;
    let mut offsets = Vec::new();
    let mut closed = 0usize;
    for (idx, obj) in objects.iter().enumerate() {
        if obj.closed {
            closed += 1;
        }
        if obj.closed && setup.pockets.active {
            for mut o in pocket_for_object(obj, radius, setup.pockets.nocontour, 6) {
                o.source_object_idx = idx;
                offsets.push(o);
            }
            continue;
        }
        let delta = match setup.mill.offset {
            ToolOffset::None | ToolOffset::On => 0.0,
            ToolOffset::Outside => -radius,
            ToolOffset::Inside => radius,
        };
        if delta.abs() < 1e-9 {
            offsets.push(PolylineOffset {
                segments: obj.segments.clone(),
                closed: obj.closed,
                level: 0,
                is_pocket: 0,
                layer: obj.layer.clone(),
                color: obj.color,
                source_object_idx: idx,
            });
        } else {
            for mut o in parallel_offset_object(obj, delta) {
                o.source_object_idx = idx;
                offsets.push(o);
            }
        }
    }

    let post_kind = req.post_processor.as_deref().unwrap_or("linuxcnc");
    let gcode = match post_kind {
        "linuxcnc" => emit_polylines(&setup, &offsets, &mut linuxcnc::Post::new()),
        "grbl" => emit_polylines(&setup, &offsets, &mut grbl::Post::new()),
        "hpgl" => emit_polylines(&setup, &offsets, &mut hpgl::Post::new()),
        other => return Err(AppError::bad_request(format!("unknown post_processor: {other}"))),
    };
    let toolpath = preview::interpret(&gcode);
    Ok(Json(GenerateResponse {
        stats: GenerateStats {
            object_count: objects.len(),
            closed_object_count: closed,
            offset_count: offsets.len(),
        },
        gcode,
        toolpath,
    }))
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
        match e {
            wiac_core::Error::UnsupportedFormat(_) => Self::bad_request(e.to_string()),
            _ => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                message: e.to_string(),
            },
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
