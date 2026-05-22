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
};

#[cfg(test)]
mod test_helpers;

use op_drivers::{run_halfpipe_op, run_standard_op, run_thread_op, run_vcarve_op};
use regions::build_region_previews;
pub use setup_resolver::fit_helix_radius_for_selection;
use setup_resolver::{header_setup_for, resolve_auto_helix_radius, synthesize_op_setup};

use std::collections::HashSet;
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
    let regions = build_region_previews(&project, &objects);
    let tool_changes = count_tool_changes(&gcode);
    let spindle_warmup_s = spindle_warmup_seconds(&project);
    // v7f5: build per-op tool-rate lookup so the estimator clamps
    // Plunge segments to the tool's plunge_rate even when the post
    // emitted a single F<feed> line.
    let op_rates: Vec<crate::sim::timing::OpRates> = project
        .operations
        .iter()
        .filter_map(|op| {
            let tool = project.tools.iter().find(|t| t.id == op.tool_id)?;
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

fn count_tool_changes(gcode: &str) -> u32 {
    let mut n = 0u32;
    for line in gcode.lines() {
        let stripped = line.split(';').next().unwrap_or("");
        for tok in stripped.split_whitespace() {
            if tok.eq_ignore_ascii_case("M6") {
                n += 1;
            }
        }
    }
    n
}

fn spindle_warmup_seconds(project: &Project) -> f64 {
    let mut used: HashSet<u32> = HashSet::new();
    for op in project.operations.iter().filter(|o| o.enabled) {
        used.insert(op.tool_id);
    }
    project
        .tools
        .iter()
        .filter(|t| used.contains(&t.id))
        .map(|t| f64::from(t.pause))
        .sum()
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
    // k2ew: track the tool number last asserted via post.tool() so we
    // can emit T<n> M6 + Z-shift at every op boundary where the
    // primary tool changes — and at the FIRST op so the program never
    // silently uses whatever was in the spindle. Pause ops don't have
    // a tool and don't reset this state. We track by ToolEntry.id
    // (the project-level tool key), not by tool.number (which can be
    // shared across entries on some configs).
    let mut prev_tool_id: Option<u32> = None;
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

        // Reset the post's delta-encoding state at every op boundary so
        // the captured body lines are independent of whatever state the
        // previous op (cached or fresh) left behind. Both fresh-emit
        // and cache-hit paths see the same entry state — the only
        // difference is whether the body comes from re-emission or
        // from the cache. Exit state is captured/restored separately.
        post.reset_state();

        // k2ew: emit M6 toolchange BEFORE body_marker so the M6 is
        // NOT captured into the per-op cache body — the M6 decision
        // depends on prev_tool_id which is runtime state, not op
        // state. On cache hit we still get this block; on cache miss
        // the body that follows starts at body_marker. Skip Pause ops
        // entirely (no tool, no toolchange — and they don't reset
        // prev_tool_id either).
        if !matches!(op.kind, OpKind::Pause { .. }) {
            let tool_changes = prev_tool_id != Some(op.tool_id);
            if tool_changes {
                if let Some(tool) = project.tools.iter().find(|t| t.id == op.tool_id) {
                    // Setup synthesis maps ToolEntry.id → ToolConfig.number
                    // 1:1 (setup_resolver), so tool.id is the spindle
                    // tool number we want here.
                    if project.machine.supports_toolchange {
                        post.comment(&format!(
                            "toolchange: T{} ({}) for op {} ({})",
                            tool.id, tool.name, op.id, op.name
                        ));
                        post.tool(tool.id);
                    }
                    // rt1.30: apply the tool's Z shift after every
                    // (re-)assertion so the work-Z=0 line matches the
                    // newly-loaded tool. Fires on the first op too,
                    // replacing the pre-loop tool_z_shift block.
                    if let Some(shift) = tool.z_shift_mm {
                        post.tool_z_shift(shift);
                    }
                }
                prev_tool_id = Some(op.tool_id);
            }
        }
        let body_marker = post.out_lines_count();

        // rt1.34: Pause op — emit M5 → optional-stop → M3 and skip the
        // rest of the op machinery (no tool, no source, no setup, no
        // cache). The controller halts on M0; pressing Cycle Start
        // resumes. M3 with no S argument restores the spindle to its
        // last commanded RPM (handled controller-side; the post's
        // last_speed state is unchanged so the next op's spindle_cw at
        // the same RPM correctly elides its own M3).
        if let OpKind::Pause { message } = &op.kind {
            post.raw(&format!("; OP {} (pause)", op.id));
            post.raw("M5");
            if !message.is_empty() {
                post.comment(message);
            }
            post.raw("M0");
            post.raw("M3");
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

        // Cache lookup. We skip caching when no cache is provided.
        let cache_key = cache.and_then(|_| {
            let tool = project.tools.iter().find(|t| t.id == op.tool_id)?;
            // For dual-tool Pocket ops (rt1.33), fold the finish tool's
            // properties into the key so changing its diameter / feeds
            // / RPMs invalidates cached output. Single-tool ops pass
            // None and route through op_cache_key_with_finish's
            // is-finish-tool sentinel.
            let finish_tool = op
                .finish_tool_id
                .filter(|id| *id != op.tool_id)
                .and_then(|id| project.tools.iter().find(|t| t.id == id));
            Some(op_cache_key_with_finish(
                op,
                tool,
                finish_tool,
                &project.machine,
                &resolve_op_segments(op, &project.segments, objects),
                &project.fixtures,
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
                last_pos = Point2::new(cached.exit_xy.0, cached.exit_xy.1);
                {
                    let mut s = stats.borrow_mut();
                    s.0 += cached.closed_count;
                    s.1 += cached.offset_count;
                }
                // k2ew: same end-of-op prev_tool_id update as the
                // non-cached path — cached bodies for dual_tool ops
                // include the internal toolchange.
                if let Some(finish_id) = op.finish_tool_id {
                    if finish_id != op.tool_id {
                        prev_tool_id = Some(finish_id);
                    }
                }
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

        let mut setup = synthesize_op_setup(op, project, warnings)?;
        resolve_auto_helix_radius(op, objects, &mut setup, warnings);
        let mut closed_count_emitted: usize = 0;
        let mut offset_count_emitted: usize = 0;
        if matches!(op.kind, OpKind::VCarve { .. }) {
            post.raw(&format!("; OP {}", op.id));
            run_vcarve_op(
                op,
                project,
                objects,
                &setup,
                post,
                &mut last_pos,
                warnings,
                cancel,
            )?;
        } else if matches!(op.kind, OpKind::Thread { .. }) {
            post.raw(&format!("; OP {}", op.id));
            run_thread_op(
                op,
                project,
                objects,
                &setup,
                post,
                &mut last_pos,
                warnings,
                cancel,
            )?;
        } else if matches!(
            op.kind,
            OpKind::Pocket {
                strategy: PocketStrategy::Halfpipe { .. },
                ..
            }
        ) {
            post.raw(&format!("; OP {}", op.id));
            run_halfpipe_op(
                op,
                project,
                objects,
                &setup,
                post,
                &mut last_pos,
                warnings,
                cancel,
            )?;
        } else {
            let (closed_count, offset_count) = run_standard_op(
                op,
                project,
                objects,
                &setup,
                post,
                &mut last_pos,
                warnings,
                cancel,
            )?;
            closed_count_emitted = closed_count;
            offset_count_emitted = offset_count;
            let mut s = stats.borrow_mut();
            s.0 += closed_count;
            s.1 += offset_count;
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
                },
            );
        }
        // k2ew: if this op declared a distinct finish tool (dual_tool
        // Pocket or Stufenfase drill chamfer), the op's internal
        // driver may have switched to it mid-op. We pessimistically
        // record that as the end-of-op tool so the next op's M6
        // decision is safety-biased: if the driver actually swapped,
        // the next op gets a correct M6 decision; if it didn't swap
        // (e.g. no finish offsets produced), the next op may emit an
        // extra T<rough> M6 — wasteful, but safe. Same-tool ops
        // (no finish or finish == rough) keep prev_tool_id == op.tool_id
        // so back-to-back same-tool ops still emit at most one M6.
        if let Some(finish_id) = op.finish_tool_id {
            if finish_id != op.tool_id {
                prev_tool_id = Some(finish_id);
            }
        }
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

// 56a: pipeline integration tests live in `pipeline/tests.rs` so this
// dispatcher file stays navigable (was 2223 lines pre-split; the 1300+
// lines of test cases dominated the view).
#[cfg(test)]
mod tests;
