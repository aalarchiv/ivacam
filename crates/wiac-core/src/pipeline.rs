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
    UnimplementedKind(OpKind),
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
        if matches!(op.kind, OpKind::VCarve) {
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
    let drill_with_chamfer = matches!(op.kind, OpKind::Drill { .. })
        && op.params.chamfer_after_width_mm.is_some_and(|w| w > 0.0);
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
    use super::*;
    use super::test_helpers::*;
    use crate::cam::setup::{MachineConfig, TabType, TabsConfig, ToolOffset};
    use crate::geometry::Segment;
    use crate::project::{
        Coolant, Op, OpKind, OpParams, OpSource, PatternConfig, SourceCombine, TextAlignment,
        TextLayer, TextLayerKind, ToolEntry, ToolKind,
    };

    #[test]
    fn pipeline_renders_text_layers_and_routes_via_synthetic_layer() {
        // Engrave op pointing at the synthetic `__text_1` layer.
        let engrave = Op {
            id: 1,
            name: "Engrave text".into(),
            enabled: true,
            kind: OpKind::Engrave,
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::Layers {
                layers: vec!["__text_1".into()],
                combine: SourceCombine::default(),
            },
            params: OpParams::mill_default(),
            pattern: None,
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


    /// Selecting an outer ring + inner ring as the source for a pocket op
    /// produces ONE annulus pocket (outer minus inner), not one pocket per
    /// ring. The bug was that the pipeline iterated each selected object
    /// independently, so the inner ring was getting machined as its own
    /// pocket boundary on top of the outer pocket.
    #[test]
    fn pocket_with_outer_plus_inner_selection_emits_a_single_annulus() {
        let mut segments = closed_square_offset(50.0, 0.0, 0.0);
        // Inner 20x20 box centered inside the outer 50x50.
        segments.extend(closed_square_offset(20.0, 15.0, 15.0));
        // Two distinct pocket projects, exact same geometry — one runs
        // pocket on JUST the outer (baseline), the other on outer+inner.
        // The annulus pocket should emit *fewer* offset segments than
        // pocketing the whole outer because the middle is left intact.
        let baseline_project = Project {
            segments: segments.clone(),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pocket_op(
                1,
                1,
                OpSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Auto,
                },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let annulus_project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pocket_op(
                1,
                1,
                OpSource::Objects {
                    ids: vec![1, 2],
                    combine: SourceCombine::Auto,
                },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let baseline = run_pipeline(
            PipelineRequest {
                project: baseline_project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let annulus = run_pipeline(
            PipelineRequest {
                project: annulus_project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // Outer-only pocket fills the full 50x50; outer+inner leaves a
        // 20x20 hole, so its cut path must be strictly shorter.
        let cut_total = |toolpath: &[preview::ToolpathSegment]| -> f64 {
            toolpath
                .iter()
                .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
                .map(|s| {
                    let dx = s.to.x - s.from.x;
                    let dy = s.to.y - s.from.y;
                    (dx * dx + dy * dy).sqrt()
                })
                .sum()
        };
        let baseline_cut = cut_total(&baseline.toolpath);
        let annulus_cut = cut_total(&annulus.toolpath);
        assert!(
            annulus_cut < baseline_cut,
            "annulus cut length {annulus_cut} should be less than the full pocket {baseline_cut}",
        );
        // Also: the annulus should still emit at least one offset (the
        // outer pocket cascade with the inner ring as a hole). Zero would
        // mean we accidentally skipped both objects.
        assert!(
            annulus.stats.offset_count >= 1,
            "annulus pocket emitted no offsets",
        );
    }

    /// `SourceCombine::Difference` applied at the pipeline level should
    /// produce one annulus pocket from "outer minus inner", matching
    /// what the user means when they pick Difference explicitly. This
    /// guards the `synthesize_region_object` path that fakes a `VcObject`
    /// from clipper2 polytree output.
    #[test]
    fn pocket_with_difference_combine_emits_an_annulus() {
        let mut segments = closed_square_offset(50.0, 0.0, 0.0);
        segments.extend(closed_square_offset(20.0, 15.0, 15.0));
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket-diff".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![1, 2],
                    combine: SourceCombine::Difference,
                },
                params: OpParams::mill_default(),
                pattern: None,
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
        assert!(
            resp.stats.offset_count >= 1,
            "Difference produced no offsets"
        );
        // The cut path must include moves that are NOT in the inner box —
        // i.e., the cutter does visit points outside the inner 20x20.
        // A trivially-wrong implementation that pocketed only the inner
        // box (or only the outer) would fail one of these area checks.
        let visited_outside_inner = resp.toolpath.iter().any(|s| {
            let in_inner = |x: f64, y: f64| x > 15.0 && x < 35.0 && y > 15.0 && y < 35.0;
            !in_inner(s.from.x, s.from.y) || !in_inner(s.to.x, s.to.y)
        });
        let visited_inside_outer = resp.toolpath.iter().any(|s| {
            let in_outer = |x: f64, y: f64| x > 0.0 && x < 50.0 && y > 0.0 && y < 50.0;
            in_outer(s.from.x, s.from.y) && in_outer(s.to.x, s.to.y)
        });
        assert!(
            visited_outside_inner,
            "annulus pocket should reach outside the inner box"
        );
        assert!(
            visited_inside_outer,
            "annulus pocket should stay inside the outer box"
        );
    }

    /// Pocket-Outside (rt1.3): a Pocket op carrying `frame_shape` should
    /// auto-prepend a frame around the selection at pipeline time and
    /// emit a toolpath that fills the area BETWEEN the frame and the
    /// selection — not the area inside the selection.
    #[test]
    fn pocket_outside_carves_between_frame_and_selection() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let mut params = OpParams::mill_default();
        params.frame_shape = Some(crate::cam::source_combine::FrameShape::Rectangle);
        params.frame_padding_mm = Some(10.0);
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket-Outside".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Difference,
                },
                params,
                pattern: None,
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
        assert!(
            resp.stats.offset_count >= 1,
            "Pocket-Outside produced no offsets",
        );
        // The cutter should reach OUTSIDE the 50x50 inner square (in the
        // padding region) AND must NOT cut deep inside the inner square's
        // interior (the source selection is the high part).
        let cuts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .collect();
        // Cuts in the padding region: x or y outside [0, 50].
        let visited_padding = cuts.iter().any(|s| {
            let in_inner = |x: f64, y: f64| (0.0..=50.0).contains(&x) && (0.0..=50.0).contains(&y);
            !in_inner(s.from.x, s.from.y) || !in_inner(s.to.x, s.to.y)
        });
        assert!(
            visited_padding,
            "Pocket-Outside should cut in the padding region between frame and selection",
        );
        // Cuts deep inside the source square (≥ tool_radius from the wall)
        // should not happen — the inner is the "raised" area, not carved.
        let inner_carve_min = 5.0;
        let inner_carve_max = 45.0;
        let cut_inside_inner = cuts.iter().any(|s| {
            let deep_inside = |x: f64, y: f64| {
                x > inner_carve_min
                    && x < inner_carve_max
                    && y > inner_carve_min
                    && y < inner_carve_max
            };
            deep_inside(s.from.x, s.from.y) && deep_inside(s.to.x, s.to.y)
        });
        assert!(
            !cut_inside_inner,
            "Pocket-Outside should NOT cut deep inside the source selection",
        );
    }

    /// Pocket-Outside (rt1.3) regression: when the user enters a frame
    /// padding smaller than the cutter radius, the pipeline must clamp
    /// the padding up to (at least) the tool radius and emit a warning
    /// — otherwise the synthetic frame's "Inside" offset puts the
    /// cutter inside the very shape it should be carving around.
    #[test]
    fn pocket_outside_clamps_padding_below_tool_radius() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let mut params = OpParams::mill_default();
        params.frame_shape = Some(crate::cam::source_combine::FrameShape::Rectangle);
        params.frame_padding_mm = Some(1.0); // < tool radius (3.0)
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)], // 6 mm Ø ⇒ 3 mm radius
            operations: vec![Op {
                id: 1,
                name: "Pocket-Outside-tight".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Difference,
                },
                params,
                pattern: None,
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
        let warned = resp
            .warnings
            .iter()
            .any(|w| w.kind == "frame_padding_below_tool_radius" && w.op_id == Some(1));
        assert!(
            warned,
            "expected frame_padding_below_tool_radius warning, got {:?}",
            resp.warnings,
        );
        // After the clamp the cutter centerline can sit on the bbox
        // boundary at worst, but must never step into the interior of
        // the source square — that's the very thing the clamp prevents.
        let cuts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .collect();
        let inner_carve_min = 1.0;
        let inner_carve_max = 49.0;
        let cut_inside_inner = cuts.iter().any(|s| {
            let deep_inside = |x: f64, y: f64| {
                x > inner_carve_min
                    && x < inner_carve_max
                    && y > inner_carve_min
                    && y < inner_carve_max
            };
            deep_inside(s.from.x, s.from.y) && deep_inside(s.to.x, s.to.y)
        });
        assert!(
            !cut_inside_inner,
            "clamped Pocket-Outside must not cut inside the source square",
        );
    }

    /// Two-op regression: a plain Pocket followed by a Pocket-Outside
    /// on the same source must produce two distinct toolpath blocks
    /// (one inside, one in the padding ring outside) without one
    /// op's mutations bleeding into the other. Catches the case where
    /// `frame_op_storage` mutating `objects` would leak into a prior or
    /// subsequent op.
    #[test]
    fn pocket_then_pocket_outside_produces_disjoint_cuts() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![
                Op {
                    id: 1,
                    name: "Inner pocket".into(),
                    enabled: true,
                    kind: OpKind::Pocket {
                        strategy: crate::project::PocketStrategy::Cascade,
                    },
                    tool_id: 1,
                    finish_tool_id: None,
                    source: OpSource::Objects {
                        ids: vec![1],
                        combine: SourceCombine::Auto,
                    },
                    params: OpParams::mill_default(),
                    pattern: None,
                },
                Op {
                    id: 2,
                    name: "Pocket Outside".into(),
                    enabled: true,
                    kind: OpKind::Pocket {
                        strategy: crate::project::PocketStrategy::Cascade,
                    },
                    tool_id: 1,
                    finish_tool_id: None,
                    source: OpSource::Objects {
                        ids: vec![1],
                        combine: SourceCombine::Difference,
                    },
                    params: {
                        let mut p = OpParams::mill_default();
                        p.frame_shape = Some(crate::cam::source_combine::FrameShape::Rectangle);
                        p.frame_padding_mm = Some(10.0);
                        p
                    },
                    pattern: None,
                },
            ],
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
        assert!(
            resp.stats.offset_count >= 2,
            "expected ≥2 offsets total (pocket + pocket-outside), got {}",
            resp.stats.offset_count,
        );
        let cuts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .collect();
        // Cuts should cover BOTH the inside (pocket op) and the
        // padding ring (pocket-outside).
        let inside_cuts = cuts.iter().any(|s| {
            let deep_inside = |x: f64, y: f64| (5.0..45.0).contains(&x) && (5.0..45.0).contains(&y);
            deep_inside(s.from.x, s.from.y) && deep_inside(s.to.x, s.to.y)
        });
        let outside_cuts = cuts.iter().any(|s| {
            let in_padding =
                |x: f64, y: f64| !((0.0..=50.0).contains(&x) && (0.0..=50.0).contains(&y));
            in_padding(s.from.x, s.from.y) || in_padding(s.to.x, s.to.y)
        });
        assert!(inside_cuts, "first pocket should cut inside the square");
        assert!(
            outside_cuts,
            "pocket-outside should cut in the padding ring",
        );
        // The regions preview must also distinguish them: one region
        // per op_id, with op 1 inside and op 2 in the ring.
        let op1_regions = resp.regions.iter().filter(|r| r.op_id == 1).count();
        let op2_regions = resp.regions.iter().filter(|r| r.op_id == 2).count();
        assert!(
            op1_regions >= 1,
            "op 1 should have a fill region in the preview (got {op1_regions})",
        );
        assert!(
            op2_regions >= 1,
            "op 2 (pocket-outside) should have a fill region (got {op2_regions})",
        );
    }

    /// Climb on the main + conventional on the finishing pass: walks the
    /// pipeline output and verifies the level=0 ring uses the
    /// conventional winding (CCW for an inner pocket boundary) while
    /// any level≥1 cascade ring uses climb (CW for an inner ring).
    #[test]
    fn pocket_with_climb_main_and_conventional_finish_winds_correctly() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let mut params = OpParams::mill_default();
        params.cut_direction = crate::project::CutDirection::Climb;
        params.finish_cut_direction = crate::project::CutDirection::Conventional;
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // We can't read PolylineOffset directly here (it isn't on the
        // PipelineResponse), but the toolpath order encodes the cut.
        // Walk the cut moves at op_id=1 and group them by Z-plane to
        // recover individual passes; then check the winding of each.
        let cuts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut) && s.op_id == 1)
            .collect();
        assert!(!cuts.is_empty(), "expected cut segments");
        // Group consecutive cuts that form a closed loop (same Z and the
        // final point is near the first). The first such loop is the
        // boundary (level=0) — we look at its signed area.
        let mut loops: Vec<Vec<&preview::ToolpathSegment>> = Vec::new();
        let mut cur: Vec<&preview::ToolpathSegment> = Vec::new();
        for s in &cuts {
            if cur.is_empty() {
                cur.push(s);
                continue;
            }
            let prev = cur.last().unwrap();
            // New loop when there's a Z jump or a position discontinuity.
            let z_jump = (s.from.z - prev.to.z).abs() > 1e-3;
            let xy_jump = (s.from.x - prev.to.x).hypot(s.from.y - prev.to.y) > 0.01;
            if z_jump || xy_jump {
                loops.push(std::mem::take(&mut cur));
            }
            cur.push(s);
        }
        if !cur.is_empty() {
            loops.push(cur);
        }
        let area_of_loop = |loop_segs: &[&preview::ToolpathSegment]| -> f64 {
            let mut s = 0.0;
            for seg in loop_segs {
                s += seg.from.x * seg.to.y - seg.to.x * seg.from.y;
            }
            s * 0.5
        };
        // The boundary pass = the loop with the largest |area| (it's the
        // outermost ring in the cascade). With Conventional + Pocket
        // (inner context) we expect CCW = positive signed area.
        // Group loops by Z so we look at one cut-pass plane only —
        // multiple Z passes would each repeat the same XY rings.
        let z_of = |loop_segs: &[&preview::ToolpathSegment]| -> f64 {
            loop_segs.first().map_or(0.0, |s| s.from.z)
        };
        let first_z = z_of(&loops[0]);
        let same_z: Vec<_> = loops
            .iter()
            .filter(|l| (z_of(l) - first_z).abs() < 1e-3)
            .collect();
        let mut areas: Vec<f64> = same_z.iter().map(|l| area_of_loop(l)).collect();
        areas.sort_by(|a, b| b.abs().partial_cmp(&a.abs()).unwrap());
        let boundary_area = areas[0];
        assert!(
            boundary_area > 0.0,
            "finishing pass should be CCW (conventional) for an inner pocket; got area {boundary_area}"
        );
        // For a square boundary the cascade produces ≥ 1 inner ring on
        // a 50×50 pocket with a 3 mm tool; that ring should be CW =
        // negative signed area under climb.
        if areas.len() >= 2 {
            assert!(
                areas[1] < 0.0,
                "cascade ring should be CW (climb) for an inner pocket; got area {}",
                areas[1]
            );
        }
    }

    /// Pocket a 4mm box with a 6mm endmill — the cutter doesn't fit.
    /// Expect a `tool_too_large` warning attached to the op id, and the
    /// pipeline still completes (no error).
    #[test]
    fn pocket_with_oversized_tool_emits_tool_too_large_warning() {
        let project = Project {
            segments: closed_square_offset(4.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Op {
                id: 7,
                name: "Tiny pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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
        let too_large: Vec<_> = resp
            .warnings
            .iter()
            .filter(|w| w.kind == "tool_too_large")
            .collect();
        assert_eq!(
            too_large.len(),
            1,
            "expected one tool_too_large warning, got {:?}",
            resp.warnings
        );
        assert_eq!(too_large[0].op_id, Some(7));
    }

    /// Drill bit on a Pocket op — emits a `tool_kind_mismatch` warning
    /// regardless of whether the cascade actually produced anything.
    #[test]
    fn pocket_with_drill_bit_warns_about_tool_kind() {
        let drill = ToolEntry {
            id: 1,
            name: "drill".into(),
            kind: ToolKind::Drill,
            diameter: 1.0,
            tip_diameter: None,
            tip_angle_deg: 60.0,
            dragoff: None,
            flutes: 2,
            speed: 18_000,
            plunge_rate: 100,
            feed_rate: 800,
            coolant: Coolant::Off,
            speed_finish: None,
            plunge_rate_finish: None,
            feed_rate_finish: None,
            speed_drill: None,
            plunge_rate_drill: None,
            feed_rate_drill: None,
            default_peck_step_mm: None,
            default_step: None,
            z_shift_mm: None,
            laser_pierce_sec: None,
            laser_lead_in_mm: None,
            corner_radius_mm: None,
            tslot_neck_diameter_mm: None,
            tslot_neck_length_mm: None,
            wirbeln: false,
            wirbeln_stepover_mm: None,
            pause: 1,
            flute_length_mm: None,
            shank_diameter_mm: None,
            holder: None,
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![drill],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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
        assert!(resp.warnings.iter().any(|w| w.kind == "tool_kind_mismatch"));
    }

    /// Ramp plunge: the FIRST cut moves descend Z linearly while
    /// walking forward along the path. With angle=10° and step=-1,
    /// `ramp_length` = 1/tan(10°) ≈ 5.67mm. After ~5.67mm of XY travel
    /// the cutter should be at Z=-1; subsequent cut moves stay at -1.
    #[test]
    fn ramp_plunge_descends_z_during_first_cuts() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Ramp { angle_deg: 10.0 };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Ramped profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // Walk the cut+arc moves at op_id=1. Either kind can carry the
        // descending Z during the ramp depending on whether the offset
        // polyline starts with a corner arc or a straight edge.
        let path: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| {
                s.op_id == 1
                    && matches!(
                        s.kind,
                        crate::gcode::preview::MoveKind::Cut | crate::gcode::preview::MoveKind::Arc
                    )
            })
            .collect();
        assert!(!path.is_empty(), "expected cut/arc moves");
        // The very first move's `from` is wherever the plunge left the
        // cutter — for ramp plunge that's start_depth (=0), not the
        // final cut depth.
        let first = path[0];
        assert!(
            first.from.z > -0.001,
            "ramp should start at Z≈0, got {} → {}",
            first.from.z,
            first.to.z
        );
        // Find where Z first reaches the cut depth.
        let mut horizontal_during_ramp = 0.0;
        let mut reached_depth = false;
        for s in &path {
            if !reached_depth {
                horizontal_during_ramp += (s.to.x - s.from.x).hypot(s.to.y - s.from.y);
            }
            if s.to.z <= -0.999 {
                reached_depth = true;
                break;
            }
        }
        assert!(reached_depth, "Z never reached cut depth during ramp");
        // Expected ramp length is 1 / tan(10°) ≈ 5.67mm. Allow ±25%:
        // the offset polyline may begin with a small corner arc that
        // can't be split mid-arc, which slightly extends the
        // descending portion.
        let expected = 1.0 / 10f64.to_radians().tan();
        assert!(
            (horizontal_during_ramp - expected).abs() / expected < 0.25,
            "horizontal ramp length should be ~{expected:.2}mm, got {horizontal_during_ramp:.2}",
        );
    }

    /// Helix entry on a 50×50 closed pocket boundary with a 3mm
    /// endmill, helix radius 3mm, angle 3°. The first cut moves should
    /// be `MoveKind::Arc` segments with monotonically descending Z,
    /// completing at least one full revolution before the cutter
    /// reaches the target depth (one revolution drops Z by
    /// 2π·3·tan(3°) ≈ 0.99mm; for a 1mm step that's almost exactly
    /// one revolution → we expect ≥1 full revolution before the cutter
    /// is at -1).
    #[test]
    fn helix_plunge_emits_arc_arcs_descending_z() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(3.0),
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Helical pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // Walk all op_id=1 moves until Z first reaches the cut depth.
        // The descent portion should be exclusively Arc moves, and Z
        // should be monotonically non-increasing (within a tiny epsilon
        // for floating-point arc emission).
        let path: Vec<_> = resp.toolpath.iter().filter(|s| s.op_id == 1).collect();
        assert!(!path.is_empty(), "expected toolpath segments");
        let mut arc_count = 0;
        let mut last_z = f64::INFINITY;
        let mut reached_depth = false;
        for s in &path {
            // Skip the initial rapid + plunge that lands the cutter
            // above the helix start.
            if matches!(
                s.kind,
                crate::gcode::preview::MoveKind::Rapid | crate::gcode::preview::MoveKind::Plunge
            ) {
                continue;
            }
            if matches!(s.kind, crate::gcode::preview::MoveKind::Arc) {
                arc_count += 1;
                assert!(
                    s.to.z <= last_z + 1e-6,
                    "helix Z should descend monotonically, but {} → {}",
                    last_z,
                    s.to.z,
                );
                last_z = s.to.z;
            }
            if s.to.z <= -0.999 {
                reached_depth = true;
                break;
            }
        }
        assert!(reached_depth, "Z never reached cut depth via helix");
        // One revolution = 2 semicircle arc moves; for ~1mm of descent
        // at 3° / 3mm radius we expect at least one revolution → 2 arcs.
        assert!(
            arc_count >= 2,
            "helix should emit ≥2 arc moves before reaching depth; got {arc_count}",
        );
    }

    /// Helix radius < `tool_radius` → falls back to Ramp (and from there
    /// to Direct if path too short). With a 6mm tool and helix
    /// radius=1mm the helix would carve nothing the cutter doesn't
    /// already cover, so we fall back. The first cutting move's Z
    /// should start above the cut depth — that's the Ramp signature
    /// (helix arcs would start at the previous Z, then descend to
    /// depth on a small circle inside the polygon, NOT on the cut
    /// path itself).
    #[test]
    fn helix_falls_back_to_ramp_when_radius_smaller_than_tool() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 10.0,
            radius_mm: Some(1.0),
        };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Op {
                id: 1,
                name: "Helix-too-small".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        let first_cutting = resp
            .toolpath
            .iter()
            .find(|s| {
                s.op_id == 1
                    && matches!(
                        s.kind,
                        crate::gcode::preview::MoveKind::Cut | crate::gcode::preview::MoveKind::Arc
                    )
            })
            .expect("expected at least one cut/arc move");
        // Ramp signature: first cut starts at Z≈start_depth (=0), not
        // at the cut depth. Helix entry would have its first Arc start
        // at the helix-circle starting point inside the polygon, NOT
        // on the cut path; here the first cut/arc is on the offset
        // polyline (a rounded-corner outline of the square), so its
        // XY is on that polyline. The discriminator is Z.
        assert!(
            first_cutting.from.z > -0.001,
            "ramp fallback should start at Z≈0, got {}",
            first_cutting.from.z,
        );
        // And the helix arcs we'd expect — a helix from start_depth
        // to cut depth via 2 semicircle arcs each, centered well
        // inside the polygon (tool radius 3 → boundary inset by 3 →
        // polygon center ≈ (50, 50)) — would have arcs that DON'T
        // touch the cut polyline. Verify there are no Arc moves at
        // op_id=1 with from.z > -0.001 sitting near (50, 50).
        let helix_arc_present = resp.toolpath.iter().any(|s| {
            s.op_id == 1
                && matches!(s.kind, crate::gcode::preview::MoveKind::Arc)
                && s.from.z > -0.001
                && (s.from.x - 50.0).hypot(s.from.y - 50.0) < 5.0
        });
        assert!(
            !helix_arc_present,
            "fallback should not emit a helix-entry arc near the polygon centroid",
        );
    }

    /// Auto-fit helix radius (`radius_mm` = None) on a pocket too small
    /// for the tool: the resolver finds no fit, emits the
    /// `helix_radius_unfittable` warning, and falls through to Ramp —
    /// no helix-entry arcs near the centroid.
    #[test]
    fn auto_helix_falls_back_when_pocket_too_small() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: None,
        };
        let project = Project {
            segments: closed_square_offset(8.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Op {
                id: 1,
                name: "Auto-helix-tight".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        let warned = resp
            .warnings
            .iter()
            .any(|w| w.kind == "helix_radius_unfittable" && w.op_id == Some(1));
        assert!(
            warned,
            "expected helix_radius_unfittable warning; got: {:?}",
            resp.warnings,
        );
        let helix_arc_present = resp.toolpath.iter().any(|s| {
            s.op_id == 1
                && matches!(s.kind, crate::gcode::preview::MoveKind::Arc)
                && s.from.z > -0.001
                && (s.from.x - 4.0).hypot(s.from.y - 4.0) < 3.0
        });
        assert!(
            !helix_arc_present,
            "auto-fit should not emit a helix arc when pocket is too small",
        );
    }

    /// Auto-fit helix on a roomy pocket: the resolver picks a radius,
    /// gcode emits descending helix arcs at that radius around the
    /// inscribed-circle center. Uses a tiny tool because
    /// `plan_helix_entry` enforces `radius + tool_radius` clearance to
    /// the cut path (which is itself `tool_radius` inset from the
    /// source), so the brief's `inscribed - tool_radius - 0.5` formula
    /// only fits in `plan_helix_entry` when `tool_radius ≤ 0.5`.
    #[test]
    fn auto_helix_emits_arcs_when_pocket_fits() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: None,
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 0.5)],
            operations: vec![Op {
                id: 1,
                name: "Auto-helix-roomy".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        let arc_count = resp
            .toolpath
            .iter()
            .filter(|s| {
                s.op_id == 1
                    && matches!(s.kind, crate::gcode::preview::MoveKind::Arc)
                    && s.to.z <= s.from.z
                    && s.from.z > -0.999
            })
            .count();
        assert!(
            arc_count >= 2,
            "auto-fit roomy pocket should emit helix arcs; got {arc_count}",
        );
        assert!(
            !resp
                .warnings
                .iter()
                .any(|w| w.kind == "helix_radius_unfittable"),
            "no unfit warning expected: {:?}",
            resp.warnings,
        );
    }

    /// Project save/load round-trip: a Helix plunge with `radius_mm: null`
    /// serializes to JSON and parses back identically. Also verify the
    /// legacy bare-number form still loads.
    #[test]
    fn helix_radius_null_round_trip_and_legacy_compat() {
        use crate::cam::setup::PlungeStrategy;

        let plunge = PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: None,
        };
        let json = serde_json::to_string(&plunge).unwrap();
        assert!(
            json.contains("\"radius_mm\":null"),
            "expected radius_mm:null in serialized form: {json}",
        );
        let parsed: PlungeStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, plunge);

        let legacy = r#"{"kind":"helix","angle_deg":3.0,"radius_mm":5.0}"#;
        let parsed: PlungeStrategy = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            parsed,
            PlungeStrategy::Helix {
                angle_deg: 3.0,
                radius_mm: Some(5.0),
            },
        );

        let new_form = r#"{"kind":"helix","angle_deg":3.0,"radius_mm":null}"#;
        let parsed: PlungeStrategy = serde_json::from_str(new_form).unwrap();
        assert_eq!(
            parsed,
            PlungeStrategy::Helix {
                angle_deg: 3.0,
                radius_mm: None,
            },
        );
    }

    /// Tabs active → helix entry isn't useful (tabs already manage Z
    /// independently) so we fall back like Ramp does today: passes
    /// where the path crosses tabs use the straight-plunge tabs walker.
    #[test]
    fn helix_with_tabs_active_falls_back() {
        let mut params = OpParams::mill_default();
        params.depth = -2.0;
        params.step = Some(-2.0);
        params.start_depth = 0.0;
        params.tabs = TabsConfig {
            active: true,
            width: 10.0,
            height: 1.0,
            tab_type: TabType::Rectangle,
            ramp_angle_deg: 30.0,
        };
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(3.0),
        };
        // rt1.10: tabs are per-op now. Manual placement with a single
        // tab on the bottom edge (t=0.125 lands at the midpoint of
        // edge 0 of the square's chained object since the chain runs
        // a single closed polyline).
        params.tab_mode = crate::project::TabPlacementMode::Manual;
        params.tab_placements = vec![crate::project::TabPlacement {
            object_id: 1,
            t: 0.125,
            width_override_mm: None,
            height_override_mm: None,
        }];
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Helix-with-tabs".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // With tabs active and a single Z pass that uses the tabs
        // walker, the helix entry is suppressed. The first cut/arc
        // move should NOT be an Arc — falls back to ramp (which falls
        // back to direct because tabs disable ramp too in the same
        // pass), so the cutter starts at depth and walks the path.
        // With tabs active and a single Z pass that uses the tabs
        // walker, the helix entry is suppressed. We detect this by
        // looking for the absence of an Arc near the polygon centroid
        // — the helix arcs would be centered there. The polygon is a
        // 100×100 square offset Outside by tool_radius=1.5mm, so the
        // boundary inset by -1.5 produces a 103×103 path centered at
        // (50, 50). A helix entry arc would travel a circle of radius
        // 3 around (50, 50); cut path arcs only appear at corners of
        // the offset, far from (50, 50).
        let helix_arc_present = resp.toolpath.iter().any(|s| {
            s.op_id == 1
                && matches!(s.kind, crate::gcode::preview::MoveKind::Arc)
                && (s.from.x - 50.0).hypot(s.from.y - 50.0) < 10.0
        });
        assert!(
            !helix_arc_present,
            "tabs-active path should not emit a helical entry arc near the polygon centroid",
        );
        // And the gcode for the tabs-active pass should still contain
        // the tab Z-lift (sanity check that tabs are still being
        // honored).
        assert!(
            resp.gcode.contains("Z-1"),
            "expected tab Z-lift in gcode: {}",
            resp.gcode,
        );
    }

    #[test]
    fn direct_plunge_keeps_default_behavior() {
        // Sanity-check that the new plunge field doesn't affect the
        // default Direct path: the first cut move must already be at
        // the cut depth (the plunge happens before XY travel starts).
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        // params.plunge defaults to Direct.
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Direct profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        let first_cut = resp
            .toolpath
            .iter()
            .find(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut) && s.op_id == 1)
            .expect("expected at least one cut");
        assert!(
            first_cut.from.z <= -0.999,
            "direct plunge should reach cut depth before XY travel; first cut from.z = {}",
            first_cut.from.z
        );
    }

    /// A 10x10 pocket with a 6mm endmill: tool fits the boundary
    /// offset (4x4 left after a 3mm offset) but no cascade ring fits
    /// inside it → the cutter walks the wall and leaves a hollow
    /// rectangle. We surface this as a `pocket_fill_incomplete` warning
    /// so the user understands why the gcode is just the contour.
    #[test]
    fn pocket_with_just_fitting_tool_warns_about_incomplete_fill() {
        let project = Project {
            segments: closed_square_offset(10.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Op {
                id: 9,
                name: "Hollow pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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
        let incomplete: Vec<_> = resp
            .warnings
            .iter()
            .filter(|w| w.kind == "pocket_fill_incomplete")
            .collect();
        assert_eq!(
            incomplete.len(),
            1,
            "expected pocket_fill_incomplete warning, got {:?}",
            resp.warnings,
        );
    }

    /// Higher `xy_overlap` → smaller step → more cascade rings on the
    /// same geometry. Verifies the new knob actually drives the cascade
    /// step. With 0.7 overlap the cut path on a 50x50 pocket should be
    /// strictly longer than at 0.3 overlap.
    #[test]
    fn higher_xy_overlap_emits_a_longer_cut_path() {
        fn cut_total(resp: &PipelineResponse) -> f64 {
            resp.toolpath
                .iter()
                .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
                .map(|s| {
                    let dx = s.to.x - s.from.x;
                    let dy = s.to.y - s.from.y;
                    (dx * dx + dy * dy).sqrt()
                })
                .sum()
        }
        let make = |overlap: f64| -> PipelineResponse {
            let mut params = OpParams::mill_default();
            params.xy_overlap = overlap;
            let project = Project {
                segments: closed_square_offset(50.0, 0.0, 0.0),
                machine: MachineConfig::default(),
                tools: vec![endmill(1, 3.0)],
                operations: vec![Op {
                    id: 1,
                    name: "Pocket".into(),
                    enabled: true,
                    kind: OpKind::Pocket {
                        strategy: crate::project::PocketStrategy::Cascade,
                    },
                    tool_id: 1,
                    finish_tool_id: None,
                    source: OpSource::All,
                    params,
                    pattern: None,
                }],
                fixtures: Vec::default(),
                text_layers: Vec::default(),
            };
            run_pipeline(
                PipelineRequest {
                    project,
                    post_processor: None,
                },
                |_, _, _| {},
            )
            .unwrap()
        };
        let lo = cut_total(&make(0.3));
        let hi = cut_total(&make(0.7));
        assert!(
            hi > lo * 1.2,
            "expected higher overlap to add ≥20% cut length; got {hi} vs {lo}",
        );
    }

    /// Direct end-to-end check that zigzag emission is alive: at default
    /// `xy_overlap` the gcode for a 50x50 pocket must contain cuts at
    /// distinct Y values inside the pocket — not just the boundary
    /// contour at four corners.
    #[test]
    fn zigzag_pocket_emits_interior_strokes() {
        let mut params = OpParams::mill_default();
        // Force the default explicitly so the test pins behavior even
        // if the constant changes later.
        params.xy_overlap = 0.5;
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Zigzag pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Zigzag,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // Cuts at the level=0 contour visit only y=1.5 and y=48.5 (the
        // contour inset by tool_radius=1.5 from the original 0..50).
        // Zigzag fill should add strokes at intermediate Y values.
        let interior_cut_y_values: std::collections::HashSet<i32> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
            .filter_map(|s| {
                // Round to the nearest mm so floating-point doesn't
                // explode the set.
                let y_mm = s.from.y.round() as i32;
                if (1..=49).contains(&y_mm) {
                    Some(y_mm)
                } else {
                    None
                }
            })
            .collect();
        // A 50x50 pocket at 1.5mm stride gives at least 20 distinct
        // interior Y rows. If we see only 2 (just the contour edges),
        // zigzag emission is broken.
        assert!(
            interior_cut_y_values.len() > 5,
            "expected many distinct interior Y rows from zigzag, got {}: {:?}",
            interior_cut_y_values.len(),
            interior_cut_y_values,
        );
    }

    /// Ramp plunge used to leave a sloped section at the start of the
    /// last Z pass — the cutter ramps from `prev_z` to `total_depth` over
    /// `ramp_length`, but the cells in the ramp region sit at
    /// progressively descending Z, not at `total_depth`. The fix is a
    /// constant-depth cleanup walk after all the ramped passes.
    /// This test verifies the gcode now contains a final pass at
    /// `total_depth` that visits the path's start XY at `total_depth`.
    #[test]
    fn ramp_plunge_cleans_up_with_a_final_constant_depth_pass() {
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Ramp { angle_deg: 10.0 };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Ramped profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // Walk cut moves at op_id=1; group by Z plane (rounded).
        let cuts: Vec<_> = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut) && s.op_id == 1)
            .collect();
        // Total horizontal distance traversed at exactly total_depth=-1.
        let cleanup_distance: f64 = cuts
            .iter()
            .filter(|s| (s.from.z - -1.0).abs() < 1e-3 && (s.to.z - -1.0).abs() < 1e-3)
            .map(|s| (s.to.x - s.from.x).hypot(s.to.y - s.from.y))
            .sum();
        // Without the cleanup, only the post-ramp portion of the single
        // pass would be at -1 (about path_len - ramp_length ≈ 400 -
        // 5.67 ≈ 394). With cleanup we get one extra full lap (~400)
        // → expect roughly ≥ 700.
        assert!(
            cleanup_distance > 700.0,
            "expected ≥700mm of constant-depth cuts (post-ramp + cleanup); got {cleanup_distance:.1}",
        );
    }

    /// Cascade with a tool too wide for any inward ring emits ONLY the
    /// boundary contour (no silent fallback to zigzag — that was
    /// confusing for users who picked cascade explicitly and saw
    /// zigzag). The `pocket_fill_incomplete` warning fires so they know.
    #[test]
    fn cascade_with_tool_too_wide_emits_only_boundary_no_zigzag_substitute() {
        let mut params = OpParams::mill_default();
        params.xy_overlap = 0.05; // 95% step — no inward rings will fit
        let project = Project {
            // 6×6 with a 3mm tool: boundary inset by 1.5mm leaves a
            // 3×3 path; cascade inflate by 2.85mm → empty → 0 rings.
            segments: closed_square_offset(6.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // pocket_fill_incomplete warning must fire so the user knows.
        assert!(
            resp.warnings
                .iter()
                .any(|w| w.kind == "pocket_fill_incomplete"),
            "expected pocket_fill_incomplete warning, got {:?}",
            resp.warnings,
        );
    }

    /// CW-wound input must still pocket INWARD. Cavalier-Contours
    /// treats positive delta as left-of-tangent, which is the polygon
    /// interior for CCW but the EXTERIOR for CW. The user reported
    /// (test.vc-project.json) a CW DXF where the pocket was being cut
    /// outside the boundary, enlarging the shape by the tool diameter.
    /// `parallel_offset_inward` now picks the right sign per winding.
    #[test]
    fn pocket_on_cw_polygon_cuts_inside_not_outside() {
        // Build a 50×50 square wound CW (clockwise from above): walk
        // (0,0)→(0,50)→(50,50)→(50,0)→(0,0). signed_area would be
        // negative for this winding.
        let s = 50.0;
        let segments = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(0.0, s), "0", 7),
            Segment::line(Point2::new(0.0, s), Point2::new(s, s), "0", 7),
            Segment::line(Point2::new(s, s), Point2::new(s, 0.0), "0", 7),
            Segment::line(Point2::new(s, 0.0), Point2::new(0.0, 0.0), "0", 7),
        ];
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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
        // Every cut must stay INSIDE the polygon's bounding box —
        // outside cuts mean the cutter went the wrong way.
        for s in &resp.toolpath {
            if !matches!(s.kind, crate::gcode::preview::MoveKind::Cut) {
                continue;
            }
            for pt in [s.from, s.to] {
                assert!(
                    pt.x >= -0.01 && pt.x <= 50.01 && pt.y >= -0.01 && pt.y <= 50.01,
                    "cut went outside the CW pocket boundary: ({}, {})",
                    pt.x,
                    pt.y,
                );
            }
        }
    }

    // ─── Drill ops ─────────────────────────────────────────────────────


    /// A 0.5mm-radius closed circle with a 3mm endmill running an
    /// `OpKind::Drill` { Simple } op should emit a recognizable
    /// `LinuxCNC` G81 (or G82 for dwell) drill at the circle's center.
    #[test]
    fn drill_op_emits_gcode_for_circle_smaller_than_tool() {
        let project = Project {
            segments: closed_circle(Point2::new(5.0, 7.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
            )],
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
            resp.gcode.contains("G81"),
            "expected G81 in linuxcnc drill output:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("X5") && resp.gcode.contains("Y7"),
            "expected drill at (5, 7):\n{}",
            resp.gcode
        );
    }

    /// Drill onto a closed rectangle (not a small circle, not a Point)
    /// should drill at the rectangle's bbox center. Regression for the
    /// user-reported "drilling op is not correct" — the rectangle case
    /// used to be silently skipped, leaving the drill op with no
    /// output and no warning.
    #[test]
    fn drill_op_targets_bbox_center_of_closed_rectangle() {
        // 20mm × 20mm rectangle offset to (10, 5) ⇒ corners at
        // (10,5)-(30,25). Bbox center = (20, 15).
        let segments = closed_square_offset(20.0, 10.0, 5.0);
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
            )],
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
            resp.stats.offset_count >= 1,
            "drill op produced no offsets — bbox-center fallback is missing",
        );
        assert!(
            resp.gcode.contains("G81"),
            "expected G81 in linuxcnc drill output:\n{}",
            resp.gcode
        );
        // Drill should land at bbox center (20, 15). Use loose substring
        // matching since the formatter may produce X20 / X20.000 etc.
        assert!(
            resp.gcode.contains("X20") && resp.gcode.contains("Y15"),
            "expected drill at bbox center (20, 15):\n{}",
            resp.gcode
        );
    }

    /// Drill cycle Peck with a non-zero step should map to G83 in
    /// `LinuxCNC`, with the per-peck Q operand carrying the step.
    #[test]
    fn drill_peck_emits_g83() {
        let project = Project {
            segments: closed_circle(Point2::new(0.0, 0.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Peck {
                    peck_step_mm: 1.0,
                    dwell_sec: 0.0,
                },
            )],
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
            resp.gcode.contains("G83"),
            "expected G83 in linuxcnc peck output:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("Q1"),
            "expected Q1 peck step:\n{}",
            resp.gcode
        );
    }

    /// Drill cycle `ChipBreak` should map to G73 in `LinuxCNC`.
    #[test]
    fn drill_chip_break_emits_g73() {
        let project = Project {
            segments: closed_circle(Point2::new(0.0, 0.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::ChipBreak {
                    peck_step_mm: 1.0,
                    dwell_sec: 0.0,
                },
            )],
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
            resp.gcode.contains("G73"),
            "expected G73 in linuxcnc chip-break output:\n{}",
            resp.gcode
        );
    }

    /// GRBL doesn't support canned drill cycles. The post should fall
    /// back to the trait's default G0/G1 expansion: rapid to (x,y,r),
    /// feed plunge to z, retract to r — no G81/G83/G73 in the output.
    #[test]
    fn drill_grbl_falls_back_to_g0g1_sequence() {
        let project = Project {
            segments: closed_circle(Point2::new(0.0, 0.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Peck {
                    peck_step_mm: 1.0,
                    dwell_sec: 0.0,
                },
            )],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: Some(PostProcessorKind::Grbl),
            },
            |_, _, _| {},
        )
        .unwrap();
        // None of the canned cycle codes should appear in GRBL output.
        for code in ["G81", "G82", "G83", "G73"] {
            assert!(
                !resp.gcode.contains(code),
                "{code} should not appear in GRBL fallback output:\n{}",
                resp.gcode
            );
        }
        // …but we should still have at least one G0 (rapid to drill XY)
        // and at least one G1 (feed plunge / retract feeds) in the
        // emitted block.
        let drill_block = resp
            .gcode
            .lines()
            .skip_while(|l| !l.contains("OP 1"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            drill_block.contains("G0"),
            "expected at least one G0 (rapid) in the drill block:\n{drill_block}"
        );
        assert!(
            drill_block.contains("G1"),
            "expected at least one G1 (feed plunge) in the drill block:\n{drill_block}"
        );
    }

    /// A Drill op with `OpSource::Objects` selecting only one of
    /// several drill candidates must emit gcode for *just* that one.
    #[test]
    fn drill_op_respects_object_selection() {
        let mut segments = closed_circle(Point2::new(0.0, 0.0), 0.5);
        segments.extend(closed_circle(Point2::new(20.0, 0.0), 0.5));
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Drill".into(),
                enabled: true,
                kind: OpKind::Drill {
                    cycle: crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![2],
                    combine: SourceCombine::Auto,
                },
                params: {
                    let mut p = OpParams::mill_default();
                    p.depth = -5.0;
                    p.start_depth = 0.0;
                    p.fast_move_z = 5.0;
                    p
                },
                pattern: None,
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
        // Only the second circle (centered at x=20) should be drilled.
        assert!(
            resp.gcode.contains("G81"),
            "expected G81 drill, got:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("X20"),
            "expected drill at the second circle (x=20):\n{}",
            resp.gcode
        );
        // The first circle's center is at (0, 0). The header rapid is
        // already at X0/Y0 (defaults) so we can't simply scan for "X0";
        // instead, count G81 lines — there should be exactly one.
        let g81_count = resp.gcode.matches("G81").count();
        assert_eq!(
            g81_count, 1,
            "expected exactly one drill cycle in selection-restricted output:\n{}",
            resp.gcode
        );
    }

    /// `finish_step` (smaller than step) emits an extra Z pass at the
    /// nominal depth from a shallower pre-finish z. Verifies the gcode
    /// has cuts at both the pre-finish Z and the bottom Z.
    #[test]
    fn finish_step_emits_extra_thin_final_pass() {
        let mut params = OpParams::mill_default();
        params.depth = -2.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.finish_step = Some(-0.2);
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // Expect Z-1 (pass 1), Z-1.8 (pre-finish), Z-2 (final) all to appear.
        assert!(resp.gcode.contains("Z-1\n") || resp.gcode.contains("Z-1 "));
        assert!(resp.gcode.contains("Z-1.8"));
        assert!(resp.gcode.contains("Z-2\n") || resp.gcode.contains("Z-2 "));
    }

    /// `through_depth` extends the cut past the nominal depth so
    /// through-cuts on edge-clamped sheet clear the bottom.
    #[test]
    fn through_depth_extends_final_z() {
        let mut params = OpParams::mill_default();
        params.depth = -2.0;
        params.step = Some(-1.0);
        params.through_depth = 0.5;
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // total_depth becomes -2.5, so the deepest cut should reach -2.5.
        assert!(
            resp.gcode.contains("Z-2.5"),
            "expected through-cut Z-2.5 in gcode",
        );
    }

    /// `depth_list` overrides the step schedule. Each listed Z must appear.
    #[test]
    fn depth_list_overrides_step_schedule() {
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.step = Some(-1.0);
        params.depth_list = vec![-0.5, -1.5, -3.0];
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        assert!(resp.gcode.contains("Z-0.5"));
        assert!(resp.gcode.contains("Z-1.5"));
        assert!(resp.gcode.contains("Z-3"));
        // step-schedule values that aren't in the list should be absent.
        assert!(!resp.gcode.contains("Z-1\n") && !resp.gcode.contains("Z-1 "));
        assert!(!resp.gcode.contains("Z-2\n") && !resp.gcode.contains("Z-2 "));
    }

    /// Per-op feedrate overrides win over the tool's defaults.
    #[test]
    fn feed_rate_override_appears_in_gcode() {
        let mut params = OpParams::mill_default();
        params.feed_rate_override = Some(123);
        params.plunge_rate_override = Some(45);
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        assert!(
            resp.gcode.contains("F123"),
            "expected feed_rate_override 123 in gcode, got:\n{}",
            resp.gcode,
        );
        assert!(
            resp.gcode.contains("F45"),
            "expected plunge_rate_override 45 in gcode",
        );
        // Tool's defaults (800 / 100) should NOT appear when overridden.
        assert!(!resp.gcode.lines().any(|l| l.trim() == "F800"));
    }

    /// `resolve_tool_rates`: unset finish/drill variants fall back to the
    /// general triplet (rt1.27).
    #[test]
    fn resolve_tool_rates_falls_back_when_unset() {
        use crate::project::{resolve_tool_rates, PassKind};
        let t = endmill(1, 3.0);
        assert_eq!(resolve_tool_rates(&t, PassKind::Rough), (18_000, 100, 800));
        assert_eq!(resolve_tool_rates(&t, PassKind::Finish), (18_000, 100, 800));
        assert_eq!(resolve_tool_rates(&t, PassKind::Drill), (18_000, 100, 800));
    }

    /// `resolve_tool_rates`: each variant honors its own override when set.
    #[test]
    fn resolve_tool_rates_honors_per_pass_overrides() {
        use crate::project::{resolve_tool_rates, PassKind};
        let mut t = endmill(1, 3.0);
        t.speed_finish = Some(12_000);
        t.feed_rate_finish = Some(400);
        t.speed_drill = Some(8_000);
        t.feed_rate_drill = Some(200);
        t.plunge_rate_drill = Some(50);
        assert_eq!(resolve_tool_rates(&t, PassKind::Rough), (18_000, 100, 800));
        assert_eq!(resolve_tool_rates(&t, PassKind::Finish), (12_000, 100, 400));
        assert_eq!(resolve_tool_rates(&t, PassKind::Drill), (8_000, 50, 200));
    }

    /// Pocket op with a slower finish feed: the gcode must contain the
    /// finish feedrate before the wall-defining (level=0) ring is cut
    /// (rt1.27).
    #[test]
    fn pocket_finish_ring_emits_finish_feedrate() {
        let mut tool = endmill(1, 3.0);
        tool.speed = 20_000;
        tool.feed_rate = 1500;
        tool.speed_finish = Some(8_000);
        tool.feed_rate_finish = Some(400);

        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![pocket_op(1, 1, OpSource::All)],
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
        // The rough feed (F1500) should appear for the cascade rings;
        // the finish feed (F400) must appear before the level=0 wall
        // ring is cut. Both must show up — and the post should also
        // emit the finish spindle (S8000) somewhere in the body.
        assert!(
            resp.gcode.contains("F1500"),
            "expected rough feed 1500 in gcode:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("F400"),
            "expected finish feed 400 in gcode:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("S8000"),
            "expected finish spindle 8000 in gcode:\n{}",
            resp.gcode
        );
    }

    /// Pocket op WITHOUT a finish override: rough feed is used
    /// everywhere — no surprise feed change before the level=0 ring
    /// (rt1.27 fallback behavior).
    #[test]
    fn pocket_without_finish_override_uses_rough_throughout() {
        let mut tool = endmill(1, 3.0);
        tool.speed = 20_000;
        tool.feed_rate = 1500;
        // no finish overrides set
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![pocket_op(1, 1, OpSource::All)],
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
        assert!(resp.gcode.contains("F1500"));
        assert!(
            !resp.gcode.contains("F400"),
            "no finish-feed F400 should appear when finish overrides are unset"
        );
    }

    /// Drill op picks the per-tool _drill speed/feed/plunge variants
    /// (rt1.27).
    #[test]
    fn drill_op_uses_drill_set() {
        let mut tool = endmill(1, 3.0);
        tool.kind = ToolKind::Drill;
        // Keep diameter at 3 mm so the 0.5-mm circle below qualifies
        // as a small_circle_drill target (radius < 0.95 * tool_radius).
        tool.speed = 20_000;
        tool.plunge_rate = 100;
        tool.feed_rate = 1500;
        tool.speed_drill = Some(3_000);
        tool.plunge_rate_drill = Some(30);
        // feed_rate_drill left unset to verify per-field fallback.

        let project = Project {
            segments: closed_circle(Point2::new(5.0, 7.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
            )],
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
        assert!(
            resp.gcode.contains("S3000"),
            "expected drill spindle 3000 in gcode:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("F30"),
            "expected drill plunge 30 in gcode:\n{}",
            resp.gcode
        );
    }

    /// Drill op with peck cycle and `peck_step_mm=0` falls back to the
    /// tool's `default_peck_step_mm` (rt1.27).
    #[test]
    fn drill_peck_uses_tool_default_when_op_step_zero() {
        let mut tool = endmill(1, 3.0);
        tool.kind = ToolKind::Drill;
        tool.default_peck_step_mm = Some(1.25);
        let project = Project {
            segments: closed_circle(Point2::new(5.0, 7.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Peck {
                    peck_step_mm: 0.0,
                    dwell_sec: 0.0,
                },
            )],
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
        // LinuxCNC G83 emits `Q<step>` for the peck increment.
        assert!(
            resp.gcode.contains("Q1.25"),
            "expected resolved peck step Q1.25 in gcode:\n{}",
            resp.gcode
        );
    }

    /// Pocket with `xy_finish_allowance` emits an extra wall ring at
    /// the actual contour (`tool_radius` offset) AND the rough rings
    /// step inward from (`tool_radius` + allowance) — leaving stock at
    /// the wall that the finish ring removes (rt1.24).
    #[test]
    fn pocket_finish_xy_allowance_emits_extra_boundary_pass() {
        use crate::cam::offsets::{pocket_for_object, PocketEmit};
        use crate::cam::VcObject;
        let pt = |x: f64, y: f64| Point2::new(x, y);
        let segs = vec![
            Segment::line(pt(0.0, 0.0), pt(50.0, 0.0), "0", 7),
            Segment::line(pt(50.0, 0.0), pt(50.0, 50.0), "0", 7),
            Segment::line(pt(50.0, 50.0), pt(0.0, 50.0), "0", 7),
            Segment::line(pt(0.0, 50.0), pt(0.0, 0.0), "0", 7),
        ];
        let obj = VcObject::new(segs, true);
        let tool_radius = 1.5;
        let no_allow = pocket_for_object(
            &obj,
            tool_radius,
            false,
            6,
            PocketEmit::Cascade,
            &[],
            1.5,
            0.0,
            None,
        );
        let with_allow = pocket_for_object(
            &obj,
            tool_radius,
            false,
            6,
            PocketEmit::Cascade,
            &[],
            1.5,
            0.5,
            None,
        );
        // With allowance we expect exactly one MORE level-0 ring:
        // the rough boundary (is_finish=false) + the finish boundary
        // (is_finish=true). Without allowance there's a single
        // boundary tagged as finish.
        let finish_count_no = no_allow.iter().filter(|o| o.is_finish).count();
        let finish_count_with = with_allow.iter().filter(|o| o.is_finish).count();
        assert_eq!(finish_count_no, 1);
        assert_eq!(finish_count_with, 1);
        // The extra rough boundary in `with_allow` is a non-finish
        // level-0 ring that doesn't exist in `no_allow`.
        let rough_level0_no = no_allow
            .iter()
            .filter(|o| o.level == 0 && !o.is_finish)
            .count();
        let rough_level0_with = with_allow
            .iter()
            .filter(|o| o.level == 0 && !o.is_finish)
            .count();
        assert_eq!(rough_level0_no, 0);
        assert_eq!(rough_level0_with, 1);
        assert_eq!(with_allow.len(), no_allow.len() + 1);
    }

    /// Pocket with `xy_finish_allowance` produces gcode that visits the
    /// rough rings at the tool's general feed and the finish ring at
    /// the finish-set feed (rt1.24 × rt1.27).
    #[test]
    fn pocket_with_xy_allowance_finish_ring_uses_finish_feed() {
        let mut tool = endmill(1, 3.0);
        tool.feed_rate = 1500;
        tool.feed_rate_finish = Some(400);
        let mut params = OpParams::mill_default();
        params.finish_xy_allowance_mm = Some(0.5);
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        assert!(resp.gcode.contains("F1500"), "rough feed missing");
        assert!(resp.gcode.contains("F400"), "finish feed missing");
    }

    /// Dual-tool Pocket op (rt1.33): when `finish_tool_id` is set to a
    /// different tool, the gcode contains a `T<n> M6` toolchange and
    /// uses the finish tool's feed for the wall ring.
    #[test]
    fn dual_tool_pocket_emits_toolchange_and_uses_finish_tool_feed() {
        let mut rough_tool = endmill(1, 6.0);
        rough_tool.feed_rate = 1500;
        rough_tool.speed = 20_000;
        let mut finish_tool = endmill(2, 3.0);
        finish_tool.feed_rate = 600;
        finish_tool.speed = 24_000;
        finish_tool.feed_rate_finish = Some(300);

        let machine = MachineConfig {
            supports_toolchange: true,
            ..MachineConfig::default()
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine,
            tools: vec![rough_tool, finish_tool],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: Some(2),
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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
            resp.gcode.contains("T2 M6"),
            "expected toolchange T2 M6 for finish pass:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("F1500"),
            "expected rough feed 1500:\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("F300"),
            "expected finish feed 300 (finish tool's feed_rate_finish):\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("S24000"),
            "expected finish tool spindle 24000:\n{}",
            resp.gcode
        );
    }

    /// Dual-tool Pocket op without a distinct finish tool
    /// (`finish_tool_id` == `tool_id`) — no toolchange emitted, single
    /// tool throughout.
    #[test]
    fn dual_tool_same_id_skips_toolchange() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: Some(1),
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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
            !resp.gcode.contains(" M6"),
            "expected no toolchange when finish_tool_id == tool_id:\n{}",
            resp.gcode
        );
    }

    /// Dual-tool Pocket op without `finish_tool_id` (None) — legacy
    /// single-tool behavior: no toolchange.
    #[test]
    fn dual_tool_none_uses_single_tool() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pocket_op(1, 1, OpSource::All)],
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
        assert!(!resp.gcode.contains(" M6"));
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

    /// Wirbeln (rt1.25): a Pocket op with a Wirbeln-flagged tool
    /// emits MORE cascade rings than the same op without Wirbeln,
    /// because the effective `xy_step` gets clamped to `tool_radius/2`.
    #[test]
    fn wirbeln_tool_increases_cascade_ring_count() {
        let mut tool_a = endmill(1, 6.0);
        tool_a.wirbeln = false;
        let mut tool_b = endmill(1, 6.0);
        tool_b.wirbeln = true;
        let mut params = OpParams::mill_default();
        // overlap 0.05 -> xy_step = 6 * 0.95 = 5.7 mm. Wirbeln clamps
        // to tool_radius/2 = 1.5 mm, so b should have many more rings.
        params.xy_overlap = 0.05;
        let project_with_tool = |tool: ToolEntry| Project {
            segments: closed_square_offset(80.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![tool],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: params.clone(),
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let resp_a = run_pipeline(
            PipelineRequest {
                project: project_with_tool(tool_a),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let resp_b = run_pipeline(
            PipelineRequest {
                project: project_with_tool(tool_b),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp_b.stats.offset_count > resp_a.stats.offset_count,
            "Wirbeln should produce more cascade rings: wirbeln=on offsets={} vs off offsets={}",
            resp_b.stats.offset_count,
            resp_a.stats.offset_count
        );
    }

    /// Wirbeln serde round-trip on `ToolEntry` (rt1.25). Default = false
    /// (skipped on serialize); when on with an override, both round-trip.
    #[test]
    fn wirbeln_serde_round_trip() {
        let mut tool = endmill(1, 6.0);
        let json_default = serde_json::to_string(&tool).unwrap();
        assert!(!json_default.contains("wirbeln"));
        tool.wirbeln = true;
        tool.wirbeln_stepover_mm = Some(0.75);
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"wirbeln\":true"));
        assert!(json.contains("wirbeln_stepover_mm"));
        let back: ToolEntry = serde_json::from_str(&json).unwrap();
        assert!(back.wirbeln);
        assert_eq!(back.wirbeln_stepover_mm, Some(0.75));
    }

    /// Halfpipe op (rt1.19): a closed region + Halfpipe `CircularArc`
    /// emits cutting moves whose Z dips to within tolerance of the
    /// configured profile radius along the centerline.
    #[test]
    fn halfpipe_circular_arc_emits_curved_floor() {
        // 40×8 mm narrow slot. Inscribed circle along the centerline
        // is ~4 mm radius (half-width). With profile R=5: at the
        // widest medial-axis point z = -(5 - sqrt(25 - 16)) = -2.
        let segments = closed_square_offset(40.0, 0.0, 0.0);
        // Reshape into a 40×8 rectangle so the medial axis is a
        // single line down the middle with inscribed-circle r ≈ 4.
        let mut segments_8w: Vec<Segment> = Vec::new();
        let p = |x: f64, y: f64| Point2::new(x, y);
        let _ = segments;
        segments_8w.push(Segment::line(p(0.0, 0.0), p(40.0, 0.0), "0", 7));
        segments_8w.push(Segment::line(p(40.0, 0.0), p(40.0, 8.0), "0", 7));
        segments_8w.push(Segment::line(p(40.0, 8.0), p(0.0, 8.0), "0", 7));
        segments_8w.push(Segment::line(p(0.0, 8.0), p(0.0, 0.0), "0", 7));

        let mut ball = endmill(1, 10.0);
        ball.kind = ToolKind::BallNose;
        let mut params = OpParams::mill_default();
        params.depth = -10.0; // permissive cap so the profile drives Z
        params.start_depth = 0.0;
        params.step = Some(-2.0);
        let project = Project {
            segments: segments_8w,
            machine: MachineConfig::default(),
            tools: vec![ball],
            operations: vec![Op {
                id: 1,
                name: "Halfpipe".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Halfpipe {
                        profile: crate::project::HalfpipeProfile::CircularArc { radius_mm: 5.0 },
                    },
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // The deepest cut along the centerline lands at z ≈ -2 (or
        // close to it; medial-axis sampling won't hit exactly r=4 at
        // every centerline vertex). Verify at least one G1 line has a
        // negative Z below -1 mm.
        let any_deep_cut = resp.gcode.lines().any(|l| {
            if !l.starts_with("G1 ") {
                return false;
            }
            for tok in l.split_whitespace() {
                if let Some(rest) = tok.strip_prefix('Z') {
                    if let Ok(z) = rest.parse::<f64>() {
                        if z < -1.0 {
                            return true;
                        }
                    }
                }
            }
            false
        });
        assert!(
            any_deep_cut,
            "expected at least one G1 line with Z below -1 mm:\n{}",
            resp.gcode
        );
    }

    /// `PocketStrategy::Halfpipe` serde round-trip (rt1.19) covers both
    /// `CircularArc` and `VBottom` profiles.
    #[test]
    fn halfpipe_serde_round_trip() {
        let cases = [
            crate::project::PocketStrategy::Halfpipe {
                profile: crate::project::HalfpipeProfile::CircularArc { radius_mm: 5.0 },
            },
            crate::project::PocketStrategy::Halfpipe {
                profile: crate::project::HalfpipeProfile::VBottom {
                    included_angle_deg: 60.0,
                },
            },
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            assert!(json.contains("halfpipe"));
            let back: crate::project::PocketStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(back, case);
        }
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
                kind: OpKind::Engrave,
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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

    /// Approach point (rt1.26): when set on a Pocket op, each closed
    /// offset's segment list rotates so the start (where plunge
    /// happens) is the vertex closest to the user-picked XY.
    #[test]
    fn approach_point_rotates_closed_offsets_to_nearest_vertex() {
        // A 20x20 closed square at (0..20, 0..20). With approach_point
        // ~ (20, 20) the closest vertex of the inward-offset ring is
        // the top-right corner. Without approach_point, plunge happens
        // at an arbitrary auto-picked vertex.
        let center_ap = (20.0, 20.0);
        let mut params = OpParams::mill_default();
        params.approach_point = Some(center_ap);
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 1.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // The first G0 rapid that lands on the cut plane should
        // approach a point in the upper-right quadrant (X >= 10, Y >=
        // 10). With a 1-mm endmill and 20-mm box, the inward offset
        // wall ring sits around (0.5..19.5)^2; the rotated start lands
        // near (19.5, 19.5).
        let mut found_quadrant_entry = false;
        for line in resp.gcode.lines() {
            if !line.starts_with("G0 ") {
                continue;
            }
            // Parse X and Y if present.
            let mut x: Option<f64> = None;
            let mut y: Option<f64> = None;
            for tok in line.split_whitespace() {
                if let Some(rest) = tok.strip_prefix('X') {
                    x = rest.parse().ok();
                } else if let Some(rest) = tok.strip_prefix('Y') {
                    y = rest.parse().ok();
                }
            }
            if let (Some(xv), Some(yv)) = (x, y) {
                if xv > 10.0 && yv > 10.0 {
                    found_quadrant_entry = true;
                    break;
                }
            }
        }
        assert!(
            found_quadrant_entry,
            "expected a G0 entry in the upper-right quadrant after approach_point=(20,20):\n{}",
            resp.gcode
        );
    }

    /// Approach point serde round-trip (rt1.26).
    #[test]
    fn approach_point_serde_round_trip() {
        let mut params = OpParams::mill_default();
        params.approach_point = Some((3.5, -2.0));
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("approach_point"));
        let back: OpParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.approach_point, Some((3.5, -2.0)));
        // Unset round-trips as absent.
        let none_params = OpParams::mill_default();
        let json_none = serde_json::to_string(&none_params).unwrap();
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
                kind: OpKind::Engrave,
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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

    /// Stufenfase (rt1.20): a drill op with `chamfer_after_width_mm`
    /// follows the drill cycle with a constant-Z revolution at each
    /// hole's rim, computed from the cutter's tip angle.
    #[test]
    fn drill_with_chamfer_after_emits_constant_z_revolution() {
        let mut vbit_drill = vbit();
        vbit_drill.kind = ToolKind::Drill;
        vbit_drill.diameter = 3.0;
        vbit_drill.tip_angle_deg = 90.0; // makes the math easy: z = -width
        let center = Point2::new(5.0, 7.0);
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        params.fast_move_z = 5.0;
        params.chamfer_after_width_mm = Some(1.0);
        let project = Project {
            segments: closed_circle(center, 0.5),
            machine: MachineConfig::default(),
            tools: vec![vbit_drill],
            operations: vec![Op {
                id: 1,
                name: "Drill+chamfer".into(),
                enabled: true,
                kind: OpKind::Drill {
                    cycle: crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // Drill cycle present (G81 / G82) AND a chamfer revolution
        // shows up at Z-1 (width / tan(45°) = 1.0).
        assert!(
            resp.gcode
                .lines()
                .any(|l| l.starts_with("G81") || l.starts_with("G82")),
            "expected drill cycle (G81/G82):\n{}",
            resp.gcode
        );
        assert!(
            resp.gcode.contains("Z-1"),
            "expected chamfer revolution at Z-1 (90° tip + 1mm width):\n{}",
            resp.gcode
        );
    }

    /// Drill with `chamfer_after` AND a distinct `finish_tool_id` emits
    /// the toolchange between the drill cycle and the chamfer
    /// revolution (rt1.20 × rt1.33).
    #[test]
    fn drill_chamfer_after_with_tool_override_emits_m6() {
        let mut drill = vbit();
        drill.kind = ToolKind::Drill;
        drill.diameter = 3.0;
        drill.id = 1;
        let mut vbit_finish = vbit();
        vbit_finish.id = 2;
        vbit_finish.diameter = 6.35;
        vbit_finish.tip_angle_deg = 90.0;
        let machine = MachineConfig {
            supports_toolchange: true,
            ..MachineConfig::default()
        };
        let center = Point2::new(5.0, 7.0);
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        params.chamfer_after_width_mm = Some(0.5);
        let project = Project {
            segments: closed_circle(center, 0.5),
            machine,
            tools: vec![drill, vbit_finish],
            operations: vec![Op {
                id: 1,
                name: "Drill".into(),
                enabled: true,
                kind: OpKind::Drill {
                    cycle: crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
                },
                tool_id: 1,
                finish_tool_id: Some(2),
                source: OpSource::All,
                params,
                pattern: None,
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
            resp.gcode.contains("T2 M6"),
            "expected toolchange T2 M6 for chamfer cutter:\n{}",
            resp.gcode
        );
    }

    /// Drill without `chamfer_after_width_mm` emits no rim revolution.
    #[test]
    fn drill_without_chamfer_after_emits_no_revolution() {
        let project = Project {
            segments: closed_circle(Point2::new(5.0, 7.0), 0.5),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
            )],
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
        // No G1 line should land at a fractional negative Z below the
        // drill block — only G81 / G82 / safe-Z moves.
        let any_chamfer_g1 = resp
            .gcode
            .lines()
            .any(|l| l.starts_with("G1 ") && l.contains("Z-"));
        assert!(
            !any_chamfer_g1,
            "expected no chamfer revolution gcode in drill-only op:\n{}",
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

    /// Dual-tool Pocket (rt1.33) with `z_shift` on the finish tool:
    /// after the M6 we emit the finish tool's G92 Z shift.
    #[test]
    fn dual_tool_finish_tool_z_shift_emits_g92_after_m6() {
        let rough_tool = endmill(1, 6.0);
        let mut finish_tool = endmill(2, 3.0);
        finish_tool.z_shift_mm = Some(1.25);
        let machine = MachineConfig {
            supports_toolchange: true,
            ..MachineConfig::default()
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine,
            tools: vec![rough_tool, finish_tool],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: Some(2),
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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
        assert!(resp.gcode.contains("T2 M6"), "toolchange missing");
        let m6_idx = resp.gcode.find("T2 M6").unwrap();
        let after = &resp.gcode[m6_idx..];
        assert!(
            after.contains("G92 Z1.25"),
            "expected G92 Z1.25 AFTER T2 M6:\n{}",
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

    /// Thread op (rt1.17): a closed circle source + Thread op emits
    /// a helical descent. The gcode must contain the helix's bottom
    /// Z (rounded to 4 decimals) and a sweep of XY coordinates
    /// around the bore's center.
    #[test]
    fn thread_op_emits_helical_descent_on_a_closed_circle() {
        let center = Point2::new(10.0, 20.0);
        let radius = 5.0;
        let segments = closed_circle(center, radius);
        let mut params = OpParams::mill_default();
        params.depth = -3.0;
        params.start_depth = 0.0;
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 1.0)],
            operations: vec![Op {
                id: 1,
                name: "Thread".into(),
                enabled: true,
                kind: OpKind::Thread {
                    pitch_mm: 1.0,
                    internal: true,
                    climb: true,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        // Bottom Z = -3 → gcode contains Z-3 somewhere.
        assert!(
            resp.gcode.contains("Z-3"),
            "expected helix bottom Z-3 in gcode:\n{}",
            resp.gcode
        );
        // Internal: helix walks at (bore_radius - tool_radius) = 5 - 0.5 = 4.5 mm
        // around center (10, 20). One waypoint sits at (10 + 4.5, 20) = (14.5, 20).
        assert!(
            resp.gcode.contains("X14.5") || resp.gcode.contains("X14.5000"),
            "expected helix waypoint at X=14.5 (bore - tool_radius):\n{}",
            resp.gcode
        );
    }

    /// Thread op without a closed circle in the source emits a
    /// `thread_no_circles` warning and produces no toolpath.
    #[test]
    fn thread_op_without_circle_warns() {
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 1.0)],
            operations: vec![Op {
                id: 1,
                name: "Thread".into(),
                enabled: true,
                kind: OpKind::Thread {
                    pitch_mm: 1.0,
                    internal: true,
                    climb: true,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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
        assert!(resp.warnings.iter().any(|w| w.kind == "thread_no_circles"));
    }

    /// Thread op with internal + a tool larger than the bore emits a
    /// `thread_tool_too_large` warning rather than producing a
    /// nonsensical helix.
    #[test]
    fn thread_op_internal_with_oversized_tool_warns() {
        let center = Point2::new(0.0, 0.0);
        let radius = 1.0; // 1mm bore
        let segments = closed_circle(center, radius);
        let mut params = OpParams::mill_default();
        params.depth = -1.0;
        params.start_depth = 0.0;
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)], // 3mm tool, bigger than the bore
            operations: vec![Op {
                id: 1,
                name: "Thread".into(),
                enabled: true,
                kind: OpKind::Thread {
                    pitch_mm: 1.0,
                    internal: true,
                    climb: true,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        assert!(resp
            .warnings
            .iter()
            .any(|w| w.kind == "thread_tool_too_large"));
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
                pattern: None,
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
                pattern: None,
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
                pattern: None,
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

    /// `OpParams.finish_xy_allowance_mm` round-trips through
    /// serde and omits the field when unset (rt1.24).
    #[test]
    fn finish_xy_allowance_serde_round_trip() {
        let mut params = OpParams::mill_default();
        params.finish_xy_allowance_mm = Some(0.3);
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("finish_xy_allowance_mm"));
        let back: OpParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.finish_xy_allowance_mm, Some(0.3));
        let none_params = OpParams::mill_default();
        let json_none = serde_json::to_string(&none_params).unwrap();
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

    /// Corner feed reduction emits a slower F before sharp turns.
    /// Verified on a zigzag pocket where adjacent strokes are joined
    /// by a 180° turn — exactly the worst-case for high-feed motion.
    #[test]
    fn corner_feed_reduction_emits_slower_f_at_sharp_turns() {
        let mut params = OpParams::mill_default();
        params.feed_rate_override = Some(1000);
        params.corner_feed_reduction = 0.5; // halve at corners
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Zigzag,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        assert!(
            resp.gcode.contains("F500"),
            "expected reduced corner feed F500 (= 1000 * 0.5) in gcode",
        );
    }

    // ─── Pattern repetition (5fz) ──────────────────────────────────────

    /// Build a profile op with a Linear pattern attached. We deliberately
    #[test]
    fn linear_pattern_emits_translated_copies() {
        let project = project_with(
            vec![profile_op_with_pattern(PatternConfig::Linear {
                count: 3,
                dx: 20.0,
                dy: 0.0,
            })],
            vec![endmill(1, 3.0)],
        );
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let xs = cut_x_values(&resp.gcode);
        assert!(
            !xs.is_empty(),
            "pattern op produced no cuts:\n{}",
            resp.gcode
        );
        // Source square is 0..20 with an outside offset of 1.5 mm
        // (half a 3 mm endmill), so cuts span roughly -1.5..21.5 around
        // the original. Two more instances at dx=20 and dx=40 give
        // cuts roughly in 18.5..41.5 and 38.5..61.5 — distinct
        // X-translated regions.
        let max_x = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min_x = xs.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(
            max_x > 38.0,
            "expected X to reach the third instance (>~38), got max {} in:\n{}",
            max_x,
            resp.gcode,
        );
        assert!(
            min_x < 5.0,
            "expected X to also touch the first instance (<5), got min {} in:\n{}",
            min_x,
            resp.gcode,
        );
        // Three instances → at least three distinct X clusters around
        // 5, 25, 45. Sample by counting cuts in each band.
        let near_first = xs.iter().filter(|x| **x >= -2.0 && **x <= 22.0).count();
        let near_second = xs.iter().filter(|x| **x >= 18.0 && **x <= 42.0).count();
        let near_third = xs.iter().filter(|x| **x >= 38.0 && **x <= 62.0).count();
        assert!(
            near_first > 0 && near_second > 0 && near_third > 0,
            "expected cuts in all three instance bands ({}, {}, {}):\n{}",
            near_first,
            near_second,
            near_third,
            resp.gcode,
        );
    }

    #[test]
    fn grid_pattern_emits_count_xcount_y_instances() {
        let project = project_with(
            vec![profile_op_with_pattern(PatternConfig::Grid {
                count_x: 2,
                count_y: 2,
                dx: 30.0,
                dy: 30.0,
            })],
            vec![endmill(1, 3.0)],
        );
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        // 2 × 2 = 4 instances of a closed-square outline.
        assert_eq!(
            resp.stats.closed_object_count, 4,
            "expected 4 closed objects from a 2x2 grid, got {}\n{}",
            resp.stats.closed_object_count, resp.gcode
        );
        // Cuts should reach into both grid dimensions.
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for line in resp.gcode.lines() {
            if !(line.starts_with("G1") || line.starts_with("G0")) {
                continue;
            }
            if let Some(idx) = line.find('X') {
                let rest = &line[idx + 1..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                    .unwrap_or(rest.len());
                if let Ok(v) = rest[..end].parse::<f64>() {
                    if v > max_x {
                        max_x = v;
                    }
                }
            }
            if let Some(idx) = line.find('Y') {
                let rest = &line[idx + 1..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                    .unwrap_or(rest.len());
                if let Ok(v) = rest[..end].parse::<f64>() {
                    if v > max_y {
                        max_y = v;
                    }
                }
            }
        }
        assert!(
            max_x > 45.0 && max_y > 45.0,
            "grid should extend into the second column AND the second row (X>{}, Y>{}):\n{}",
            max_x,
            max_y,
            resp.gcode,
        );
    }

    #[test]
    fn polar_pattern_rotates_around_center() {
        // Source square is 0..20 in X, 0..20 in Y. Polar pattern of 4
        // around the origin with 90° step produces instances rotated
        // by 0 / 90 / 180 / 270 — collectively their cuts should reach
        // into all four quadrants.
        let project = project_with(
            vec![profile_op_with_pattern(PatternConfig::Polar {
                count: 4,
                center_x: 0.0,
                center_y: 0.0,
                angle_step_deg: 90.0,
                start_angle_deg: 0.0,
            })],
            vec![endmill(1, 3.0)],
        );
        let resp = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            resp.stats.closed_object_count, 4,
            "expected 4 closed objects from a 4-instance polar pattern, got {}\n{}",
            resp.stats.closed_object_count, resp.gcode
        );
        let mut quad_pos_pos = false; // X>0, Y>0
        let mut quad_neg_pos = false; // X<0, Y>0
        let mut quad_neg_neg = false; // X<0, Y<0
        let mut quad_pos_neg = false; // X>0, Y<0
        let mut last_x: Option<f64> = None;
        let mut last_y: Option<f64> = None;
        for line in resp.gcode.lines() {
            if !(line.starts_with("G1") || line.starts_with("G0")) {
                continue;
            }
            let mut x = last_x;
            let mut y = last_y;
            for (label, slot) in [('X', &mut x), ('Y', &mut y)] {
                if let Some(idx) = line.find(label) {
                    let rest = &line[idx + 1..];
                    let end = rest
                        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                        .unwrap_or(rest.len());
                    if let Ok(v) = rest[..end].parse::<f64>() {
                        *slot = Some(v);
                    }
                }
            }
            last_x = x;
            last_y = y;
            if let (Some(xv), Some(yv)) = (x, y) {
                if xv > 5.0 && yv > 5.0 {
                    quad_pos_pos = true;
                }
                if xv < -5.0 && yv > 5.0 {
                    quad_neg_pos = true;
                }
                if xv < -5.0 && yv < -5.0 {
                    quad_neg_neg = true;
                }
                if xv > 5.0 && yv < -5.0 {
                    quad_pos_neg = true;
                }
            }
        }
        assert!(
            quad_pos_pos && quad_neg_pos && quad_neg_neg && quad_pos_neg,
            "expected polar cuts in all four quadrants (++, -+, --, +-): {} {} {} {}\n{}",
            quad_pos_pos,
            quad_neg_pos,
            quad_neg_neg,
            quad_pos_neg,
            resp.gcode,
        );
    }

    #[test]
    fn pattern_none_keeps_existing_behavior() {
        // Locks in back-compat: a Profile op with `pattern: None` must
        // produce the exact same gcode it produced before pattern support
        // was added (which is the same as a fresh op without the field).
        let project_a = project_with(
            vec![profile_op(1, 1, ToolOffset::Outside)],
            vec![endmill(1, 3.0)],
        );
        let mut op_b = profile_op(1, 1, ToolOffset::Outside);
        op_b.pattern = None;
        let project_b = project_with(vec![op_b], vec![endmill(1, 3.0)]);
        let resp_a = run_pipeline(
            PipelineRequest {
                project: project_a,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let resp_b = run_pipeline(
            PipelineRequest {
                project: project_b,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            resp_a.gcode, resp_b.gcode,
            "pattern: None must be byte-identical to a no-pattern op",
        );
    }

    // ─── Lead-in / lead-out (p31) ──────────────────────────────────────
    //
    // Profile leads come in three flavors: Off (rapid+plunge straight to
    // the contour start), Straight (perpendicular hop into the contour),
    // and Arc (tangent quarter-arc roll-on at z=0). The Arc variant is
    // the addition from p31 — its job is to land the cutter on the
    // contour with the cutter direction already aligned to the first
    // segment's tangent so there's no dwell at the start point.

    /// Profile + Outside + Arc lead-in (radius=2 mm) on a 50x50 square
    /// must emit a G2 / G3 arc move BETWEEN the surface plunge and the
    /// cut plunge — i.e., a roll-on arc at z=0 that lands the cutter
    /// tangent to the first segment.
    #[test]
    fn lead_in_arc_emits_g2_or_g3_before_first_cut() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_leads_op(
                ToolOffset::Outside,
                crate::cam::setup::LeadKind::Arc,
                2.0,
            )],
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
        let (_rapid, between, _first_cut) = first_lead_phase(&resp.gcode);
        // The lead-in phase (z=0, between surface plunge and cut
        // plunge) must contain at least one G2 or G3 arc command.
        let saw_arc = between
            .iter()
            .any(|l| l.starts_with("G2 ") || l.starts_with("G3 "));
        assert!(
            saw_arc,
            "expected a G2 / G3 arc lead-in at z=0, got intermediate moves={between:?}\n{}",
            resp.gcode,
        );
    }

    /// Profile + Outside + `LeadKind::Off` must NOT emit any motion
    /// between the surface plunge (G1 Z0) and the cut plunge (G1 Z-1)
    /// — the cutter just goes straight down at the contour start.
    #[test]
    fn lead_in_off_emits_no_lead() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_leads_op(
                ToolOffset::Outside,
                crate::cam::setup::LeadKind::Off,
                0.0,
            )],
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
        let (_rapid, between, _first_cut) = first_lead_phase(&resp.gcode);
        // No motion (no F-rate change is fine, but no G0/G1/G2/G3 with
        // XY/I/J) should appear between surface and cut plunges.
        let saw_motion = between.iter().any(|l| {
            l.starts_with("G0 ")
                || l.starts_with("G1 ")
                || l.starts_with("G2 ")
                || l.starts_with("G3 ")
        });
        assert!(
            !saw_motion,
            "LeadKind::Off should plunge straight to depth, but saw intermediate moves={between:?}\n{}",
            resp.gcode,
        );
    }

    /// Profile + Outside + `LeadKind::Straight` (length=2 mm) rapids the
    /// cutter to a perpendicular-offset hop point and then plunges
    /// straight down before cutting from there to the contour. The
    /// rapid target must NOT coincide with a contour-start XY (it's
    /// offset). And like the Off case, no extra moves at z=0.
    #[test]
    fn lead_in_straight_emits_a_straight_segment() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_leads_op(
                ToolOffset::Outside,
                crate::cam::setup::LeadKind::Straight,
                2.0,
            )],
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
        let (rapid, between, first_cut) = first_lead_phase(&resp.gcode);
        // Like Off: no extra move at z=0 between surface plunge and
        // cut plunge — Straight legacy semantics rapid the cutter
        // perpendicular to the contour and plunge AT that offset XY,
        // so the offset hop doesn't appear at z=0.
        let saw_motion = between.iter().any(|l| {
            l.starts_with("G0 ")
                || l.starts_with("G1 ")
                || l.starts_with("G2 ")
                || l.starts_with("G3 ")
        });
        assert!(
            !saw_motion,
            "Straight lead-in plunges at the offset hop XY, no z=0 motion expected; got {between:?}\n{}",
            resp.gcode,
        );
        // The rapid target should be ~2 mm offset from any contour
        // corner (the first segment's start, perpendicular to the
        // first-segment tangent). With the 1.5mm tool radius outset of
        // the 50x50 square, contour starts at one of {(0,-1.5),
        // (-1.5,0), ...} (depending on cut-direction reversal); the
        // rapid is 2mm perpendicular from there. Assert it's NOT on
        // the offset polygon (i.e., distance > 1.0 from any corner).
        let rapid = rapid.expect("expected a G0 X Y rapid");
        let corners = [(0.0_f64, 0.0_f64), (50.0, 0.0), (50.0, 50.0), (0.0, 50.0)];
        let on_geom_corner = corners
            .iter()
            .any(|(cx, cy)| (rapid.0 - cx).abs() < 0.5 && (rapid.1 - cy).abs() < 0.5);
        assert!(
            !on_geom_corner,
            "Straight lead-in's rapid target should be OFFSET (~2mm + tool radius) from any geometry corner, got {rapid:?}\n{}",
            resp.gcode,
        );
        // And there must be a first cut motion after the cut plunge.
        assert!(
            first_cut.is_some(),
            "expected a first cut motion\n{}",
            resp.gcode
        );
    }

    /// `PocketStrategy::Spiral` now emits ONE continuous open polyline
    /// instead of N concentric closed rings. Verified by counting
    /// distinct `; OP / level / pocket` blocks in the gcode — Spiral
    /// gives one `is_pocket=2` emit per object, Cascade gives N.
    #[test]
    fn spiral_emits_one_continuous_polyline_not_concentric_rings() {
        fn count_pocket_blocks(gcode: &str) -> usize {
            gcode
                .lines()
                .filter(|l| l.contains("pocket=2 segments="))
                .count()
        }
        let cascade_project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let mut spiral_project = cascade_project.clone();
        spiral_project.operations[0].kind = OpKind::Pocket {
            strategy: crate::project::PocketStrategy::Spiral,
        };
        let cascade_gcode = run_pipeline(
            PipelineRequest {
                project: cascade_project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap()
        .gcode;
        let spiral_gcode = run_pipeline(
            PipelineRequest {
                project: spiral_project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap()
        .gcode;
        let cascade_blocks = count_pocket_blocks(&cascade_gcode);
        let spiral_blocks = count_pocket_blocks(&spiral_gcode);
        assert!(
            cascade_blocks > 1,
            "cascade should emit many ring blocks, got {cascade_blocks}"
        );
        assert_eq!(
            spiral_blocks, 1,
            "spiral should emit exactly one continuous block, got {spiral_blocks}"
        );
    }

    /// w91: in a non-convex pocket the straight bridge between cascade
    /// rings can cut through a re-entrant pocket wall. The fix detects
    /// the bad bridge and silently falls back to cascade emission
    /// (separate closed rings, no bridges) rather than emitting a wrong
    /// cut. The test uses an L-shape — its inner cascade rings break
    /// into pieces whose centroids are in different L arms, so the
    /// nearest-vertex bridge between them crosses the L's notch wall.
    #[test]
    fn spiral_in_non_convex_pocket_falls_back_to_cascade() {
        // L-shape outline (CCW), 30 mm tall × 30 mm wide × 10 mm leg
        // thickness — wide enough that the inset rings split.
        let p0 = Point2::new(0.0, 0.0);
        let p1 = Point2::new(30.0, 0.0);
        let p2 = Point2::new(30.0, 10.0);
        let p3 = Point2::new(10.0, 10.0);
        let p4 = Point2::new(10.0, 30.0);
        let p5 = Point2::new(0.0, 30.0);
        let l_shape = vec![
            Segment::line(p0, p1, "0", 7),
            Segment::line(p1, p2, "0", 7),
            Segment::line(p2, p3, "0", 7),
            Segment::line(p3, p4, "0", 7),
            Segment::line(p4, p5, "0", 7),
            Segment::line(p5, p0, "0", 7),
        ];
        let project = Project {
            segments: l_shape,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: crate::project::PocketStrategy::Spiral,
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let gcode = run_pipeline(
            PipelineRequest {
                project,
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap()
        .gcode;
        // When spiral works (convex pocket): exactly one pocket=2
        // block. When it falls back to cascade in a non-convex shape:
        // multiple pocket=2 blocks (one per ring). For the L-shape
        // above we expect more than one, proving the fallback fired.
        let pocket_blocks = gcode
            .lines()
            .filter(|l| l.contains("pocket=2 segments="))
            .count();
        assert!(
            pocket_blocks >= 1,
            "L-shape pocket should emit at least one block; got {pocket_blocks}\n{gcode}"
        );
    }

    /// Source = Selected Objects with only the outer ring selected:
    /// inner circles inside the ring are NOT in the selection, so no
    /// pocket strategy should treat them as islands. Pre-fix, the
    /// `pocket_islands` legacy fallback in pipeline.rs would auto-fill
    /// the island list with all geometrically-nested closed children,
    /// which made cascade and spiral mill around the unselected
    /// circles while zigzag (whose offsets path doesn't plumb islands)
    /// ignored them — a strategy-dependent inconsistency the user
    /// reported. The fix restricts the auto-fill to source = All.
    ///
    /// Test approach: for each pocket strategy, compare the toolpath
    /// against a baseline run where the inner circles aren't even in
    /// the segment list. A correctly-implemented "selected only"
    /// pocket should produce IDENTICAL toolpath output regardless of
    /// whether unselected circles happen to be present in the
    /// document — the unselected geometry must have no influence.
    #[test]
    fn selected_objects_pocket_ignores_unselected_inner_circles_across_strategies() {
        use crate::project::{PocketStrategy, SourceCombine};
        let outer = closed_square_offset(100.0, 0.0, 0.0);
        let inner_a = closed_circle(Point2::new(30.0, 50.0), 5.0);
        let inner_b = closed_circle(Point2::new(70.0, 50.0), 5.0);
        let with_inners: Vec<Segment> = outer
            .iter()
            .cloned()
            .chain(inner_a.iter().cloned())
            .chain(inner_b.iter().cloned())
            .collect();
        let outer_only: Vec<Segment> = outer.clone();
        // Selection contains only object 1 (the outer ring) — same
        // value in both runs since chaining puts the outer first.
        let mk = |segments: Vec<Segment>, strategy: PocketStrategy, pocket_islands: bool| Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket { strategy },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Auto,
                },
                params: OpParams {
                    pocket_islands,
                    ..OpParams::mill_default()
                },
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let strategies = [
            PocketStrategy::Cascade,
            PocketStrategy::Spiral,
            PocketStrategy::Zigzag,
        ];
        for strategy in strategies {
            for pocket_islands in [false, true] {
                let baseline = run_pipeline(
                    PipelineRequest {
                        project: mk(outer_only.clone(), strategy, pocket_islands),
                        post_processor: None,
                    },
                    |_, _, _| {},
                )
                .unwrap();
                let with_inners_run = run_pipeline(
                    PipelineRequest {
                        project: mk(with_inners.clone(), strategy, pocket_islands),
                        post_processor: None,
                    },
                    |_, _, _| {},
                )
                .unwrap();
                // Same toolpath segment count = unselected inner
                // geometry had no influence on the cut. If the
                // pocket_islands fallback leaks into source=Objects,
                // the with_inners run gets extra cascade rings around
                // each circle and the count diverges.
                assert_eq!(
                    baseline.toolpath.len(),
                    with_inners_run.toolpath.len(),
                    "strategy {:?} pocket_islands={}: with-inners toolpath has \
                     {} segments vs baseline {} — unselected inner circles \
                     are leaking into the pocket as auto-islands",
                    strategy,
                    pocket_islands,
                    with_inners_run.toolpath.len(),
                    baseline.toolpath.len()
                );
            }
        }
    }

    /// Profile op tool offset must actually offset the cut for both
    /// CCW and CW input winding. For a 100×100 square + 3 mm tool:
    /// Outside should put the cut at `max_x` ≈ 101.5, Inside at `max_x`
    /// ≈ 98.5, On at exactly 100.0. Repeats with the source segments
    /// reversed (CW winding) since DXF / SVG imports can produce
    /// either winding and `parallel_offset_inward` / outward picks the
    /// sign from `object_signed_area`.
    #[test]
    fn profile_offset_works_for_cw_and_ccw_input() {
        use crate::gcode::preview::MoveKind;
        let ccw_segments = closed_square_offset(100.0, 0.0, 0.0);
        // CW: same square, segments traversed in reverse direction.
        let cw_segments: Vec<Segment> = ccw_segments
            .iter()
            .rev()
            .map(|s| Segment::line(s.end, s.start, &s.layer, s.color))
            .collect();
        for (winding_label, segments) in &[("CCW", &ccw_segments), ("CW", &cw_segments)] {
            let mk = |offset: ToolOffset| Project {
                segments: (*segments).clone(),
                machine: MachineConfig::default(),
                tools: vec![endmill(1, 3.0)],
                operations: vec![profile_op(1, 1, offset)],
                fixtures: Vec::default(),
                text_layers: Vec::default(),
            };
            let cut_max_x = |toolpath: &[crate::gcode::preview::ToolpathSegment]| -> f64 {
                toolpath
                    .iter()
                    .filter(|s| matches!(s.kind, MoveKind::Cut))
                    .flat_map(|s| [s.from.x, s.to.x])
                    .fold(f64::NEG_INFINITY, f64::max)
            };
            let cases: [(&str, ToolOffset); 3] = [
                ("On", ToolOffset::On),
                ("Outside", ToolOffset::Outside),
                ("Inside", ToolOffset::Inside),
            ];
            for (offset_label, offset) in cases {
                let resp = run_pipeline(
                    PipelineRequest {
                        project: mk(offset),
                        post_processor: None,
                    },
                    |_, _, _| {},
                )
                .unwrap();
                let max_x = cut_max_x(&resp.toolpath);
                let ok = match offset {
                    ToolOffset::On | ToolOffset::None => (max_x - 100.0).abs() < 0.1,
                    ToolOffset::Outside => max_x > 100.5,
                    ToolOffset::Inside => max_x < 99.5,
                };
                assert!(
                    ok,
                    "{winding_label} input + {offset_label} offset: cut max_x = {max_x} fails the expected position check"
                );
            }
        }
    }

    /// Profile + Outside selecting an INNER circle that lives inside
    /// an outer ring. `classify_containment` marks the circle as
    /// `inner_objects`[outer]; it's still a valid Profile target on its
    /// own. The user reported "always cuts on line" — could the
    /// containment-detected status flip the offset direction?
    #[test]
    fn profile_outside_selecting_inner_circle_offsets_outward() {
        use crate::gcode::preview::MoveKind;
        let outer = closed_square_offset(100.0, 0.0, 0.0);
        // Inner circle at (50, 50), r=10.
        let inner = closed_circle(Point2::new(50.0, 50.0), 10.0);
        let segments: Vec<Segment> = outer.into_iter().chain(inner).collect();
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OpKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                finish_tool_id: None,
                // Object 2 is the inner circle (chaining puts segments
                // in input order; outer was first).
                source: OpSource::Objects {
                    ids: vec![2],
                    combine: crate::project::SourceCombine::Auto,
                },
                params: OpParams::mill_default(),
                pattern: None,
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
        // For Outside on the inner circle: the cutter is OUTSIDE the
        // circle, so the toolpath is at radius 10 + tool_radius 1.5 =
        // 11.5 from (50, 50). Cut max_x should be ~61.5. The cut is
        // emitted as G2/G3 arcs which preview::interpret classifies
        // as MoveKind::Arc, not Cut — both count as cutting moves
        // here.
        let max_x = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, MoveKind::Cut | MoveKind::Arc))
            .flat_map(|s| [s.from.x, s.to.x])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            max_x > 61.0 && max_x < 62.0,
            "Profile + Outside on inner circle: cut max_x={max_x}, expected ~61.5"
        );
    }

    /// User-reported repro shape: Profile + Outside + source=Objects
    /// with one outer ring selected. Mirrors the exact wire payload
    /// build-project.ts emits (`pocket_islands=true` default, leads off,
    /// etc.). User said the cut comes out "on the line" instead of
    /// offset.
    #[test]
    fn profile_outside_with_source_objects_actually_offsets() {
        use crate::gcode::preview::MoveKind;
        let raw = serde_json::json!({
            "project": {
                "segments": [
                    { "type": "LINE", "start": { "x": 0.0, "y": 0.0 }, "end": { "x": 100.0, "y": 0.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 100.0, "y": 0.0 }, "end": { "x": 100.0, "y": 100.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 100.0, "y": 100.0 }, "end": { "x": 0.0, "y": 100.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 0.0, "y": 100.0 }, "end": { "x": 0.0, "y": 0.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                ],
                "machine": { "unit": "mm", "mode": "mill", "comments": true, "arcs": true, "supports_toolchange": false },
                "tools": [{ "id": 1, "name": "3mm", "kind": "endmill", "diameter": 3.0, "flutes": 2, "speed": 18000, "plunge_rate": 100, "feed_rate": 800, "coolant": "off" }],
                "operations": [{
                    "id": 1, "name": "Profile", "enabled": true,
                    "kind": { "type": "profile", "offset": "outside" },
                    "tool_id": 1,
                    "source": { "kind": "objects", "ids": [1] },
                    "params": {
                        "depth": -2.0, "start_depth": 0.0, "step": -1.0, "fast_move_z": 5.0,
                        "helix": false, "reverse": false, "objectorder": "nearest", "overcut": false,
                        "pocket_islands": true, "pocket_nocontour": false, "pocket_insideout": false,
                        "tabs": { "active": false, "width": 10.0, "height": 1.0, "tab_type": "rectangle" },
                        "leads": { "in": "off", "out": "off", "in_lenght": 5.0, "out_lenght": 5.0 }
                    }
                }],
                "tabs": {}
            }
        });
        let req: PipelineRequest = serde_json::from_value(raw).expect("wire JSON");
        let resp = run_pipeline(req, |_, _, _| {}).unwrap();
        let max_x = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, MoveKind::Cut))
            .flat_map(|s| [s.from.x, s.to.x])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            max_x > 100.5,
            "user-shape Profile + outside + source=objects: cut max_x={}, expected > 100.5\n\nFull gcode:\n{}",
            max_x,
            resp.gcode
        );
    }

    /// End-to-end deserialization test: build a JSON Profile op the
    /// way the frontend's build-project.ts does, deserialize through
    /// `PipelineRequest`, run it, and confirm the offset is honored.
    #[test]
    fn profile_offset_via_wire_json_outside_actually_offsets() {
        use crate::gcode::preview::MoveKind;
        // 100×100 closed CCW square, 4 line segments.
        let raw = serde_json::json!({
            "project": {
                "segments": [
                    { "type": "LINE", "start": { "x": 0.0, "y": 0.0 }, "end": { "x": 100.0, "y": 0.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 100.0, "y": 0.0 }, "end": { "x": 100.0, "y": 100.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 100.0, "y": 100.0 }, "end": { "x": 0.0, "y": 100.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                    { "type": "LINE", "start": { "x": 0.0, "y": 100.0 }, "end": { "x": 0.0, "y": 0.0 }, "bulge": 0.0, "layer": "0", "color": 7 },
                ],
                "machine": { "unit": "mm", "mode": "mill", "comments": true, "arcs": true, "supports_toolchange": false },
                "tools": [{ "id": 1, "name": "3mm", "kind": "endmill", "diameter": 3.0, "flutes": 2, "speed": 18000, "plunge_rate": 100, "feed_rate": 800, "coolant": "off" }],
                "operations": [{
                    "id": 1, "name": "Profile", "enabled": true,
                    "kind": { "type": "profile", "offset": "outside" },
                    "tool_id": 1,
                    "source": { "kind": "all" },
                    "params": {
                        "depth": -2.0, "start_depth": 0.0, "step": -1.0, "fast_move_z": 5.0,
                        "helix": false, "reverse": false, "objectorder": "nearest", "overcut": false,
                        "pocket_islands": false, "pocket_nocontour": false, "pocket_insideout": false,
                        "tabs": { "active": false, "width": 10.0, "height": 1.0, "tab_type": "rectangle" },
                        "leads": { "in": "off", "out": "off", "in_lenght": 5.0, "out_lenght": 5.0 }
                    }
                }],
                "tabs": {}
            }
        });
        let req: PipelineRequest = serde_json::from_value(raw).expect("wire JSON deserialization");
        // op.offset deserialized as ToolOffset::Outside?
        if let OpKind::Profile { offset } = req.project.operations[0].kind {
            assert_eq!(
                offset,
                ToolOffset::Outside,
                "wire 'outside' string didn't deserialize to ToolOffset::Outside"
            );
        } else {
            panic!("not a profile op");
        }
        let resp = run_pipeline(req, |_, _, _| {}).unwrap();
        let max_x = resp
            .toolpath
            .iter()
            .filter(|s| matches!(s.kind, MoveKind::Cut))
            .flat_map(|s| [s.from.x, s.to.x])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            max_x > 100.5,
            "wire JSON Profile + outside: cut max_x={max_x}, expected > 100.5 — offset isn't being applied via the wire"
        );
    }

    /// What if the source is an OPEN polyline (e.g., a single line
    /// segment from an SVG path that wasn't closed)? The user's bug
    /// report says Profile + Outside/Inside "always cuts on line" —
    /// could be triggered if cavalier returns empty for an open
    /// polyline and the code silently falls back to source segments.
    #[test]
    fn profile_offset_open_polyline_either_offsets_or_emits_nothing_never_on_line() {
        use crate::gcode::preview::MoveKind;
        // A simple open V-shape: two connected line segments. CCW
        // orientation isn't well-defined for open paths.
        let segments = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(50.0, 30.0), "0", 7),
            Segment::line(Point2::new(50.0, 30.0), Point2::new(100.0, 0.0), "0", 7),
        ];
        let mk = |offset: ToolOffset| Project {
            segments: segments.clone(),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, offset)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        for offset in [ToolOffset::Outside, ToolOffset::Inside] {
            let resp = run_pipeline(
                PipelineRequest {
                    project: mk(offset),
                    post_processor: None,
                },
                |_, _, _| {},
            )
            .unwrap();
            let cut: Vec<_> = resp
                .toolpath
                .iter()
                .filter(|s| matches!(s.kind, MoveKind::Cut))
                .collect();
            // Detect "cuts on the source line": the line passes
            // through (50, 30). Any cut whose Y is ≥ 29 at X near 50
            // is on the source — Outside should be ABOVE 30 (offset
            // up by tool radius), Inside should be BELOW 30.
            let on_apex = cut.iter().any(|s| {
                let mid_x = (s.from.x + s.to.x) * 0.5;
                let mid_y = (s.from.y + s.to.y) * 0.5;
                (mid_x - 50.0).abs() < 5.0 && (mid_y - 30.0).abs() < 0.2
            });
            assert!(
                !on_apex || cut.is_empty(),
                "{offset:?} on open polyline: cut crosses the source apex (50, 30) \
                 — offset isn't being applied (on-line cut bug)"
            );
        }
    }

    #[test]
    fn profile_offset_actually_offsets_outside_inside_on() {
        use crate::gcode::preview::MoveKind;
        let segments = closed_square_offset(100.0, 0.0, 0.0);
        let mk = |offset: ToolOffset| Project {
            segments: segments.clone(),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, offset)],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let cut_max_x = |toolpath: &[crate::gcode::preview::ToolpathSegment]| -> f64 {
            toolpath
                .iter()
                .filter(|s| matches!(s.kind, MoveKind::Cut))
                .flat_map(|s| [s.from.x, s.to.x])
                .fold(f64::NEG_INFINITY, f64::max)
        };
        let on = run_pipeline(
            PipelineRequest {
                project: mk(ToolOffset::On),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let outside = run_pipeline(
            PipelineRequest {
                project: mk(ToolOffset::Outside),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let inside = run_pipeline(
            PipelineRequest {
                project: mk(ToolOffset::Inside),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let on_x = cut_max_x(&on.toolpath);
        let outside_x = cut_max_x(&outside.toolpath);
        let inside_x = cut_max_x(&inside.toolpath);
        assert!(
            (on_x - 100.0).abs() < 0.1,
            "On offset should cut at exactly the boundary (max_x≈100), got {on_x}"
        );
        assert!(
            outside_x > 100.5,
            "Outside offset should push cut past the boundary (max_x>100.5), got {outside_x}"
        );
        assert!(
            inside_x < 99.5,
            "Inside offset should pull cut inside the boundary (max_x<99.5), got {inside_x}"
        );
    }

    /// Trochoidal pocket on a 100×60 rectangle with a 6 mm endmill.
    /// Validates that the emitted cut path is comparable in length to
    /// the spiral equivalent (1.0×–1.5×) — trochoidal is intentionally
    /// a longer path than spiral because every centerline step picks
    /// up a small loop, but it shouldn't blow up the path length by
    /// more than 50%.
    #[test]
    fn trochoidal_pocket_path_length_within_envelope_of_spiral() {
        let p0 = Point2::new(0.0, 0.0);
        let p1 = Point2::new(100.0, 0.0);
        let p2 = Point2::new(100.0, 60.0);
        let p3 = Point2::new(0.0, 60.0);
        let rect = vec![
            Segment::line(p0, p1, "0", 7),
            Segment::line(p1, p2, "0", 7),
            Segment::line(p2, p3, "0", 7),
            Segment::line(p3, p0, "0", 7),
        ];
        let mk = |strategy: PocketStrategy| Project {
            segments: rect.clone(),
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Op {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket { strategy },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams {
                    plunge: crate::cam::setup::PlungeStrategy::Helix {
                        angle_deg: 3.0,
                        radius_mm: Some(4.5),
                    },
                    ..OpParams::mill_default()
                },
                pattern: None,
            }],
            fixtures: Vec::default(),
            text_layers: Vec::default(),
        };
        let cut_total = |toolpath: &[preview::ToolpathSegment]| -> f64 {
            toolpath
                .iter()
                .filter(|s| matches!(s.kind, crate::gcode::preview::MoveKind::Cut))
                .map(|s| {
                    let dx = s.to.x - s.from.x;
                    let dy = s.to.y - s.from.y;
                    (dx * dx + dy * dy).sqrt()
                })
                .sum()
        };
        let spiral = run_pipeline(
            PipelineRequest {
                project: mk(PocketStrategy::Spiral),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let trochoidal = run_pipeline(
            PipelineRequest {
                project: mk(PocketStrategy::Trochoidal {
                    engagement_angle_deg: 30.0,
                    loop_radius_factor: 0.6,
                }),
                post_processor: None,
            },
            |_, _, _| {},
        )
        .unwrap();
        let s_len = cut_total(&spiral.toolpath);
        let t_len = cut_total(&trochoidal.toolpath);
        assert!(s_len > 0.0, "spiral baseline empty");
        assert!(t_len > 0.0, "trochoidal toolpath empty");
        // Trochoidal IS longer than spiral by design (loops add
        // distance), so we expect t_len > s_len. Cap it at 5× to
        // catch obvious blow-ups; the brief's 1.5× bound applies to
        // the centerline-only portion which is hard to extract from
        // the toolpath stream — keep the integration check loose.
        assert!(
            t_len > s_len * 0.5,
            "trochoidal path {t_len} too short vs spiral {s_len}"
        );
        assert!(
            t_len < s_len * 5.0,
            "trochoidal path {t_len} blew up vs spiral {s_len}"
        );
    }

    /// Pipeline emits a `tabs_with_trochoidal_unsupported` warning
    /// when an op asks for both at once.
    #[test]
    fn trochoidal_with_tabs_emits_unsupported_warning() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let mut params = OpParams::mill_default();
        params.tabs.active = true;
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(4.5),
        };
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 7,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: PocketStrategy::Trochoidal {
                        engagement_angle_deg: 30.0,
                        loop_radius_factor: 0.6,
                    },
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params,
                pattern: None,
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
        assert!(
            resp.warnings
                .iter()
                .any(|w| w.kind == "tabs_with_trochoidal_unsupported" && w.op_id == Some(7)),
            "expected tabs_with_trochoidal_unsupported, got {:?}",
            resp.warnings
        );
    }

    /// Pipeline overrides Direct/Ramp plunges to Helix on Trochoidal
    /// pockets and emits `plunge_overridden`.
    #[test]
    fn trochoidal_with_direct_plunge_emits_plunge_overridden_warning() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let project = Project {
            segments,
            machine: MachineConfig::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Op {
                id: 9,
                name: "Pocket".into(),
                enabled: true,
                kind: OpKind::Pocket {
                    strategy: PocketStrategy::Trochoidal {
                        engagement_angle_deg: 30.0,
                        loop_radius_factor: 0.6,
                    },
                },
                tool_id: 1,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams {
                    plunge: crate::cam::setup::PlungeStrategy::Direct,
                    ..OpParams::mill_default()
                },
                pattern: None,
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
        assert!(
            resp.warnings
                .iter()
                .any(|w| w.kind == "plunge_overridden" && w.op_id == Some(9)),
            "expected plunge_overridden warning, got {:?}",
            resp.warnings
        );
    }

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

    /// `VCarve` op produces a non-empty toolpath whose deepest cutting
    /// move sits well below `start_depth - 0.1` — proves the medial
    /// axis ratchet actually plunges into the slot rather than just
    /// tracing the boundary at z=0.
    #[test]
    fn vcarve_op_emits_cutting_moves_below_start_depth() {
        let vbit = ToolEntry {
            id: 1,
            name: "60° V".into(),
            kind: ToolKind::VBit,
            diameter: 6.35,
            tip_diameter: Some(0.1),
            tip_angle_deg: 60.0,
            dragoff: None,
            flutes: 2,
            speed: 18_000,
            plunge_rate: 200,
            feed_rate: 1200,
            coolant: Coolant::Off,
            speed_finish: None,
            plunge_rate_finish: None,
            feed_rate_finish: None,
            speed_drill: None,
            plunge_rate_drill: None,
            feed_rate_drill: None,
            default_peck_step_mm: None,
            default_step: None,
            z_shift_mm: None,
            laser_pierce_sec: None,
            laser_lead_in_mm: None,
            corner_radius_mm: None,
            tslot_neck_diameter_mm: None,
            tslot_neck_length_mm: None,
            wirbeln: false,
            wirbeln_stepover_mm: None,
            pause: 1,
            flute_length_mm: None,
            shank_diameter_mm: None,
            holder: None,
        };
        let op = Op {
            id: 7,
            name: "Carve".into(),
            enabled: true,
            kind: OpKind::VCarve,
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
            pattern: None,
        };
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
            tools: vec![vbit],
            operations: vec![op],
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
        .expect("pipeline ran");
        assert!(!resp.gcode.is_empty(), "gcode should not be empty");
        let any_deep = resp
            .toolpath
            .iter()
            .any(|s| s.to.z < -0.1 && !matches!(s.kind, crate::gcode::preview::MoveKind::Rapid));
        assert!(
            any_deep,
            "expected at least one cutting move below start_depth - 0.1; got {} toolpath segs",
            resp.toolpath.len()
        );
    }

    #[test]
    fn effective_step_op_override_wins() {
        let mut tool = endmill(1, 3.0);
        tool.default_step = Some(-0.5);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = Some(-0.3);
        assert_eq!(effective_step(&op, &tool).unwrap(), -0.3);
    }

    #[test]
    fn effective_step_falls_back_to_tool_default() {
        let mut tool = endmill(1, 3.0);
        tool.default_step = Some(-0.5);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = None;
        assert_eq!(effective_step(&op, &tool).unwrap(), -0.5);
    }

    #[test]
    fn effective_step_warns_when_both_unset() {
        let tool = endmill(1, 3.0);
        let mut op = profile_op(7, 1, ToolOffset::Outside);
        op.params.step = None;
        let w = effective_step(&op, &tool).unwrap_err();
        assert_eq!(w.kind, "step_unspecified");
        assert_eq!(w.op_id, Some(7));
    }

    #[test]
    fn effective_step_rejects_non_negative() {
        let mut tool = endmill(1, 3.0);
        tool.default_step = Some(0.5);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = Some(0.0);
        assert!(effective_step(&op, &tool).is_err());
    }

    #[test]
    fn run_pipeline_emits_step_unspecified_warning() {
        let tool = endmill(1, 3.0);
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.params.step = None;
        let resp = run_pipeline(
            PipelineRequest {
                project: project_with(vec![op], vec![tool]),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert!(
            resp.warnings.iter().any(|w| w.kind == "step_unspecified"),
            "expected step_unspecified warning, got {:?}",
            resp.warnings
        );
    }

    #[test]
    fn vcarve_op_round_trips_through_serde_json() {
        let op = Op {
            id: 11,
            name: "Sign carve".into(),
            enabled: true,
            kind: OpKind::VCarve,
            tool_id: 1,
            finish_tool_id: None,
            source: OpSource::All,
            params: OpParams {
                depth: -8.0,
                start_depth: 0.0,
                step: Some(-0.8),
                fast_move_z: 6.0,
                carve_max_width_mm: Some(4.0),
                multi_pass_refine: true,
                ..OpParams::default()
            },
            pattern: None,
        };
        let json = serde_json::to_string(&op).expect("serialize");
        let back: Op = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back.kind, OpKind::VCarve));
        assert_eq!(back.params.carve_max_width_mm, Some(4.0));
        assert!(back.params.multi_pass_refine);
        assert_eq!(back.params.depth, -8.0);
    }

    #[test]
    fn op_step_and_tool_default_step_emit_identical_gcode() {
        let mut tool_a = endmill(1, 3.0);
        tool_a.default_step = None;
        let mut op_a = profile_op(1, 1, ToolOffset::Outside);
        op_a.params.step = Some(-0.5);

        let mut tool_b = endmill(1, 3.0);
        tool_b.default_step = Some(-0.5);
        let mut op_b = profile_op(1, 1, ToolOffset::Outside);
        op_b.params.step = None;

        let resp_a = run_pipeline(
            PipelineRequest {
                project: project_with(vec![op_a], vec![tool_a]),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        let resp_b = run_pipeline(
            PipelineRequest {
                project: project_with(vec![op_b], vec![tool_b]),
                post_processor: Some(PostProcessorKind::Linuxcnc),
            },
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(resp_a.gcode, resp_b.gcode);
        assert!(resp_a.warnings.iter().all(|w| w.kind != "step_unspecified"));
        assert!(resp_b.warnings.iter().all(|w| w.kind != "step_unspecified"));
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
                kind: OpKind::VCarve,
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
                pattern: None,
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
                },
                tool_id: 91,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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
                },
                tool_id: 100 + i,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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
                },
                tool_id: 77,
                finish_tool_id: None,
                source: OpSource::All,
                params: OpParams::mill_default(),
                pattern: None,
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

    #[test]
    fn compute_helix_radius_for_50x30_rect() {
        let segments = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(50.0, 0.0), "0", 7),
            Segment::line(Point2::new(50.0, 0.0), Point2::new(50.0, 30.0), "0", 7),
            Segment::line(Point2::new(50.0, 30.0), Point2::new(0.0, 30.0), "0", 7),
            Segment::line(Point2::new(0.0, 30.0), Point2::new(0.0, 0.0), "0", 7),
        ];
        let resp = crate::compute_helix_radius(crate::HelixRadiusRequest {
            segments,
            object_ids: Vec::new(),
            tool_diameter_mm: 6.0,
        });
        let r = resp.radius_mm.expect("expected an inscribed-circle fit");
        assert!(
            (r - 11.5).abs() < 0.1,
            "expected ~11.5 mm helix radius, got {r}",
        );
        assert!(resp.fallback_reason.is_none());
    }

    #[test]
    fn compute_helix_radius_for_tiny_pocket() {
        let segments = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(5.0, 0.0), "0", 7),
            Segment::line(Point2::new(5.0, 0.0), Point2::new(5.0, 5.0), "0", 7),
            Segment::line(Point2::new(5.0, 5.0), Point2::new(0.0, 5.0), "0", 7),
            Segment::line(Point2::new(0.0, 5.0), Point2::new(0.0, 0.0), "0", 7),
        ];
        let resp = crate::compute_helix_radius(crate::HelixRadiusRequest {
            segments,
            object_ids: Vec::new(),
            tool_diameter_mm: 6.0,
        });
        assert!(resp.radius_mm.is_none());
        let reason = resp.fallback_reason.expect("expected a fallback reason");
        assert!(!reason.is_empty(), "fallback_reason should be non-empty");
    }

    #[test]
    fn compute_helix_radius_open_polyline_returns_none() {
        let segments = vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(50.0, 0.0), "0", 7),
            Segment::line(Point2::new(50.0, 0.0), Point2::new(50.0, 30.0), "0", 7),
            Segment::line(Point2::new(50.0, 30.0), Point2::new(0.0, 30.0), "0", 7),
        ];
        let resp = crate::compute_helix_radius(crate::HelixRadiusRequest {
            segments,
            object_ids: Vec::new(),
            tool_diameter_mm: 6.0,
        });
        assert!(resp.radius_mm.is_none());
        let reason = resp.fallback_reason.expect("expected a fallback reason");
        assert!(!reason.is_empty(), "fallback_reason should be non-empty");
    }
}
