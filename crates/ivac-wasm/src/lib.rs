//! ivaCAM WASM bindings — exposes the same JSON contract the HTTP
//! and Tauri transports speak so the frontend can run the entire CAM
//! pipeline in-browser without any server.
//!
//! Built with `wasm-pack build crates/ivac-wasm --target web --release`.
//! The resulting `pkg/` ships JS glue + a `ivac_wasm_bg.wasm` blob that the
//! Vite frontend can import directly.
//!
//! ## Streaming + cancellation
//!
//! WASM threading (web workers + COOP/COEP) is out of scope for v1.
//! `generate_streaming` runs synchronously on the JS event loop and
//! invokes the supplied callback once per [`ivac_core::pipeline::PipelineEvent`].
//! The callback may flip a shared cancel flag (returned at start of
//! the call) to bail out of long inner loops without blocking the
//! JS task queue.

#![forbid(unsafe_code)]
// juvx: every #[wasm_bindgen] entry returns `JsValue` errors that surface
// to JS as a thrown Error with the message attached. The Rust-side
// `# Errors` doc would be redundant — the JS caller already sees the
// message via the standard try/catch. Whole crate gets the allow.
#![allow(clippy::missing_errors_doc)]

use serde::Serialize;
use wasm_bindgen::prelude::*;

use ivac_core::input::text::{render_text_api, render_text_layer_api, RenderTextRequest};
use ivac_core::pipeline::{generate_streaming, run_pipeline, CancelToken, PipelineRequest};
use ivac_core::project::TextLayer;
use ivac_core::{
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
    guard(|| {
        let opts = ImportOptions::default();
        let out = ivac_core::input::import_bytes(filename, bytes, &opts)
            .map_err(structured_error_to_js)?;
        serde_wasm_bindgen::to_value(&out).map_err(into_js_error)
    })
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
            ivac_core::Error::internal(format!("panic: {}", panic_message(&panic)))
                .with_hint("Please report this bug — see the toast for details."),
        )),
    }
}

/// Streaming variant: invokes `on_event` once per
/// [`ivac_core::pipeline::PipelineEvent`] as the pipeline runs. WASM v1
/// is single-threaded — the JS callback cannot actually flip the cancel
/// flag mid-run because the Rust call holds the event loop. The
/// streaming shape exists so the frontend's progress UI updates per
/// op without an extra synthetic-events shim. Cancel support arrives
/// with web-worker threading in v2.
// `js_sys::Function` is exchange-by-value at the wasm-bindgen ABI; taking
// `&Function` would force callers (and wasm-bindgen's generated JS glue)
// to keep a stable reference, which the SSE-callback pattern doesn't.
#[allow(clippy::needless_pass_by_value)]
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
        Ok(Err(ivac_core::pipeline::PipelineError::Cancelled)) => Ok(JsValue::NULL),
        Ok(Err(e)) => match e.to_structured(Some(&project)) {
            Some(structured) => Err(structured_error_to_js(structured)),
            None => Ok(JsValue::NULL),
        },
        Err(panic) => Err(structured_error_to_js(
            ivac_core::Error::internal(format!("panic: {}", panic_message(&panic)))
                .with_hint("Please report this bug — see the toast for details."),
        )),
    }
}

#[wasm_bindgen(js_name = renderText)]
pub fn render_text(request: JsValue) -> Result<JsValue, JsValue> {
    let req: RenderTextRequest = serde_wasm_bindgen::from_value(request).map_err(into_js_error)?;
    // Guarded: parses untrusted font bytes (ttf-parser / svg_font), which
    // can panic on malformed input.
    guard(|| {
        let resp = render_text_api(&req).map_err(structured_error_to_js)?;
        serde_wasm_bindgen::to_value(&resp).map_err(into_js_error)
    })
}

#[wasm_bindgen(js_name = renderTextLayer)]
pub fn render_text_layer(layer: JsValue) -> Result<JsValue, JsValue> {
    let layer: TextLayer = serde_wasm_bindgen::from_value(layer).map_err(into_js_error)?;
    // Guarded: same untrusted-font-bytes parsing as `render_text`.
    guard(|| {
        let resp = render_text_layer_api(&layer).map_err(structured_error_to_js)?;
        serde_wasm_bindgen::to_value(&resp).map_err(into_js_error)
    })
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

/// 7iej.15: run `f` under a panic guard so a panic in a transitive parser
/// (dxf-rs / usvg / ttf font shaping) on malformed user input surfaces as a
/// structured JS error instead of aborting the whole wasm instance — the
/// same protection `generate` / `generate_streaming` already have. Wraps
/// the entry points that take untrusted file / font bytes. `AssertUnwindSafe`
/// is sound here: the wasm module is single-threaded and the closure owns
/// no state we read again after a panic.
fn guard<T>(f: impl FnOnce() -> Result<T, JsValue>) -> Result<T, JsValue> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(panic) => Err(structured_error_to_js(
            ivac_core::Error::internal(format!("panic: {}", panic_message(&panic)))
                .with_hint("Please report this bug — see the toast for details."),
        )),
    }
}

/// Serialize a structured `ivac_core::Error` to a JS value the frontend
/// can detect (object with a `kind` field) and render through
/// `ErrorToast.svelte`. Falls back to the message string if serialization
/// somehow fails — the frontend's plain-string path catches that.
// Takes `err` by value because every caller produces it from a fallible
// step (`map_err(structured_error_to_js)`) and drops it after the JsValue
// is built — borrowing would force the call sites to keep the Error
// alive past the conversion.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn structured_error_to_js(err: ivac_core::Error) -> JsValue {
    serde_wasm_bindgen::to_value(&err).unwrap_or_else(|_| JsValue::from_str(&err.to_string()))
}

pub(crate) fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}
