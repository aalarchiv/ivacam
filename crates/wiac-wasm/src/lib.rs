//! wiaConstructor WASM bindings — exposes the same JSON contract the HTTP
//! and Tauri transports speak so the frontend can run the entire CAM
//! pipeline in-browser without any server.
//!
//! Built with `wasm-pack build crates/wiac-wasm --target web --release`.
//! The resulting `pkg/` ships JS glue + a wiac_wasm_bg.wasm blob that the
//! Vite frontend can import directly.
//!
//! ## Streaming + cancellation
//!
//! WASM threading (web workers + COOP/COEP) is out of scope for v1.
//! `generate_streaming` runs synchronously on the JS event loop and
//! invokes the supplied callback once per [`wiac_core::pipeline::PipelineEvent`].
//! The callback may flip a shared cancel flag (returned at start of
//! the call) to bail out of long inner loops without blocking the
//! JS task queue.

#![forbid(unsafe_code)]

use serde::Serialize;
use wasm_bindgen::prelude::*;

use wiac_core::input::text::{render_text_api, RenderTextRequest};
use wiac_core::pipeline::{generate_streaming, run_pipeline, CancelToken, PipelineRequest};
use wiac_core::{
    compute_helix_radius as core_compute_helix_radius, HelixRadiusRequest, ImportOptions,
};

pub mod sim;

#[wasm_bindgen(start)]
pub fn start() {
    // Surface Rust panics in the JS console with stack traces. Silently
    // becomes a no-op if the hook is already installed.
    console_error_panic_hook::set_once();
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Serialize)]
struct VersionResponse<'a> {
    version: &'a str,
    transport: &'a str,
    git_sha: Option<&'a str>,
}

#[wasm_bindgen]
pub fn healthz() -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&HealthResponse { ok: true }).map_err(into_js_error)
}

#[wasm_bindgen]
pub fn version() -> Result<JsValue, JsValue> {
    let v = VersionResponse {
        version: env!("CARGO_PKG_VERSION"),
        transport: "wasm",
        git_sha: option_env!("GIT_SHA"),
    };
    serde_wasm_bindgen::to_value(&v).map_err(into_js_error)
}

/// Import a DXF/SVG/HPGL byte buffer. The web client sends `File`
/// contents as a Uint8Array and provides the filename so the Rust core
/// can match the format detector.
#[wasm_bindgen(js_name = importBytes)]
pub fn import_bytes(filename: &str, bytes: &[u8]) -> Result<JsValue, JsValue> {
    let opts = ImportOptions::default();
    let out =
        wiac_core::input::import_bytes(filename, bytes, &opts).map_err(structured_error_to_js)?;
    serde_wasm_bindgen::to_value(&out).map_err(into_js_error)
}

#[wasm_bindgen]
pub fn generate(request: JsValue) -> Result<JsValue, JsValue> {
    let req: PipelineRequest = serde_wasm_bindgen::from_value(request).map_err(into_js_error)?;
    let project = req.project.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_pipeline(req, |_phase, _fraction, _msg| {})
    }));
    match result {
        Ok(Ok(resp)) => serde_wasm_bindgen::to_value(&resp).map_err(into_js_error),
        Ok(Err(e)) => match e.to_structured(Some(&project)) {
            Some(structured) => Err(structured_error_to_js(structured)),
            None => Err(JsValue::from_str("cancelled")),
        },
        Err(panic) => Err(structured_error_to_js(
            wiac_core::Error::internal(format!("panic: {}", panic_message(&panic)))
                .with_hint("Please report this bug — see the toast for details."),
        )),
    }
}

/// Streaming variant: invokes `on_event` once per
/// [`wiac_core::pipeline::PipelineEvent`] as the pipeline runs. WASM v1
/// is single-threaded — the JS callback cannot actually flip the cancel
/// flag mid-run because the Rust call holds the event loop. The
/// streaming shape exists so the frontend's progress UI updates per
/// op without an extra synthetic-events shim. Cancel support arrives
/// with web-worker threading in v2.
#[wasm_bindgen(js_name = generateStreaming)]
pub fn generate_streaming_wasm(
    request: JsValue,
    on_event: js_sys::Function,
) -> Result<JsValue, JsValue> {
    let req: PipelineRequest = serde_wasm_bindgen::from_value(request).map_err(into_js_error)?;
    let cancel = CancelToken::new();
    let project = req.project.clone();
    let mut sink = |ev| {
        if let Ok(js) = serde_wasm_bindgen::to_value(&ev) {
            let _ = on_event.call1(&JsValue::NULL, &js);
        }
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        generate_streaming(req, &cancel, &mut sink)
    }));
    match result {
        Ok(Ok(resp)) => serde_wasm_bindgen::to_value(&resp).map_err(into_js_error),
        Ok(Err(wiac_core::pipeline::PipelineError::Cancelled)) => Ok(JsValue::NULL),
        Ok(Err(e)) => match e.to_structured(Some(&project)) {
            Some(structured) => Err(structured_error_to_js(structured)),
            None => Ok(JsValue::NULL),
        },
        Err(panic) => Err(structured_error_to_js(
            wiac_core::Error::internal(format!("panic: {}", panic_message(&panic)))
                .with_hint("Please report this bug — see the toast for details."),
        )),
    }
}

#[wasm_bindgen(js_name = renderText)]
pub fn render_text(request: JsValue) -> Result<JsValue, JsValue> {
    let req: RenderTextRequest = serde_wasm_bindgen::from_value(request).map_err(into_js_error)?;
    let resp = render_text_api(&req).map_err(structured_error_to_js)?;
    serde_wasm_bindgen::to_value(&resp).map_err(into_js_error)
}

#[wasm_bindgen(js_name = computeHelixRadius)]
pub fn compute_helix_radius(request: JsValue) -> Result<JsValue, JsValue> {
    let req: HelixRadiusRequest = serde_wasm_bindgen::from_value(request).map_err(into_js_error)?;
    let resp = core_compute_helix_radius(req);
    serde_wasm_bindgen::to_value(&resp).map_err(into_js_error)
}

pub(crate) fn into_js_error<E: std::fmt::Display>(err: E) -> JsValue {
    JsValue::from_str(&err.to_string())
}

/// Serialize a structured `wiac_core::Error` to a JS value the frontend
/// can detect (object with a `kind` field) and render through
/// `ErrorToast.svelte`. Falls back to the message string if serialization
/// somehow fails — the frontend's plain-string path catches that.
pub(crate) fn structured_error_to_js(err: wiac_core::Error) -> JsValue {
    serde_wasm_bindgen::to_value(&err).unwrap_or_else(|_| JsValue::from_str(&err.to_string()))
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
