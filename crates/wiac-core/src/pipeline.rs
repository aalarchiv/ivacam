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
mod setup_resolver;
mod tabs;
mod warnings;

#[cfg(test)]
mod test_helpers;

use op_drivers::{run_halfpipe_op, run_standard_op, run_thread_op, run_vcarve_op};
use regions::build_region_previews;
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
use crate::geometry::{Point2, Segment};
use crate::pipeline_cache::{op_cache_key_with_finish, OpCacheValue, PipelineCache};
use crate::project::{Op, OpKind, OpSource, PocketStrategy, Project, SourceCombine, ToolEntry};

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
    // One working copy of `objects` for the chosen post — the original
    // is kept intact for the region-preview pass below. Previously each
    // match arm called `objects.clone()`, which made the compiler evaluate
    // all three even though only one runs (jzpl).
    let mut work_objects = objects.clone();
    let gcode = match post_kind {
        PostProcessorKind::Linuxcnc => run_per_op(
            &project,
            &mut work_objects,
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
            &mut work_objects,
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
            &mut work_objects,
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
    let time_estimate = crate::sim::timing::estimate_from_gcode(
        &gcode,
        &toolpath,
        &project.machine,
        tool_changes,
        spindle_warmup_s,
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
    objects: &mut [VcObject],
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
    // rt1.30: apply the first enabled op's tool's Z shift right after
    // program_begin so even single-tool programs honor the offset.
    if let Some(first) = project.operations.iter().find(|o| o.enabled) {
        if let Some(tool) = project.tools.iter().find(|t| t.id == first.tool_id) {
            if let Some(shift) = tool.z_shift_mm {
                post.tool_z_shift(shift);
            }
        }
    }
    let gcode_progress = |emitted: usize, total: usize| -> f64 {
        let denom = total.max(1) as f64;
        GCODE_PROGRESS_START + GCODE_PROGRESS_SPAN * (emitted as f64 / denom)
    };
    let mut last_pos = Point2::new(0.0, 0.0);
    let mut emitted_ops = 0usize;
    let enabled_ops: Vec<&Op> = project.operations.iter().filter(|o| o.enabled).collect();
    let total_ops = enabled_ops.len();
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
        let body_marker = post.out_lines_count();

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

/// Slice the project's segments down to the subset this op consumes.
/// Used by the cache key — hashing the relevant segments only keeps the
/// hit rate up when the user adds unrelated geometry on a different
/// layer or another object that this op never touches.
///
/// `objects` is the current chained-object set (which the per-op loop
/// may have expanded with patterns or frame synthesis); for
/// `OpSource::Objects { ids }` we walk only the segments owned
/// by the selected objects in id order so adding an unrelated object
/// doesn't invalidate this op's cached output. Ids are 1-based; ids
/// that fall outside the current `objects` set are silently skipped
/// (e.g. after a prior op's pattern expansion replaced the chained
/// set — the resulting empty segment list still hashes deterministically).
fn resolve_op_segments(op: &Op, all: &[Segment], objects: &[VcObject]) -> Vec<Segment> {
    match &op.source {
        OpSource::All => all.to_vec(),
        OpSource::Layers { layers, .. } => all
            .iter()
            .filter(|s| layers.iter().any(|l| l == &s.layer))
            .cloned()
            .collect(),
        OpSource::Objects { ids, .. } => {
            let mut out = Vec::new();
            for &id in ids {
                let idx = (id as usize).saturating_sub(1);
                if let Some(obj) = objects.get(idx) {
                    out.extend(obj.segments.iter().cloned());
                }
            }
            out
        }
    }
}

/// Trochoidal-specific guards: tabs are not yet supported and the
/// plunge must be Helix. We emit warnings for unsupported tabs and
/// override Direct/Ramp plunges to Helix at the `synthesize_op_setup`
/// site (see `effective_plunge_for`).
/// Walk the op's source in user-specified order and return the matching
/// object indices. Used by non-Auto combine modes — Difference in
/// particular is order-sensitive ("first selected minus the rest"), so
/// we cannot iterate the unordered `selected_set` there.
pub(super) fn ordered_selection(op: &Op, objects: &[VcObject]) -> Vec<usize> {
    match &op.source {
        OpSource::All => (0..objects.len()).collect(),
        OpSource::Layers { layers, .. } => objects
            .iter()
            .enumerate()
            .filter(|(_, obj)| layers.iter().any(|l| l == &obj.layer))
            .map(|(i, _)| i)
            .collect(),
        OpSource::Objects { ids, .. } => ids
            .iter()
            .filter_map(|id| {
                let idx = (*id as usize).checked_sub(1)?;
                objects.get(idx).map(|_| idx)
            })
            .collect(),
    }
}

/// Pull the `SourceCombine` mode out of an op's source.
///
/// `OpSource::All` always reports `Auto` — by design. "All
/// objects" has no UI affordance for a combine selector, so the
/// pipeline treats it as "let each op kind decide". Pocket then falls
/// through to its containment-aware per-object loop (outer carves +
/// inner holes); Profile / Engrave / `DragKnife` emit one path per
/// selected object. Layers / Objects sources carry an explicit
/// `combine` field and that value is honored verbatim — including
/// `Auto`, which means the same per-op-kind dispatch path.
pub(super) fn source_combine_mode(op: &Op) -> SourceCombine {
    match &op.source {
        OpSource::All => SourceCombine::Auto,
        OpSource::Layers { combine, .. } | OpSource::Objects { combine, .. } => *combine,
    }
}

pub(super) fn op_includes_object(op: &Op, obj: &VcObject, idx: usize) -> bool {
    match &op.source {
        OpSource::All => true,
        OpSource::Layers { layers, .. } => layers.iter().any(|l| l == &obj.layer),
        // OpSource::Objects ids are 1-based, matching the
        // ImportOutput.objects[i] mapping the frontend uses for
        // selection.
        OpSource::Objects { ids, .. } => {
            let chain_id = (idx as u32) + 1;
            ids.contains(&chain_id)
        }
    }
}

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

#[cfg(test)]
// Test assertions like `assert_eq!(effective_step(&op, &tool).unwrap(), -0.5)`
// compare values that propagate through the pipeline by direct assignment
// from a literal — exact equality is the right test.
#[allow(clippy::float_cmp)]
mod tests {
    use super::test_helpers::*;
    use super::*;
    use crate::cam::setup::{MachineConfig, ToolOffset};
    use crate::geometry::Segment;
    use crate::project::{
        Op, OpKind, OpParams, OpSource, SourceCombine, TextAlignment, TextLayer, TextLayerKind,
        ToolEntry, ToolKind,
    };

    #[test]
    fn pipeline_renders_text_layers_and_routes_via_synthetic_layer() {
        // Engrave op pointing at the synthetic `__text_1` layer.
        let engrave = Op {
            id: 1,
            name: "Engrave text".into(),
            enabled: true,
            kind: OpKind::Engrave {
                contour: crate::project::ContourParams::default(),
            },
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::Layers {
                layers: vec!["__text_1".into()],
                combine: SourceCombine::default(),
            },
            params: OpParams::mill_default(),
        };
        let text_layer = TextLayer {
            id: 1,
            kind: TextLayerKind::Text,
            name: "Hello".into(),
            text: "A".into(),
            font_bytes: dejavu_font_bytes(),
            size_mm: 10.0,
            origin: (0.0, 0.0),
            rotation_deg: 0.0,
            letter_spacing_mm: 0.0,
            line_spacing_mm: 0.0,
            alignment: TextAlignment::Left,
        };
        let project = Project {
            segments: Vec::new(), // pipeline pre-pass appends the rendered text
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 1.0)],
            operations: vec![engrave],
            fixtures: Vec::default(),
            text_layers: vec![text_layer],
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .expect("pipeline should run text engraving end-to-end");
        // Pipeline emitted gcode and at least one cut move tagged to op #1.
        assert!(resp.gcode.contains("; OP 1"), "no op marker for text op");
        assert!(
            resp.toolpath.iter().any(|s| s.op_id == 1),
            "no cut segments emitted for the text op"
        );
    }

    #[test]
    fn run_pipeline_emits_a_recognizable_program() {
        let resp = run_pipeline(
            PipelineRequest {
                project: project_with(
                    vec![profile_op(1, 1, ToolOffset::Outside)],
                    vec![endmill(1, 3.0)],
                ),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.gcode.contains("G21"));
        assert!(resp.gcode.contains("G90"));
        assert!(!resp.toolpath.is_empty());
        assert_eq!(resp.stats.object_count, 1);
        assert_eq!(resp.stats.closed_object_count, 1);
        assert!(resp.stats.offset_count >= 1);
        assert!(resp.gcode.contains("; OP 1"));
        // Cut segments carry the op id; program-header rapids carry op_id=0.
        assert!(resp.toolpath.iter().any(|s| s.op_id == 1));
        assert!(resp
            .toolpath
            .iter()
            .filter(|s| s.op_id != 0)
            .all(|s| s.op_id == 1));
    }

    #[test]
    fn run_pipeline_picks_grbl_when_requested() {
        let resp = run_pipeline(
            PipelineRequest {
                project: project_with(
                    vec![profile_op(1, 1, ToolOffset::Outside)],
                    vec![endmill(1, 3.0)],
                ),
                post_processor: Some(PostProcessorKind::Grbl),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(!resp.gcode.is_empty());
    }

    #[test]
    fn two_op_project_emits_two_distinct_op_blocks() {
        let project = project_with(
            vec![
                profile_op(1, 1, ToolOffset::Outside),
                profile_op(2, 1, ToolOffset::Outside),
            ],
            vec![endmill(1, 3.0)],
        );
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.gcode.contains("; OP 1"));
        assert!(resp.gcode.contains("; OP 2"));
        assert!(resp.toolpath.iter().any(|s| s.op_id == 1));
        assert!(resp.toolpath.iter().any(|s| s.op_id == 2));
    }

    #[test]
    fn progress_callback_fires_each_phase() {
        let phases = std::cell::RefCell::new(Vec::<String>::new());
        let _ = run_pipeline(
            PipelineRequest {
                project: project_with(
                    vec![profile_op(1, 1, ToolOffset::Outside)],
                    vec![endmill(1, 3.0)],
                ),
                post_processor: None,
            },
            |phase, _f, _m| phases.borrow_mut().push(phase.to_string()),
        )
        .unwrap();
        let phases = phases.into_inner();
        for expected in ["import", "objects", "gcode", "preview", "done"] {
            assert!(
                phases.contains(&expected.to_string()),
                "missing {expected} in {phases:?}"
            );
        }
    }

    /// Post profile (rt1.15): a custom `program_start` template
    /// replaces the `LinuxCNC` `(generated by …)` header, with token
    /// substitution honoring the active tool and unit.
    #[test]
    fn post_profile_overrides_program_start_and_end() {
        use crate::gcode::post_profile::PostProfile;
        let machine = MachineConfig {
            post_profile: Some(PostProfile {
                name: "Test".into(),
                program_start: Some("; wiac <version>\n; tool <t> <n>".into()),
                program_end: Some("; bye\nM30".into()),
                ..Default::default()
            }),
            ..MachineConfig::default()
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine,
            tools: vec![{
                let mut t = endmill(7, 3.0);
                t.name = "3mm endmill".into();
                t
            }],
            operations: vec![profile_op(1, 7, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // Header has the custom prologue (multi-line via \n in
        // template) + the version + tool number / name tokens
        // substituted.
        assert!(
            resp.gcode.contains("; wiac "),
            "expected custom version prologue:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("; tool 7 3mm endmill"),
            "expected tool token substitution:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("; bye"),
            "expected custom footer:\n{}",
            resp.gcode
        );
        // Default LinuxCNC header is NOT emitted when a profile is set.
        assert!(
            !resp.gcode.contains("(generated by wiaConstructor)"),
            "default header leaked through with profile set:\n{}",
            resp.gcode
        );
    }

    /// Post profile (hev): per-axis config flips Z scale, renames Y to
    /// V, disables I/J emission, and re-formats F with two decimals.
    /// The emitted gcode reflects every knob.
    #[test]
    fn post_profile_axes_config_drives_axis_emission() {
        use crate::gcode::post_profile::{AxesConfig, AxisFormat, PostProfile};
        let mut axes = AxesConfig::default();
        axes.z.scale = -1.0; // flip Z-up to Z-down
        axes.y.name = "V".into(); // rotary swap
        axes.i.enabled = false;
        axes.j.enabled = false;
        axes.feed = AxisFormat {
            enabled: true,
            name: "F".into(),
            format: "%.2f".into(),
            scale: 1.0,
        };
        let machine = MachineConfig {
            post_profile: Some(PostProfile {
                name: "Test axes".into(),
                axes: Some(axes),
                ..Default::default()
            }),
            ..MachineConfig::default()
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine,
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // Z is scaled by -1: the depth dives below zero in source units
        // (typically Z-2 or similar), so the emitted Z must be POSITIVE.
        let z_lines: Vec<&str> = resp
            .gcode
            .lines()
            .filter(|l| l.contains('Z') && (l.starts_with("G0") || l.starts_with("G1")))
            .collect();
        assert!(
            !z_lines.is_empty(),
            "expected some Z moves:\n{}",
            resp.gcode
        );
        assert!(
            z_lines
                .iter()
                .any(|l| l.contains("Z2.") || l.contains("Z3.") || l.contains("Z5.")),
            "expected at least one positive Z move after scale=-1 flip:\n{}",
            z_lines.join("\n")
        );
        // Y has been renamed to V. Some Y move should now show up as V.
        assert!(
            resp.gcode.contains(" V"),
            "expected renamed Y→V axis:\n{}",
            resp.gcode
        );
        assert!(
            !resp
                .gcode
                .lines()
                .any(|l| { (l.starts_with("G0") || l.starts_with("G1")) && l.contains(" Y") }),
            "Y should no longer be emitted on G0/G1:\n{}",
            resp.gcode
        );
        // Profile op walks a closed square — no arcs => no I/J in the
        // baseline. But the F line should use two decimals now.
        assert!(
            resp.gcode
                .lines()
                .any(|l| l.starts_with('F') && l.contains('.')),
            "feed line should now carry decimals from %.2f:\n{}",
            resp.gcode
        );
    }

    /// Post profile (hev): disabling Z entirely drops every Z word
    /// from G0 / G1 moves — useful for laser controllers that don't
    /// have a Z axis.
    #[test]
    fn post_profile_disabled_axis_drops_the_word() {
        use crate::gcode::post_profile::{AxesConfig, PostProfile};
        let mut axes = AxesConfig::default();
        axes.z.enabled = false;
        let machine = MachineConfig {
            post_profile: Some(PostProfile {
                name: "No Z".into(),
                axes: Some(axes),
                ..Default::default()
            }),
            ..MachineConfig::default()
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine,
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // No G0/G1 line should mention Z when the axis is disabled.
        let bad: Vec<&str> = resp
            .gcode
            .lines()
            .filter(|l| (l.starts_with("G0 ") || l.starts_with("G1 ")) && l.contains('Z'))
            .collect();
        assert!(
            bad.is_empty(),
            "G0/G1 lines still carry Z words after disabling Z:\n{}",
            bad.join("\n")
        );
    }

    /// Post profile (hev): unset `axes` means baseline behavior — the
    /// `LinuxCNC` `(generated by …)` header is gone (we set a custom
    /// `program_start`) but coordinate emission stays exactly the same.
    #[test]
    fn post_profile_without_axes_keeps_legacy_output() {
        use crate::gcode::post_profile::PostProfile;
        let machine_with = MachineConfig {
            post_profile: Some(PostProfile {
                name: "Test".into(),
                program_start: Some("; header".into()),
                axes: None,
                ..Default::default()
            }),
            ..MachineConfig::default()
        };
        let machine_without = MachineConfig::default();
        let project = |m: crate::cam::setup::MachineConfig| Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: m,
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp_a = run_pipeline(
            PipelineRequest {
                project: project(machine_with),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let resp_b = run_pipeline(
            PipelineRequest {
                project: project(machine_without),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // Skip the first two header lines so the program_start text
        // doesn't drown out the comparison; everything after must
        // match between the axes=None profile run and the no-profile
        // run.
        let strip = |s: &str| {
            s.lines()
                .filter(|l| !l.starts_with("; header"))
                .filter(|l| !l.starts_with("(generated by wiaConstructor)"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(
            strip(&resp_a.gcode),
            strip(&resp_b.gcode),
            "axes=None should be a bit-identical no-op vs. no profile",
        );
    }

    /// New `ToolKind` variants (rt1.28): `BullNose` / Compression /
    /// `TSlot` / `FormProfile` all serialize + deserialize cleanly and
    /// carry their geometry fields through round-trip.
    #[test]
    fn extended_tool_kinds_serde_round_trip() {
        for (kind, label) in [
            (ToolKind::BullNose, "bull_nose"),
            (ToolKind::Compression, "compression"),
            (ToolKind::TSlot, "t_slot"),
            (ToolKind::FormProfile, "form_profile"),
        ] {
            let mut t = endmill(7, 6.0);
            t.kind = kind;
            t.corner_radius_mm = Some(0.5);
            t.tslot_neck_diameter_mm = Some(3.0);
            t.tslot_neck_length_mm = Some(8.0);
            let json = serde_json::to_string(&t).unwrap();
            assert!(json.contains(label), "expected '{label}' in {json}");
            let back: ToolEntry = serde_json::from_str(&json).unwrap();
            assert_eq!(back.kind, kind);
            assert_eq!(back.corner_radius_mm, Some(0.5));
            assert_eq!(back.tslot_neck_diameter_mm, Some(3.0));
            assert_eq!(back.tslot_neck_length_mm, Some(8.0));
        }
    }

    /// Plot-mode Z (rt1.35): with `plot_mode_z` enabled, every Z value
    /// in the gcode is one of {`fast_move_z`, `cut_depth`}. No
    /// intermediate Z values from a step-down schedule.
    #[test]
    fn plot_mode_emits_only_two_z_values() {
        let machine = MachineConfig {
            plot_mode_z: true,
            ..MachineConfig::default()
        };
        let mut params = OpParams::mill_default();
        params.depth = -3.0; // would normally cascade through Z=-1, -2, -3
        params.start_depth = 0.0;
        params.fast_move_z = 5.0;
        params.step = Some(-1.0);
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine,
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Laser cut".into(),
                enabled: true,
                kind: OpKind::Engrave {
                    contour: crate::project::ContourParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let z_values: std::collections::HashSet<String> = resp
            .gcode
            .lines()
            .flat_map(|l| {
                l.split_whitespace()
                    .filter_map(|t| t.strip_prefix('Z'))
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
        // Expect Z values only at {5, -3} (plus possibly 0 for the
        // pre-plunge "drop to z=0" line — that's still in the
        // emit_offset prelude before multi_pass takes over).
        let allowed = ["5", "-3", "0"];
        for z in &z_values {
            assert!(
                allowed.contains(&z.as_str()),
                "unexpected Z value {z} in plot mode:\n{}",
                resp.gcode
            );
        }
        // And the intermediate descent values must NOT appear.
        assert!(
            !z_values.contains("-1"),
            "Z=-1 leaked through plot mode:\n{}",
            resp.gcode
        );
        assert!(
            !z_values.contains("-2"),
            "Z=-2 leaked through plot mode:\n{}",
            resp.gcode
        );
    }

    /// Approach point serde round-trip (rt1.26).
    #[test]
    fn approach_point_serde_round_trip() {
        let contour = crate::project::ContourParams {
            approach_point: Some((3.5, -2.0)),
            ..crate::project::ContourParams::default()
        };
        let json = serde_json::to_string(&contour).unwrap();
        assert!(json.contains("approach_point"));
        let back: crate::project::ContourParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.approach_point, Some((3.5, -2.0)));
        // Unset round-trips as absent.
        let none_contour = crate::project::ContourParams::default();
        let json_none = serde_json::to_string(&none_contour).unwrap();
        assert!(!json_none.contains("approach_point"));
    }

    /// Laser pierce time (rt1.29): a laser tool with
    /// `laser_pierce_sec` set emits a `G4 P<sec>` dwell between
    /// rapid-to-entry and plunge.
    #[test]
    fn laser_op_emits_pierce_dwell_before_cut() {
        let mut tool = endmill(1, 0.1);
        tool.kind = ToolKind::LaserBeam;
        tool.laser_pierce_sec = Some(0.3);
        let machine = MachineConfig {
            mode: crate::cam::setup::MachineMode::Laser,
            ..MachineConfig::default()
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine,
            tools: vec![tool],
            operations: vec![Op {
                id: 1,
                name: "Laser cut".into(),
                enabled: true,
                kind: OpKind::Engrave {
                    contour: crate::project::ContourParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("G4 P0.3"),
            "expected pierce dwell G4 P0.3 before cut:\n{}",
            resp.gcode
        );
    }

    /// Non-laser tools never get the pierce dwell even if
    /// `laser_pierce_sec` is somehow set (e.g. legacy projects).
    #[test]
    fn non_laser_tool_ignores_pierce_field() {
        let mut tool = endmill(1, 3.0);
        // Endmill kind, but pierce field set (shouldn't fire).
        tool.laser_pierce_sec = Some(0.5);
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            !resp.gcode.contains("G4 P0.5"),
            "endmill should ignore laser_pierce_sec:\n{}",
            resp.gcode
        );
    }

    /// Per-tool Z shift (rt1.30): when set on the first op's tool, a
    /// `G92 Z<shift>` line follows `program_begin` to pin work-Z=0 to
    /// the new tool's tip.
    #[test]
    fn first_tool_z_shift_emits_g92_after_program_begin() {
        let mut tool = endmill(1, 3.0);
        tool.z_shift_mm = Some(-0.5);
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.gcode.contains("G92 Z-0.5"),
            "expected G92 Z-0.5 for tool z_shift:\n{}",
            resp.gcode
        );
    }

    /// Zero / unset `z_shift` emits no G92 (rt1.30 fallback).
    #[test]
    fn no_z_shift_emits_no_g92() {
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            !resp.gcode.contains("G92 Z"),
            "no G92 Z expected when z_shift_mm is unset:\n{}",
            resp.gcode
        );
    }

    /// Comma decimal separator (rt1.36) makes the `LinuxCNC` post emit
    /// `X1,5` instead of `X1.5`. Activated via `MachineConfig`.
    #[test]
    fn comma_decimal_separator_emits_commas_in_numbers() {
        let machine = MachineConfig {
            decimal_separator: ',',
            ..MachineConfig::default()
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.5, 0.5),
            machine,
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // At least one coordinate with a fractional part survives in
        // the gcode (e.g. `X-1,5` from offsetting the 20-mm box).
        assert!(
            resp.gcode
                .lines()
                .any(|l| l.contains(',') && (l.starts_with("G0") || l.starts_with("G1"))),
            "expected at least one comma-decimal in a coordinate line:\n{}",
            resp.gcode
        );
        // No '.' inside coordinate words (allowing '.' in '; OP' lines
        // is fine since post.raw bypasses the formatter).
        for l in resp.gcode.lines() {
            assert!(
                !((l.starts_with("G0 ") || l.starts_with("G1 ")) && l.contains('.')),
                "decimal '.' leaked into a coordinate line under comma-mode: {l}"
            );
        }
    }

    /// Line numbering (rt1.36): when `MachineConfig.line_number_start` is
    /// Some(10), every emitted line gets `N10`, `N20`, … prefix.
    #[test]
    fn line_numbering_prefixes_every_line() {
        let machine = MachineConfig {
            line_number_start: Some(10),
            ..MachineConfig::default()
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine,
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let lines: Vec<&str> = resp.gcode.lines().collect();
        // First non-empty line should have N10; subsequent N20, N30, ...
        let mut expected = 10u32;
        let mut found_count = 0;
        for l in &lines {
            if l.is_empty() {
                continue;
            }
            assert!(
                l.starts_with(&format!("N{expected} ")),
                "expected line to start with 'N{expected} ', got: {l}\nFull:\n{}",
                resp.gcode
            );
            expected += 10;
            found_count += 1;
        }
        assert!(found_count > 3, "expected several numbered lines");
    }

    /// No numbering by default (rt1.36 fallback): lines do not get an
    /// N-prefix when `MachineConfig.line_number_start` is None.
    #[test]
    fn no_line_numbering_by_default() {
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // No line should start with N\d+\s.
        for l in resp.gcode.lines() {
            assert!(
                !(l.starts_with('N') && l.chars().nth(1).is_some_and(|c| c.is_ascii_digit())),
                "unexpected N-prefix: {l}"
            );
        }
    }

    /// Chamfer op (rt1.18): walks the source contour at constant Z,
    /// computed from the V-bit cone math. A 60° V-bit + 1mm width
    /// gives ~1.732 mm depth; the gcode must contain Z-1.732.
    #[test]
    fn chamfer_op_emits_constant_z_pass_at_computed_depth() {
        let vbit = vbit();
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![vbit],
            operations: vec![Op {
                id: 1,
                name: "Chamfer".into(),
                enabled: true,
                kind: OpKind::Chamfer {
                    width_mm: 1.0,
                    finish_pass: false,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        // Cone depth: 1 / tan(30°) ≈ 1.7320508; the gcode rounds to
        // 4 decimals so we look for Z-1.732.
        assert!(
            resp.gcode.contains("Z-1.732"),
            "expected chamfer depth Z-1.732 in gcode:\n{}",
            resp.gcode
        );
    }

    /// Chamfer with `finish_pass=true` emits the source path twice —
    /// once at rough feed, once tagged `is_finish` so the finish-set
    /// feed wins. Verified by counting how many times the contour's
    /// starting move appears (= number of passes through the path).
    #[test]
    fn chamfer_finish_pass_emits_second_pass_at_finish_feed() {
        let mut vbit = vbit();
        vbit.feed_rate = 1200;
        vbit.feed_rate_finish = Some(400);
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![vbit],
            operations: vec![Op {
                id: 1,
                name: "Chamfer".into(),
                enabled: true,
                kind: OpKind::Chamfer {
                    width_mm: 1.0,
                    finish_pass: true,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.gcode.contains("F1200"), "rough feed missing");
        assert!(resp.gcode.contains("F400"), "finish feed missing");
    }

    /// Chamfer on a non-V-bit tool emits a warning so the user knows
    /// the cone math is approximate.
    #[test]
    fn chamfer_with_non_vbit_warns() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Chamfer".into(),
                enabled: true,
                kind: OpKind::Chamfer {
                    width_mm: 1.0,
                    finish_pass: false,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(resp.warnings.iter().any(|w| w.kind == "chamfer_non_vbit"));
    }

    /// `Op.finish_tool_id` round-trips through serde and is
    /// omitted from the wire payload when None.
    #[test]
    fn operation_finish_tool_id_serde_round_trip() {
        let mut op = pocket_op(1, 5, OpSource::All);
        op.finish_tool_id = Some(9);
        let json = serde_json::to_string(&op).unwrap();
        assert!(json.contains("finish_tool_id"));
        let back: Op = serde_json::from_str(&json).unwrap();
        assert_eq!(back.finish_tool_id, Some(9));

        let none_op = pocket_op(1, 5, OpSource::All);
        let json_none = serde_json::to_string(&none_op).unwrap();
        assert!(!json_none.contains("finish_tool_id"));
    }

    /// `PocketParams.finish_xy_allowance_mm` round-trips through
    /// serde and omits the field when unset (rt1.24).
    #[test]
    fn finish_xy_allowance_serde_round_trip() {
        let pocket = crate::project::PocketParams {
            finish_xy_allowance_mm: Some(0.3),
            ..crate::project::PocketParams::default()
        };
        let json = serde_json::to_string(&pocket).unwrap();
        assert!(json.contains("finish_xy_allowance_mm"));
        let back: crate::project::PocketParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.finish_xy_allowance_mm, Some(0.3));
        let none_pocket = crate::project::PocketParams::default();
        let json_none = serde_json::to_string(&none_pocket).unwrap();
        assert!(!json_none.contains("finish_xy_allowance_mm"));
    }

    /// Tool round-trips through serde with the new finish/drill fields
    /// (rt1.27). Empty overrides serialize as omitted entries.
    #[test]
    fn tool_entry_serde_round_trip_with_finish_and_drill_overrides() {
        let mut t = endmill(1, 3.0);
        t.speed_finish = Some(12_000);
        t.feed_rate_finish = Some(400);
        t.plunge_rate_drill = Some(50);
        t.default_peck_step_mm = Some(1.5);
        let json = serde_json::to_string(&t).unwrap();
        let back: ToolEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.speed_finish, Some(12_000));
        assert_eq!(back.feed_rate_finish, Some(400));
        assert_eq!(back.plunge_rate_drill, Some(50));
        assert_eq!(back.default_peck_step_mm, Some(1.5));
        // Unset finish/drill overrides round-trip as None and don't
        // appear in the serialized form.
        assert!(back.speed_drill.is_none());
        assert!(!json.contains("speed_drill"));
    }

    // ─── Lead-in / lead-out (p31) ──────────────────────────────────────

    #[test]
    fn unknown_post_processor_is_a_deserialization_failure() {
        let raw = serde_json::json!({
            "project": {
                "segments": [],
                "machine": { "unit": "mm", "mode": "mill", "comments": true,
                             "arcs": true, "supports_toolchange": false },
                "tools": [],
                "operations": []
            },
            "post_processor": "robotic_arm"
        });
        let res: Result<PipelineRequest, _> = serde_json::from_value(raw);
        assert!(res.is_err());
    }

    #[test]
    fn generate_streaming_emits_op_events_in_order() {
        let project = Project {
            segments: closed_square(20.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![
                profile_op(1, 1, ToolOffset::Outside),
                profile_op(2, 1, ToolOffset::Inside),
                profile_op(3, 1, ToolOffset::On),
            ],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let cancel = CancelToken::new();
        let mut events: Vec<PipelineEvent> = Vec::new();
        let resp = generate_streaming(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            &cancel,
            &mut |e| events.push(e),
        )
        .expect("streaming pipeline ran");

        let mut started: Vec<u32> = Vec::new();
        let mut completed: Vec<u32> = Vec::new();
        let mut done_count = 0usize;
        for ev in &events {
            match ev {
                PipelineEvent::OpStarted { op_id, .. } => started.push(*op_id),
                PipelineEvent::OpCompleted { op_id, .. } => completed.push(*op_id),
                PipelineEvent::Done { .. } => done_count += 1,
                PipelineEvent::Cancelled => panic!("unexpected Cancelled event"),
                PipelineEvent::OpProgress { .. } => {}
            }
        }
        assert_eq!(
            started,
            vec![1, 2, 3],
            "OpStarted fires once per op in order"
        );
        assert_eq!(
            completed,
            vec![1, 2, 3],
            "OpCompleted fires once per op in order"
        );
        assert_eq!(done_count, 1, "exactly one Done event at the end");
        assert!(!resp.gcode.is_empty());
    }

    #[test]
    fn generate_streaming_done_event_carries_aggregated_stats() {
        let project = Project {
            segments: closed_square(20.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let cancel = CancelToken::new();
        let mut last: Option<PipelineEvent> = None;
        let resp = generate_streaming(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            &cancel,
            &mut |e| last = Some(e),
        )
        .expect("streaming pipeline ran");
        match last {
            Some(PipelineEvent::Done {
                total_time_s,
                op_count,
            }) => {
                assert!((total_time_s - resp.time_estimate.total_s).abs() < 1e-9);
                assert_eq!(op_count, resp.stats.offset_count);
            }
            other => panic!("expected Done event last, got {other:?}"),
        }
    }

    #[test]
    fn generate_streaming_cancellation() {
        // V-Carve a triangle on a background thread; from the main
        // thread set the cancel flag immediately. We expect the
        // streaming run to bail with Err(Cancelled) and emit a
        // Cancelled event within ≤200 ms.
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        let project = Project {
            segments: vec![
                Segment::line(Point2::new(0.0, 0.0), Point2::new(20.0, 0.0), "0", 7),
                Segment::line(
                    Point2::new(20.0, 0.0),
                    Point2::new(10.0, 17.320_508),
                    "0",
                    7,
                ),
                Segment::line(Point2::new(10.0, 17.320_508), Point2::new(0.0, 0.0), "0", 7),
            ],
            machine: MachineConfig::default(),
            tools: vec![vbit()],
            operations: vec![Op {
                id: 9,
                name: "Carve".into(),
                enabled: true,
                kind: OpKind::VCarve {
                    carve: crate::project::VCarveParams::default(),
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams {
                    depth: -10.0,
                    start_depth: 0.0,
                    step: Some(-1.0),
                    fast_move_z: 5.0,
                    ..OpParams::default()
                },
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let cancel = CancelToken::new();
        let cancel_clone = cancel.clone();
        let events: Arc<Mutex<Vec<PipelineEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        let request = PipelineRequest {
            project,
            post_processor: Some(PostProcessorKind::Linuxcnc),
        };
        cancel_clone.cancel();
        let start = Instant::now();
        let result = std::thread::spawn(move || {
            generate_streaming(request, &cancel_clone, &mut |e| {
                events_clone.lock().unwrap().push(e);
            })
        })
        .join()
        .expect("worker thread panicked");
        let elapsed = start.elapsed();
        assert!(
            matches!(result, Err(PipelineError::Cancelled)),
            "expected Err(Cancelled), got {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(200),
            "cancellation took too long: {elapsed:?}"
        );
        let evs = events.lock().unwrap();
        assert!(
            evs.iter().any(|e| matches!(e, PipelineEvent::Cancelled)),
            "expected a Cancelled event in stream, got {evs:?}"
        );
        assert!(
            !evs.iter().any(|e| matches!(e, PipelineEvent::Done { .. })),
            "should not emit Done after Cancelled",
        );
    }

    fn collect_cached_flags(project: Project) -> Vec<(u32, bool)> {
        let cancel = CancelToken::new();
        let mut flags: Vec<(u32, bool)> = Vec::new();
        let _ = generate_streaming(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            &cancel,
            &mut |e| {
                if let PipelineEvent::OpCompleted { op_id, cached } = e {
                    flags.push((op_id, cached));
                }
            },
        )
        .expect("pipeline ran");
        flags
    }

    /// Generating twice with no edits should serve every op from cache
    /// on the second run.
    #[test]
    fn regenerate_with_no_edits_hits_cache() {
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(91, 3.0)],
            operations: vec![Op {
                id: 91,
                name: "Profile cache test".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 91,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        clear_pipeline_cache();
        let first = collect_cached_flags(project.clone());
        assert_eq!(first, vec![(91, false)], "first run misses cache");
        let second = collect_cached_flags(project);
        assert_eq!(second, vec![(91, true)], "second run hits cache");
    }

    /// Editing one op of many should miss only that op; the others
    /// should still hit the cache.
    #[test]
    fn edit_one_op_misses_only_that() {
        // Five profile ops, distinct tool ids so each gets its own
        // cache slot regardless of segments (they all share the same
        // square geometry).
        let tools: Vec<ToolEntry> = (1..=5).map(|i| endmill(100 + i, 3.0)).collect();
        let ops: Vec<Op> = (1..=5)
            .map(|i| Op {
                id: 100 + i,
                name: format!("Profile {i}"),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 100 + i,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            })
            .collect();
        let mut project = Project {
            segments: closed_square_offset(30.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools,
            operations: ops,
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        clear_pipeline_cache();
        let first = collect_cached_flags(project.clone());
        assert!(
            first.iter().all(|(_, c)| !c),
            "first run should miss every op: {first:?}"
        );
        // Edit op 3's depth — only it should miss on the second run.
        project.operations[2].params.depth -= 0.1;
        let second = collect_cached_flags(project);
        let edited_id = 100 + 3;
        let expected: Vec<(u32, bool)> = (1..=5)
            .map(|i| (100 + i as u32, (100 + i) != edited_id))
            .collect();
        assert_eq!(second, expected, "only op {edited_id} should miss");
    }

    /// Cache hit must reproduce the same gcode + toolpath as a fresh
    /// run. Asserted by clearing the cache, running once, then running
    /// again with the cache primed.
    #[test]
    fn cache_hit_produces_identical_response() {
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(77, 3.0)],
            operations: vec![Op {
                id: 77,
                name: "Profile identity".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                    contour: crate::project::ContourParams::default(),
                    profile: crate::project::ProfileParams::default(),
                },
                tool_id: 77,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        clear_pipeline_cache();
        let req = || PipelineRequest {
            project: project.clone(),
            post_processor: Some(PostProcessorKind::Linuxcnc),
        };
        let r1 = run_pipeline(req(), |_, _, _| {}).expect("first run");
        let r2 = run_pipeline(req(), |_, _, _| {}).expect("cached run");
        assert_eq!(r1.gcode, r2.gcode, "gcode must match across cache hit");
        assert_eq!(
            r1.toolpath.len(),
            r2.toolpath.len(),
            "toolpath segment count must match"
        );
        assert_eq!(r1.stats.offset_count, r2.stats.offset_count);
        assert_eq!(r1.stats.closed_object_count, r2.stats.closed_object_count);
    }

    #[test]
    fn missing_tool_returns_structured_error() {
        let project = project_with(
            vec![profile_op(1, 99, ToolOffset::Outside)],
            vec![endmill(7, 3.0)],
        );
        let err = run_pipeline(
            PipelineRequest {
                project: project.clone(),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .expect_err("missing tool should fail");
        let structured = err
            .to_structured(Some(&project))
            .expect("UnknownTool should lift to a structured Error");
        assert_eq!(structured.kind, crate::errors::ErrorKind::Misconfigured);
        match structured.auto_fix {
            Some(crate::errors::AutoFix::AssignTool {
                op_id,
                suggested_tool_id,
            }) => {
                assert_eq!(op_id, 1);
                assert_eq!(suggested_tool_id, 7);
            }
            other => panic!("expected AssignTool auto_fix, got {other:?}"),
        }
        assert!(structured.recovery_hint.is_some());
    }

    #[test]
    fn unsupported_op_kind_returns_structured_error() {
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.kind = OpKind::Helix;
        let project = project_with(vec![op], vec![endmill(1, 3.0)]);
        let err = run_pipeline(
            PipelineRequest {
                project: project.clone(),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .expect_err("Thread op should fail");
        let structured = err
            .to_structured(Some(&project))
            .expect("UnimplementedKind should lift to a structured Error");
        assert_eq!(structured.kind, crate::errors::ErrorKind::Unsupported);
    }

    #[test]
    fn cancelled_lifts_to_none() {
        let err = PipelineError::Cancelled;
        assert!(err.to_structured(None).is_none());
    }
}
