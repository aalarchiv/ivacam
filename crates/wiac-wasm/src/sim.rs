//! Heightmap simulator bindings — wraps `wiac_core::sim::Heightmap` and
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

use serde::Deserialize;
use wasm_bindgen::prelude::*;

use wiac_core::gcode::preview::ToolpathSegment;
use wiac_core::project::{Fixture, ToolEntry};
use wiac_core::sim::diagnostics::SimDiagnostics;
use wiac_core::sim::heightmap::{Heightmap, ToolProfile};
use wiac_core::sim::holder::HolderProfile;
use wiac_core::sim::sweep::sweep_range;

use crate::into_js_error;

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
}

#[wasm_bindgen]
impl Simulator {
    /// Build a fresh simulator covering the rectangle
    /// `[min_x, max_x] × [min_y, max_y]` with `cell_size`-mm cells. Every
    /// cell starts at `top_z` (i.e. the un-cut stock surface).
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
        cell_size: f64,
        top_z: f32,
    ) -> Self {
        Self {
            heightmap: Heightmap::from_bbox(min_x, min_y, max_x, max_y, cell_size, top_z),
            last_diagnostics: SimDiagnostics::new(),
            fixtures: Vec::new(),
            toolpath: Vec::new(),
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
    pub fn take_diagnostics(&mut self) -> Result<JsValue, JsValue> {
        let taken = std::mem::take(&mut self.last_diagnostics);
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
        // Inline the body that advance_inner provides for the test-only
        // path. We need disjoint borrows of `self.toolpath` (read) and
        // `self.heightmap` / `self.last_diagnostics` (mutate), which
        // Rust's field-level split borrowing allows here.
        self.heightmap.clear_dirty();
        self.last_diagnostics = SimDiagnostics::new();
        let profile = ToolProfile::from_tool(&tool_entry);
        let holder = HolderProfile::from_tool(&tool_entry);
        let _touched = sweep_range(
            &mut self.heightmap,
            &self.toolpath,
            from_idx as usize,
            to_idx as usize,
            profile,
            &self.fixtures,
            holder.as_ref(),
            &mut self.last_diagnostics,
        );
        Ok(match self.heightmap.dirty_aabb() {
            Some((ix0, iy0, ix1, iy1)) => vec![ix0, iy0, ix1, iy1],
            None => Vec::new(),
        })
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
            profile,
            &self.fixtures,
            holder.as_ref(),
            &mut self.last_diagnostics,
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiac_core::gcode::preview::{MoveKind, Pose3, ToolpathSegment};
    use wiac_core::project::{Coolant, ToolKind};

    fn endmill(diameter: f64) -> ToolEntry {
        ToolEntry {
            id: 1,
            name: "test endmill".into(),
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
        assert_eq!(sim.cols(), 20);
        assert_eq!(sim.rows(), 20);
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
        assert!((center - -1.0).abs() < 1e-5, "plunge center expected -1, got {center}");
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
        assert!(aabb[0] >= 28 && aabb[2] <= 32, "second-plunge AABB drifted: {aabb:?}");
    }

    #[test]
    fn advance_with_no_cuts_returns_empty_aabb() {
        let mut sim = Simulator::new(0.0, 0.0, 20.0, 20.0, 1.0, 0.0);
        let rapid = vec![ToolpathSegment {
            from: Pose3 { x: 0.0, y: 0.0, z: 5.0 },
            to: Pose3 { x: 10.0, y: 10.0, z: 5.0 },
            kind: MoveKind::Rapid,
            gcode_line: 0,
            op_id: 0,
        }];
        let aabb = sim.advance_inner(&rapid, &endmill(2.0), 0, 1);
        assert!(aabb.is_empty(), "rapid-only advance should report no dirty cells");
    }

    #[test]
    fn data_ptr_and_len_consistent_with_cols_rows() {
        let sim = Simulator::new(0.0, 0.0, 10.0, 10.0, 0.5, 0.0);
        let len = (sim.cols() as usize) * (sim.rows() as usize);
        assert_eq!(len, sim.heightmap().data_len());
        assert!(!sim.data_ptr().is_null());
    }
}
