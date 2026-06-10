//! Shared scaffolding for end-to-end CAM op validation tests.
//!
//! Each per-op test in `tests/<op>_volume_validation.rs` follows the
//! same shape: build a tiny project programmatically → run the
//! pipeline → carve the toolpath into a heightmap → sum the carved
//! volume → compare against the closed-form expected.
//!
//! The harness encapsulates the repeated bits — tool builders,
//! heightmap setup, sweep + volume summation, STL dump — so the test
//! body stays focused on the per-op cone / pocket / cascade math
//! being validated.
//!
//! Cast-precision lints are allowed at module scope because the
//! harness deliberately crosses f64 (geometry) / f32 (heightmap +
//! STL) / usize (grid indices) / u32 (STL header) boundaries the way
//! the underlying APIs require. Each cast site is by design.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::unnecessary_cast
)]
// Each test binary that pulls in `mod common;` consumes a different
// subset; the unused ones would otherwise trip dead-code.
#![allow(dead_code)]

use std::f64::consts::PI;
use std::fs;

use ivac_core::gcode::preview::ToolpathSegment;
use ivac_core::geometry::{Point2, Segment, SegmentKind};
use ivac_core::pipeline::{run_pipeline, PipelineRequest, PipelineResponse};
use ivac_core::project::MachineConfig;
use ivac_core::project::{
    Coolant, Op, OpKind, OpParams, OpSource, Project, SpindleDirection, StockConfig, ToolEntry,
    ToolKind, WorkOffset,
};
use ivac_core::schema::PostProcessorKind;
use ivac_core::sim::diagnostics::SimDiagnostics;
use ivac_core::sim::heightmap::{Heightmap, ToolProfile};
use ivac_core::sim::stl::heightmap_to_stl_binary;
use ivac_core::sim::sweep::sweep_range;

// ─────────────────────────────────────────────────────────────────────
// Tool builders
// ─────────────────────────────────────────────────────────────────────

/// Build a `ToolEntry` with every optional field set to None / default.
/// Callers override the kind-specific fields they care about.
fn base_tool(id: u32, name: &str, kind: ToolKind, diameter_mm: f64) -> ToolEntry {
    ToolEntry {
        id,
        name: name.into(),
        kind,
        diameter: diameter_mm,
        tip_diameter: None,
        tip_angle_deg: 60.0,
        dragoff: None,
        drag_knife_self_align_angle_deg: None,
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
        default_xy_overlap: None,
        comment: None,
        z_shift_mm: None,
        laser_pierce_sec: None,
        laser_lead_in_mm: None,
        kerf_mm: None,
        corner_radius_mm: None,
        form_profile_mm: Vec::new(),
        whirl: false,
        whirl_stepover_mm: None,
        whirl_extra_width_mm: None,
        whirl_osc_mm: None,
        pause: 1,
        flute_length_mm: None,
        length_mm: None,
        compression_transition_mm: None,
        thread_pitch_mm: None,
        shank_diameter_mm: None,
        stickout_length_mm: None,
        holder: None,
        spindle_direction: SpindleDirection::default(),
        pierce_height_mm: None,
        cut_height_mm: None,
        pierce_delay_sec: None,
        vcarve_lead_in_angle_deg: None,
    }
}

/// Flat-bottom endmill of the given diameter.
pub fn endmill_tool(id: u32, diameter_mm: f64) -> ToolEntry {
    base_tool(
        id,
        &format!("{diameter_mm}mm endmill"),
        ToolKind::Endmill,
        diameter_mm,
    )
}

/// Twist drill of the given diameter and full included tip angle (the
/// classic split-point twist drill is 118°). The pipeline drives the
/// drill `r / tan(tip_angle / 2)` past the requested shoulder depth so
/// the cone tip clears the hole bottom; the sim treats
/// `ToolKind::Drill` as a flat-bottomed cylinder, so the carved hole
/// is a clean cylinder reaching to that tip Z. Use [`drill_volume`]
/// to model the resulting cylinder volume.
pub fn drill_tool(id: u32, diameter_mm: f64, tip_angle_deg: f64) -> ToolEntry {
    let mut t = base_tool(
        id,
        &format!("{diameter_mm}mm {tip_angle_deg}° drill"),
        ToolKind::Drill,
        diameter_mm,
    );
    t.tip_angle_deg = tip_angle_deg;
    t
}

/// V-bit. `tip_angle_deg` is the full included angle (apex of the cone).
/// `tip_diameter_mm = 0` for a perfectly pointed bit; non-zero for
/// engraving / chamfering bits with a nose flat.
pub fn vbit_tool(id: u32, diameter_mm: f64, tip_angle_deg: f64, tip_diameter_mm: f64) -> ToolEntry {
    let mut t = base_tool(
        id,
        &format!("{diameter_mm}mm {tip_angle_deg}° v-bit"),
        ToolKind::VBit,
        diameter_mm,
    );
    t.tip_angle_deg = tip_angle_deg;
    t.tip_diameter = Some(tip_diameter_mm);
    t
}

// ─────────────────────────────────────────────────────────────────────
// Stock + project + sources
// ─────────────────────────────────────────────────────────────────────

/// Stock at origin (0, 0); top at `z = 0`, bottom at `z = -thickness_mm`.
pub fn stock_at_origin(width_mm: f64, height_mm: f64, thickness_mm: f64) -> StockConfig {
    StockConfig {
        origin: [0.0, 0.0],
        width_mm,
        height_mm,
        thickness_mm,
        ..Default::default()
    }
}

/// Build a tiny one-op project with the given source segments.
/// `stock` is taken by reference and cloned so the caller can keep
/// using the same `StockConfig` to size the heightmap afterwards.
pub fn build_project(
    stock: &StockConfig,
    tools: Vec<ToolEntry>,
    op: Op,
    segments: Vec<Segment>,
) -> Project {
    Project {
        segments,
        machine: MachineConfig::default(),
        tools,
        operations: vec![op],
        fixtures: Vec::default(),
        text_layers: Vec::default(),
        work_offset: WorkOffset::default(),
        stock: Some(stock.clone()),
        relief_sources: Vec::new(),
        group_ops_by_tool: false,
    }
}

/// Run the pipeline → toolpath. Panics with the underlying error.
pub fn run(project: Project, post: PostProcessorKind) -> PipelineResponse {
    run_pipeline(
        PipelineRequest {
            project,
            post_processor: Some(post),
        },
        |_, _, _| {},
    )
    .expect("pipeline runs")
}

/// Closed rectangle outline (4 CCW line segments forming a loop).
pub fn closed_rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Segment> {
    vec![
        Segment::line(Point2::new(x0, y0), Point2::new(x1, y0), "0", 7),
        Segment::line(Point2::new(x1, y0), Point2::new(x1, y1), "0", 7),
        Segment::line(Point2::new(x1, y1), Point2::new(x0, y1), "0", 7),
        Segment::line(Point2::new(x0, y1), Point2::new(x0, y0), "0", 7),
    ]
}

/// Single open line segment.
pub fn line(from: (f64, f64), to: (f64, f64)) -> Vec<Segment> {
    vec![Segment::line(
        Point2::new(from.0, from.1),
        Point2::new(to.0, to.1),
        "0",
        7,
    )]
}

/// Closed circle: two semicircle `Circle`-kind segments joined at the
/// horizontal diameter. Mirrors the pipeline `test_helpers` fixture.
pub fn closed_circle(cx: f64, cy: f64, radius: f64) -> Vec<Segment> {
    let center = Point2::new(cx, cy);
    let p_right = Point2::new(cx + radius, cy);
    let p_left = Point2::new(cx - radius, cy);
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

/// One `SegmentKind::Point` per (x, y) — the source format the Drill
/// op consumes.
pub fn points(coords: &[(f64, f64)]) -> Vec<Segment> {
    coords
        .iter()
        .map(|&(x, y)| Segment::point(Point2::new(x, y), "0", 7))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────
// Heightmap + sim + volume math
// ─────────────────────────────────────────────────────────────────────

/// Build a fresh heightmap covering the stock footprint at the given
/// XY cell size, initialised to the stock's top Z.
pub fn build_heightmap(stock: &StockConfig, cell_mm: f64) -> Heightmap {
    let cols = ((stock.width_mm / cell_mm).round() as u32) + 1;
    let rows = ((stock.height_mm / cell_mm).round() as u32) + 1;
    let origin = Point2::new(stock.origin[0], stock.origin[1]);
    Heightmap::new(origin, cell_mm, cols, rows, 0.0)
}

/// Sweep a toolpath into a heightmap with the given tool. Returns
/// `(cell_writes, diagnostics)`. Callers can assert on either.
pub fn sim_carve(
    hm: &mut Heightmap,
    toolpath: &[ToolpathSegment],
    tool: &ToolEntry,
) -> (u32, SimDiagnostics) {
    let profile = ToolProfile::from_tool(tool);
    let mut diag = SimDiagnostics::default();
    let writes = sweep_range(
        hm,
        toolpath,
        0,
        toolpath.len(),
        &profile,
        &[],
        None,
        &mut diag,
    );
    (writes, diag)
}

/// Sum `(top_z - h[i])` over every cell → carved volume in mm³.
pub fn removed_volume(hm: &Heightmap, top_z: f32) -> f64 {
    let cell_area = (hm.cell * hm.cell) as f64;
    let mut sum = 0.0;
    for &h in &hm.data {
        let drop_mm = f64::from(top_z) - f64::from(h);
        if drop_mm > 0.0 {
            sum += drop_mm;
        }
    }
    sum * cell_area
}

/// Deepest Z sample in the heightmap (should never be below
/// `-stock.thickness_mm`).
pub fn deepest_z(hm: &Heightmap) -> f32 {
    hm.data.iter().copied().fold(f32::INFINITY, f32::min)
}

/// Write the heightmap as a binary STL for hands-on inspection
/// (`FreeCAD` / `MeshLab` / Blender). Returns byte size.
///
/// Callers pass unix-style `/tmp/foo.stl` debug paths, but the suite
/// also runs on the Windows / macOS CI matrix where `/tmp` is absent —
/// so we keep only the basename and write it into the OS temp dir. That
/// keeps the dump portable (no panic on a missing `/tmp`) while still
/// landing somewhere the dev can open it.
pub fn dump_stl(hm: &Heightmap, path: &str, stock_bottom_z: f32) -> usize {
    let bytes = heightmap_to_stl_binary(hm, stock_bottom_z);
    let len = bytes.len();
    let file = std::path::Path::new(path)
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("ivac_dump.stl"));
    let out = std::env::temp_dir().join(file);
    fs::write(&out, &bytes).unwrap_or_else(|e| panic!("write STL {}: {e}", out.display()));
    len
}

// ─────────────────────────────────────────────────────────────────────
// Closed-form expected volumes
// ─────────────────────────────────────────────────────────────────────

/// Chamfer expected volume (mm³): trench of length `path_len` whose
/// V-shaped cross-section has half-width `effective_width` and depth
/// `depth`, plus `n_end_caps` half-cones (= 2 for an open path with
/// distinct endpoints, 0 for a closed loop, 1 for a half-open path).
#[must_use]
pub fn chamfer_volume(effective_width: f64, depth: f64, path_len: f64, n_end_caps: usize) -> f64 {
    let trench_area = effective_width * depth; // ½·(2W)·D
    let trench = trench_area * path_len;
    let half_cone = 0.5 * (1.0 / 3.0) * PI * effective_width * effective_width * depth;
    trench + n_end_caps as f64 * half_cone
}

/// Pocket expected volume (mm³) for a rectangular source `W × H` cut
/// to `depth` with a round endmill of `tool_radius`. The tool can
/// reach all 4 inside-corners only up to a quarter-arc fillet of
/// radius `tool_radius`; the uncut artefact per corner is
/// `r² · (1 − π/4)`. Assumes `min(W, H) ≥ 2·tool_radius`.
#[must_use]
pub fn pocket_rect_volume(width: f64, height: f64, depth: f64, tool_radius: f64) -> f64 {
    assert!(
        width.min(height) >= 2.0 * tool_radius,
        "pocket_rect_volume requires min(W,H) ≥ 2·tool_radius"
    );
    let uncut_per_corner = tool_radius * tool_radius * (1.0 - PI * 0.25);
    let carved_area = width * height - 4.0 * uncut_per_corner;
    carved_area * depth
}

/// Outside-profile-of-circle expected volume (mm³): tool centerline
/// rides at radius `R + r`, sweeping the annulus from the source
/// radius `R` outward to `R + 2r`. Annular area is
/// `π · ((R + 2r)² − R²)`.
#[must_use]
pub fn profile_outside_circle_volume(source_radius: f64, tool_radius: f64, depth: f64) -> f64 {
    let outer = source_radius + 2.0 * tool_radius;
    let area = PI * (outer * outer - source_radius * source_radius);
    area * depth
}

/// Drill volume for `n_holes` cylinders of radius `tool_radius`.
/// `hole_depth_mm` is the user-requested shoulder depth (positive);
/// the pipeline plunges an extra `r / tan(tip_angle/2)` so the cone
/// tip clears that plane, so the sim carves a cylinder of that
/// extended depth.
#[must_use]
pub fn drill_volume(
    n_holes: usize,
    tool_radius: f64,
    hole_depth_mm: f64,
    tip_angle_deg: f64,
) -> f64 {
    let half_angle_rad = (tip_angle_deg * 0.5).to_radians();
    let tip_extra = tool_radius / half_angle_rad.tan();
    let cylinder_depth = hole_depth_mm + tip_extra;
    (n_holes as f64) * PI * tool_radius * tool_radius * cylinder_depth
}

// ─────────────────────────────────────────────────────────────────────
// Convenience: minimal Op builders
// ─────────────────────────────────────────────────────────────────────

/// Build a one-pass `Op` with the given kind. Tool id, op name, and
/// pass schedule come from the caller. `depth_mm` is the cut depth
/// (negative). `step_mm` = `None` → single pass; `Some(s)` → cascade.
pub fn op_single_pass(id: u32, name: &str, kind: OpKind, tool_id: u32, depth_mm: f64) -> Op {
    let mut params = OpParams::mill_default();
    params.depth = depth_mm;
    params.step = Some(depth_mm); // single pass: step == depth
    params.fast_move_z = 5.0;
    Op {
        id,
        name: name.into(),
        enabled: true,
        kind,
        tool_id,
        finish_tool_id: None,
        source: OpSource::All,
        params,
        group: None,
        pin_order: false,
    }
}
