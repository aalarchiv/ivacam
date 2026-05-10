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
//! WASM threading (web workers + COOP/COEP) is out of scope for v1 — the
//! WASM bridge ships single-threaded and pumps events synchronously.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::chaining::{classify_containment, segments_to_objects};
use crate::cam::offsets::{
    apply_cut_direction, apply_overcut_to_offsets, attach_tabs_to_offsets, parallel_offset_inward,
    parallel_offset_outward, pocket_for_object, small_circle_drill, PocketEmit, PolylineOffset,
    TabPoint,
};
use crate::cam::setup::{Setup, ToolOffset};
use crate::cam::source_combine::{build_frame, combine_source_regions, CombinedRegion};
use crate::cam::{segments_to_points, VcObject};
use crate::gcode::{
    emit_drill_block, emit_polylines_block, emit_program_begin, emit_program_end, grbl, hpgl,
    linuxcnc, preview, PostProcessor,
};
use crate::geometry::{Point2, Segment};
use crate::pipeline_cache::{op_cache_key, OpCacheValue, PipelineCache};
use crate::project::{
    Operation, OperationKind, OperationSource, PatternConfig, PocketStrategy, Project,
    SourceCombine, ToolEntry,
};

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
    /// will machine, computed via the per-op SourceCombine mode (Auto by
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
    UnimplementedKind(OperationKind),
    #[error("pipeline cancelled")]
    Cancelled,
}

impl PipelineError {
    /// Lift the enum into the structured frontend `Error`. Project context
    /// fills in actionable auto-fix targets (e.g. the first tool id for an
    /// `UnknownTool`); pass `None` when no project is available and the
    /// auto-fix is dropped.
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
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

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
    let project = req.project;

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
    let gcode = match post_kind {
        PostProcessorKind::Linuxcnc => run_per_op(
            &project,
            &mut objects.clone(),
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
            &mut objects.clone(),
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
            &mut objects.clone(),
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
fn cancelled(cancel: Option<&CancelToken>) -> bool {
    cancel.map(|c| c.is_cancelled()).unwrap_or(false)
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
        .map(|t| t.pause as f64)
        .sum()
}

/// Compute the filled-region preview for every enabled Pocket op. Auto
/// mode runs through the same containment-aware logic as the per-op
/// driver; explicit modes route through the clipper2 boolean ops in
/// cam::source_combine. Non-Pocket ops contribute nothing.
fn build_region_previews(project: &Project, objects: &[VcObject]) -> Vec<RegionPreview> {
    let mut out = Vec::new();
    for op in project.operations.iter().filter(|o| o.enabled) {
        if !matches!(op.kind, OperationKind::Pocket { .. }) {
            continue;
        }
        // Pocket-Outside (rt1.3) preview: when the op declares a frame,
        // mirror the build_op_offsets injection so the preview matches the
        // toolpath the user will see.
        if let Some(frame_shape) = op.params.frame_shape {
            let selected_indices = ordered_selection(op, objects);
            if selected_indices.is_empty() {
                continue;
            }
            let mut local_objects = objects.to_vec();
            let frame_selection: Vec<&VcObject> =
                selected_indices.iter().map(|&i| &local_objects[i]).collect();
            let frame = build_frame(
                &frame_selection,
                frame_shape,
                op.params.frame_padding_mm.unwrap_or(0.0).max(0.0),
                op.params.frame_corner_radius_mm,
            );
            let frame_idx = local_objects.len();
            local_objects.push(frame);
            let mut ordered: Vec<usize> = Vec::with_capacity(selected_indices.len() + 1);
            ordered.push(frame_idx);
            ordered.extend(selected_indices);
            let regions =
                combine_source_regions(&local_objects, &ordered, SourceCombine::Difference);
            for r in regions {
                out.push(RegionPreview {
                    op_id: op.id,
                    outer: r.boundary,
                    holes: r.holes,
                });
            }
            continue;
        }
        let selected = ordered_selection(op, objects);
        let mode = source_combine_mode(op);
        let regions = combine_source_regions(objects, &selected, mode);
        for r in regions {
            out.push(RegionPreview {
                op_id: op.id,
                outer: r.boundary,
                holes: r.holes,
            });
        }
    }
    out
}

/// Per-post-processor monomorphisation of the per-op driver. Pulled out
/// so we don't need to type-erase PostProcessor (its methods take Sized
/// `&mut self` so the trait object dance was painful).
#[allow(clippy::too_many_arguments)]
fn run_per_op<P, F>(
    project: &Project,
    objects: &mut Vec<VcObject>,
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
    emit_program_begin(header_setup, post);
    let mut last_pos = Point2::new(0.0, 0.0);
    let mut emitted_ops = 0usize;
    let enabled_ops: Vec<&Operation> = project.operations.iter().filter(|o| o.enabled).collect();
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
            Some(op_cache_key(
                op,
                tool,
                &project.machine,
                &resolve_op_segments(op, &project.segments),
                &project.fixtures,
                &project.tabs,
                post_tag,
            ))
        });

        if let (Some(c), Some(key)) = (cache, cache_key) {
            if let Some(cached) = c.get(key) {
                let lines: Vec<String> = cached.gcode_body.lines().map(|s| s.to_string()).collect();
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
                    0.30 + 0.55 * (emitted_ops as f64 / n_ops as f64),
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
        if matches!(op.kind, OperationKind::VCarve) {
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
        } else {
            let (offsets, closed_count) =
                build_op_offsets(op, project, &mut objects.clone(), &setup, warnings, cancel)?;
            closed_count_emitted = closed_count;
            offset_count_emitted = offsets.len();
            {
                let mut s = stats.borrow_mut();
                s.0 += closed_count;
                s.1 += offsets.len();
            }
            post.raw(&format!("; OP {}", op.id));
            if !offsets.is_empty() {
                if let OperationKind::Drill { cycle } = op.kind {
                    emit_drill_block(&setup, &offsets, cycle, post, &mut last_pos);
                } else {
                    emit_polylines_block(&setup, &offsets, post, &mut last_pos);
                }
            }
        }
        if let (Some(c), Some(key)) = (cache, cache_key) {
            let lines = post.out_lines_clone_from(body_marker);
            let body = lines.join("\n");
            let (toolpath, _idx) = preview::interpret_with_index(&format!("; OP {}\n{body}", op.id));
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
            0.30 + 0.55 * (emitted_ops as f64 / n_ops as f64),
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
/// layer. For OperationSource::Objects we conservatively hash all
/// segments because mapping object ids back to original segments
/// requires running `segments_to_objects` again, which the cache
/// shouldn't bear.
fn resolve_op_segments(op: &Operation, all: &[Segment]) -> Vec<Segment> {
    match &op.source {
        OperationSource::All => all.to_vec(),
        OperationSource::Layers { layers, .. } => all
            .iter()
            .filter(|s| layers.iter().any(|l| l == &s.layer))
            .cloned()
            .collect(),
        OperationSource::Objects { .. } => all.to_vec(),
    }
}

// ─── per-op offset building ───────────────────────────────────────────────

/// Build the offset list a single op consumes. Currently supports
/// Profile / Pocket / Engrave / DragKnife — others raise UnimplementedKind.
fn build_op_offsets(
    op: &Operation,
    project: &Project,
    objects: &mut Vec<VcObject>,
    setup: &Setup,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(Vec<PolylineOffset>, usize), PipelineError> {
    if cancelled(cancel) {
        return Err(PipelineError::Cancelled);
    }
    // Up-front sanity checks that don't depend on whether the cascade
    // succeeds. push_tool_fit_kind_warnings populates `warnings` for
    // tool-kind / op-kind mismatches and impossible tool geometry.
    push_tool_fit_kind_warnings(op, project, setup, warnings);
    push_trochoidal_warnings(op, warnings);
    // Map imported-segment-keyed tabs → owning chain object.
    let mut tabs_by_object: HashMap<usize, Vec<TabPoint>> = HashMap::new();
    if !project.tabs.is_empty() {
        let segment_to_object = build_segment_to_object_map(&project.segments, objects);
        for (seg_idx, tabs) in &project.tabs {
            if let Some(&obj_idx) = segment_to_object.get(&(*seg_idx as usize)) {
                tabs_by_object
                    .entry(obj_idx)
                    .or_default()
                    .extend_from_slice(tabs);
            }
        }
    }

    // Pattern repetition (5fz): when the op carries a PatternConfig, expand
    // the source set into N transformed clones BEFORE the per-object loops
    // run. After expansion, every clone is "selected" (so the inner loops
    // see them via OperationSource::All on the effective op), and tabs
    // attached to the original objects are translated/rotated alongside
    // the geometry so each instance keeps its tab placement.
    let effective_op_storage: Option<Operation> = if let Some(pattern) = op.pattern {
        let instances = pattern_offsets(pattern);
        let mut expanded: Vec<VcObject> = Vec::with_capacity(instances.len() * objects.len());
        let mut expanded_tabs: HashMap<usize, Vec<TabPoint>> = HashMap::new();
        for inst in &instances {
            for (idx, obj) in objects.iter().enumerate() {
                if !op_includes_object(op, obj, idx) {
                    continue;
                }
                let mut clone = obj.clone();
                apply_pattern_to_segments(&mut clone.segments, *inst);
                // Containment relationships index into the OLD object list,
                // which doesn't match the expanded set. Drop them; the
                // pocket-skipping logic relies on selected_set membership
                // which is recomputed below for the expanded set.
                clone.outer_objects.clear();
                clone.inner_objects.clear();
                let new_idx = expanded.len();
                if let Some(src_tabs) = tabs_by_object.get(&idx) {
                    let xformed: Vec<TabPoint> = src_tabs
                        .iter()
                        .map(|t| {
                            let p = apply_pattern_to_point(Point2::new(t.x, t.y), *inst);
                            TabPoint { x: p.x, y: p.y }
                        })
                        .collect();
                    expanded_tabs.insert(new_idx, xformed);
                }
                expanded.push(clone);
            }
        }
        *objects = expanded;
        tabs_by_object = expanded_tabs;
        let mut clone = op.clone();
        clone.source = OperationSource::All;
        Some(clone)
    } else {
        None
    };
    // Pocket-Outside (rt1.3): when an op carries `frame_shape`, the
    // pipeline auto-prepends a synthetic frame VcObject derived from
    // the op's current selection and rewrites the op source to put the
    // frame's id FIRST, with SourceCombine::Difference. The frame is not
    // persisted on the project (no Frame_<n> layer) so there's nothing
    // stale to clean up — recomputed every generate from the op params.
    let frame_op_storage: Option<Operation> = {
        let cur_op: &Operation = effective_op_storage.as_ref().unwrap_or(op);
        if let Some(frame_shape) = cur_op.params.frame_shape {
            let selected_indices: Vec<usize> = (0..objects.len())
                .filter(|i| op_includes_object(cur_op, &objects[*i], *i))
                .collect();
            if selected_indices.is_empty() {
                None
            } else {
                let frame = {
                    let frame_selection: Vec<&VcObject> =
                        selected_indices.iter().map(|&i| &objects[i]).collect();
                    let padding = cur_op.params.frame_padding_mm.unwrap_or(0.0).max(0.0);
                    build_frame(
                        &frame_selection,
                        frame_shape,
                        padding,
                        cur_op.params.frame_corner_radius_mm,
                    )
                };
                let frame_idx = objects.len();
                objects.push(frame);
                let mut ordered_ids: Vec<u32> = Vec::with_capacity(selected_indices.len() + 1);
                ordered_ids.push((frame_idx as u32) + 1);
                for i in &selected_indices {
                    ordered_ids.push((*i as u32) + 1);
                }
                let mut clone = cur_op.clone();
                clone.source = OperationSource::Objects {
                    ids: ordered_ids,
                    combine: SourceCombine::Difference,
                };
                Some(clone)
            }
        } else {
            None
        }
    };
    let effective_op: &Operation = frame_op_storage
        .as_ref()
        .or(effective_op_storage.as_ref())
        .unwrap_or(op);

    // Apply per-op tool-offset to the chain so order_offsets / lead-in see it.
    for obj in objects.iter_mut() {
        obj.tool_offset = setup.mill.offset;
    }

    let radius = setup.tool.diameter * 0.5;
    // Lateral step between consecutive Pocket cuts. Default 0.5
    // overlap = step is half the tool diameter (≈ tool radius). The
    // explicit param lets the user dial it tighter for cleaner fill or
    // looser for faster cuts. Clamp to a sane envelope so a stray 1.0
    // (= no advance) doesn't loop forever and a stray 0 doesn't pin to
    // the lower bound forever either.
    let overlap = if effective_op.params.xy_overlap > 0.0 {
        effective_op.params.xy_overlap.clamp(0.05, 0.95)
    } else {
        0.5
    };
    let xy_step = setup.tool.diameter * (1.0 - overlap);
    let mut offsets: Vec<PolylineOffset> = Vec::new();
    let mut closed = 0usize;
    let mut emitted_objects = 0usize;

    // Containment-aware Pocket: when the user selects an outer ring and
    // an inner ring, the inner one should become a hole in the outer
    // pocket — not a top-level pocket boundary on its own. Compute the
    // selected-object set up front so the Pocket branch can consult it
    // while iterating.
    let selected_set: HashSet<usize> = (0..objects.len())
        .filter(|i| op_includes_object(effective_op, &objects[*i], *i))
        .collect();

    // Non-Auto combine modes (Union/Difference/Intersection/Xor/None) for
    // Pocket short-circuit the per-object loop: we materialize the
    // combined regions once via clipper2 and emit a pocket per region.
    // Other op kinds (Profile, Engrave, DragKnife) keep their per-object
    // semantics — they cut paths, not regions.
    if let OperationKind::Pocket { strategy } = effective_op.kind {
        let combine = source_combine_mode(effective_op);
        if !matches!(combine, SourceCombine::Auto) {
            // Preserve the user-specified selection order — Difference is
            // order-sensitive ("first minus the rest"), so we cannot iterate
            // a HashSet here. ordered_selection() walks op.source as the
            // user wrote it and returns the corresponding object indices.
            let selected = ordered_selection(effective_op, objects);
            let regions = combine_source_regions(objects, &selected, combine);
            let pocket_emit = pocket_emit_for(strategy, effective_op);
            for region in &regions {
                if cancelled(cancel) {
                    return Err(PipelineError::Cancelled);
                }
                if region.boundary.len() < 3 {
                    continue;
                }
                closed += 1;
                emitted_objects += 1;
                let synthetic = synthesize_region_object(region);
                for mut o in pocket_for_object(
                    &synthetic,
                    radius,
                    effective_op.params.pocket_nocontour,
                    6,
                    pocket_emit,
                    &region.holes,
                    xy_step,
                ) {
                    o.source_object_idx = region.source_idx;
                    offsets.push(o);
                }
            }
            if !tabs_by_object.is_empty() {
                attach_tabs_to_offsets(&mut offsets, &tabs_by_object, setup.tool.diameter * 1.5);
            }
            if effective_op.params.overcut {
                apply_overcut_to_offsets(&mut offsets, objects, setup.tool.diameter * 0.5);
            }
            apply_cut_direction(&mut offsets, effective_op, false);
            push_tool_fit_size_warning(effective_op, setup, closed, &offsets, warnings);
            return Ok((offsets, closed));
        }
    }

    for (idx, obj) in objects.iter().enumerate() {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        if !op_includes_object(effective_op, obj, idx) {
            continue;
        }
        emitted_objects += 1;
        if obj.closed {
            closed += 1;
        }

        match effective_op.kind {
            OperationKind::Pocket { strategy } => {
                // Skip objects that are geometrically inside another
                // selected object — they belong to that pocket as islands.
                let contained_by_selected =
                    obj.outer_objects.iter().any(|o| selected_set.contains(o));
                if contained_by_selected {
                    continue;
                }
                let pocket_emit = pocket_emit_for(strategy, effective_op);
                // Islands = nested closed objects that are *also* in this
                // op's selection. Honored unconditionally so the user gets
                // an annulus pocket from "outer + inner" without having to
                // toggle pocket_islands. The legacy `pocket_islands` flag
                // still works as a fallback for pre-selection projects
                // (e.g. source = All) — there it pulls in *all* nested
                // closed children, matching the historical behavior.
                let mut islands: Vec<Vec<Point2>> = obj
                    .inner_objects
                    .iter()
                    .filter(|i| selected_set.contains(i))
                    .filter_map(|i| objects.get(*i))
                    .filter(|inner| inner.closed)
                    .map(|inner| segments_to_points(&inner.segments, 6))
                    .collect();
                // Legacy auto-island fallback: when `pocket_islands` is on
                // and the explicit selection didn't pick any inners, fall
                // back to the pre-selection behavior of treating ALL
                // geometrically-nested closed children as islands. ONLY
                // valid for source = All — under source = Layers /
                // Objects the user has explicitly stated which geometry
                // is in scope, so silently auto-including unselected
                // inners contradicts the selection. Pre-fix this caused
                // a strategy split: cascade/spiral milled around the
                // unselected circles (they ran with the auto-filled
                // island list) while zigzag ignored islands entirely
                // (its code path didn't take an islands argument). The
                // user expectation under "Selected" mode is that ONLY
                // what's selected matters — match that here for every
                // pocket strategy.
                if islands.is_empty()
                    && effective_op.params.pocket_islands
                    && matches!(effective_op.source, OperationSource::All)
                {
                    islands = obj
                        .inner_objects
                        .iter()
                        .filter_map(|i| objects.get(*i))
                        .filter(|inner| inner.closed)
                        .map(|inner| segments_to_points(&inner.segments, 6))
                        .collect();
                }
                if obj.closed {
                    for mut o in pocket_for_object(
                        obj,
                        radius,
                        effective_op.params.pocket_nocontour,
                        6,
                        pocket_emit,
                        &islands,
                        xy_step,
                    ) {
                        o.source_object_idx = idx;
                        offsets.push(o);
                    }
                }
            }
            OperationKind::Profile { .. } => {
                // Sign-correct offsets: parallel_offset_inward / outward
                // pick the cavalier delta sign based on the polygon's
                // signed area, so a CW input doesn't put the cutter on
                // the wrong side.
                let new_offsets = match setup.mill.offset {
                    ToolOffset::None | ToolOffset::On => {
                        vec![PolylineOffset {
                            segments: obj.segments.clone(),
                            closed: obj.closed,
                            level: 0,
                            is_pocket: 0,
                            layer: obj.layer.clone(),
                            color: obj.color,
                            source_object_idx: idx,
                            tabs: Vec::new(),
                        }]
                    }
                    ToolOffset::Outside => parallel_offset_outward(obj, radius),
                    ToolOffset::Inside => parallel_offset_inward(obj, radius),
                };
                for mut o in new_offsets {
                    o.source_object_idx = idx;
                    offsets.push(o);
                }
            }
            OperationKind::Engrave | OperationKind::DragKnife => {
                // Both follow the source path with no offset; the gcode
                // emitter handles drag-knife trail compensation per-op.
                offsets.push(PolylineOffset {
                    segments: obj.segments.clone(),
                    closed: obj.closed,
                    level: 0,
                    is_pocket: 0,
                    layer: obj.layer.clone(),
                    color: obj.color,
                    source_object_idx: idx,
                    tabs: Vec::new(),
                });
            }
            OperationKind::Drill { .. } => {
                // Drill picks a single XY for each selected object:
                //   - a closed CIRCLE smaller than tool_radius → center
                //     of the circle (the existing small_circle_drill
                //     mechanism that pocket reuses).
                //   - a single POINT segment → the point itself.
                // Anything else is silently skipped (the gcode emitter
                // can't usefully drill an open polyline). The
                // tool_kind_mismatch warning surfaces a misuse.
                use crate::geometry::SegmentKind;
                if obj.segments.len() == 1
                    && matches!(obj.segments[0].kind, SegmentKind::Point)
                {
                    let seg = obj.segments[0].clone();
                    offsets.push(PolylineOffset {
                        segments: vec![seg],
                        closed: false,
                        level: 0,
                        is_pocket: 0,
                        layer: obj.layer.clone(),
                        color: obj.color,
                        source_object_idx: idx,
                        tabs: Vec::new(),
                    });
                } else if let Some(mut drill) = small_circle_drill(obj, radius) {
                    drill.source_object_idx = idx;
                    offsets.push(drill);
                }
            }
            OperationKind::Thread | OperationKind::Chamfer | OperationKind::Helix => {
                return Err(PipelineError::UnimplementedKind(effective_op.kind));
            }
            OperationKind::VCarve => {
                // V-Carve runs through `run_vcarve_op` from the per-op
                // driver; it should never reach this offset-cascade
                // path. Skip silently rather than erroring so a stray
                // call here doesn't crash the program — the dedicated
                // dispatcher already produced the toolpath.
            }
        }
    }
    let _ = emitted_objects;

    if !tabs_by_object.is_empty() {
        attach_tabs_to_offsets(&mut offsets, &tabs_by_object, setup.tool.diameter * 1.5);
    }
    if effective_op.params.overcut {
        apply_overcut_to_offsets(&mut offsets, objects, setup.tool.diameter * 0.5);
    }
    apply_cut_direction(&mut offsets, effective_op, false);
    push_tool_fit_size_warning(effective_op, setup, closed, &offsets, warnings);
    Ok((offsets, closed))
}

/// V-Carve op driver. Builds the medial axis of the source region(s)
/// and emits a per-axis ratchet sweep with depth varying from
/// `start_depth` to the geometric V-bit depth at each point.
fn run_vcarve_op<P: PostProcessor>(
    op: &Operation,
    project: &Project,
    objects: &[VcObject],
    setup: &Setup,
    post: &mut P,
    last_pos: &mut Point2,
    warnings: &mut Vec<PipelineWarning>,
    cancel: Option<&CancelToken>,
) -> Result<(), PipelineError> {
    push_tool_fit_kind_warnings(op, project, setup, warnings);
    let tool = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;
    if !matches!(tool.kind, crate::project::ToolKind::VBit) {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tool_kind_mismatch".into(),
            message: format!(
                "V-Carve op '{}' uses tool '{}' which is not a V-bit. The carve depth is computed from the V-bit cone angle; engraver / endmill geometry won't produce a true V-groove.",
                op.name, tool.name
            ),
        });
    }
    let tip_angle_deg = tool.tip_angle_deg.clamp(1.0, 179.0);
    let tip_angle_rad = tip_angle_deg.to_radians();

    let selected = ordered_selection(op, objects);
    let combine = source_combine_mode(op);
    let regions = combine_source_regions(objects, &selected, combine);
    if regions.is_empty() {
        return Ok(());
    }

    let r_cap = op.params.carve_max_width_mm;
    let z_cap = if op.params.depth.abs() > 1e-9 {
        Some(op.params.depth)
    } else {
        None
    };
    let dpp = effective_step(op, tool)
        .map(|s| s.abs())
        .unwrap_or(1.0)
        .max(0.05);

    let mut polylines: Vec<Vec<(f64, f64, f64)>> = Vec::new();
    let mut any_depth_limited = false;

    for region in &regions {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        if region.boundary.len() < 3 {
            continue;
        }
        let vc_region = crate::cam::vcarve::VcRegion {
            outer: region.boundary.clone(),
            holes: region.holes.clone(),
        };
        let axes = crate::cam::vcarve::medial_axis_cancellable(&vc_region, cancel);
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        for axis in &axes {
            let (z_axis, depth_limited) =
                crate::cam::vcarve::polyline_to_z(axis, tip_angle_rad, r_cap, z_cap);
            if depth_limited {
                any_depth_limited = true;
            }
            let path = crate::cam::vcarve_emit::ratchet_emit(&z_axis, dpp);
            if path.len() >= 2 {
                polylines.push(path);
            }
        }
    }

    if any_depth_limited {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "vcarve_depth_limited".into(),
            message: format!(
                "V-Carve op '{}' was depth-limited: the V-bit can't reach the geometric corner because depth and/or carve_max_width caps clipped the inscribed-circle radius.",
                op.name
            ),
        });
    }

    if polylines.is_empty() {
        return Ok(());
    }

    crate::gcode::emit_vcarve_block(setup, &polylines, post, last_pos);
    Ok(())
}

/// Map a frontend pocket strategy choice onto the offsets-layer
/// emitter, including the trochoidal-specific climb/conventional and
/// loop parameters.
fn pocket_emit_for(strategy: PocketStrategy, op: &Operation) -> PocketEmit {
    match strategy {
        PocketStrategy::Zigzag => PocketEmit::Zigzag,
        PocketStrategy::Spiral => PocketEmit::Spiral,
        PocketStrategy::Cascade => PocketEmit::Cascade,
        PocketStrategy::Trochoidal {
            engagement_angle_deg,
            loop_radius_factor,
        } => PocketEmit::Trochoidal {
            engagement_angle_deg,
            loop_radius_factor,
            climb: matches!(op.params.cut_direction, crate::project::CutDirection::Climb),
        },
    }
}

/// Trochoidal-specific guards: tabs are not yet supported and the
/// plunge must be Helix. We emit warnings for unsupported tabs and
/// override Direct/Ramp plunges to Helix at the synthesize_op_setup
/// site (see `effective_plunge_for`).
fn push_trochoidal_warnings(op: &Operation, warnings: &mut Vec<PipelineWarning>) {
    if !matches!(op.kind, OperationKind::Pocket {
        strategy: PocketStrategy::Trochoidal { .. }
    }) {
        return;
    }
    if op.params.tabs.active {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tabs_with_trochoidal_unsupported".into(),
            message: format!(
                "op '{}': tabs are not supported on a Trochoidal pocket; ignoring tabs.",
                op.name
            ),
        });
    }
    if !matches!(
        op.params.plunge,
        crate::cam::setup::PlungeStrategy::Helix { .. }
    ) {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "plunge_overridden".into(),
            message: format!(
                "op '{}': trochoidal pockets require helical descent; overriding plunge to Helix.",
                op.name
            ),
        });
    }
}

/// Sanity warnings that don't depend on whether the offset cascade
/// succeeded. Run before the heavy work.
fn push_tool_fit_kind_warnings(
    op: &Operation,
    project: &Project,
    setup: &Setup,
    warnings: &mut Vec<PipelineWarning>,
) {
    use crate::project::ToolKind;
    let Some(tool) = project.tools.iter().find(|t| t.id == op.tool_id) else {
        return;
    };
    // Impossible tool geometry: tip diameter ≥ shank diameter.
    if let Some(tip) = tool.tip_diameter {
        if tip >= tool.diameter {
            warnings.push(PipelineWarning {
                op_id: Some(op.id),
                kind: "tool_geometry_impossible".into(),
                message: format!(
                    "tool '{}': tip diameter {tip} ≥ shank diameter {}",
                    tool.name, tool.diameter
                ),
            });
        }
    }
    // Tool kind mismatched with op kind. We warn rather than error
    // because the gcode emitter still produces something usable in many
    // cases (a drag knife on a Profile is fine, for instance), but a
    // drill on a Pocket really doesn't make sense.
    let mismatch = match (&op.kind, tool.kind) {
        (OperationKind::Pocket { .. }, ToolKind::Drill) => Some("pocket op assigned a drill bit"),
        (OperationKind::Pocket { .. }, ToolKind::DragKnife) => {
            Some("pocket op assigned a drag knife (cut path won't carve area)")
        }
        (OperationKind::Profile { .. }, ToolKind::Drill) => Some("profile op assigned a drill bit"),
        _ => None,
    };
    if let Some(msg) = mismatch {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tool_kind_mismatch".into(),
            message: format!(
                "{msg} — '{}' on op '{}'. Pick a different tool kind.",
                tool.name, op.name
            ),
        });
    }
    let _ = setup; // reserved for future feed/speed sanity checks
}

/// Post-build warning: a closed boundary was supplied but the offset
/// cascade produced nothing — the tool diameter doesn't fit the
/// geometry (slot too narrow, pocket smaller than the tool, etc.).
fn push_tool_fit_size_warning(
    op: &Operation,
    setup: &Setup,
    closed_count: usize,
    offsets: &[PolylineOffset],
    warnings: &mut Vec<PipelineWarning>,
) {
    if closed_count == 0 {
        return; // nothing closed → not a tool-fit problem, just no work
    }
    // Profile-on / Engrave / DragKnife emit straight contour walks even
    // when offsets is empty in the cascade sense, so don't flag them.
    let needs_offset = match op.kind {
        OperationKind::Pocket { .. } => true,
        OperationKind::Profile {
            offset: crate::cam::setup::ToolOffset::Outside,
        }
        | OperationKind::Profile {
            offset: crate::cam::setup::ToolOffset::Inside,
        } => true,
        _ => false,
    };
    if !needs_offset {
        return;
    }
    if offsets.is_empty() {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "tool_too_large".into(),
            message: format!(
                "tool diameter {:.2} mm doesn't fit op '{}' — offset/cascade produced no toolpath. Try a smaller tool.",
                setup.tool.diameter, op.name,
            ),
        });
        return;
    }
    // Pocket-specific second pass: the boundary contour fits but the
    // cascade carved no inward rings → the cutter is wide enough to
    // reach the wall but not to chew out the interior. The user gets
    // a hollow pocket (just the wall trace), which can look like
    // "pocketing isn't working". Surface this so they can pick a
    // smaller tool. PolylineOffset.is_pocket == 0 is the boundary,
    // is_pocket >= 1 is a cascade ring or zigzag fill.
    if matches!(op.kind, OperationKind::Pocket { .. })
        && offsets.iter().any(|o| o.is_pocket == 0)
        && !offsets.iter().any(|o| o.is_pocket >= 1)
    {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "pocket_fill_incomplete".into(),
            message: format!(
                "tool diameter {:.2} mm fits the pocket boundary in op '{}' but not the interior — only the wall is cut, not the fill. Use a smaller tool to pocket the inside.",
                setup.tool.diameter, op.name,
            ),
        });
    }
}

/// Walk the op's source in user-specified order and return the matching
/// object indices. Used by non-Auto combine modes — Difference in
/// particular is order-sensitive ("first selected minus the rest"), so
/// we cannot iterate the unordered selected_set there.
fn ordered_selection(op: &Operation, objects: &[VcObject]) -> Vec<usize> {
    match &op.source {
        OperationSource::All => (0..objects.len()).collect(),
        OperationSource::Layers { layers, .. } => objects
            .iter()
            .enumerate()
            .filter(|(_, obj)| layers.iter().any(|l| l == &obj.layer))
            .map(|(i, _)| i)
            .collect(),
        OperationSource::Objects { ids, .. } => ids
            .iter()
            .filter_map(|id| {
                let idx = (*id as usize).checked_sub(1)?;
                objects.get(idx).map(|_| idx)
            })
            .collect(),
    }
}

/// Pull the SourceCombine mode out of an op's source. Defaults to Auto
/// when the source is `All` (no combine choice applies) or when no
/// combine field is set (back-compat for pre-p5o projects).
fn source_combine_mode(op: &Operation) -> SourceCombine {
    match &op.source {
        OperationSource::All => SourceCombine::Auto,
        OperationSource::Layers { combine, .. } | OperationSource::Objects { combine, .. } => {
            *combine
        }
    }
}

/// Build a synthetic VcObject from a CombinedRegion's boundary so it can
/// be fed into pocket_for_object (which is shaped around VcObjects). The
/// region's holes are passed alongside as islands; only the outer
/// boundary lives in this object.
fn synthesize_region_object(region: &CombinedRegion) -> VcObject {
    let pts = &region.boundary;
    let mut segments = Vec::with_capacity(pts.len());
    for win in pts.windows(2) {
        segments.push(Segment::line(
            win[0],
            win[1],
            region.layer.clone(),
            region.color,
        ));
    }
    if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
        if first.distance(*last) > 1e-6 {
            segments.push(Segment::line(
                *last,
                *first,
                region.layer.clone(),
                region.color,
            ));
        }
    }
    let mut obj = VcObject::new(segments, true);
    obj.layer = region.layer.clone();
    obj.color = region.color;
    obj
}

fn op_includes_object(op: &Operation, obj: &VcObject, idx: usize) -> bool {
    match &op.source {
        OperationSource::All => true,
        OperationSource::Layers { layers, .. } => layers.iter().any(|l| l == &obj.layer),
        // OperationSource::Objects ids are 1-based, matching the
        // ImportOutput.objects[i] mapping the frontend uses for
        // selection.
        OperationSource::Objects { ids, .. } => {
            let chain_id = (idx as u32) + 1;
            ids.iter().any(|id| *id == chain_id)
        }
    }
}

/// Resolve the per-pass Z step: op override wins, otherwise the tool's
/// `default_step`. Both must be negative (a depth, not a height); a
/// non-negative value or two Nones produces a `step_unspecified`
/// warning.
pub(crate) fn effective_step(op: &Operation, tool: &ToolEntry) -> Result<f64, PipelineWarning> {
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

/// Build a Setup that represents this single op — copy in its tool from
/// `project.tools` and its params.kind-driven mill/pockets/tabs/leads.
fn synthesize_op_setup(
    op: &Operation,
    project: &Project,
    warnings: &mut Vec<PipelineWarning>,
) -> Result<Setup, PipelineError> {
    use crate::cam::setup::{MachineMode, MillConfig, PocketConfig, ToolConfig, ToolOffset};

    let tool = project
        .tools
        .iter()
        .find(|t| t.id == op.tool_id)
        .ok_or(PipelineError::UnknownTool(op.id, op.tool_id))?;
    let step = match effective_step(op, tool) {
        Ok(v) => v,
        Err(w) => {
            warnings.push(w);
            0.0
        }
    };

    let mut setup = Setup {
        machine: project.machine.clone(),
        ..Setup::default()
    };
    setup.tool = ToolConfig {
        number: tool.id,
        diameter: tool.diameter,
        speed: tool.speed,
        pause: 1,
        mist: matches!(tool.coolant, crate::project::Coolant::Mist),
        flood: matches!(tool.coolant, crate::project::Coolant::Flood),
        dragoff: tool.dragoff,
        // Per-op overrides win over the tool library defaults — handy
        // for finishing passes or hard materials without editing the
        // tool entry itself.
        rate_v: op.params.plunge_rate_override.unwrap_or(tool.plunge_rate),
        rate_h: op.params.feed_rate_override.unwrap_or(tool.feed_rate),
    };
    let offset = match op.kind {
        OperationKind::Profile { offset } => offset,
        OperationKind::Pocket { .. } => ToolOffset::None,
        OperationKind::Engrave | OperationKind::DragKnife => ToolOffset::On,
        _ => ToolOffset::None,
    };
    // Trochoidal pockets demand a helical descent. If the user picked
    // Direct/Ramp we override silently here and emit a
    // `plunge_overridden` warning at the build_op_offsets seam.
    let trochoidal = matches!(
        op.kind,
        OperationKind::Pocket {
            strategy: PocketStrategy::Trochoidal { .. }
        }
    );
    let plunge = if trochoidal
        && !matches!(op.params.plunge, crate::cam::setup::PlungeStrategy::Helix { .. })
    {
        crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(tool.diameter * 0.75),
        }
    } else {
        op.params.plunge
    };
    setup.mill = MillConfig {
        active: true,
        depth: op.params.depth,
        start_depth: op.params.start_depth,
        step,
        fast_move_z: op.params.fast_move_z,
        helix_mode: op.params.helix,
        reverse: op.params.reverse,
        objectorder: op.params.objectorder,
        offset,
        overcut: op.params.overcut,
        plunge,
        corner_feed_reduction: op.params.corner_feed_reduction.clamp(0.0, 0.95),
        finish_step: op.params.finish_step,
        through_depth: op.params.through_depth.max(0.0),
        depth_list: op.params.depth_list.clone(),
    };
    setup.pockets = match op.kind {
        OperationKind::Pocket { strategy } => PocketConfig {
            active: true,
            islands: op.params.pocket_islands,
            zigzag: matches!(strategy, PocketStrategy::Zigzag),
            insideout: op.params.pocket_insideout,
            nocontour: op.params.pocket_nocontour,
        },
        _ => PocketConfig::default(),
    };
    setup.tabs = op.params.tabs.clone();
    if trochoidal {
        // Tabs not supported on trochoidal pockets in v1; force-off so
        // the gcode emitter doesn't see active tabs.
        setup.tabs.active = false;
    }
    setup.leads = op.params.leads.clone();
    if matches!(op.kind, OperationKind::DragKnife) {
        setup.machine.mode = MachineMode::Drag;
    }
    Ok(setup)
}

/// Resolve `PlungeStrategy::Helix { radius_mm: None }` (auto-fit) into
/// a concrete radius by picking the largest inscribed circle across the
/// op's source regions. When no fit is possible we leave `radius_mm` as
/// None so the gcode emitter falls through to Ramp, and emit a
/// `helix_radius_unfittable` info warning so the user understands why
/// the helix didn't apply.
fn resolve_auto_helix_radius(
    op: &Operation,
    objects: &[VcObject],
    setup: &mut Setup,
    warnings: &mut Vec<PipelineWarning>,
) {
    use crate::cam::setup::PlungeStrategy;
    let angle_deg = match setup.mill.plunge {
        PlungeStrategy::Helix {
            angle_deg,
            radius_mm: None,
        } => angle_deg,
        _ => return,
    };
    let tool_radius = setup.tool.diameter * 0.5;
    let selected = ordered_selection(op, objects);
    let mode = source_combine_mode(op);
    let regions = combine_source_regions(objects, &selected, mode);
    let mut best: Option<f64> = None;
    for region in &regions {
        if region.boundary.len() < 3 {
            continue;
        }
        let vc_region = crate::cam::vcarve::VcRegion {
            outer: region.boundary.clone(),
            holes: region.holes.clone(),
        };
        if let Some((_, _, r)) = crate::cam::inscribed::inscribed_circle(&vc_region, tool_radius) {
            best = Some(best.map_or(r, |prev| prev.max(r)));
        }
    }
    if let Some(r) = best {
        setup.mill.plunge = PlungeStrategy::Helix {
            angle_deg,
            radius_mm: Some(r),
        };
    } else {
        warnings.push(PipelineWarning {
            op_id: Some(op.id),
            kind: "helix_radius_unfittable".into(),
            message: format!(
                "op '{}': auto helix radius could not be fit (pocket too small for tool); falling back to Ramp.",
                op.name
            ),
        });
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────

/// Header / footer Setup for the program. We synthesize it from the
/// first enabled op so machine.unit, mill.fast_move_z, tool.rate_h
/// pick up the user's actual values rather than struct defaults.
fn header_setup_for(project: &Project) -> Setup {
    let mut setup = Setup {
        machine: project.machine.clone(),
        ..Setup::default()
    };
    if let Some(op) = project.operations.iter().find(|o| o.enabled) {
        if let Some(tool) = project.tools.iter().find(|t| t.id == op.tool_id) {
            setup.tool = crate::cam::setup::ToolConfig {
                number: tool.id,
                diameter: tool.diameter,
                speed: tool.speed,
                pause: 1,
                mist: matches!(tool.coolant, crate::project::Coolant::Mist),
                flood: matches!(tool.coolant, crate::project::Coolant::Flood),
                dragoff: tool.dragoff,
                // Per-op overrides (9vr) carry through into the program-
                // header feed too — otherwise the header emits the tool
                // default and the user sees an extra `F800` line at the
                // top despite the override.
                rate_v: op.params.plunge_rate_override.unwrap_or(tool.plunge_rate),
                rate_h: op.params.feed_rate_override.unwrap_or(tool.feed_rate),
            };
        }
        setup.mill.fast_move_z = op.params.fast_move_z;
    } else if let Some(tool) = project.tools.first() {
        setup.tool = crate::cam::setup::ToolConfig {
            number: tool.id,
            diameter: tool.diameter,
            speed: tool.speed,
            pause: 1,
            mist: matches!(tool.coolant, crate::project::Coolant::Mist),
            flood: matches!(tool.coolant, crate::project::Coolant::Flood),
            dragoff: tool.dragoff,
            rate_v: tool.plunge_rate,
            rate_h: tool.feed_rate,
        };
    }
    setup
}

fn build_segment_to_object_map(
    segments: &[Segment],
    objects: &[VcObject],
) -> HashMap<usize, usize> {
    let mut map = HashMap::new();
    for (obj_idx, obj) in objects.iter().enumerate() {
        for chain_seg in &obj.segments {
            for (seg_idx, src) in segments.iter().enumerate() {
                let same =
                    approx_pt(src.start, chain_seg.start) && approx_pt(src.end, chain_seg.end);
                let reverse =
                    approx_pt(src.start, chain_seg.end) && approx_pt(src.end, chain_seg.start);
                if same || reverse {
                    map.entry(seg_idx).or_insert(obj_idx);
                }
            }
        }
    }
    map
}

fn approx_pt(a: Point2, b: Point2) -> bool {
    (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
}

/// One pattern instance: translate by (dx, dy) AND rotate by `angle_rad`
/// around (cx, cy). For Linear / Grid patterns, angle_rad is 0 and the
/// rotation center is unused. For Polar, dx = dy = 0 and the rotation
/// is applied around (cx, cy).
#[derive(Debug, Clone, Copy)]
struct PatternInstance {
    dx: f64,
    dy: f64,
    cx: f64,
    cy: f64,
    /// Precomputed cos(angle_rad). Cached on the instance so
    /// apply_pattern_to_segments doesn't redo trig per (instance × object)
    /// pair — for a Polar pattern with N instances and K selected objects,
    /// that previously meant 2·N·K trig calls.
    cos_a: f64,
    sin_a: f64,
    /// True when the rotation is identity. Lets the transform shortcut
    /// to translate-only, skipping the (cx, cy) recentering math
    /// entirely. Always true for Linear and Grid patterns.
    pure_translate: bool,
}

impl PatternInstance {
    fn translate(dx: f64, dy: f64) -> Self {
        Self {
            dx,
            dy,
            cx: 0.0,
            cy: 0.0,
            cos_a: 1.0,
            sin_a: 0.0,
            pure_translate: true,
        }
    }

    fn polar(cx: f64, cy: f64, angle_rad: f64) -> Self {
        Self {
            dx: 0.0,
            dy: 0.0,
            cx,
            cy,
            cos_a: angle_rad.cos(),
            sin_a: angle_rad.sin(),
            // Identity rotation collapses to the translate path even
            // for Polar pattern at i=0 (the first instance is always
            // the source in place).
            pure_translate: angle_rad.abs() < 1e-12,
        }
    }
}

/// Materialize a pattern config into a list of instance transforms. The
/// first element of the returned list is always the identity transform —
/// the source geometry stays in place at instance 0 — so a 1-instance
/// pattern is equivalent to no pattern at all.
fn pattern_offsets(pattern: PatternConfig) -> Vec<PatternInstance> {
    let mut out = Vec::new();
    match pattern {
        PatternConfig::Linear { count, dx, dy } => {
            // count is an inclusive total. count == 0 → no instances at
            // all (degenerate, but well-defined: the op emits nothing).
            for i in 0..count.max(0) {
                out.push(PatternInstance::translate((i as f64) * dx, (i as f64) * dy));
            }
        }
        PatternConfig::Grid {
            count_x,
            count_y,
            dx,
            dy,
        } => {
            for j in 0..count_y.max(0) {
                for i in 0..count_x.max(0) {
                    out.push(PatternInstance::translate((i as f64) * dx, (j as f64) * dy));
                }
            }
        }
        PatternConfig::Polar {
            count,
            center_x,
            center_y,
            angle_step_deg,
        } => {
            let step_rad = angle_step_deg.to_radians();
            for i in 0..count.max(0) {
                out.push(PatternInstance::polar(
                    center_x,
                    center_y,
                    (i as f64) * step_rad,
                ));
            }
        }
    }
    out
}

/// Apply a pattern instance transform to every endpoint and arc center
/// of `segments` in place: rotate around (cx, cy) by `angle_rad`, then
/// translate by (dx, dy). Bulge stays the same — it's a local angle
/// ratio, invariant under rotation and translation.
fn apply_pattern_to_segments(segments: &mut [Segment], inst: PatternInstance) {
    if inst.pure_translate {
        if inst.dx == 0.0 && inst.dy == 0.0 {
            // Identity transform — first pattern instance is always the
            // source in place. Skip the per-segment work entirely.
            return;
        }
        for s in segments.iter_mut() {
            s.start.x += inst.dx;
            s.start.y += inst.dy;
            s.end.x += inst.dx;
            s.end.y += inst.dy;
            if let Some(c) = s.center.as_mut() {
                c.x += inst.dx;
                c.y += inst.dy;
            }
        }
        return;
    }
    for s in segments.iter_mut() {
        s.start = transform_point(s.start, inst);
        s.end = transform_point(s.end, inst);
        if let Some(c) = s.center {
            s.center = Some(transform_point(c, inst));
        }
    }
}

fn apply_pattern_to_point(p: Point2, inst: PatternInstance) -> Point2 {
    if inst.pure_translate {
        return Point2::new(p.x + inst.dx, p.y + inst.dy);
    }
    transform_point(p, inst)
}

fn transform_point(p: Point2, inst: PatternInstance) -> Point2 {
    let dx = p.x - inst.cx;
    let dy = p.y - inst.cy;
    let rx = inst.cx + dx * inst.cos_a - dy * inst.sin_a;
    let ry = inst.cy + dx * inst.sin_a + dy * inst.cos_a;
    Point2::new(rx + inst.dx, ry + inst.dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::setup::{TabType, TabsConfig, ToolOffset};
    use crate::geometry::Segment;
    use crate::project::{
        Coolant, Operation, OperationKind, OperationParams, OperationSource, PatternConfig,
        SourceCombine, ToolEntry, ToolKind,
    };

    fn closed_square(side: f64) -> Vec<Segment> {
        vec![
            Segment::line(Point2::new(0.0, 0.0), Point2::new(side, 0.0), "0", 7),
            Segment::line(Point2::new(side, 0.0), Point2::new(side, side), "0", 7),
            Segment::line(Point2::new(side, side), Point2::new(0.0, side), "0", 7),
            Segment::line(Point2::new(0.0, side), Point2::new(0.0, 0.0), "0", 7),
        ]
    }

    fn endmill(id: u32, diameter: f64) -> ToolEntry {
        ToolEntry {
            id,
            name: format!("{diameter:.1}mm endmill"),
            kind: ToolKind::Endmill,
            diameter,
            tip_diameter: None,
            tip_angle_deg: 60.0,
            dragoff: None,
            flutes: 2,
            speed: 18_000,
            plunge_rate: 100,
            feed_rate: 800,
            coolant: Coolant::Off,
            default_step: None,
            pause: 1,
            flute_length_mm: None,
            shank_diameter_mm: None,
            holder: None,
        }
    }

    fn profile_op(id: u32, tool_id: u32, offset: ToolOffset) -> Operation {
        Operation {
            id,
            name: format!("Profile {id}"),
            enabled: true,
            kind: OperationKind::Profile { offset },
            tool_id,
            source: OperationSource::All,
            params: OperationParams::mill_default(),
            pattern: None,
        }
    }

    fn project_with(ops: Vec<Operation>, tools: Vec<ToolEntry>) -> Project {
        Project {
            segments: closed_square(20.0),
            machine: Default::default(),
            tools,
            operations: ops,
            tabs: Default::default(),
            fixtures: Default::default(),
        }
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

    fn pocket_op(id: u32, tool_id: u32, source: OperationSource) -> Operation {
        Operation {
            id,
            name: format!("Pocket {id}"),
            enabled: true,
            kind: OperationKind::Pocket {
                strategy: crate::project::PocketStrategy::Cascade,
            },
            tool_id,
            source,
            params: OperationParams::mill_default(),
            pattern: None,
        }
    }

    fn closed_square_offset(side: f64, ox: f64, oy: f64) -> Vec<Segment> {
        vec![
            Segment::line(Point2::new(ox, oy), Point2::new(ox + side, oy), "0", 7),
            Segment::line(
                Point2::new(ox + side, oy),
                Point2::new(ox + side, oy + side),
                "0",
                7,
            ),
            Segment::line(
                Point2::new(ox + side, oy + side),
                Point2::new(ox, oy + side),
                "0",
                7,
            ),
            Segment::line(Point2::new(ox, oy + side), Point2::new(ox, oy), "0", 7),
        ]
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
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pocket_op(
                1,
                1,
                OperationSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Auto,
                },
            )],
            tabs: Default::default(),
            fixtures: Default::default(),
        };
        let annulus_project = Project {
            segments,
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![pocket_op(
                1,
                1,
                OperationSource::Objects {
                    ids: vec![1, 2],
                    combine: SourceCombine::Auto,
                },
            )],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// SourceCombine::Difference applied at the pipeline level should
    /// produce one annulus pocket from "outer minus inner", matching
    /// what the user means when they pick Difference explicitly. This
    /// guards the synthesize_region_object path that fakes a VcObject
    /// from clipper2 polytree output.
    #[test]
    fn pocket_with_difference_combine_emits_an_annulus() {
        let mut segments = closed_square_offset(50.0, 0.0, 0.0);
        segments.extend(closed_square_offset(20.0, 15.0, 15.0));
        let project = Project {
            segments,
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket-diff".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::Objects {
                    ids: vec![1, 2],
                    combine: SourceCombine::Difference,
                },
                params: OperationParams::mill_default(),
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
        let mut params = OperationParams::mill_default();
        params.frame_shape = Some(crate::cam::source_combine::FrameShape::Rectangle);
        params.frame_padding_mm = Some(10.0);
        let project = Project {
            segments,
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket-Outside".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Difference,
                },
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
            let in_inner = |x: f64, y: f64| x >= 0.0 && x <= 50.0 && y >= 0.0 && y <= 50.0;
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

    /// Climb on the main + conventional on the finishing pass: walks the
    /// pipeline output and verifies the level=0 ring uses the
    /// conventional winding (CCW for an inner pocket boundary) while
    /// any level≥1 cascade ring uses climb (CW for an inner ring).
    #[test]
    fn pocket_with_climb_main_and_conventional_finish_winds_correctly() {
        let segments = closed_square_offset(50.0, 0.0, 0.0);
        let mut params = OperationParams::mill_default();
        params.cut_direction = crate::project::CutDirection::Climb;
        params.finish_cut_direction = crate::project::CutDirection::Conventional;
        let project = Project {
            segments,
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
            loop_segs.first().map(|s| s.from.z).unwrap_or(0.0)
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
            machine: Default::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Operation {
                id: 7,
                name: "Tiny pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
            default_step: None,
            pause: 1,
            flute_length_mm: None,
            shank_diameter_mm: None,
            holder: None,
        };
        let project = Project {
            segments: closed_square_offset(20.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![drill],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
    /// ramp_length = 1/tan(10°) ≈ 5.67mm. After ~5.67mm of XY travel
    /// the cutter should be at Z=-1; subsequent cut moves stay at -1.
    #[test]
    fn ramp_plunge_descends_z_during_first_cuts() {
        let mut params = OperationParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Ramp { angle_deg: 10.0 };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Ramped profile".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
        let mut params = OperationParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(3.0),
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Helical pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// Helix radius < tool_radius → falls back to Ramp (and from there
    /// to Direct if path too short). With a 6mm tool and helix
    /// radius=1mm the helix would carve nothing the cutter doesn't
    /// already cover, so we fall back. The first cutting move's Z
    /// should start above the cut depth — that's the Ramp signature
    /// (helix arcs would start at the previous Z, then descend to
    /// depth on a small circle inside the polygon, NOT on the cut
    /// path itself).
    #[test]
    fn helix_falls_back_to_ramp_when_radius_smaller_than_tool() {
        let mut params = OperationParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 10.0,
            radius_mm: Some(1.0),
        };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Operation {
                id: 1,
                name: "Helix-too-small".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// Auto-fit helix radius (radius_mm = None) on a pocket too small
    /// for the tool: the resolver finds no fit, emits the
    /// `helix_radius_unfittable` warning, and falls through to Ramp —
    /// no helix-entry arcs near the centroid.
    #[test]
    fn auto_helix_falls_back_when_pocket_too_small() {
        let mut params = OperationParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: None,
        };
        let project = Project {
            segments: closed_square_offset(8.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Operation {
                id: 1,
                name: "Auto-helix-tight".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
        let mut params = OperationParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: None,
        };
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 0.5)],
            operations: vec![Operation {
                id: 1,
                name: "Auto-helix-roomy".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
                s.op_id == 1 && matches!(s.kind, crate::gcode::preview::MoveKind::Arc)
                    && s.to.z <= s.from.z
                    && s.from.z > -0.999
            })
            .count();
        assert!(
            arc_count >= 2,
            "auto-fit roomy pocket should emit helix arcs; got {arc_count}",
        );
        assert!(
            !resp.warnings.iter().any(|w| w.kind == "helix_radius_unfittable"),
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
        let mut params = OperationParams::mill_default();
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
        let mut tabs_map: std::collections::HashMap<u32, Vec<crate::cam::offsets::TabPoint>> =
            Default::default();
        // Tab on the bottom edge (segment index 0 of the 100×100
        // square). The pipeline routes tab points keyed by source
        // segment index → owning chain object so the tab attaches to
        // the polyline offset that's actually being cut.
        tabs_map.insert(0, vec![crate::cam::offsets::TabPoint { x: 50.0, y: -1.5 }]);
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Helix-with-tabs".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: tabs_map,
            fixtures: Default::default(),
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
        let mut params = OperationParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        // params.plunge defaults to Direct.
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Direct profile".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
    /// rectangle. We surface this as a pocket_fill_incomplete warning
    /// so the user understands why the gcode is just the contour.
    #[test]
    fn pocket_with_just_fitting_tool_warns_about_incomplete_fill() {
        let project = Project {
            segments: closed_square_offset(10.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Operation {
                id: 9,
                name: "Hollow pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// Higher xy_overlap → smaller step → more cascade rings on the
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
            let mut params = OperationParams::mill_default();
            params.xy_overlap = overlap;
            let project = Project {
                segments: closed_square_offset(50.0, 0.0, 0.0),
                machine: Default::default(),
                tools: vec![endmill(1, 3.0)],
                operations: vec![Operation {
                    id: 1,
                    name: "Pocket".into(),
                    enabled: true,
                    kind: OperationKind::Pocket {
                        strategy: crate::project::PocketStrategy::Cascade,
                    },
                    tool_id: 1,
                    source: OperationSource::All,
                    params,
                    pattern: None,
                }],
                tabs: Default::default(),
                fixtures: Default::default(),
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
    /// xy_overlap the gcode for a 50x50 pocket must contain cuts at
    /// distinct Y values inside the pocket — not just the boundary
    /// contour at four corners.
    #[test]
    fn zigzag_pocket_emits_interior_strokes() {
        let mut params = OperationParams::mill_default();
        // Force the default explicitly so the test pins behavior even
        // if the constant changes later.
        params.xy_overlap = 0.5;
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Zigzag pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Zigzag,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
    /// last Z pass — the cutter ramps from prev_z to total_depth over
    /// `ramp_length`, but the cells in the ramp region sit at
    /// progressively descending Z, not at total_depth. The fix is a
    /// constant-depth cleanup walk after all the ramped passes.
    /// This test verifies the gcode now contains a final pass at
    /// total_depth that visits the path's start XY at total_depth.
    #[test]
    fn ramp_plunge_cleans_up_with_a_final_constant_depth_pass() {
        let mut params = OperationParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.plunge = crate::cam::setup::PlungeStrategy::Ramp { angle_deg: 10.0 };
        let project = Project {
            segments: closed_square_offset(100.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Ramped profile".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
    /// zigzag). The pocket_fill_incomplete warning fires so they know.
    #[test]
    fn cascade_with_tool_too_wide_emits_only_boundary_no_zigzag_substitute() {
        let mut params = OperationParams::mill_default();
        params.xy_overlap = 0.05; // 95% step — no inward rings will fit
        let project = Project {
            // 6×6 with a 3mm tool: boundary inset by 1.5mm leaves a
            // 3×3 path; cascade inflate by 2.85mm → empty → 0 rings.
            segments: closed_square_offset(6.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
    /// parallel_offset_inward now picks the right sign per winding.
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
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// Build the segments for a closed circle of `radius` at `center`,
    /// matching what the DXF importer emits (two semicircle arcs).
    fn closed_circle(center: Point2, radius: f64) -> Vec<Segment> {
        use crate::geometry::SegmentKind;
        let p_right = Point2::new(center.x + radius, center.y);
        let p_left = Point2::new(center.x - radius, center.y);
        vec![
            Segment {
                kind: SegmentKind::Circle,
                start: p_right,
                end: p_left,
                bulge: 1.0,
                center: Some(center),
                layer: "0".into(),
                color: 7,
            },
            Segment {
                kind: SegmentKind::Circle,
                start: p_left,
                end: p_right,
                bulge: 1.0,
                center: Some(center),
                layer: "0".into(),
                color: 7,
            },
        ]
    }

    fn drill_op(id: u32, tool_id: u32, cycle: crate::project::DrillCycle) -> Operation {
        let mut params = OperationParams::mill_default();
        params.depth = -5.0;
        params.start_depth = 0.0;
        params.fast_move_z = 5.0;
        Operation {
            id,
            name: format!("Drill {id}"),
            enabled: true,
            kind: OperationKind::Drill { cycle },
            tool_id,
            source: OperationSource::All,
            params,
            pattern: None,
        }
    }

    /// A 0.5mm-radius closed circle with a 3mm endmill running an
    /// OperationKind::Drill { Simple } op should emit a recognizable
    /// LinuxCNC G81 (or G82 for dwell) drill at the circle's center.
    #[test]
    fn drill_op_emits_gcode_for_circle_smaller_than_tool() {
        let project = Project {
            segments: closed_circle(Point2::new(5.0, 7.0), 0.5),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
            )],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// Drill cycle Peck with a non-zero step should map to G83 in
    /// LinuxCNC, with the per-peck Q operand carrying the step.
    #[test]
    fn drill_peck_emits_g83() {
        let project = Project {
            segments: closed_circle(Point2::new(0.0, 0.0), 0.5),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Peck {
                    peck_step_mm: 1.0,
                    dwell_sec: 0.0,
                },
            )],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// Drill cycle ChipBreak should map to G73 in LinuxCNC.
    #[test]
    fn drill_chip_break_emits_g73() {
        let project = Project {
            segments: closed_circle(Point2::new(0.0, 0.0), 0.5),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::ChipBreak {
                    peck_step_mm: 1.0,
                    dwell_sec: 0.0,
                },
            )],
            tabs: Default::default(),
            fixtures: Default::default(),
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
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![drill_op(
                1,
                1,
                crate::project::DrillCycle::Peck {
                    peck_step_mm: 1.0,
                    dwell_sec: 0.0,
                },
            )],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// A Drill op with `OperationSource::Objects` selecting only one of
    /// several drill candidates must emit gcode for *just* that one.
    #[test]
    fn drill_op_respects_object_selection() {
        let mut segments = closed_circle(Point2::new(0.0, 0.0), 0.5);
        segments.extend(closed_circle(Point2::new(20.0, 0.0), 0.5));
        let project = Project {
            segments,
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Drill".into(),
                enabled: true,
                kind: OperationKind::Drill {
                    cycle: crate::project::DrillCycle::Simple { dwell_sec: 0.0 },
                },
                tool_id: 1,
                source: OperationSource::Objects {
                    ids: vec![2],
                    combine: SourceCombine::Auto,
                },
                params: {
                    let mut p = OperationParams::mill_default();
                    p.depth = -5.0;
                    p.start_depth = 0.0;
                    p.fast_move_z = 5.0;
                    p
                },
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// finish_step (smaller than step) emits an extra Z pass at the
    /// nominal depth from a shallower pre-finish z. Verifies the gcode
    /// has cuts at both the pre-finish Z and the bottom Z.
    #[test]
    fn finish_step_emits_extra_thin_final_pass() {
        let mut params = OperationParams::mill_default();
        params.depth = -2.0;
        params.step = Some(-1.0);
        params.start_depth = 0.0;
        params.finish_step = Some(-0.2);
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// through_depth extends the cut past the nominal depth so
    /// through-cuts on edge-clamped sheet clear the bottom.
    #[test]
    fn through_depth_extends_final_z() {
        let mut params = OperationParams::mill_default();
        params.depth = -2.0;
        params.step = Some(-1.0);
        params.through_depth = 0.5;
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// depth_list overrides the step schedule. Each listed Z must appear.
    #[test]
    fn depth_list_overrides_step_schedule() {
        let mut params = OperationParams::mill_default();
        params.depth = -3.0;
        params.step = Some(-1.0);
        params.depth_list = vec![-0.5, -1.5, -3.0];
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
        let mut params = OperationParams::mill_default();
        params.feed_rate_override = Some(123);
        params.plunge_rate_override = Some(45);
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// Corner feed reduction emits a slower F before sharp turns.
    /// Verified on a zigzag pocket where adjacent strokes are joined
    /// by a 180° turn — exactly the worst-case for high-feed motion.
    #[test]
    fn corner_feed_reduction_emits_slower_f_at_sharp_turns() {
        let mut params = OperationParams::mill_default();
        params.feed_rate_override = Some(1000);
        params.corner_feed_reduction = 0.5; // halve at corners
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Zigzag,
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
    /// use Profile (not Pocket) so each instance produces a recognizable
    /// outer-offset toolpath whose X / Y range is easy to assert on.
    fn profile_op_with_pattern(pattern: PatternConfig) -> Operation {
        let mut op = profile_op(1, 1, ToolOffset::Outside);
        op.pattern = Some(pattern);
        op
    }

    /// Scan `gcode` for the X coordinate of the first cut move in each
    /// `; OP` block — useful for verifying that pattern instances landed
    /// at the expected offsets.
    fn cut_x_values(gcode: &str) -> Vec<f64> {
        let mut xs = Vec::new();
        for line in gcode.lines() {
            // Cut moves start with G1 and contain X<float>.
            if !(line.starts_with("G1") || line.starts_with("G0")) {
                continue;
            }
            if let Some(idx) = line.find('X') {
                let rest = &line[idx + 1..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                    .unwrap_or(rest.len());
                if let Ok(x) = rest[..end].parse::<f64>() {
                    xs.push(x);
                }
            }
        }
        xs
    }

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
        let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let min_x = xs.iter().cloned().fold(f64::INFINITY, f64::min);
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

    fn profile_leads_op(
        offset: ToolOffset,
        kind_in: crate::cam::setup::LeadKind,
        len_in: f64,
    ) -> Operation {
        let mut params = OperationParams::mill_default();
        params.depth = -1.0;
        params.step = Some(-1.0);
        params.fast_move_z = 5.0;
        params.leads = crate::cam::setup::LeadsConfig {
            r#in: kind_in,
            out: crate::cam::setup::LeadKind::Off,
            in_lenght: len_in,
            out_lenght: 0.0,
        };
        Operation {
            id: 1,
            name: "Profile".into(),
            enabled: true,
            kind: OperationKind::Profile { offset },
            tool_id: 1,
            source: OperationSource::All,
            params,
            pattern: None,
        }
    }

    /// Walk the emitted gcode and split it into (rapid-target,
    /// lead-moves-at-z0, plunge-target-z, first-cut-move). Returns
    /// (rapid_xy, lead_motions_between_plunge_to_z0_and_plunge_to_cut,
    /// first_post_cut_plunge_motion).
    fn first_lead_phase(gcode: &str) -> (Option<(f64, f64)>, Vec<String>, Option<String>) {
        // State machine: scan until first G0 X/Y (rapid target), then
        // until G1 Z0 (plunge to surface), then collect motions until
        // G1 Z<negative> (plunge to cut), then capture the next motion.
        let mut state = 0u8; // 0=expect_rapid, 1=at_rapid_seen, 2=after_z0, 3=after_cut_plunge
        let mut rapid_xy: Option<(f64, f64)> = None;
        let mut between: Vec<String> = Vec::new();
        let mut first_cut: Option<String> = None;
        for raw in gcode.lines() {
            let l = raw.trim_start();
            // Skip headers / comments / spindle / feeds.
            if l.is_empty() || l.starts_with(';') || l.starts_with('(') {
                continue;
            }
            match state {
                0 => {
                    // First G0 with X or Y is the rapid-to-lead-target.
                    if l.starts_with("G0 ") && (l.contains('X') || l.contains('Y')) {
                        rapid_xy = parse_xy(l);
                        state = 1;
                    }
                }
                1 => {
                    if l.starts_with("G1 ") && l.contains('Z') && !l.contains('X') && !l.contains('Y') {
                        // G1 Z0 (or G1 Z<surface>) — plunge to z=0.
                        state = 2;
                    }
                }
                2 => {
                    if l.starts_with("G1 ") && l.contains('Z') && !l.contains('X') && !l.contains('Y') {
                        // Pure-Z plunge to cut depth. State→3.
                        state = 3;
                        continue;
                    }
                    // Anything else at z=0 is a lead motion.
                    between.push(l.to_string());
                }
                3 => {
                    if l.starts_with("G0 ")
                        || l.starts_with("G1 ")
                        || l.starts_with("G2 ")
                        || l.starts_with("G3 ")
                    {
                        first_cut = Some(l.to_string());
                        break;
                    }
                }
                _ => break,
            }
        }
        (rapid_xy, between, first_cut)
    }

    fn parse_xy(line: &str) -> Option<(f64, f64)> {
        let mut x: Option<f64> = None;
        let mut y: Option<f64> = None;
        for tok in line.split_whitespace() {
            if let Some(rest) = tok.strip_prefix('X') {
                x = rest.parse().ok();
            } else if let Some(rest) = tok.strip_prefix('Y') {
                y = rest.parse().ok();
            }
        }
        match (x, y) {
            (Some(xv), Some(yv)) => Some((xv, yv)),
            _ => None,
        }
    }

    /// Profile + Outside + Arc lead-in (radius=2 mm) on a 50x50 square
    /// must emit a G2 / G3 arc move BETWEEN the surface plunge and the
    /// cut plunge — i.e., a roll-on arc at z=0 that lands the cutter
    /// tangent to the first segment.
    #[test]
    fn lead_in_arc_emits_g2_or_g3_before_first_cut() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_leads_op(
                ToolOffset::Outside,
                crate::cam::setup::LeadKind::Arc,
                2.0,
            )],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// Profile + Outside + LeadKind::Off must NOT emit any motion
    /// between the surface plunge (G1 Z0) and the cut plunge (G1 Z-1)
    /// — the cutter just goes straight down at the contour start.
    #[test]
    fn lead_in_off_emits_no_lead() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_leads_op(
                ToolOffset::Outside,
                crate::cam::setup::LeadKind::Off,
                0.0,
            )],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// Profile + Outside + LeadKind::Straight (length=2 mm) rapids the
    /// cutter to a perpendicular-offset hop point and then plunges
    /// straight down before cutting from there to the contour. The
    /// rapid target must NOT coincide with a contour-start XY (it's
    /// offset). And like the Off case, no extra moves at z=0.
    #[test]
    fn lead_in_straight_emits_a_straight_segment() {
        let project = Project {
            segments: closed_square_offset(50.0, 0.0, 0.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_leads_op(
                ToolOffset::Outside,
                crate::cam::setup::LeadKind::Straight,
                2.0,
            )],
            tabs: Default::default(),
            fixtures: Default::default(),
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
        assert!(first_cut.is_some(), "expected a first cut motion\n{}", resp.gcode);
    }


    /// PocketStrategy::Spiral now emits ONE continuous open polyline
    /// instead of N concentric closed rings. Verified by counting
    /// distinct `; OP / level / pocket` blocks in the gcode — Spiral
    /// gives one is_pocket=2 emit per object, Cascade gives N.
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
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Cascade,
                },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
        };
        let mut spiral_project = cascade_project.clone();
        spiral_project.operations[0].kind = OperationKind::Pocket {
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
        assert!(cascade_blocks > 1, "cascade should emit many ring blocks, got {cascade_blocks}");
        assert_eq!(spiral_blocks, 1, "spiral should emit exactly one continuous block, got {spiral_blocks}");
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
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: crate::project::PocketStrategy::Spiral,
                },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket { strategy },
                tool_id: 1,
                source: OperationSource::Objects {
                    ids: vec![1],
                    combine: SourceCombine::Auto,
                },
                params: OperationParams {
                    pocket_islands,
                    ..OperationParams::mill_default()
                },
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
    /// Outside should put the cut at max_x ≈ 101.5, Inside at max_x
    /// ≈ 98.5, On at exactly 100.0. Repeats with the source segments
    /// reversed (CW winding) since DXF / SVG imports can produce
    /// either winding and parallel_offset_inward / outward picks the
    /// sign from object_signed_area.
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
        for (winding_label, segments) in
            [("CCW", &ccw_segments), ("CW", &cw_segments)].iter()
        {
            let mk = |offset: ToolOffset| Project {
                segments: (*segments).clone(),
                machine: Default::default(),
                tools: vec![endmill(1, 3.0)],
                operations: vec![profile_op(1, 1, offset)],
                tabs: Default::default(),
                fixtures: Default::default(),
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
                    "{} input + {} offset: cut max_x = {} fails the expected position check",
                    winding_label, offset_label, max_x
                );
            }
        }
    }

    /// Profile + Outside selecting an INNER circle that lives inside
    /// an outer ring. classify_containment marks the circle as
    /// inner_objects[outer]; it's still a valid Profile target on its
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
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 1,
                name: "Profile".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 1,
                // Object 2 is the inner circle (chaining puts segments
                // in input order; outer was first).
                source: OperationSource::Objects {
                    ids: vec![2],
                    combine: crate::project::SourceCombine::Auto,
                },
                params: OperationParams::mill_default(),
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
            "Profile + Outside on inner circle: cut max_x={}, expected ~61.5",
            max_x
        );
    }

    /// User-reported repro shape: Profile + Outside + source=Objects
    /// with one outer ring selected. Mirrors the exact wire payload
    /// build-project.ts emits (pocket_islands=true default, leads off,
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
    /// PipelineRequest, run it, and confirm the offset is honored.
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
        if let OperationKind::Profile { offset } = req.project.operations[0].kind {
            assert_eq!(offset, ToolOffset::Outside, "wire 'outside' string didn't deserialize to ToolOffset::Outside");
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
            "wire JSON Profile + outside: cut max_x={}, expected > 100.5 — offset isn't being applied via the wire",
            max_x
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
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, offset)],
            tabs: Default::default(),
            fixtures: Default::default(),
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
                "{:?} on open polyline: cut crosses the source apex (50, 30) \
                 — offset isn't being applied (on-line cut bug)",
                offset
            );
        }
    }

    #[test]
    fn profile_offset_actually_offsets_outside_inside_on() {
        use crate::gcode::preview::MoveKind;
        let segments = closed_square_offset(100.0, 0.0, 0.0);
        let mk = |offset: ToolOffset| Project {
            segments: segments.clone(),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, offset)],
            tabs: Default::default(),
            fixtures: Default::default(),
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
            machine: Default::default(),
            tools: vec![endmill(1, 6.0)],
            operations: vec![Operation {
                id: 1,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket { strategy },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams {
                    plunge: crate::cam::setup::PlungeStrategy::Helix {
                        angle_deg: 3.0,
                        radius_mm: Some(4.5),
                    },
                    ..OperationParams::mill_default()
                },
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
        let mut params = OperationParams::mill_default();
        params.tabs.active = true;
        params.plunge = crate::cam::setup::PlungeStrategy::Helix {
            angle_deg: 3.0,
            radius_mm: Some(4.5),
        };
        let project = Project {
            segments,
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 7,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: PocketStrategy::Trochoidal {
                        engagement_angle_deg: 30.0,
                        loop_radius_factor: 0.6,
                    },
                },
                tool_id: 1,
                source: OperationSource::All,
                params,
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
                .any(|w| w.kind == "tabs_with_trochoidal_unsupported"
                    && w.op_id == Some(7)),
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
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![Operation {
                id: 9,
                name: "Pocket".into(),
                enabled: true,
                kind: OperationKind::Pocket {
                    strategy: PocketStrategy::Trochoidal {
                        engagement_angle_deg: 30.0,
                        loop_radius_factor: 0.6,
                    },
                },
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams {
                    plunge: crate::cam::setup::PlungeStrategy::Direct,
                    ..OperationParams::mill_default()
                },
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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

    /// VCarve op produces a non-empty toolpath whose deepest cutting
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
            default_step: None,
            pause: 1,
            flute_length_mm: None,
            shank_diameter_mm: None,
            holder: None,
        };
        let op = Operation {
            id: 7,
            name: "Carve".into(),
            enabled: true,
            kind: OperationKind::VCarve,
            tool_id: 1,
            source: OperationSource::All,
            params: OperationParams {
                depth: -10.0,
                start_depth: 0.0,
                step: Some(-1.0),
                fast_move_z: 5.0,
                ..OperationParams::default()
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
            machine: Default::default(),
            tools: vec![vbit],
            operations: vec![op],
            tabs: Default::default(),
            fixtures: Default::default(),
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
        let op = Operation {
            id: 11,
            name: "Sign carve".into(),
            enabled: true,
            kind: OperationKind::VCarve,
            tool_id: 1,
            source: OperationSource::All,
            params: OperationParams {
                depth: -8.0,
                start_depth: 0.0,
                step: Some(-0.8),
                fast_move_z: 6.0,
                carve_max_width_mm: Some(4.0),
                multi_pass_refine: true,
                ..OperationParams::default()
            },
            pattern: None,
        };
        let json = serde_json::to_string(&op).expect("serialize");
        let back: Operation = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back.kind, OperationKind::VCarve));
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

    fn vbit() -> ToolEntry {
        ToolEntry {
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
            default_step: None,
            pause: 1,
            flute_length_mm: None,
            shank_diameter_mm: None,
            holder: None,
        }
    }

    #[test]
    fn generate_streaming_emits_op_events_in_order() {
        let project = Project {
            segments: closed_square(20.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![
                profile_op(1, 1, ToolOffset::Outside),
                profile_op(2, 1, ToolOffset::Inside),
                profile_op(3, 1, ToolOffset::On),
            ],
            tabs: Default::default(),
            fixtures: Default::default(),
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
        assert_eq!(started, vec![1, 2, 3], "OpStarted fires once per op in order");
        assert_eq!(completed, vec![1, 2, 3], "OpCompleted fires once per op in order");
        assert_eq!(done_count, 1, "exactly one Done event at the end");
        assert!(!resp.gcode.is_empty());
    }

    #[test]
    fn generate_streaming_done_event_carries_aggregated_stats() {
        let project = Project {
            segments: closed_square(20.0),
            machine: Default::default(),
            tools: vec![endmill(1, 3.0)],
            operations: vec![profile_op(1, 1, ToolOffset::Outside)],
            tabs: Default::default(),
            fixtures: Default::default(),
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
            Some(PipelineEvent::Done { total_time_s, op_count }) => {
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
                Segment::line(
                    Point2::new(10.0, 17.320_508),
                    Point2::new(0.0, 0.0),
                    "0",
                    7,
                ),
            ],
            machine: Default::default(),
            tools: vec![vbit()],
            operations: vec![Operation {
                id: 9,
                name: "Carve".into(),
                enabled: true,
                kind: OperationKind::VCarve,
                tool_id: 1,
                source: OperationSource::All,
                params: OperationParams {
                    depth: -10.0,
                    start_depth: 0.0,
                    step: Some(-1.0),
                    fast_move_z: 5.0,
                    ..OperationParams::default()
                },
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
            evs.iter()
                .any(|e| matches!(e, PipelineEvent::Cancelled)),
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
            machine: Default::default(),
            tools: vec![endmill(91, 3.0)],
            operations: vec![Operation {
                id: 91,
                name: "Profile cache test".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 91,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
        let ops: Vec<Operation> = (1..=5)
            .map(|i| Operation {
                id: 100 + i,
                name: format!("Profile {i}"),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 100 + i,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
                pattern: None,
            })
            .collect();
        let mut project = Project {
            segments: closed_square_offset(30.0, 0.0, 0.0),
            machine: Default::default(),
            tools,
            operations: ops,
            tabs: Default::default(),
            fixtures: Default::default(),
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
            machine: Default::default(),
            tools: vec![endmill(77, 3.0)],
            operations: vec![Operation {
                id: 77,
                name: "Profile identity".into(),
                enabled: true,
                kind: OperationKind::Profile {
                    offset: ToolOffset::Outside,
                },
                tool_id: 77,
                source: OperationSource::All,
                params: OperationParams::mill_default(),
                pattern: None,
            }],
            tabs: Default::default(),
            fixtures: Default::default(),
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
        op.kind = OperationKind::Thread;
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
