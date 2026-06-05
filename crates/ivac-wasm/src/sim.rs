//! Heightmap simulator bindings — wraps `ivac_core::sim::Heightmap` and
//! `sweep_range` so the frontend can drive incremental cutting preview at
//! 60 fps. JS gets a `Float32Array` view directly into WASM memory via
//! `data_ptr()`; each `advance()` call mutates cells in place and reports
//! the dirty AABB so the renderer can re-upload only the touched
//! sub-rectangle.
//!
//! Wire shapes:
//! * `segments`: serde-serialized `Vec<ToolpathSegment>` (the same shape
//!   `PipelineResponse.toolpath` already carries).
//! * `tool`: serde-serialized `ToolEntry` (same shape as the project's
//!   tool library entries — `snake_case` fields).
//!
//! Perf: the toolpath is deserialized ONCE per Generate via
//! `set_toolpath(...)` and cached on the Simulator. `advance(from, to,
//! tool)` then indexes into the cached vec — no per-frame serde of the
//! full segment array (audit-9l52). Tool stays as an `advance()` arg
//! because it's tiny and may change between ops.

// # CAM/sim pedantic-lint exemptions
// WASM-JS bridge passes cell counts as u32 (clamped at JS Number safe range);
// similar names (`row0`/`row1`, `col0`/`col1`) come from AABB-to-cell
// conversion.
#![allow(clippy::cast_possible_truncation, clippy::similar_names)]

use serde::Deserialize;
use wasm_bindgen::prelude::*;

use ivac_core::gcode::preview::ToolpathSegment;
use ivac_core::project::{Fixture, ToolEntry};
use ivac_core::sim::diagnostics::{SimDiagnostics, SimRunSummary};
use ivac_core::sim::heightmap::{Heightmap, ToolProfile};
use ivac_core::sim::holder::HolderProfile;
use ivac_core::sim::sweep::{sweep_range, sweep_segment_partial};

use crate::{into_js_error, panic_message, structured_error_to_js};

/// Owns a `Heightmap` plus enough state to apply incremental sweeps.
/// Constructed with a world-space stock bbox + cell size; the frontend
/// then calls `advance()` with slices of `PipelineResponse.toolpath` as
/// the playhead moves.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Simulator {
    heightmap: Heightmap,
    /// Warnings collected by the most recent `advance()` call. The JS
    /// driver pulls these via `take_diagnostics()` after each frame so
    /// the playbar / scene can mark offending segments. Reset on every
    /// `advance()` so each call's payload is self-contained.
    last_diagnostics: SimDiagnostics,
    /// Project-level fixtures threaded into every advance() so the
    /// fixture-collision check fires per segment. Set via
    /// `set_fixtures(...)`; default empty.
    fixtures: Vec<Fixture>,
    /// Toolpath cached at Generate time so subsequent `advance()`
    /// calls don't re-deserialize the whole array per frame
    /// (audit-9l52). Refreshed via `set_toolpath(...)` whenever a
    /// new toolpath replaces the previous one.
    toolpath: Vec<ToolpathSegment>,
    /// wpzm: sticky setup-time warnings (e.g. cell_size coarsening)
    /// that survive across `advance()` resets of `last_diagnostics`.
    /// Merged into `last_diagnostics` on every advance so the JS
    /// driver's `take_diagnostics()` keeps seeing them.
    sticky_warnings: Vec<ivac_core::sim::diagnostics::SimWarning>,
}

#[wasm_bindgen]
impl Simulator {
    /// Build a fresh simulator covering the rectangle
    /// `[min_x, max_x] × [min_y, max_y]` with `cell_size`-mm cells. Every
    /// cell starts at `top_z` (i.e. the un-cut stock surface).
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64, cell_size: f64, top_z: f32) -> Self {
        Self {
            heightmap: Heightmap::from_bbox(min_x, min_y, max_x, max_y, cell_size, top_z),
            last_diagnostics: SimDiagnostics::new(),
            fixtures: Vec::new(),
            toolpath: Vec::new(),
            sticky_warnings: Vec::new(),
        }
    }

    /// Reset every cell to `top_z` and clear the dirty AABB. Call this
    /// when a new Generate response replaces the toolpath the simulator
    /// was tracking.
    pub fn reset(&mut self) {
        self.heightmap.reset();
        self.last_diagnostics = SimDiagnostics::new();
    }

    /// Cache the full toolpath on the WASM side. Called once per
    /// Generate; subsequent `advance(...)` calls index into this vec
    /// without per-frame serde. Returns the cached segment count so the
    /// caller can assert the round-trip succeeded.
    pub fn set_toolpath(&mut self, segments: JsValue) -> Result<u32, JsValue> {
        let parsed: Vec<ToolpathSegment> =
            serde_wasm_bindgen::from_value(segments).map_err(into_js_error)?;
        let n = parsed.len() as u32;
        self.toolpath = parsed;
        Ok(n)
    }

    /// Drop the cached toolpath. Call when the project's toolpath is
    /// invalidated (e.g. the Generate response is cleared) to free
    /// WASM-side memory.
    pub fn clear_toolpath(&mut self) {
        self.toolpath = Vec::new();
    }

    /// Number of cached segments.
    #[must_use]
    pub fn toolpath_len(&self) -> u32 {
        self.toolpath.len() as u32
    }

    /// wpzm: record that the driver coarsened cell_size to fit the
    /// user's `maxSimulationCells` budget. The driver should call this
    /// once at `Simulator::new`-time when it coarsens, passing the
    /// originally-requested cell size and the coarsened one. The
    /// warning rides out via `take_diagnostics()` like any other sim
    /// warning. Stored on the simulator (NOT cleared by `advance()`)
    /// so the UI keeps seeing it across playhead changes.
    pub fn push_cell_size_coarsened(
        &mut self,
        original_cell_size_mm: f64,
        coarsened_cell_size_mm: f64,
        reason: String,
    ) {
        use ivac_core::sim::diagnostics::SimWarning;
        // Replace any existing sticky CellSizeCoarsened — only the most
        // recent coarsening matters (a rebuild with different cell
        // counts overrides the prior decision).
        self.sticky_warnings
            .retain(|w| !matches!(w, SimWarning::CellSizeCoarsened { .. }));
        let warn = SimWarning::CellSizeCoarsened {
            original_cell_size_mm,
            coarsened_cell_size_mm,
            reason,
        };
        self.last_diagnostics.push(warn.clone());
        self.sticky_warnings.push(warn);
    }

    /// Replace the simulator's fixture set. Pass the project's fixtures
    /// array (serialized as `Vec<Fixture>`) so subsequent `advance()`
    /// calls can emit `FixtureCollision` warnings. Pass an empty array
    /// to clear.
    pub fn set_fixtures(&mut self, fixtures: JsValue) -> Result<(), JsValue> {
        let parsed: Vec<Fixture> =
            serde_wasm_bindgen::from_value(fixtures).map_err(into_js_error)?;
        self.fixtures = parsed;
        Ok(())
    }

    /// Pull and clear the diagnostics collected by the most recent
    /// `advance()` call. Returns a JSON-shaped `SimDiagnostics`.
    /// wpzm: sticky warnings (cell-size coarsening) are merged in so
    /// the UI keeps seeing them across playhead movements.
    pub fn take_diagnostics(&mut self) -> Result<JsValue, JsValue> {
        let mut taken = std::mem::take(&mut self.last_diagnostics);
        for w in &self.sticky_warnings {
            taken.push(w.clone());
        }
        serde_wasm_bindgen::to_value(&taken).map_err(into_js_error)
    }

    /// Apply sweeps for toolpath segments `[from_idx, to_idx)` from the
    /// cached toolpath (set via `set_toolpath(...)`). Returns the
    /// resulting dirty AABB encoded as `[ix0, iy0, ix1, iy1]` so the
    /// JS renderer knows which mesh vertices to update; an empty `Vec`
    /// means no cells changed. The heightmap's internal dirty AABB is
    /// cleared first so the returned bounds reflect only this call.
    /// `tool` stays as an arg because it's tiny and may change between
    /// ops within a single toolpath.
    pub fn advance(
        &mut self,
        tool: JsValue,
        from_idx: u32,
        to_idx: u32,
    ) -> Result<Vec<u32>, JsValue> {
        let tool_entry: ToolEntry = from_tool_value(tool)?;
        // mg77: guard the sweep with catch_unwind so a panic inside the
        // per-frame carve surfaces as a structured JS error rather than
        // trapping (aborting) the whole wasm instance mid-playback —
        // mirrors the pipeline `generate()` envelope.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Inline the body that advance_inner provides for the test-only
            // path. We need disjoint borrows of `self.toolpath` (read) and
            // `self.heightmap` / `self.last_diagnostics` (mutate), which
            // Rust's field-level split borrowing allows here.
            self.heightmap.clear_dirty();
            self.last_diagnostics = SimDiagnostics::new();
            let profile = ToolProfile::from_tool(&tool_entry);
            let holder = HolderProfile::from_tool(&tool_entry);
            let touched = sweep_range(
                &mut self.heightmap,
                &self.toolpath,
                from_idx as usize,
                to_idx as usize,
                &profile,
                &self.fixtures,
                holder.as_ref(),
                &mut self.last_diagnostics,
            );
            // 03zx: emit a single tracing::info line per advance so the
            // frontend (and post-mortem tooling) have a stable telemetry
            // record of cells_carved + per-kind warning
            // counts. `total_seconds` is left 0 here because advance()
            // doesn't wall-clock itself — the JS driver can pair this
            // with a Performance.now() delta when persisting.
            SimRunSummary::from_diagnostics(&self.last_diagnostics, u64::from(touched), 0.0).log();
            match self.heightmap.dirty_aabb() {
                Some((ix0, iy0, ix1, iy1)) => vec![ix0, iy0, ix1, iy1],
                None => Vec::new(),
            }
        }));
        result.map_err(|p| sweep_panic_to_js(&p))
    }

    /// Carve only the chunk `[t_start, t_end]` (parametric position) of
    /// segment `seg_idx` from the cached toolpath. Same wire shape as
    /// `advance(...)`: returns the dirty AABB as `[ix0, iy0, ix1, iy1]`,
    /// empty when no cells changed. Used by the per-frame driver so the
    /// 3D-sim destruction visually tracks the cutter inside long
    /// segments (drill plunges, long cuts) instead of popping in at
    /// segment-start (pi8r). Fixture / holder / rapid warnings fire only
    /// on the first slice of the segment (`t_start ≈ 0`) so 60 fps
    /// driver frames don't duplicate diagnostics.
    pub fn partial_advance(
        &mut self,
        tool: JsValue,
        seg_idx: u32,
        t_start: f64,
        t_end: f64,
    ) -> Result<Vec<u32>, JsValue> {
        let tool_entry: ToolEntry = from_tool_value(tool)?;
        let idx = seg_idx as usize;
        if idx >= self.toolpath.len() {
            return Err(JsValue::from_str(
                "partial_advance: seg_idx out of range for cached toolpath",
            ));
        }
        // mg77: same catch_unwind guard as advance() — a sweep panic in
        // the per-frame partial carve must not trap the wasm module.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.heightmap.clear_dirty();
            self.last_diagnostics = SimDiagnostics::new();
            let profile = ToolProfile::from_tool(&tool_entry);
            let holder = HolderProfile::from_tool(&tool_entry);
            let _touched = sweep_segment_partial(
                &mut self.heightmap,
                &self.toolpath[idx],
                &profile,
                idx,
                &self.fixtures,
                holder.as_ref(),
                &mut self.last_diagnostics,
                t_start,
                t_end,
            );
            match self.heightmap.dirty_aabb() {
                Some((ix0, iy0, ix1, iy1)) => vec![ix0, iy0, ix1, iy1],
                None => Vec::new(),
            }
        }));
        result.map_err(|p| sweep_panic_to_js(&p))
    }

    /// Number of grid columns (X cells).
    #[must_use]
    pub fn cols(&self) -> u32 {
        self.heightmap.cols
    }

    /// Number of grid rows (Y cells).
    #[must_use]
    pub fn rows(&self) -> u32 {
        self.heightmap.rows
    }

    /// Cell side length in world units (mm).
    #[must_use]
    pub fn cell_size(&self) -> f64 {
        self.heightmap.cell
    }

    /// World X of the heightmap origin (cell `(0, 0)`'s lower-left corner).
    #[must_use]
    pub fn origin_x(&self) -> f64 {
        self.heightmap.origin.x
    }

    /// World Y of the heightmap origin.
    #[must_use]
    pub fn origin_y(&self) -> f64 {
        self.heightmap.origin.y
    }

    /// Stock-top Z. Cells the cutter has not reached still report this.
    #[must_use]
    pub fn top_z(&self) -> f32 {
        self.heightmap.top_z
    }

    /// 9c34: serialize the carved heightfield as a binary STL. The mesh
    /// drops to `stock_bottom_z` at every perimeter sample so the result
    /// is watertight. Wired up via the File menu's "Export simulated
    /// stock as STL..." entry.
    #[must_use]
    pub fn export_stl(&self, stock_bottom_z: f32) -> Vec<u8> {
        ivac_core::sim::stl::heightmap_to_stl_binary(&self.heightmap, stock_bottom_z)
    }

    /// Pointer to the f32 heightmap buffer. JS wraps it as
    /// `new Float32Array(wasm.memory.buffer, sim.data_ptr(),
    /// sim.cols() * sim.rows())`.
    ///
    /// IMPORTANT: any operation that grows WASM linear memory invalidates
    /// the underlying `ArrayBuffer` of `WebAssembly.Memory.buffer`, which
    /// detaches every existing typed-array view. `advance()` allocates
    /// transiently while deserializing segments, so it MAY trigger
    /// growth — re-take the `Float32Array` view after every `advance()`
    /// call. The construction itself is O(1).
    #[must_use]
    pub fn data_ptr(&self) -> *const f32 {
        self.heightmap.data_ptr()
    }
}

impl Simulator {
    /// Pure-Rust core of `advance()` — used by tests that don't want to
    /// route through `JsValue`. Gated behind `#[cfg(test)]` to silence
    /// `dead_code` on the wasm production build.
    #[cfg(test)]
    pub(crate) fn advance_inner(
        &mut self,
        segments: &[ToolpathSegment],
        tool: &ToolEntry,
        from_idx: u32,
        to_idx: u32,
    ) -> Vec<u32> {
        self.heightmap.clear_dirty();
        self.last_diagnostics = SimDiagnostics::new();
        let profile = ToolProfile::from_tool(tool);
        let holder = HolderProfile::from_tool(tool);
        let _touched = sweep_range(
            &mut self.heightmap,
            segments,
            from_idx as usize,
            to_idx as usize,
            &profile,
            &self.fixtures,
            holder.as_ref(),
            &mut self.last_diagnostics,
        );
        match self.heightmap.dirty_aabb() {
            Some((ix0, iy0, ix1, iy1)) => vec![ix0, iy0, ix1, iy1],
            None => Vec::new(),
        }
    }

    /// Pure-Rust core of `partial_advance()` — same role as
    /// `advance_inner`, used by Rust-side tests that can't pass
    /// `JsValue`.
    #[cfg(test)]
    pub(crate) fn partial_advance_inner(
        &mut self,
        segments: &[ToolpathSegment],
        tool: &ToolEntry,
        seg_idx: u32,
        t_start: f64,
        t_end: f64,
    ) -> Vec<u32> {
        self.heightmap.clear_dirty();
        self.last_diagnostics = SimDiagnostics::new();
        let profile = ToolProfile::from_tool(tool);
        let holder = HolderProfile::from_tool(tool);
        let idx = seg_idx as usize;
        if idx < segments.len() {
            let _touched = sweep_segment_partial(
                &mut self.heightmap,
                &segments[idx],
                &profile,
                idx,
                &self.fixtures,
                holder.as_ref(),
                &mut self.last_diagnostics,
                t_start,
                t_end,
            );
        }
        match self.heightmap.dirty_aabb() {
            Some((ix0, iy0, ix1, iy1)) => vec![ix0, iy0, ix1, iy1],
            None => Vec::new(),
        }
    }

    /// Test-only handle on the inner heightmap. Lets the unit tests
    /// inspect cells without going through `data_ptr` (which would force
    /// `unsafe` to deref).
    #[cfg(test)]
    pub(crate) fn heightmap(&self) -> &Heightmap {
        &self.heightmap
    }
}

/// Decode the JS-side tool spec. Goes through the long-form deserializer
/// path (rather than `from_value`) to keep us flexible if we later need
/// to relax unknown-field handling.
fn from_tool_value(value: JsValue) -> Result<ToolEntry, JsValue> {
    let de = serde_wasm_bindgen::Deserializer::from(value);
    ToolEntry::deserialize(de).map_err(into_js_error)
}

/// mg77: convert a caught sweep panic into the same structured JS error
/// shape the pipeline `generate()` envelope produces, so the frontend's
/// `ErrorToast` renders it instead of the wasm instance trapping.
fn sweep_panic_to_js(panic: &Box<dyn std::any::Any + Send>) -> JsValue {
    structured_error_to_js(
        ivac_core::Error::internal(format!("sim sweep panic: {}", panic_message(panic)))
            .with_hint("Please report this bug — see the toast for details."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ivac_core::gcode::preview::{MoveKind, Pose3, ToolpathSegment};
    use ivac_core::project::{Coolant, SpindleDirection, ToolKind};

    fn endmill(diameter: f64) -> ToolEntry {
        ToolEntry {
            id: 1,
            name: "test endmill".into(),
            kind: ToolKind::Endmill,
            diameter,
            tip_diameter: None,
            tip_angle_deg: 60.0,
            dragoff: None,
            drag_knife_self_align_angle_deg: None,
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
            // chgd: spindle_direction landed on ToolEntry — mirror the
            // core test fixture (sim/heightmap.rs) so WASM tests still
            // compile. Default is Cw, matches pre-spindle behavior.
            spindle_direction: SpindleDirection::default(),
            // zpuk/r2af specialty fields — plasma pierce/cut heights +
            // vcarve lead-in. None = inactive, matches a plain endmill.
            pierce_height_mm: None,
            cut_height_mm: None,
            pierce_delay_sec: None,
            vcarve_lead_in_angle_deg: None,
        }
    }

    fn plunge(x: f64, y: f64, top: f64, bottom: f64) -> ToolpathSegment {
        ToolpathSegment {
            from: Pose3 { x, y, z: top },
            to: Pose3 { x, y, z: bottom },
            kind: MoveKind::Plunge,
            gcode_line: 0,
            op_id: 0,
        }
    }

    #[test]
    fn new_initializes_heightmap_to_top_z() {
        let sim = Simulator::new(0.0, 0.0, 20.0, 20.0, 1.0, 0.0);
        // ceil(width / cell) + 1 grid lines — the +1 fencepost (see
        // Heightmap::from_bbox) so the bbox max-corner stays on-grid.
        // 20 mm / 1 mm = 20 cells → 21 nodes per axis.
        assert_eq!(sim.cols(), 21);
        assert_eq!(sim.rows(), 21);
        assert!((sim.cell_size() - 1.0).abs() < 1e-9);
        assert!((sim.top_z() - 0.0).abs() < 1e-6);
        assert!(sim.heightmap().data.iter().all(|&z| (z - 0.0).abs() < 1e-6));
    }

    #[test]
    fn advance_endmill_plunge_lowers_cells_and_returns_dirty_aabb() {
        let mut sim = Simulator::new(0.0, 0.0, 40.0, 40.0, 1.0, 0.0);
        let segs = vec![plunge(20.0, 20.0, 0.0, -1.0)];
        let tool = endmill(4.0);
        let aabb = sim.advance_inner(&segs, &tool, 0, 1);
        assert_eq!(aabb.len(), 4, "non-empty dirty AABB expected");
        let (ix0, iy0, ix1, iy1) = (aabb[0], aabb[1], aabb[2], aabb[3]);
        assert!(ix0 < ix1 && iy0 < iy1, "AABB must be non-empty");
        // Cell directly under the plunge sits at the plunge depth.
        let hm = sim.heightmap();
        let center = hm.data[(20 * hm.cols + 20) as usize];
        assert!(
            (center - -1.0).abs() < 1e-5,
            "plunge center expected -1, got {center}"
        );
        // At least one cell is below top_z.
        assert!(hm.data.iter().any(|&z| z < hm.top_z));
    }

    #[test]
    fn reset_restores_top_z_and_no_dirty() {
        let mut sim = Simulator::new(0.0, 0.0, 20.0, 20.0, 1.0, 0.0);
        let _ = sim.advance_inner(&[plunge(10.0, 10.0, 0.0, -1.0)], &endmill(4.0), 0, 1);
        sim.reset();
        let hm = sim.heightmap();
        assert!(hm.data.iter().all(|&z| (z - 0.0).abs() < 1e-6));
        assert!(hm.dirty_aabb().is_none());
    }

    #[test]
    fn advance_clears_previous_dirty_so_aabb_reflects_only_this_call() {
        let mut sim = Simulator::new(0.0, 0.0, 40.0, 40.0, 1.0, 0.0);
        let first = vec![plunge(5.0, 5.0, 0.0, -1.0)];
        let second = vec![plunge(30.0, 30.0, 0.0, -1.0)];
        let tool = endmill(2.0);
        let _ = sim.advance_inner(&first, &tool, 0, 1);
        let aabb = sim.advance_inner(&second, &tool, 0, 1);
        // Should report only the second plunge's region, not the union.
        assert!(
            aabb[0] >= 28 && aabb[2] <= 32,
            "second-plunge AABB drifted: {aabb:?}"
        );
    }

    #[test]
    fn advance_with_no_cuts_returns_empty_aabb() {
        let mut sim = Simulator::new(0.0, 0.0, 20.0, 20.0, 1.0, 0.0);
        let rapid = vec![ToolpathSegment {
            from: Pose3 {
                x: 0.0,
                y: 0.0,
                z: 5.0,
            },
            to: Pose3 {
                x: 10.0,
                y: 10.0,
                z: 5.0,
            },
            kind: MoveKind::Rapid,
            gcode_line: 0,
            op_id: 0,
        }];
        let aabb = sim.advance_inner(&rapid, &endmill(2.0), 0, 1);
        assert!(
            aabb.is_empty(),
            "rapid-only advance should report no dirty cells"
        );
    }

    #[test]
    fn data_ptr_and_len_consistent_with_cols_rows() {
        let sim = Simulator::new(0.0, 0.0, 10.0, 10.0, 0.5, 0.0);
        let len = (sim.cols() as usize) * (sim.rows() as usize);
        assert_eq!(len, sim.heightmap().data_len());
        assert!(!sim.data_ptr().is_null());
    }

    /// `partial_advance(idx, 0, 0.5)` on a Plunge segment should carve
    /// the column down to the midpoint Z, not the full final depth.
    /// Calling `partial_advance(idx, 0.5, 1.0)` afterwards lowers the
    /// same column to the final depth.
    #[test]
    fn partial_advance_plunge_grows_as_t_advances() {
        let mut sim = Simulator::new(0.0, 0.0, 40.0, 40.0, 1.0, 0.0);
        let segs = vec![plunge(20.0, 20.0, 0.0, -2.0)];
        let tool = endmill(4.0);
        let aabb_half = sim.partial_advance_inner(&segs, &tool, 0, 0.0, 0.5);
        assert_eq!(aabb_half.len(), 4, "expected non-empty AABB at half-plunge");
        let center_after_half = sim.heightmap().data[(20 * sim.heightmap().cols + 20) as usize];
        assert!(
            (center_after_half - -1.0).abs() < 1e-5,
            "plunge halfway should reach z=-1, got {center_after_half}"
        );
        let _ = sim.partial_advance_inner(&segs, &tool, 0, 0.5, 1.0);
        let center_after_full = sim.heightmap().data[(20 * sim.heightmap().cols + 20) as usize];
        assert!(
            (center_after_full - -2.0).abs() < 1e-5,
            "plunge fully should reach z=-2, got {center_after_full}"
        );
    }

    /// A straight cut from x=5 to x=25 carved up to t=0.5 should only
    /// touch cells in the left half of the segment. The right half stays
    /// at `top_z`.
    #[test]
    fn partial_advance_cut_only_touches_swept_chunk() {
        let mut sim = Simulator::new(0.0, 0.0, 40.0, 40.0, 1.0, 0.0);
        let cut = ToolpathSegment {
            from: Pose3 {
                x: 5.0,
                y: 20.0,
                z: -1.0,
            },
            to: Pose3 {
                x: 25.0,
                y: 20.0,
                z: -1.0,
            },
            kind: MoveKind::Cut,
            gcode_line: 0,
            op_id: 0,
        };
        let segs = vec![cut];
        let tool = endmill(2.0);
        let _ = sim.partial_advance_inner(&segs, &tool, 0, 0.0, 0.5);
        let hm = sim.heightmap();
        // Cell at x≈10 (within carved chunk [5..15]) is below top_z.
        let near = hm.data[(20 * hm.cols + 10) as usize];
        assert!(near < hm.top_z, "cell in carved half should be lowered");
        // Cell at x≈22 (in uncarved chunk [15..25]) is still at top_z.
        let far = hm.data[(20 * hm.cols + 22) as usize];
        assert!(
            (far - hm.top_z).abs() < 1e-6,
            "cell in un-carved half should still be at top_z, got {far}"
        );
        // After t goes 0.5→1, the right half also drops.
        let _ = sim.partial_advance_inner(&segs, &tool, 0, 0.5, 1.0);
        let far_after = sim.heightmap().data[(20 * sim.heightmap().cols + 22) as usize];
        assert!(
            far_after < sim.heightmap().top_z,
            "right half should be carved after full partial sweep, got {far_after}"
        );
    }

    /// Partial slices with `t_start > 0` MUST NOT emit fixture / holder /
    /// rapid diagnostics — a 60 fps driver would otherwise spam the same
    /// warning each frame. The first slice (`t_start ≈ 0`) emits once.
    #[test]
    fn partial_advance_emits_rapid_warning_only_on_first_slice() {
        let mut sim = Simulator::new(0.0, 0.0, 40.0, 40.0, 1.0, 0.0);
        // Rapid through material: starts below top_z, so check_rapid_against_stock
        // reports a collision.
        let rapid = ToolpathSegment {
            from: Pose3 {
                x: 0.0,
                y: 20.0,
                z: -5.0,
            },
            to: Pose3 {
                x: 40.0,
                y: 20.0,
                z: -5.0,
            },
            kind: MoveKind::Rapid,
            gcode_line: 0,
            op_id: 0,
        };
        let segs = vec![rapid];
        let tool = endmill(2.0);
        let _ = sim.partial_advance_inner(&segs, &tool, 0, 0.0, 0.3);
        assert_eq!(
            sim.last_diagnostics.count("rapid_through_material"),
            1,
            "first partial slice should emit one warning"
        );
        let _ = sim.partial_advance_inner(&segs, &tool, 0, 0.3, 0.6);
        assert_eq!(
            sim.last_diagnostics.count("rapid_through_material"),
            0,
            "mid-segment partial slice must not re-emit the warning"
        );
    }
}
