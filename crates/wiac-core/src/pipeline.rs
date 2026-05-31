//! Shared CAM pipeline driver — per-operation gcode emission.
//!
//! All three transports (HTTP, Tauri, WASM) funnel through `run_pipeline`.
//! Each enabled operation produces a gcode block prefixed with a
//! `; OP <id>` marker so the preview interpreter (UX-2) can stamp the
//! right `op_id` on every resulting [`preview::ToolpathSegment`]. The
//! whole program shares a single header/footer; cut blocks concatenate
//! between them.
//!
//! ## Streaming + cancellation
//!
//! [`generate_streaming`] is a parallel entry point that reports
//! per-operation progress and supports cooperative cancellation via a
//! [`CancelToken`]. The pipeline is CPU-bound and synchronous; the
//! caller is expected to drive it on a background thread (Tauri spawns
//! a `std::thread`, the HTTP server uses `tokio::task::spawn_blocking`,
//! and WASM runs it on the JS event loop and yields between events).
//!
//! WASM threading (web workers + COOP/COEP) is a follow-up — the
//! WASM bridge ships single-threaded and pumps events synchronously.
//!
//! ## Module split
//!
//! Per-op-kind drivers that don't follow the standard offset-cascade path
//! (V-Carve, Halfpipe, Thread, Stufenfase) live in [`op_drivers`]. The
//! orchestrator (`run_pipeline_impl` / `run_per_op`) and the offset /
//! pocket logic remain in this file.

// # CAM/sim pedantic-lint exemptions
// Test helpers and op-progress arithmetic walk bounded index ranges; similar
// names (`machine_with`/`machine_without`, `endmill_a`/`_b`) enumerate
// variants in test setup where renaming would lose meaning.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names,
    // OpKind / PocketStrategy dispatch tables enumerate every
    // variant explicitly so adding a new one forces a deliberate
    // choice — keeping it strict at the type level beats clippy's
    // "merge equal arms" suggestion that hides the dispatch shape.
    clippy::match_same_arms,
)]

mod frame;
mod offset_builder;
mod op_drivers;
mod patterns;
mod regions;
mod selection;
mod setup_resolver;
mod tabs;
mod warnings;

// 56a: re-export the op-source selection helpers so child modules can
// keep doing `use super::ordered_selection;` etc. without caring that
// they moved out of pipeline.rs. Visibility matches the underlying
// pub(in crate::pipeline) declarations in selection.rs.
pub(in crate::pipeline) use selection::{
    op_includes_object, ordered_selection, resolve_op_segments, source_combine_mode,
    validate_op_source_layers, validate_op_source_objects,
};

#[cfg(test)]
mod test_helpers;

use op_drivers::{
    halfpipe_would_emit, relief_would_emit, run_halfpipe_op, run_relief_op, run_standard_op,
    run_thread_op, run_vcarve_op, thread_would_emit, vcarve_would_emit,
};
use regions::build_region_previews;
pub use setup_resolver::fit_helix_radius_for_selection;
use setup_resolver::{header_setup_for, resolve_auto_helix_radius, synthesize_op_setup};

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::chaining::{classify_containment, segments_to_objects};
use crate::cam::setup::Setup;
use crate::cam::VcObject;
use crate::gcode::{
    emit_program_begin, emit_program_end, grbl, hpgl, linuxcnc, preview, PostProcessor,
};
use crate::geometry::Point2;
use crate::pipeline_cache::{op_cache_key_with_finish, OpCacheValue, PipelineCache};
use crate::project::{Op, OpKind, PocketStrategy, Project, ToolEntry};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PipelineRequest {
    /// The full project — geometry + machine + tools + operations + tabs.
    pub project: Project,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_processor: Option<PostProcessorKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PostProcessorKind {
    #[default]
    Linuxcnc,
    Grbl,
    Hpgl,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PipelineResponse {
    pub gcode: String,
    pub toolpath: Vec<preview::ToolpathSegment>,
    pub gcode_index: preview::GcodeIndex,
    pub stats: PipelineStats,
    /// Filled-area preview for Pocket ops: the actual region the cutter
    /// will machine, computed via the per-op `SourceCombine` mode (Auto by
    /// default — outer + inner = annulus). The frontend paints these as
    /// translucent fills so the user sees what they're cutting before
    /// reading the toolpath. Empty for non-Pocket ops.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<RegionPreview>,
    /// Non-fatal warnings raised during planning — mostly tool-fit
    /// problems (cutter doesn't fit the geometry, kind mismatch, …).
    /// The frontend surfaces these in the operations list status badge
    /// and a sidebar list; the gcode is still emitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PipelineWarning>,
    /// Acceleration- and jerk-aware program-time estimate. See
    /// [`crate::sim::timing`] for the integrator. The total accounts for
    /// motion under the trapezoidal profile, tool-change time
    /// (`MachineConfig.toolchange_s` × number of M6s), and per-tool
    /// spindle pauses summed across used tools.
    pub time_estimate: crate::sim::timing::TimeEstimate,
}

/// One non-fatal warning attached to (optionally) a specific op.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PipelineWarning {
    /// Op the warning applies to. `None` means project-wide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<u32>,
    /// Stable identifier — frontend can branch on this to render an
    /// icon, link to docs, etc.
    pub kind: String,
    /// Human-readable description.
    pub message: String,
}

/// One filled region attached to a specific operation. `outer` is the
/// outer boundary; `holes` are the islands the cutter must avoid. Both
/// in project units (typically mm).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RegionPreview {
    pub op_id: u32,
    pub outer: Vec<Point2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<Vec<Point2>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PipelineStats {
    pub object_count: usize,
    pub closed_object_count: usize,
    pub offset_count: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("unknown post_processor: {0}")]
    UnknownPostProcessor(String),
    #[error("operation #{0} references unknown tool id {1}")]
    UnknownTool(u32, u32),
    #[error("operation kind {0:?} is not implemented yet")]
    UnimplementedKind(Box<OpKind>),
    #[error("text render failed: {0}")]
    TextRender(String),
    #[error("pipeline cancelled")]
    Cancelled,
}

impl PipelineError {
    /// Lift the enum into the structured frontend `Error`. Project context
    /// fills in actionable auto-fix targets (e.g. the first tool id for an
    /// `UnknownTool`); pass `None` when no project is available and the
    /// auto-fix is dropped.
    #[must_use]
    pub fn to_structured(&self, project: Option<&Project>) -> Option<crate::Error> {
        use crate::errors::{AutoFix, Error as Structured};
        match self {
            PipelineError::Cancelled => None,
            PipelineError::UnknownPostProcessor(name) => Some(
                Structured::misconfigured(format!("unknown post_processor: {name}"))
                    .with_hint("Pick a known post: linuxcnc, grbl, or hpgl."),
            ),
            PipelineError::UnknownTool(op_id, tool_id) => {
                let mut e = Structured::misconfigured(format!(
                    "op {op_id} references missing tool {tool_id}"
                ))
                .with_hint("Pick a tool from the library.");
                if let Some(suggested) = project.and_then(|p| p.tools.first().map(|t| t.id)) {
                    e = e.with_auto_fix(AutoFix::AssignTool {
                        op_id: *op_id,
                        suggested_tool_id: suggested,
                    });
                }
                Some(e)
            }
            PipelineError::UnimplementedKind(kind) => Some(
                Structured::unsupported(format!("operation kind {kind:?} is not implemented yet"))
                    .with_hint("This op kind is not available yet — disable it or pick another."),
            ),
            PipelineError::TextRender(msg) => Some(
                Structured::misconfigured(format!("text render: {msg}"))
                    .with_hint("Pick a different font or fix the text contents."),
            ),
        }
    }
}

/// Run the pipeline with panic safety. Captures the panic and surfaces it
/// as `Error::internal(...)` so the frontend gets a structured error
/// rather than a renderer crash. Cancellation is preserved as `None` to
/// match the existing transport-layer pattern matching.
///
/// # Errors
///
/// Returns `Err(Some(Error))` on a pipeline failure (gcode emit
/// error, cache miss recovery failure, panic caught from the
/// underlying [`run_pipeline`]), or `Err(None)` when the run was
/// cancelled. Otherwise `Ok(PipelineResponse)`.
pub fn run_pipeline_safe(
    request: PipelineRequest,
) -> std::result::Result<PipelineResponse, Option<crate::Error>> {
    let project = request.project.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_pipeline(request, |_p, _f, _m| {})
    }));
    match result {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(PipelineError::Cancelled)) => Err(None),
        Ok(Err(e)) => Err(e.to_structured(Some(&project))),
        Err(panic) => {
            let msg = panic_message(&panic);
            Err(Some(
                crate::Error::internal(format!("panic: {msg}"))
                    .with_hint("Please report this bug — see the toast for details."),
            ))
        }
    }
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

/// Cooperative-cancellation handle. Cloned cheaply; the inner flag is
/// shared. Long inner loops in CAM primitives consult `is_cancelled`
/// at a coarse-enough granularity to bail within ≤200 ms p95.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Streaming pipeline event — one per phase boundary or per op.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PipelineEvent {
    OpStarted {
        op_id: u32,
        idx: usize,
        total: usize,
        name: String,
    },
    OpProgress {
        op_id: u32,
        fraction: f64,
        message: String,
    },
    OpCompleted {
        op_id: u32,
        /// True when this op was served from the per-op result cache
        /// (see [`crate::pipeline_cache`]) rather than recomputed.
        #[serde(default)]
        cached: bool,
    },
    Cancelled,
    Done {
        op_count: usize,
        total_time_s: f64,
    },
}

/// Process-global toolpath result cache. Lazily initialized on first
/// generate. Bounded LRU; capacity is sized for ≈ 5 ops × 10 recent
/// project states = 50, doubled for headroom.
static GLOBAL_CACHE: OnceLock<PipelineCache> = OnceLock::new();

fn global_cache() -> &'static PipelineCache {
    GLOBAL_CACHE.get_or_init(|| PipelineCache::new(200))
}

/// Clear the process-global pipeline cache. Exposed for tests and for
/// transports that want to flush after a project-wide reload.
pub fn clear_pipeline_cache() {
    if let Some(cache) = GLOBAL_CACHE.get() {
        cache.clear();
    }
}

/// Run the full CAM pipeline. `progress(phase, fraction, message)` is
/// called at each phase boundary; pass a no-op closure for non-streaming
/// callers.
///
/// # Errors
///
/// Returns `PipelineError` on any phase failure: offset cascade
/// collapse, gcode emit failure, or an invalid project (missing
/// tool, source-segment selection drift).
pub fn run_pipeline<F: Fn(&str, f64, &str)>(
    req: PipelineRequest,
    progress: F,
) -> Result<PipelineResponse, PipelineError> {
    let mut no_events = |_e: PipelineEvent| {};
    run_pipeline_impl(req, &progress, &mut no_events, None, Some(global_cache()))
}

/// Streaming entry point: walks ops one at a time, emitting
/// `PipelineEvent`s through `sink` and consulting `cancel` between ops
/// (and inside long inner loops). On cancellation, emits
/// `PipelineEvent::Cancelled` and returns `Err(PipelineError::Cancelled)`
/// — partial work is discarded.
///
/// # Errors
///
/// Returns `PipelineError::Cancelled` when the cancel token fires,
/// or the same per-phase failures `run_pipeline` does (offset
/// collapse, gcode emit failure, invalid project).
pub fn generate_streaming(
    request: PipelineRequest,
    cancel: &CancelToken,
    sink: &mut dyn FnMut(PipelineEvent),
) -> Result<PipelineResponse, PipelineError> {
    let progress = |_p: &str, _f: f64, _m: &str| {};
    match run_pipeline_impl(request, &progress, sink, Some(cancel), Some(global_cache())) {
        Ok(resp) => {
            sink(PipelineEvent::Done {
                op_count: resp.stats.offset_count,
                total_time_s: resp.time_estimate.total_s,
            });
            Ok(resp)
        }
        Err(PipelineError::Cancelled) => {
            sink(PipelineEvent::Cancelled);
            Err(PipelineError::Cancelled)
        }
        Err(e) => Err(e),
    }
}

// The orchestrator threads through import → chaining → per-op → sim → time
// estimate; splitting it loses the linear top-down read. The 55o4 bd issue
// tracks the per-op-driver extraction that will reduce this naturally.
#[allow(clippy::too_many_lines)]
fn run_pipeline_impl<F: Fn(&str, f64, &str)>(
    req: PipelineRequest,
    progress: &F,
    sink: &mut dyn FnMut(PipelineEvent),
    cancel: Option<&CancelToken>,
    cache: Option<&PipelineCache>,
) -> Result<PipelineResponse, PipelineError> {
    progress("import", 0.05, "preparing project");
    if cancelled(cancel) {
        return Err(PipelineError::Cancelled);
    }
    let mut project = req.project;

    // Pre-pipeline: render every TextLayer to segments and append them
    // to the project's geometry pool. Each layer's segments live under
    // the synthetic name `__text_<id>` so ops can target them via
    // `OpSource::Layers`. The render is purely additive — the
    // user-imported `project.segments` are untouched, and a project
    // with no text layers behaves exactly as before.
    if !project.text_layers.is_empty() {
        for layer in &project.text_layers {
            match crate::input::text::render_text_layer(layer) {
                Ok(mut segs) => project.segments.append(&mut segs),
                Err(e) => {
                    return Err(PipelineError::TextRender(format!(
                        "text layer {} (\"{}\"): {}",
                        layer.id, layer.name, e
                    )));
                }
            }
        }
        progress("text", 0.10, "rendered text layers");
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
    }

    let mut objects = segments_to_objects(&project.segments);
    classify_containment(&mut objects);
    progress("objects", 0.20, "chained segments into objects");
    if cancelled(cancel) {
        return Err(PipelineError::Cancelled);
    }

    let post_kind = req.post_processor.unwrap_or_default();
    // Use the first enabled op's setup as the program-level header /
    // footer basis. This lets unit / fast_move_z / feed-rate come from
    // a real op rather than a synthetic default.
    let header_setup = header_setup_for(&project);
    let stats_collector = std::cell::RefCell::new((0usize, 0usize, 0usize)); // (closed, offsets, _)
    let n_ops = project
        .operations
        .iter()
        .filter(|o| o.enabled)
        .count()
        .max(1);
    let mut warnings: Vec<PipelineWarning> = Vec::new();
    // tnxu: scan the op sequence for obviously wrong orderings (Profile
    // that cuts the part free preceding drill / finish on the same
    // source). Warnings only — no auto-reorder, because the user may
    // have a real reason for the declared order (jig, manual reset). The
    // 94sf safety gate downgrades the program when an `op_order_suspect`
    // surfaces, so the user has to acknowledge before the gcode ships.
    warnings::push_op_order_warnings(&project, &mut warnings);
    // f60x-E: nudge users to rough the bulk before a ball-nose relief finish.
    warnings::push_relief_roughing_warnings(&project, &mut warnings);
    // i5g4 MVP: warn when geometry bbox doesn't contain (0,0). Full
    // WCS / G54..G59 support is a feature on the roadmap; this loud
    // warning closes the silent-misalignment case the audit caught
    // (part-center DXF + corner-zero G54 → sim shows cuts in the
    // wrong place, user trusts the sim, runs the program).
    warnings::push_wcs_origin_warning(&project, &mut warnings);

    let post_tag: u8 = match post_kind {
        PostProcessorKind::Linuxcnc => 0,
        PostProcessorKind::Grbl => 1,
        PostProcessorKind::Hpgl => 2,
    };
    // jzpl Phase 1: run_per_op + every downstream driver now take
    // `&[VcObject]`. No working copy needed — pass the imported chain
    // by reference; pattern / frame expansion is owned inside
    // build_op_offsets.
    let gcode = match post_kind {
        PostProcessorKind::Linuxcnc => run_per_op(
            &project,
            &objects,
            &header_setup,
            &mut linuxcnc::Post::new(),
            &stats_collector,
            progress,
            n_ops,
            &mut warnings,
            sink,
            cancel,
            cache,
            post_tag,
        )?,
        PostProcessorKind::Grbl => run_per_op(
            &project,
            &objects,
            &header_setup,
            &mut grbl::Post::new(),
            &stats_collector,
            progress,
            n_ops,
            &mut warnings,
            sink,
            cancel,
            cache,
            post_tag,
        )?,
        PostProcessorKind::Hpgl => run_per_op(
            &project,
            &objects,
            &header_setup,
            &mut hpgl::Post::new(),
            &stats_collector,
            progress,
            n_ops,
            &mut warnings,
            sink,
            cancel,
            cache,
            post_tag,
        )?,
    };
    let (total_closed, total_offsets, _) = *stats_collector.borrow();

    progress("preview", 0.92, "interpreting toolpath");
    if cancelled(cancel) {
        return Err(PipelineError::Cancelled);
    }
    let (toolpath, gcode_index) = preview::interpret_with_index(&gcode);
    // v0ez: scan the emitted toolpath against the machine work-area
    // envelope here (core-side) so every transport — not just the
    // frontend — surfaces soft-limit / gantry-crash risk as a critical
    // `out_of_work_area` warning.
    warnings::push_work_area_warning(&toolpath, &project.machine, &mut warnings);
    // vrrr: stock envelope scan — the other half of v0ez. Runs on the
    // same assembled toolpath; emits a critical `out_of_stock` warning so
    // CLI / server / wasm consumers get the same guard the frontend used
    // to synthesize. No-op when `project.stock` is unset.
    warnings::push_stock_warning(&toolpath, project.stock.as_ref(), &mut warnings);
    let regions = build_region_previews(&project, &objects);
    let tool_changes = count_tool_changes(&project);
    let spindle_warmup_s = spindle_warmup_seconds(&project);
    // v7f5: build per-op tool-rate lookup so the estimator clamps
    // Plunge segments to the tool's plunge_rate even when the post
    // emitted a single F<feed> line.
    //
    // rnw6: route through the per-pipeline `tool_index` HashMap so the
    // per-op tool fetch is O(1) — was O(tools) per op via the prior
    // `iter().find(...)` chain.
    let tool_index = build_tool_index(&project.tools);
    let op_rates: Vec<crate::sim::timing::OpRates> = project
        .operations
        .iter()
        .filter_map(|op| {
            let tool = tool_index.get(&op.tool_id)?;
            Some(crate::sim::timing::OpRates {
                op_id: op.id,
                plunge_rate_mm_min: tool.plunge_rate,
                feed_rate_mm_min: tool.feed_rate,
            })
        })
        .collect();
    let time_estimate = crate::sim::timing::estimate_from_gcode_with_rates(
        &gcode,
        &toolpath,
        &project.machine,
        tool_changes,
        spindle_warmup_s,
        &op_rates,
    );
    progress("done", 1.0, "complete");
    Ok(PipelineResponse {
        stats: PipelineStats {
            object_count: objects.len(),
            closed_object_count: total_closed,
            offset_count: total_offsets,
        },
        gcode,
        toolpath,
        gcode_index,
        regions,
        warnings,
        time_estimate,
    })
}

#[inline]
pub(super) fn cancelled(cancel: Option<&CancelToken>) -> bool {
    cancel.is_some_and(CancelToken::is_cancelled)
}

/// rnw6: per-pipeline tool-id index built once at pipeline entry. The
/// hot-path lookups (every op's primary tool, every op's finish tool
/// for the cache key, per-op feed rate seeding for the time estimator)
/// previously did `project.tools.iter().find(...)` — O(tools) per
/// hit, called O(ops) times. For projects with dozens of tools and
/// dozens of ops that's a measurable cost. A `HashMap` collapses each
/// lookup to O(1) at the cost of one allocation per Generate.
fn build_tool_index(tools: &[ToolEntry]) -> HashMap<u32, &ToolEntry> {
    tools.iter().map(|t| (t.id, t)).collect()
}

/// ye4b: count tool changes by walking the project's enabled op list
/// in pipeline state, mirroring `run_per_op`'s `prev_tool_id` boundary
/// logic. The previous implementation grepped the emitted gcode for
/// literal "M6", which broke under custom post profiles whose
/// toolchange template emits something else (e.g. "TC1").
///
/// Counting rules:
///   * The first cutting op (non-Pause) always counts — the program
///     enters the spindle with whatever was loaded, so wiac always
///     emits an explicit toolchange at the first op.
///   * Each subsequent op whose `tool_id` differs from the previous
///     cutting op's effective end-of-op tool counts.
///   * Pause ops don't touch the spindle and don't change
///     `prev_tool_id` (they skip the toolchange envelope entirely).
///   * Dual-tool ops (`finish_tool_id` distinct from `tool_id`)
///     bias the end-of-op tool to the finish id, matching the
///     `run_per_op` invariant — back-to-back same-finish-tool ops
///     emit at most one extra change.
// rxm9: Expand `{name}` tokens in an `OpKind::GcodeInclude` payload
// against the post's live state. Returns the expanded body plus the
// list of distinct unknown variable names encountered (so the caller
// can fan them into per-variable warnings).
//
// Supported variables (case-insensitive — gcode is conventionally
// case-insensitive too):
//   * `{x}` / `{y}` / `{z}` — last commanded XYZ, formatted to 4
//     decimal places; "0" if the post hasn't moved yet.
//   * `{f}` — last feedrate (mm / min); "0" if not yet set.
//   * `{s}` — last spindle RPM; "0" if not yet set.
//   * `{safe_z}` — the op's `fast_move_z` (always present), 4
//     decimal places.
//
// Unterminated `{` (no closing brace on the same line) is left as
// literal text — the caller's program ships unchanged. Unknown
// variable names pass through bracketed (`{xyz}`) AND get added to
// the returned list so the caller surfaces them as warnings.
fn expand_gcode_include_vars(
    content: &str,
    state: &crate::gcode::CapturedPostState,
    safe_z: f64,
) -> (String, Vec<String>) {
    use std::fmt::Write;
    let mut out = String::with_capacity(content.len());
    let mut unknown: Vec<String> = Vec::new();
    let mut chars = content.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            out.push(c);
            continue;
        }
        // Collect up to a closing `}` on the same line.
        let mut name = String::new();
        let mut closed = false;
        while let Some(&p) = chars.peek() {
            if p == '}' {
                chars.next();
                closed = true;
                break;
            }
            if p == '\n' {
                break;
            }
            name.push(p);
            chars.next();
        }
        if !closed {
            // Unterminated brace — emit verbatim and continue.
            out.push('{');
            out.push_str(&name);
            continue;
        }
        let key = name.to_ascii_lowercase();
        match key.as_str() {
            "x" => write!(out, "{:.4}", state.last_x.unwrap_or(0.0)).expect("string write"),
            "y" => write!(out, "{:.4}", state.last_y.unwrap_or(0.0)).expect("string write"),
            "z" => write!(out, "{:.4}", state.last_z.unwrap_or(0.0)).expect("string write"),
            "f" => write!(out, "{}", state.last_rate.unwrap_or(0)).expect("string write"),
            "s" => write!(out, "{}", state.last_speed.unwrap_or(0)).expect("string write"),
            "safe_z" => write!(out, "{safe_z:.4}").expect("string write"),
            _ => {
                out.push('{');
                out.push_str(&name);
                out.push('}');
                if !unknown.iter().any(|u| u.eq_ignore_ascii_case(&name)) {
                    unknown.push(name);
                }
            }
        }
    }
    (out, unknown)
}

// yhen: Outcome of classifying one line of an expanded GcodeInclude
// body. Mirrors the supported set of `gcode::preview::interpret`:
// anything that interpreter tessellates into ToolpathSegments lands
// in Simulated; anything heightmap-neutral (M-codes, units, modal
// switches, blank/comment lines) lands in NoOp; explicit unsupported
// G-codes or multi-axis A/B/C/U/V/W words land in Unsimulated with
// a short reason string the caller can surface in a warning.
//
// Modal continuation (e.g. a bare `X10 Y10` after a prior `G1`) is
// classified Simulated — the preview interpreter does carry modal
// state across lines, and the heightmap will get carved correctly.
#[derive(Debug, Clone)]
pub(crate) enum GcodeIncludeLineClass {
    Simulated,
    NoOp,
    Unsimulated(String),
}

#[derive(Debug, Clone)]
pub(crate) struct SkippedIncludeLine {
    /// 1-based line offset within the EXPANDED body.
    pub line_no: u32,
    /// Trimmed source text (without trailing comment).
    pub trimmed: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct GcodeIncludeClassification {
    pub n_simulated: usize,
    pub n_noop: usize,
    pub skipped: Vec<SkippedIncludeLine>,
}

/// Classify each line of an expanded `GcodeInclude` body. The blanket
/// `gcode_include_not_simulated` warning (pre-yhen) lied to the user
/// for the common case of a hand-rolled return-home block that's
/// 100 % G0/G1/G2/G3 — the sim DOES already carve those, because the
/// unified `preview::interpret_with_index` at `run_pipeline`'s tail
/// ingests them via `post.raw()`. This classifier lets the caller
/// emit a counted, accurate "X of Y lines skipped" summary instead.
fn classify_gcode_include_body(expanded: &str) -> GcodeIncludeClassification {
    let mut out = GcodeIncludeClassification::default();
    for (idx0, raw) in expanded.lines().enumerate() {
        let line_no = u32::try_from(idx0 + 1).unwrap_or(u32::MAX);
        match classify_gcode_include_line(raw) {
            GcodeIncludeLineClass::Simulated => out.n_simulated += 1,
            GcodeIncludeLineClass::NoOp => out.n_noop += 1,
            GcodeIncludeLineClass::Unsimulated(reason) => {
                out.skipped.push(SkippedIncludeLine {
                    line_no,
                    trimmed: strip_gcode_comment(raw).trim().to_string(),
                    reason,
                });
            }
        }
    }
    out
}

fn classify_gcode_include_line(raw: &str) -> GcodeIncludeLineClass {
    let stripped = strip_gcode_comment(raw);
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        return GcodeIncludeLineClass::NoOp;
    }
    let mut has_movement = false;
    let mut multi_axis_word: Option<char> = None;
    let mut simulated_g: Option<u32> = None;
    let mut unsupported_g: Option<u32> = None;
    for tok in trimmed.split_whitespace() {
        if tok.is_empty() {
            continue;
        }
        let head = tok.as_bytes()[0].to_ascii_uppercase();
        let rest = &tok[1..];
        match head {
            b'G' => {
                if let Ok(n) = rest.parse::<u32>() {
                    if matches!(n, 0 | 1 | 2 | 3 | 73 | 81 | 82 | 83) {
                        simulated_g = Some(n);
                    } else if matches!(
                        n,
                        4 | 17
                            | 18
                            | 19
                            | 20
                            | 21
                            | 28
                            | 30
                            | 40
                            | 49
                            | 53
                            | 54
                            | 55
                            | 56
                            | 57
                            | 58
                            | 59
                            | 80
                            | 90
                            | 91
                            | 92
                            | 93
                            | 94
                            | 95
                    ) {
                        // dwell / plane select / unit / work offsets /
                        // modal cancel / distance mode / feed mode —
                        // all heightmap-neutral.
                    } else {
                        unsupported_g = Some(n);
                    }
                }
            }
            b'M' => {
                // Every M-code is heightmap-neutral as far as the sim
                // is concerned: spindle, coolant, pause, end-of-program
                // don't move the cutter. Tool-change M6 happens outside
                // the heightmap sweep too. So we classify all M-codes
                // as NoOp; the carving correctness is unaffected.
                // (If a future op kind models coolant or spindle in the
                // heightmap, revisit.)
            }
            b'X' | b'Y' | b'Z' => has_movement = true,
            b'A' | b'B' | b'C' | b'U' | b'V' | b'W' => {
                if multi_axis_word.is_none() {
                    multi_axis_word = Some(head as char);
                }
            }
            // Arc-center (I/J/K), radius / canned-cycle params (R/P/Q),
            // line number (N), tool (T), offsets (H/D), feed (F), speed
            // (S) — none of these alone change the carve.
            _ => {}
        }
    }
    if let Some(n) = unsupported_g {
        return GcodeIncludeLineClass::Unsimulated(format!(
            "unsupported G{n} — sim recognizes only G0/G1/G2/G3 + canned cycles G73/G81/G82/G83"
        ));
    }
    if let Some(axis) = multi_axis_word {
        return GcodeIncludeLineClass::Unsimulated(format!(
            "{axis}-axis word — sim is 3-axis (XYZ) only"
        ));
    }
    if simulated_g.is_some() || has_movement {
        GcodeIncludeLineClass::Simulated
    } else {
        // Lone F / S / T / N / standalone modal — no carve impact.
        GcodeIncludeLineClass::NoOp
    }
}

/// In-line comment stripper for `classify_gcode_include_line`. Mirrors
/// `gcode::preview::strip_comment` (parens-delimited inline AND
/// trailing `;` to EOL) but lives here as a private duplicate to
/// avoid widening the gcode module's public surface.
fn strip_gcode_comment(line: &str) -> String {
    let mut out = String::new();
    let mut in_paren = false;
    for ch in line.chars() {
        match ch {
            '(' => in_paren = true,
            ')' => in_paren = false,
            ';' => break,
            _ if !in_paren => out.push(ch),
            _ => {}
        }
    }
    out
}

fn count_tool_changes(project: &Project) -> u32 {
    let mut n = 0u32;
    let mut prev_tool_id: Option<u32> = None;
    for op in project.operations.iter().filter(|o| o.enabled) {
        // 8n4k: program-only ops (Pause, Homing, Probe, CycleMarker)
        // don't carry a tool — they neither cause nor break a
        // toolchange envelope.
        if op.is_program_only() {
            continue;
        }
        if prev_tool_id != Some(op.tool_id) {
            n += 1;
            prev_tool_id = Some(op.tool_id);
        }
        if let Some(finish_id) = op.finish_tool_id {
            if finish_id != op.tool_id && op_can_emit_internal_swap(op) {
                // vmm0: only count an internal dual-tool swap when the
                // op kind actually exercises the dual-tool / chamfer
                // path. Previously the +1 fired for ANY op carrying a
                // distinct finish_tool_id, but `synthesize_finish_setup`
                // only returns Some for Pocket kinds OR drill ops with
                // chamfer_after_width_mm > 0 (see synthesize_finish_setup
                // at L1037 — non-Pocket / non-chamfer ops fall through
                // to None, dual_tool.rs:34 hits the single-emit branch
                // with no envelope, and runtime M6 count is N, not N+1).
                // This brings the estimator into structural agreement
                // with the runtime; the remaining edge cases — Pocket
                // with no is_finish offsets, or drill+chamfer with no
                // Circle objects — still over-count by one but those
                // require the full offsets cascade / object inspection
                // to detect and are documented as acceptable in the
                // bug report's "Either accept over-count" trade-off.
                n += 1;
                prev_tool_id = Some(finish_id);
            }
        }
    }
    n
}

/// vmm0: structural mirror of `synthesize_finish_setup`'s op-kind
/// guard at L1037 (non-Pocket / non-drill-chamfer return None).
/// Used by `count_tool_changes` to skip the internal +1 for ops that
/// would fall through to single-emit with no envelope. Keep in sync
/// when new op kinds gain dual-tool support.
fn op_can_emit_internal_swap(op: &Op) -> bool {
    if matches!(op.kind, OpKind::Pocket { .. }) {
        return true;
    }
    op.drill_chamfer_after_width_mm().is_some_and(|w| w > 0.0)
}

/// keyl: spindle-warmup time accrues PER tool-change envelope, not per
/// unique tool. The old implementation summed `tool.pause` once per
/// distinct `tool_id`, which under-reports the duration for sequences
/// like `A(tool1) -> B(tool2) -> C(tool1)`: that program physically
/// loads tool1 twice (first and third op), so the operator-set
/// `pause` runs twice. Walk the enabled op stream with the same
/// rules `count_tool_changes` uses (skip pause ops, account for
/// dual-tool finishes) and tally `tool.pause` per actual envelope
/// event so the warmup estimate tracks the gcode that ships.
fn spindle_warmup_seconds(project: &Project) -> f64 {
    let pause_for = |tool_id: u32| -> f64 {
        project
            .tools
            .iter()
            .find(|t| t.id == tool_id)
            .map_or(0.0, |t| f64::from(t.pause))
    };
    let mut total = 0.0;
    let mut prev_tool_id: Option<u32> = None;
    for op in project.operations.iter().filter(|o| o.enabled) {
        // 8n4k: program-only ops never load a tool, so they don't
        // contribute to spindle warmup time.
        if op.is_program_only() {
            continue;
        }
        if prev_tool_id != Some(op.tool_id) {
            total += pause_for(op.tool_id);
            prev_tool_id = Some(op.tool_id);
        }
        if let Some(finish_id) = op.finish_tool_id {
            if finish_id != op.tool_id {
                // Internal dual-tool change inside this op: an extra
                // toolchange envelope fires for the finish tool.
                total += pause_for(finish_id);
                prev_tool_id = Some(finish_id);
            }
        }
    }
    total
}

/// Per-post-processor monomorphisation of the per-op driver. Pulled out
/// so we don't need to type-erase `PostProcessor` (its methods take Sized
/// `&mut self` so the trait object dance was painful).
// Per-op dispatch + dual-tool finish coordination is a long state machine
// that doesn't usefully split — see 55o4 for the planned per-op-driver
// extraction.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_per_op<P, F>(
    project: &Project,
    objects: &[VcObject],
    header_setup: &Setup,
    post: &mut P,
    stats: &std::cell::RefCell<(usize, usize, usize)>,
    progress: &F,
    n_ops: usize,
    warnings: &mut Vec<PipelineWarning>,
    sink: &mut dyn FnMut(PipelineEvent),
    cancel: Option<&CancelToken>,
    cache: Option<&PipelineCache>,
    post_tag: u8,
) -> Result<String, PipelineError>
where
    P: PostProcessor,
    F: Fn(&str, f64, &str),
{
    // Pipeline progress budget for the gcode-emission phase. The full
    // curve is import (0 → 0.20) → gcode (0.30 → 0.85) → preview (0.92)
    // → done (1.0). Each emitted op advances the fraction by
    // `GCODE_PROGRESS_SPAN / n_ops` so a long op count still hits every
    // progress tick monotonically without stepping past the post-gcode
    // preview phase.
    const GCODE_PROGRESS_START: f64 = 0.30;
    const GCODE_PROGRESS_SPAN: f64 = 0.55;

    emit_program_begin(header_setup, post);
    let gcode_progress = |emitted: usize, total: usize| -> f64 {
        let denom = total.max(1) as f64;
        GCODE_PROGRESS_START + GCODE_PROGRESS_SPAN * (emitted as f64 / denom)
    };
    let mut last_pos = Point2::new(0.0, 0.0);
    let mut emitted_ops = 0usize;
    let enabled_ops: Vec<&Op> = project.operations.iter().filter(|o| o.enabled).collect();
    let total_ops = enabled_ops.len();
    // rnw6: per-pipeline tool-id index used by the per-op loop below so
    // the M6 envelope decision (`op.tool_id`), the cache-key tool lookup,
    // and the finish-tool lookup all run in O(1) instead of O(tools).
    let tool_index = build_tool_index(&project.tools);
    // k2ew: track the tool number last asserted via post.tool() so we
    // can emit T<n> M6 + Z-shift at every op boundary where the
    // primary tool changes — and at the FIRST op so the program never
    // silently uses whatever was in the spindle. Pause ops don't have
    // a tool and don't reset this state. We track by ToolEntry.id
    // (the project-level tool key), not by tool.number (which can be
    // shared across entries on some configs).
    let mut prev_tool_id: Option<u32> = None;
    // dp6b: track the previous op's `group` so we can emit a single
    // boundary marker (`; === GROUP: <name> ===`) when the value
    // changes. None / empty string means "no group" and never
    // generates a boundary line on its own. Untouched-group
    // sequences emit nothing — legacy projects without any group
    // field stay byte-identical.
    let mut prev_group: Option<&str> = None;
    for (idx, op) in enabled_ops.iter().enumerate() {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        sink(PipelineEvent::OpStarted {
            op_id: op.id,
            idx,
            total: total_ops,
            name: op.name.clone(),
        });

        // dp6b: group boundary marker. Fire ONCE when the live group
        // changes from the previous op's (treating None and `Some("")`
        // as the same "no group" state). Lands BEFORE the per-op
        // reset / toolchange envelope / `; OP N` marker so the user
        // scanning the gcode sees the phase change before any
        // motion lines that belong to it.
        let cur_group: Option<&str> = match op.group.as_deref() {
            Some(g) if !g.is_empty() => Some(g),
            _ => None,
        };
        if cur_group != prev_group {
            if let Some(g) = cur_group {
                post.raw(&format!("; === GROUP: {g} ==="));
            } else {
                // Transitioning OUT of a group into no-group. Emit a
                // closing marker so the operator can see the phase
                // ended; leave the body of the next op unannotated.
                post.raw("; === END GROUP ===");
            }
            prev_group = cur_group;
        }

        // rxm9: snapshot the live state BEFORE the per-op reset so
        // the GcodeInclude variable-expansion path can read
        // `{x}`/`{y}`/`{z}`/`{f}`/`{s}` against the previous op's
        // exit position. The reset below clears these to None for
        // delta-encoding determinism; the include block needs the
        // pre-reset values.
        let state_before_reset = post.capture_state();
        // Reset the post's delta-encoding state at every op boundary so
        // the captured body lines are independent of whatever state the
        // previous op (cached or fresh) left behind. Both fresh-emit
        // and cache-hit paths see the same entry state — the only
        // difference is whether the body comes from re-emission or
        // from the cache. Exit state is captured/restored separately.
        post.reset_state();

        // o3od: specialty drivers have structural "no output" cases (open
        // source polylines, no closed circles, missing relief source) that
        // emit ZERO cut moves. Gate the M6 envelope on emit-ability so we
        // don't warm up the spindle and burn a hand-swap on a no-output op.
        // The driver still runs, so any "no output" warning still surfaces.
        let will_emit = specialty_will_emit(op, project, objects);

        // k2ew: emit the M6 toolchange envelope BEFORE body_marker so it is
        // NOT captured into the per-op cache body — the decision depends on
        // prev_tool_id, which is runtime state, not op state. Pause ops have
        // no tool and don't reset prev_tool_id; no-emit ops skip the swap.
        // 8n4k: program-only ops bypass the M6 toolchange envelope.
        if !op.is_program_only() && will_emit {
            prev_tool_id = emit_boundary_toolchange(
                op,
                project,
                header_setup,
                &tool_index,
                post,
                prev_tool_id,
            );
        }
        let body_marker = post.out_lines_count();

        // rt1.34: Pause op — emit M5 → optional-stop and skip the rest
        // of the op machinery (no tool, no source, no setup, no cache).
        // The controller halts on M0; pressing Cycle Start resumes.
        //
        // yc2a: after M0 we DON'T emit a raw `M3` — that hard-codes CW
        // and would lock a CCW-tool program into the wrong direction
        // (and would emit no S<rpm>, leaving the controller at
        // whatever last speed it cached). Instead, call
        // `post.reset_state()` so the post's delta encoder forgets
        // `last_speed` / `last_spindle_dir`; the NEXT op's lazy
        // `spindle_on(...)` (driven by the op's tool's
        // `spindle_direction`) will then re-emit M3/M4 S<rpm>
        // explicitly. Net effect: the pause behaves as a true
        // mid-program restart for the spindle, honoring whatever
        // direction the next op needs.
        if let OpKind::Pause { message } = &op.kind {
            post.raw(&format!("; OP {} (pause)", op.id));
            post.raw("M5");
            if !message.is_empty() {
                post.comment(message);
            }
            post.raw("M0");
            post.reset_state();
            emitted_ops += 1;
            progress(
                "gcode",
                gcode_progress(emitted_ops, n_ops),
                &format!("emitted op {} (pause)", op.id),
            );
            sink(PipelineEvent::OpCompleted {
                op_id: op.id,
                cached: false,
            });
            continue;
        }

        // 8n4k: Homing op — emit a comment + `G28`, optionally
        // followed by a rapid retract to the op's safe Z so the next
        // op starts from a known clearance. Like Pause we don't
        // touch tool state, but unlike Pause we DO call
        // `post.reset_state()` because some controllers reset
        // modal state at G28 too.
        if let OpKind::Homing { retract_to_safe_z } = &op.kind {
            post.raw(&format!("; OP {} (homing)", op.id));
            post.raw("G28");
            if *retract_to_safe_z {
                post.move_to(None, None, Some(op.params.fast_move_z));
            }
            post.reset_state();
            emitted_ops += 1;
            progress(
                "gcode",
                gcode_progress(emitted_ops, n_ops),
                &format!("emitted op {} (homing)", op.id),
            );
            sink(PipelineEvent::OpCompleted {
                op_id: op.id,
                cached: false,
            });
            continue;
        }

        // 8n4k: Probe op — emit a comment + `G38.2 <axis><dist> F<feed>`.
        // The controller halts at the trigger; we re-set state so the
        // delta-encoder doesn't think the tool stayed at the probe XYZ
        // — the next move re-emits its targets explicitly.
        if let OpKind::Probe {
            axis,
            distance_mm,
            feed_mm_min,
        } = &op.kind
        {
            post.raw(&format!("; OP {} (probe)", op.id));
            post.raw(&format!(
                "G38.2 {}{:.4} F{}",
                axis.letter(),
                distance_mm,
                feed_mm_min,
            ));
            post.reset_state();
            emitted_ops += 1;
            progress(
                "gcode",
                gcode_progress(emitted_ops, n_ops),
                &format!("emitted op {} (probe)", op.id),
            );
            sink(PipelineEvent::OpCompleted {
                op_id: op.id,
                cached: false,
            });
            continue;
        }

        // 8n4k: CycleMarker — emit ONLY a comment line marking the
        // operator-readable label, no controller motion or state
        // change. Wrap the label with `--- … ---` so it stands out
        // among the cut-block comments above and below.
        if let OpKind::CycleMarker { label } = &op.kind {
            post.raw(&format!("; OP {} (cycle marker)", op.id));
            if label.is_empty() {
                post.raw("; ---");
            } else {
                post.raw(&format!("; --- {label} ---"));
            }
            emitted_ops += 1;
            progress(
                "gcode",
                gcode_progress(emitted_ops, n_ops),
                &format!("emitted op {} (cycle marker)", op.id),
            );
            sink(PipelineEvent::OpCompleted {
                op_id: op.id,
                cached: false,
            });
            continue;
        }

        // rxm9: GcodeInclude — substitute `{x}`/`{y}`/`{z}`/`{f}`/
        // `{s}`/`{safe_z}` against the post's live state, then emit
        // each line via `post.raw()`. Unknown variables pass through
        // as literal text and surface a warning. The sim doesn't
        // model the included block; a `gcode_include_not_simulated`
        // warning makes that explicit so the user can sanity-check
        // the canned cycle by hand.
        if let OpKind::GcodeInclude {
            path,
            content,
            verbose_unsim_warnings,
        } = &op.kind
        {
            let header = if path.is_empty() {
                format!("; OP {} (gcode include)", op.id)
            } else {
                format!("; OP {} (gcode include: {path})", op.id)
            };
            post.raw(&header);
            let safe_z = op.params.fast_move_z;
            let (expanded, unknown) =
                expand_gcode_include_vars(content, &state_before_reset, safe_z);
            for name in &unknown {
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "gcode_include_unknown_variable".into(),
                    message: format!(
                        "Op '{}': unknown variable `{{{name}}}` in included G-code passed through verbatim — fix or remove to silence.",
                        op.name,
                    ),
                });
            }
            if expanded.trim().is_empty() {
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "gcode_include_empty".into(),
                    message: format!(
                        "Op '{}': included G-code is empty — no lines emitted at this slot.",
                        op.name,
                    ),
                });
            }
            for line in expanded.lines() {
                post.raw(line);
            }
            // yhen: classify the expanded body and emit a counted
            // summary instead of the legacy blanket
            // `gcode_include_not_simulated` warning. The unified
            // `preview::interpret_with_index` pass at the tail of
            // run_pipeline already tessellates G0/G1/G2/G3 +
            // G73/G81/G82/G83 lines into ToolpathSegments that the
            // sim sweeps — saying "not simulated" for a 100 % G1
            // return-home block was a lie. Now we warn only when
            // there are genuinely unsimulatable lines, and we say
            // how many.
            let classification = classify_gcode_include_body(&expanded);
            if !classification.skipped.is_empty() {
                let n_total = classification.n_simulated
                    + classification.n_noop
                    + classification.skipped.len();
                // Lead with the first skipped line so the user has a
                // concrete starting point without scanning the
                // warnings panel.
                let head = &classification.skipped[0];
                warnings.push(PipelineWarning {
                    op_id: Some(op.id),
                    kind: "gcode_include_lines_skipped".into(),
                    message: format!(
                        "Op '{}': {n_skipped} of {n_total} included G-code line(s) cannot be simulated — the carved stock state across this slot may be incomplete. First skipped: line {head_line} `{head_text}` ({head_reason}). Inspect the included file by hand.",
                        op.name,
                        n_skipped = classification.skipped.len(),
                        head_line = head.line_no,
                        head_text = head.trimmed,
                        head_reason = head.reason,
                    ),
                });
                // xi2g: when the op opts into verbose mode, fan out a
                // per-line warning for each skipped line. Off by
                // default so the warnings panel stays readable on a
                // multi-skip block; useful when the user is debugging
                // a single specific include and wants the full list.
                if *verbose_unsim_warnings {
                    for skipped in &classification.skipped {
                        warnings.push(PipelineWarning {
                            op_id: Some(op.id),
                            kind: "gcode_include_unsim_line".into(),
                            message: format!(
                                "Op '{}': included G-code line {n}: `{text}` — {reason}.",
                                op.name,
                                n = skipped.line_no,
                                text = skipped.trimmed,
                                reason = skipped.reason,
                            ),
                        });
                    }
                }
            }
            // Note: a 100 %-simulated body emits NO warning (was the
            // blanket gcode_include_not_simulated before yhen). A
            // body that's nothing but no-op (only comments / lone
            // M-codes / no movement at all) ALSO emits no per-line
            // warning here — the pre-existing `gcode_include_empty`
            // check above already catches `expanded.trim().is_empty()`,
            // and a comment-only body is the user's choice.
            // Reset post state — we have no idea where the included
            // block left the spindle. The next op re-emits all its
            // targets explicitly so subsequent moves are still
            // correct.
            post.reset_state();
            emitted_ops += 1;
            progress(
                "gcode",
                gcode_progress(emitted_ops, n_ops),
                &format!("emitted op {} (gcode include)", op.id),
            );
            sink(PipelineEvent::OpCompleted {
                op_id: op.id,
                cached: false,
            });
            continue;
        }

        // 7l0a: validate OpSource::Objects references against the
        // current chained-object set BEFORE the cache lookup so the
        // warnings ride along even when the gcode body is served from
        // cache.
        validate_op_source_objects(op, objects, warnings);
        // dcna: same treatment for OpSource::Layers — a typoed layer
        // name (or one whose import was removed) used to silently
        // produce zero segments. Now we surface op_source_missing_layer
        // (+ op_source_empty when every requested layer is missing).
        validate_op_source_layers(op, &project.segments, warnings);

        // Cache lookup. We skip caching when no cache is provided.
        let cache_key = cache.and_then(|_| {
            let tool = tool_index.get(&op.tool_id).copied()?;
            // For dual-tool Pocket ops (rt1.33), fold the finish tool's
            // properties into the key so changing its diameter / feeds
            // / RPMs invalidates cached output. Single-tool ops pass
            // None and route through op_cache_key_with_finish's
            // is-finish-tool sentinel.
            let finish_tool = op
                .finish_tool_id
                .filter(|id| *id != op.tool_id)
                .and_then(|id| tool_index.get(&id).copied());
            Some(op_cache_key_with_finish(
                op,
                tool,
                finish_tool,
                &project.machine,
                &resolve_op_segments(op, &project.segments, objects),
                &project.fixtures,
                &project.text_layers,
                &project.relief_sources,
                &project.work_offset,
                post_tag,
            ))
        });

        if let (Some(c), Some(key)) = (cache, cache_key) {
            if let Some(cached) = c.get(key) {
                let lines: Vec<String> = cached
                    .gcode_body
                    .lines()
                    .map(std::string::ToString::to_string)
                    .collect();
                post.out_extend_lines(&lines);
                post.restore_state(&cached.exit_state);
                // my03: replay the op's planning warnings. build_op_offsets
                // / the driver / synthesize_op_setup don't re-run on a hit,
                // so without this their warnings (tool_kind_mismatch,
                // tslot_requires_stem_slot, depth-limited, zero-rate, …)
                // would vanish on the second+ identical Generate — and a
                // critical one could let a program slip past the safety gate.
                warnings.extend(cached.warnings.iter().cloned());
                last_pos = Point2::new(cached.exit_xy.0, cached.exit_xy.1);
                {
                    let mut s = stats.borrow_mut();
                    s.0 += cached.closed_count;
                    s.1 += cached.offset_count;
                }
                // k2ew + nguf: end-of-op tool bookkeeping, shared with the
                // fresh-emit path via next_prev_tool_id.
                prev_tool_id = Some(next_prev_tool_id(op, cached.internal_swap_emitted));
                emitted_ops += 1;
                progress(
                    "gcode",
                    gcode_progress(emitted_ops, n_ops),
                    &format!("emitted op {} (cached)", op.id),
                );
                sink(PipelineEvent::OpCompleted {
                    op_id: op.id,
                    cached: true,
                });
                continue;
            }
        }

        // my03: snapshot the warning count so everything this op pushes
        // during its fresh emit (setup synthesis + the driver) can be
        // captured into the cache value and replayed on a future hit. The
        // pre-cache-lookup `validate_op_source_*` warnings sit below this
        // mark, so they're never double-counted (they run on both paths).
        let warn_start = warnings.len();
        let mut setup = synthesize_op_setup(op, project, warnings)?;
        resolve_auto_helix_radius(op, objects, &mut setup, warnings);
        // Dispatch to the per-kind driver. Specialty drivers (VCarve /
        // Thread / Halfpipe / ReliefMill) emit XYZ blocks directly and
        // report no offset stats; the standard cascade reports closed /
        // offset counts and whether it emitted an internal dual-tool
        // (rough→finish) swap (nguf).
        let (closed_count_emitted, offset_count_emitted, internal_swap_emitted) = run_op_driver(
            op,
            project,
            objects,
            &setup,
            post,
            &mut last_pos,
            warnings,
            cancel,
        )?;
        {
            let mut s = stats.borrow_mut();
            s.0 += closed_count_emitted;
            s.1 += offset_count_emitted;
        }
        if let (Some(c), Some(key)) = (cache, cache_key) {
            let lines = post.out_lines_clone_from(body_marker);
            let body = lines.join("\n");
            let (toolpath, _idx) =
                preview::interpret_with_index(&format!("; OP {}\n{body}", op.id));
            c.put(
                key,
                OpCacheValue {
                    toolpath,
                    gcode_body: body,
                    closed_count: closed_count_emitted,
                    offset_count: offset_count_emitted,
                    exit_state: post.capture_state(),
                    exit_xy: (last_pos.x, last_pos.y),
                    internal_swap_emitted,
                    // my03: capture exactly the warnings this op produced
                    // (everything pushed since `warn_start`) so a future
                    // cache hit can replay them.
                    warnings: warnings[warn_start..].to_vec(),
                },
            );
        }
        // k2ew + nguf: end-of-op tool bookkeeping (see next_prev_tool_id).
        prev_tool_id = Some(next_prev_tool_id(op, internal_swap_emitted));
        emitted_ops += 1;
        progress(
            "gcode",
            gcode_progress(emitted_ops, n_ops),
            &format!("emitted op {}", op.id),
        );
        sink(PipelineEvent::OpCompleted {
            op_id: op.id,
            cached: false,
        });
    }
    emit_program_end(header_setup, post);
    Ok(post.finish())
}

/// o3od: whether the op's kind-specific driver will emit any cut moves.
/// Specialty drivers have structural "no output" cases; standard ops have
/// their own emptiness guards downstream and always report `true` here so
/// the inter-op M6 still surfaces intent on multi-tool programs.
fn specialty_will_emit(op: &Op, project: &Project, objects: &[VcObject]) -> bool {
    match &op.kind {
        OpKind::VCarve { .. } => vcarve_would_emit(op, objects),
        OpKind::Thread { .. } => thread_would_emit(op, objects),
        OpKind::Pocket {
            strategy: PocketStrategy::Halfpipe { .. },
            ..
        } => halfpipe_would_emit(op, objects),
        // f60x: relief surfacing emits only when its referenced source exists.
        OpKind::ReliefMill { .. } => relief_would_emit(op, project),
        _ => true,
    }
}

/// At an op boundary, emit the toolchange safety envelope (safe-Z →
/// M5+dwell → M6 → z-shift → M3+dwell) when the primary tool changes, and
/// return the updated `prev_tool_id`. When the tool is unknown we still
/// advance `prev_tool_id` (matching the historical behaviour) but skip the
/// envelope. Setup synthesis maps `ToolEntry.id` → `ToolConfig.number` 1:1,
/// so `tool.id` is the spindle tool number. See eaeq / m8sq / rwv8 / rfow.
fn emit_boundary_toolchange<P: PostProcessor>(
    op: &Op,
    project: &Project,
    header_setup: &Setup,
    tool_index: &HashMap<u32, &ToolEntry>,
    post: &mut P,
    prev_tool_id: Option<u32>,
) -> Option<u32> {
    if prev_tool_id == Some(op.tool_id) {
        return prev_tool_id;
    }
    if let Some(tool) = tool_index.get(&op.tool_id).copied() {
        let is_first_tool = prev_tool_id.is_none();
        if project.machine.supports_toolchange {
            post.comment(&format!(
                "toolchange: T{} ({}) for op {} ({})",
                tool.id, tool.name, op.id, op.name
            ));
        }
        emit_toolchange_envelope(
            post,
            &project.machine,
            header_setup,
            Some(tool),
            tool.id,
            is_first_tool,
            // Inter-op boundary: the next op's resolved cut speed isn't
            // synthesized at this site, so fall back to the tool's library
            // speed (liyy).
            None,
        );
    }
    Some(op.tool_id)
}

/// Dispatch one op to its kind-specific driver. Specialty drivers emit XYZ
/// blocks directly (prefixed with the `; OP <id>` marker) and report no
/// offset stats; the standard cascade returns `(closed_count, offset_count,
/// internal_swap_emitted)`. `internal_swap_emitted` (nguf) is only ever set
/// by the standard path — the specialty drivers don't dual-tool.
#[allow(clippy::too_many_arguments)]
fn run_op_driver<P: PostProcessor>(
    op: &Op,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(usize, usize, bool), PipelineError> {
    match &op.kind {
        OpKind::VCarve { .. } => {
            post.raw(&format!("; OP {}", op.id));
            run_vcarve_op(
                op, project, objects, setup, post, last_pos, warnings, cancel,
            )?;
            Ok((0, 0, false))
        }
        OpKind::Thread { .. } => {
            post.raw(&format!("; OP {}", op.id));
            run_thread_op(
                op, project, objects, setup, post, last_pos, warnings, cancel,
            )?;
            Ok((0, 0, false))
        }
        OpKind::Pocket {
            strategy: PocketStrategy::Halfpipe { .. },
            ..
        } => {
            post.raw(&format!("; OP {}", op.id));
            run_halfpipe_op(
                op, project, objects, setup, post, last_pos, warnings, cancel,
            )?;
            Ok((0, 0, false))
        }
        // f60x: 3-axis ball-nose relief surfacing — own drop-cutter driver,
        // like Halfpipe/VCarve it emits XYZ blocks directly.
        OpKind::ReliefMill { .. } => {
            post.raw(&format!("; OP {}", op.id));
            run_relief_op(op, project, setup, post, last_pos, warnings, cancel)?;
            Ok((0, 0, false))
        }
        _ => run_standard_op(
            op, project, objects, setup, post, last_pos, warnings, cancel,
        ),
    }
}

/// End-of-op tool bookkeeping: the tool id the spindle holds after this op,
/// shared by the cache-hit and fresh-emit paths. Bias to the finish tool
/// ONLY when the driver actually emitted the internal rough→finish (or
/// drill→chamfer) envelope (nguf); otherwise keep the rough tool so the next
/// same-tool op correctly elides its M6 (k2ew). The previous "pessimistic"
/// bias to `finish_id` whenever `finish_tool_id` was set caused a real bug: a
/// `dual_tool` op that skipped the swap left the held tool == `finish_id`, so
/// the next op asking for the rough tool saw "tool changes — skip" and cut
/// with the wrong T still in the spindle.
fn next_prev_tool_id(op: &Op, internal_swap_emitted: bool) -> u32 {
    if internal_swap_emitted {
        if let Some(finish_id) = op.finish_tool_id {
            if finish_id != op.tool_id {
                return finish_id;
            }
        }
    }
    op.tool_id
}

// 56a: resolve_op_segments / ordered_selection / source_combine_mode /
// op_includes_object live in pipeline/selection.rs. Re-exported via the
// `mod selection;` + `pub(super) use selection::*` block near the
// other `mod` declarations so child modules keep doing
// `use super::ordered_selection`.

/// Resolve the per-pass Z step: op override wins, otherwise the tool's
/// `default_step`. Both must be negative (a depth, not a height); a
/// non-negative value or two Nones produces a `step_unspecified`
/// warning.
pub(crate) fn effective_step(op: &Op, tool: &ToolEntry) -> Result<f64, PipelineWarning> {
    op.params
        .step
        .or(tool.default_step)
        .filter(|v| *v < 0.0)
        .ok_or_else(|| PipelineWarning {
            op_id: Some(op.id),
            kind: "step_unspecified".into(),
            message: "depth-per-pass not set on the operation or its tool's default_step".into(),
        })
}

/// Build a Setup whose `ToolConfig` comes from `op.finish_tool_id` —
/// used for the dual-tool finish block (rt1.33). Returns `Ok(None)`
/// when the op is single-tool or its `finish_tool_id` is missing /
/// equal to `tool_id`; `Ok(Some(setup))` when a distinct finish tool
/// exists. Falls through `Err(PipelineError::UnknownTool)` if the
/// referenced finish tool id isn't in the project.
pub(super) fn synthesize_finish_setup(
    op: &Op,
    project: &Project,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<Option<crate::cam::setup::Setup>, PipelineError> {
    let Some(ft_id) = op.finish_tool_id else {
        return Ok(None);
    };
    if ft_id == op.tool_id {
        return Ok(None);
    }
    // Pocket dual-tool (rt1.33) AND Drill+chamfer (rt1.20) both
    // funnel through here; other op kinds shouldn't reach this path
    // (no offset would be tagged finish), but be defensive — return
    // None for anything else.
    let drill_with_chamfer = op.drill_chamfer_after_width_mm().is_some_and(|w| w > 0.0);
    if !matches!(op.kind, OpKind::Pocket { .. }) && !drill_with_chamfer {
        return Ok(None);
    }
    // Synthesize a temporary op pointing at the finish tool and use
    // the regular synth path so feed / plunge / spindle resolution
    // stays in one place. The temporary op runs as PassKind::Finish
    // via synthesize_op_setup's pass selection.
    let mut finish_op = op.clone();
    finish_op.tool_id = ft_id;
    finish_op.finish_tool_id = None;
    let mut setup = synthesize_op_setup(&finish_op, project, warnings)?;
    // Force the finish block to use the finish tool's finish-set as
    // its rough rates too — every offset in the finish block is the
    // wall ring, the dedicated "finish" pass for the dual-tool flow.
    setup.tool.rate_h = setup.tool.rate_h_finish;
    setup.tool.rate_v = setup.tool.rate_v_finish;
    setup.tool.speed = setup.tool.speed_finish;
    Ok(Some(setup))
}

/// Wrap a `post.tool(new_tool_id)` call in the standard safety envelope:
/// safe-Z retract → spindle stop + dwell → toolchange → tool Z-shift →
/// spindle start (at the NEW tool's RPM) + dwell.
///
/// Fixes bd issues `eaeq` / `m8sq` / `rwv8` / `rfow`: every M6 wiac emits
/// now lifts the cutter clear, stops the spindle, performs the change,
/// and spins back up at the new tool's commanded speed BEFORE the next
/// cut move. Without this envelope the previous behavior emitted a bare
/// `T<n> M6` with the spindle still running and the cutter potentially
/// still engaged — a real safety hazard on every multi-tool program.
///
/// Routed through from three sites:
/// * `run_per_op` — inter-op tool boundary (k2ew + this fix together).
/// * `op_drivers/dual_tool.rs` — within-op rough → finish split.
/// * `op_drivers/drill.rs::emit_stufenfase` — drill → chamfer split.
///
/// When `machine.supports_toolchange == false` the function emits a
/// manual-swap pause envelope instead: M5 + dwell + a `; pause: swap to
/// tool <n>` comment + M0, so the operator hand-changes the bit. Resume
/// requires pressing Cycle Start. After resume the helper emits an
/// explicit M3 at the new tool's RPM (going through
/// [`PostProcessor::spindle_cw`]) so the next cut starts with the
/// spindle already at commanded speed — we can't trust the
/// delta-encoder's `last_speed` after a hand-swap.
/// liyy: `target_speed` is the RPM the envelope spins the spindle back
/// up to. Pass `Some(rpm)` when the caller knows the first cut after the
/// change runs at a non-default speed — notably the dual-tool and
/// stufenfase finish passes, whose blocks emit at `speed_finish`. Passing
/// the rough `ToolEntry.speed` there would emit a transient M3 at the
/// rough RPM that the following cut block immediately overrides via the
/// delta-encoder. `None` falls back to the tool's library `speed` (the
/// inter-op boundary case, where the next op's resolved speed isn't known
/// at this site).
pub(in crate::pipeline) fn emit_toolchange_envelope<P: PostProcessor>(
    post: &mut P,
    machine: &crate::cam::setup::MachineConfig,
    header_setup: &Setup,
    new_tool: Option<&ToolEntry>,
    new_tool_id: u32,
    is_first_tool: bool,
    target_speed: Option<u32>,
) {
    // Conservative: always lift to the program-wide safe Z before
    // touching the spindle. The post delta-encodes Z so this collapses
    // to nothing on the FIRST op (program_begin already moved there).
    // Skipping a needed lift is more dangerous than an extra rapid.
    let fast_z = header_setup.mill.fast_move_z;
    post.move_to(None, None, Some(fast_z));

    // ad0v: once clear in Z, rapid to the configured tool-change station
    // (machine coords, G53) BEFORE the M0 / M6 pause so a manual bit-swap
    // doesn't happen directly over the workpiece / clamps. Skip on the
    // first tool: it's already loaded by the operator before Cycle Start
    // (no pause is emitted), so there's nothing to clear yet. Opt-in —
    // an unset `toolchange_xy` keeps the prior behavior (safe-Z lift
    // only). Applies to both manual and ATC paths; HPGL / pen posts drop
    // the G53 (no machine frame). The post invalidates its WCS position
    // cache so the next op's rapid re-establishes XY in the work frame.
    if !is_first_tool {
        if let Some((tx, ty)) = machine.toolchange_xy {
            post.rapid_machine_xy(tx, ty);
        }
    }

    // lx1u: the toolchange envelope only manages a SPINDLE — laser /
    // drag-knife / pen-plotter modes don't have one. The per-cut
    // `cut_tool_on` (gcode.rs::emit_*) is mode-aware and fires the
    // laser / no-ops drag on its own; emitting M3/M4 S<rpm> here would
    // (a) on GRBL laser, turn the beam steady-on at the clamped-min
    // RPM during toolchange — a real safety hazard, and (b) on pen
    // plotter modes, leak a spindle line a controller may reject.
    // Stop-side M5 is similarly out of scope: many laser controllers
    // accept M5 as "beam off" which is fine, but the per-cut
    // `cut_tool_off` already arms that — and on Drag/HPGL plotters M5
    // is meaningless. Gate the entire spindle envelope on Mill mode.
    let is_mill = machine.mode == crate::cam::setup::MachineMode::Mill;

    // 3lf0: turn off active coolant BEFORE stopping the spindle / opening
    // the tool holder. With flood (M8) still running through M5 + M6,
    // water sprays into the open spindle taper / collet — operator
    // safety hazard AND contamination that ruins the chuck's grip. Many
    // auto-changers refuse to operate with coolant active. Mist (M7)
    // has the same problem on a smaller scale. Gate on
    // `!is_first_tool` so the program-start path (no coolant ever
    // commanded) doesn't emit a leading M9; gate on the post's tracked
    // `last_coolant` so we don't emit a redundant M9 when the previous
    // op already had coolant off. The next op's `coolant_flood` /
    // `coolant_mist` call (inside emit_offset / emit_drill_block /
    // emit_vcarve_block) will re-engage based on the new tool's
    // coolant setting — the post dedupes against `last_coolant=Off`
    // so the re-emit is just one M7/M8 line at the right place.
    if !is_first_tool && is_mill {
        let live_coolant = post.capture_state().last_coolant;
        if matches!(
            live_coolant,
            crate::gcode::CoolantState::Mist | crate::gcode::CoolantState::Flood
        ) {
            post.coolant_off();
        }
    }

    // Stop the spindle BEFORE the change. On the first op the spindle
    // isn't running yet — M5 is a harmless idempotent assertion and
    // costs one line. Skip the stop dwell when we know there's no
    // motion to wait for (first tool) so initial-state programs stay
    // identical to pre-fix output minus the M5 line.
    if !is_first_tool && is_mill {
        post.spindle_off();
        let stop_dwell = machine.effective_spindle_stop_dwell_sec();
        if stop_dwell > 0.0 {
            post.dwell(stop_dwell);
        }
    }

    if machine.supports_toolchange {
        // hat3: a manual touch-off / reference-tool prompt (if the
        // strategy calls for one) goes before the M6 so it's visible in
        // CAM-review; ATC machines don't actually pause on it.
        if let Some(prompt) = post_change_z_prompt(machine, new_tool_id, is_first_tool) {
            post.comment(&prompt);
        }
        // Auto-changer / macro-driven manual-with-prompt. The post's
        // tool() emits T<n> M6 (or the user's profile template).
        post.tool(new_tool_id);
        // hat3: re-establish the new tool's Z (probe / fixed sensor /
        // static shift) right after the change.
        emit_post_change_z(post, machine, new_tool, new_tool_id, is_first_tool);
        // Spin back up at the NEW tool's RPM. Pass pause=0 so the post
        // emits M3/M4 S<rpm> without an integer-second dwell tail; we
        // follow with an explicit `dwell(...)` so the machine-wide
        // spin-up (sub-second supported) AND the per-tool warm-up both
        // fire in the right order. zjgt: route through the central
        // `spindle_on` dispatcher so a CCW tool emits M4 here — the
        // previous unconditional `spindle_cw` baked M3 into the
        // post's `last_speed` snapshot, so the next op's lazy
        // `spindle_ccw(speed, 0)` saw last_speed == speed and elided
        // the M4 entirely (program ran CW with a CCW tool).
        if let Some(t) = new_tool {
            if is_mill {
                crate::gcode::spindle_on(
                    post,
                    t.spindle_direction,
                    setup_resolver::clamp_rpm_silent(target_speed.unwrap_or(t.speed), machine),
                    0,
                );
                let start_dwell = machine.effective_spindle_start_dwell_sec();
                if start_dwell > 0.0 {
                    post.dwell(start_dwell);
                }
                if t.pause > 0 {
                    post.dwell(f64::from(t.pause));
                }
            }
        }
    } else {
        // Manual hand-swap on a hobby controller. We can't trust the
        // controller to halt for an M6 — emit an explicit M0 program
        // pause so the operator confirms the bit swap with Cycle Start.
        // Tool Z-shift is applied AFTER the pause so the operator can
        // jog the new bit to the surface before the work-Z=0 line is
        // moved by G92.
        if let Some(t) = new_tool {
            post.comment(&format!("pause: swap to tool {} ({})", new_tool_id, t.name));
        } else {
            post.comment(&format!("pause: swap to tool {new_tool_id}"));
        }
        // hat3: emit any manual touch-off / reference-tool instruction
        // BEFORE the M0 so the operator reads it while the program is
        // halted (a post-pause comment lands after Cycle Start — too
        // late to act on).
        if let Some(prompt) = post_change_z_prompt(machine, new_tool_id, is_first_tool) {
            post.comment(&prompt);
        }
        if !is_first_tool {
            // M0: program-pause. Operator presses Cycle Start to
            // resume. Skip on first-tool because the spindle isn't
            // running yet — the program-start state is already
            // tool-swap-equivalent (operator loaded a bit before
            // hitting Cycle Start).
            post.raw("M0");
        }
        // hat3: re-establish the new tool's Z AFTER the pause — the
        // probe / fixed-sensor cycle runs automatically once the
        // operator confirms the swap with Cycle Start. `None` keeps the
        // legacy static z_shift here.
        emit_post_change_z(post, machine, new_tool, new_tool_id, is_first_tool);
        if let Some(t) = new_tool {
            // Force the next M3/M4 to actually emit (the operator may
            // have hand-spun the spindle off during the pause; we
            // can't trust the delta-encoder's last_speed snapshot
            // anymore). lx1u: only meaningful for Mill mode — laser /
            // drag-knife envelopes don't drive the spindle from here.
            if is_mill {
                post.reset_state();
                // Explicit spindle-up so the next cut starts with the
                // spindle at commanded RPM — don't rely on lazy emit.
                // zjgt: route through `spindle_on` so a CCW tool emits M4.
                crate::gcode::spindle_on(
                    post,
                    t.spindle_direction,
                    setup_resolver::clamp_rpm_silent(target_speed.unwrap_or(t.speed), machine),
                    0,
                );
                let start_dwell = machine.effective_spindle_start_dwell_sec();
                if start_dwell > 0.0 {
                    post.dwell(start_dwell);
                }
                if t.pause > 0 {
                    post.dwell(f64::from(t.pause));
                }
            }
        }
    }
}

/// hat3: the operator-facing prompt (if any) that must appear BEFORE
/// the tool-change pause — manual touch-off instructions the operator
/// acts on while the program is halted. Returns `None` for the
/// fully-automatic strategies (None / Probe / FixedSensor non-reference),
/// whose flow `emit_post_change_z` emits AFTER the pause. Always `None`
/// for the first tool (operator-loaded at program start, no pause).
fn post_change_z_prompt(
    machine: &crate::cam::setup::MachineConfig,
    new_tool_id: u32,
    is_first_tool: bool,
) -> Option<String> {
    use crate::cam::setup::PostChangeZStrategy as S;
    if is_first_tool {
        return None;
    }
    match &machine.post_change_z {
        S::ManualTouchoff => Some(format!(
            "touch off: jog tool {new_tool_id} to the work surface and zero Z before resuming"
        )),
        // The reference tool defines work Z0 by a workpiece touch-off
        // (not the sensor), so it gets the manual prompt too.
        S::FixedSensor {
            reference_tool_id: Some(ref_id),
            ..
        } if *ref_id == new_tool_id => Some(format!(
            "reference tool {new_tool_id}: touch off on the workpiece to set Z0 before resuming"
        )),
        _ => None,
    }
}

/// hat3: emit the post-tool-change Z re-establish flow AFTER the pause
/// / M6. `PostChangeZStrategy::None`, any strategy on the first tool
/// (operator-loaded at program start), and any non-Mill mode (no
/// spindle tool length to probe) all fall back to the legacy static
/// `ToolEntry.z_shift_mm`, so existing output stays byte-for-byte
/// identical. Probe / fixed-sensor strategies chain a `G38.2` cycle.
fn emit_post_change_z<P: PostProcessor>(
    post: &mut P,
    machine: &crate::cam::setup::MachineConfig,
    new_tool: Option<&ToolEntry>,
    new_tool_id: u32,
    is_first_tool: bool,
) {
    use crate::cam::setup::PostChangeZStrategy as S;
    let is_mill = machine.mode == crate::cam::setup::MachineMode::Mill;

    // Legacy static-shift fallback: the `None` default, the first tool,
    // and non-Mill modes all keep the pre-hat3 `tool_z_shift` behavior.
    if matches!(machine.post_change_z, S::None) || is_first_tool || !is_mill {
        if let Some(shift) = new_tool.and_then(|t| t.z_shift_mm) {
            post.tool_z_shift(shift);
        }
        return;
    }

    match &machine.post_change_z {
        // Handled by the fallback above; here only to satisfy the match.
        S::None => {}
        // The operator established Z by hand during the pause (prompt
        // emitted pre-pause). Intentionally NO static z_shift — it would
        // fight the hand touch-off.
        S::ManualTouchoff => {}
        S::Probe {
            distance_mm,
            feed_mm_min,
            plate_thickness_mm,
        } => {
            post.comment(&format!("post-change Z: probe touch plate (tool {new_tool_id})"));
            post.probe_toward_z(*distance_mm, *feed_mm_min);
            // Pin work Z to the plate top so Z0 stays the stock surface.
            // `set_work_z_here` (not `tool_z_shift`) so a 0 mm plate
            // still re-zeros Z.
            post.set_work_z_here(*plate_thickness_mm);
        }
        S::FixedSensor {
            position,
            seek_mm,
            feed_mm_min,
            reference_tool_id,
        } => {
            // The reference tool defines Z0 via a workpiece touch-off
            // (prompt emitted pre-pause) — no sensor probe for it.
            if *reference_tool_id == Some(new_tool_id) {
                return;
            }
            let (px, py, pz) = *position;
            post.comment(&format!("post-change Z: fixed sensor (tool {new_tool_id})"));
            post.rapid_machine_z(pz); // safe approach height above sensor
            post.rapid_machine_xy(px, py); // over the sensor
            post.probe_toward_z(*seek_mm, *feed_mm_min);
            post.apply_probed_tool_length();
            post.rapid_machine_z(pz); // retract to the approach height
        }
    }
}

// 56a: pipeline integration tests live in `pipeline/tests.rs` so this
// dispatcher file stays navigable (was 2223 lines pre-split; the 1300+
// lines of test cases dominated the view).
#[cfg(test)]
mod tests;

#[cfg(test)]
mod count_tool_changes_tests {
    use super::count_tool_changes;
    use crate::pipeline::test_helpers::{endmill, profile_op, project_with};
    use crate::project::{Op, OpKind, OpParams, OpSource};

    fn pause_op(id: u32) -> Op {
        Op {
            id,
            name: format!("Pause {id}"),
            enabled: true,
            kind: OpKind::Pause {
                message: "swap".into(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams::mill_default(),
            group: None,
        }
    }

    /// ye4b: a single-op program counts one tool change — the spindle
    /// enters the program empty, so the first op always emits a load.
    #[test]
    fn single_op_counts_one_change() {
        let project = project_with(
            vec![profile_op(1, 1, crate::cam::setup::ToolOffset::Outside)],
            vec![endmill(1, 3.0)],
        );
        assert_eq!(count_tool_changes(&project), 1);
    }

    /// ye4b: back-to-back same-tool ops collapse to one change.
    #[test]
    fn back_to_back_same_tool_counts_one() {
        let project = project_with(
            vec![
                profile_op(1, 1, crate::cam::setup::ToolOffset::Outside),
                profile_op(2, 1, crate::cam::setup::ToolOffset::Outside),
            ],
            vec![endmill(1, 3.0)],
        );
        assert_eq!(count_tool_changes(&project), 1);
    }

    /// ye4b: switching tools counts the boundary.
    #[test]
    fn two_distinct_tools_count_two() {
        let project = project_with(
            vec![
                profile_op(1, 1, crate::cam::setup::ToolOffset::Outside),
                profile_op(2, 2, crate::cam::setup::ToolOffset::Outside),
            ],
            vec![endmill(1, 3.0), endmill(2, 6.0)],
        );
        assert_eq!(count_tool_changes(&project), 2);
    }

    /// ye4b: Pause ops don't touch the spindle and don't affect the
    /// next op's boundary decision — three same-tool cuts with a Pause
    /// in between still count as one change.
    #[test]
    fn pause_op_does_not_break_same_tool_run() {
        let project = project_with(
            vec![
                profile_op(1, 1, crate::cam::setup::ToolOffset::Outside),
                pause_op(2),
                profile_op(3, 1, crate::cam::setup::ToolOffset::Outside),
            ],
            vec![endmill(1, 3.0)],
        );
        assert_eq!(count_tool_changes(&project), 1);
    }

    /// ye4b: disabled ops are skipped.
    #[test]
    fn disabled_ops_are_skipped() {
        let mut a = profile_op(1, 1, crate::cam::setup::ToolOffset::Outside);
        a.enabled = false;
        let project = project_with(
            vec![a, profile_op(2, 2, crate::cam::setup::ToolOffset::Outside)],
            vec![endmill(1, 3.0), endmill(2, 6.0)],
        );
        assert_eq!(count_tool_changes(&project), 1);
    }

    /// vmm0: a Profile op with `finish_tool_id` set to a different tool
    /// MUST NOT count an internal swap. The runtime `dual_tool` path
    /// only synthesizes a finish setup for Pocket / drill-with-chamfer
    /// ops (`synthesize_finish_setup` at pipeline.rs:1037); a Profile
    /// op falls through to single-emit with no envelope, so the actual
    /// M6 count is 1, not 2. Pre-fix the estimator added +1
    /// unconditionally on `finish_tool_id != tool_id`.
    #[test]
    fn profile_op_with_distinct_finish_tool_counts_one_change() {
        let mut op = profile_op(1, 1, crate::cam::setup::ToolOffset::Outside);
        op.finish_tool_id = Some(2);
        let project = project_with(vec![op], vec![endmill(1, 3.0), endmill(2, 6.0)]);
        // One load + zero internal swap (Profile op kind doesn't dual-tool).
        assert_eq!(count_tool_changes(&project), 1);
    }

    /// vmm0: Pocket op WITH a distinct `finish_tool_id` still counts the
    /// internal swap — Pocket is the canonical dual-tool path. The
    /// estimator slightly over-counts when the offsets cascade fails
    /// to produce an `is_finish` ring (e.g. zero-size pocket), but that
    /// edge is intentionally pessimistic per the bug report — the
    /// alternative is running the full offsets cascade twice.
    #[test]
    fn pocket_op_with_distinct_finish_tool_still_counts_internal_swap() {
        use crate::pipeline::test_helpers::pocket_op;
        let mut op = pocket_op(1, 1, crate::project::OpSource::All);
        op.finish_tool_id = Some(2);
        let project = project_with(vec![op], vec![endmill(1, 6.0), endmill(2, 3.0)]);
        // One load (tool 1) + one internal swap to tool 2.
        assert_eq!(count_tool_changes(&project), 2);
    }
}
