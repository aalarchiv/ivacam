/// Glue between the WASM Simulator (jbj) and the HeightfieldMesh (hvv):
/// owns one Simulator + one HeightfieldMesh per active project.generated,
/// drives advance() on playhead change, and refreshes the affected mesh
/// region using the Float32Array view re-taken after every advance.
///
/// The lifecycle is:
///   * mount: wait for the WASM module to load, then create Simulator
///     (needs stock bbox + cell size) and HeightfieldMesh (matching grid).
///   * playhead change: call sim.advance(prev, next), get the dirty AABB,
///     re-take the Float32Array view (memory growth can detach the old
///     one), and call mesh.updateHeights(view, aabb).
///   * scrub backward: reset the simulator and replay 0..head.
///   * project.generated changes: rebuild both Simulator + mesh.

import * as THREE from 'three';
import { HeightfieldMeshPyramid, pickMinLodLevelForBudget } from './heightfield_mesh';
import { planAdvance, playheadToSegment } from './playhead';
import { computeFootprint } from './footprint';
import { isWasmTransport } from '../api/transport-mode';
import { simWarningKey } from './warnings';
import type {
  GenerateResponse,
  ImportResponse,
  SimDiagnostics,
  ToolpathSegment,
} from '../api/types';
import type { AppSettings, Fixture, ToolEntry } from '../state/project.svelte';
import { toWireToolKind } from '../api/build-project';

interface SimulatorWasm {
  // wasm-bindgen produces a constructor on the generated class; this
  // interface stands in for both the constructor signature and the
  // instance shape so we can `new SimulatorWasm(...)` without importing
  // the generated d.ts type. The no-misused-new lint would prefer a
  // class — wasm-bindgen owns the actual class definition.
  // eslint-disable-next-line @typescript-eslint/no-misused-new
  new (
    minX: number,
    minY: number,
    maxX: number,
    maxY: number,
    cellSize: number,
    topZ: number,
  ): SimulatorWasm;
  reset(): void;
  /// wpzm: record that the JS driver coarsened cell_size to fit the
  /// user's maxSimulationCells budget. The warning rides out via
  /// take_diagnostics() so the UI surfaces it like any other sim
  /// warning instead of silently smoothing out small features.
  push_cell_size_coarsened(
    original_cell_size_mm: number,
    coarsened_cell_size_mm: number,
    reason: string,
  ): void;
  advance(tool: unknown, from_idx: number, to_idx: number): Uint32Array;
  /// Carve only chunk `[t_start, t_end]` of segment `seg_idx`. Used per
  /// render frame so the heightfield destruction follows the cutter
  /// inside long segments (drill plunges) instead of popping at segment
  /// boundaries (pi8r).
  partial_advance(tool: unknown, seg_idx: number, t_start: number, t_end: number): Uint32Array;
  set_fixtures(fixtures: unknown): void;
  set_toolpath(segments: unknown): number;
  clear_toolpath(): void;
  toolpath_len(): number;
  take_diagnostics(): SimDiagnostics;
  cols(): number;
  rows(): number;
  cell_size(): number;
  origin_x(): number;
  origin_y(): number;
  top_z(): number;
  data_ptr(): number;
  /// 9c34: serialize the carved heightfield as a binary STL. The mesh
  /// drops to `stock_bottom_z` at every perimeter sample so the result
  /// is watertight.
  export_stl(stock_bottom_z: number): Uint8Array;
  free(): void;
}

interface WasmModule {
  default?: (
    module_or_path?: unknown,
  ) => Promise<{ memory: WebAssembly.Memory } & Record<string, unknown>>;
  Simulator: new (
    minX: number,
    minY: number,
    maxX: number,
    maxY: number,
    cellSize: number,
    topZ: number,
  ) => SimulatorWasm;
}

/// The bundle of things the driver actually uses: the constructor
/// reference (so we can `new wasm.Simulator(...)`) plus the
/// WebAssembly.Memory grabbed from the InitOutput. Captured separately
/// because the imported module's namespace is read-only — assigning
/// `module.memory = ...` silently fails in strict mode (and ESM is
/// always strict). Stuffing memory back on the module object was a
/// real bug pre-fix; the Float32Array view never had valid memory and
/// the heightfield mesh stayed at top_z.
interface WasmHandle {
  Simulator: WasmModule['Simulator'];
  memory: WebAssembly.Memory;
}

let wasmPromise: Promise<WasmHandle> | null = null;

async function loadWasm(): Promise<WasmHandle> {
  if (!wasmPromise) {
    wasmPromise = (async () => {
      // The pkg is built by `wasm-pack build crates/wiac-wasm --target web`
      // and linked via package.json. Letting vite resolve the import is
      // critical here: the browser can't fetch a bare `wiac-wasm` URL,
      // so an @vite-ignore'd dynamic import would fail silently in a
      // bundled app. Without the ignore, vite splits this into a chunk,
      // copies wiac_wasm_bg.wasm with a hashed name, and rewrites the
      // js's `import.meta.url`-relative .wasm fetch to match.
      const mod = (await import('wiac-wasm')) as unknown as WasmModule;
      if (typeof mod.default !== 'function') {
        throw new Error('wiac-wasm pkg missing default init export');
      }
      const init = await mod.default();
      if (!init.memory) {
        throw new Error('wiac-wasm init returned no memory');
      }
      return { Simulator: mod.Simulator, memory: init.memory };
    })();
  }
  return wasmPromise;
}

/// Project-state-shaped tool spec the WASM Simulator expects. Mirrors
/// what wiac_core::project::ToolEntry deserializes from (snake_case).
///
/// IMPORTANT: this is the SECOND wire seam after `buildTool`, both of
/// them feeding the same Rust `ToolKind` deserializer. The kind name
/// must go through `toWireToolKind` so the frontend `cone` becomes the
/// backend `kegel` (8njb / regression filed and fixed after the first
/// rollout missed this seam).
export function toWireTool(t: ToolEntry): Record<string, unknown> {
  return {
    id: t.id,
    name: t.name,
    kind: toWireToolKind(t.kind),
    diameter: t.diameter,
    ...(t.tipDiameter !== undefined ? { tip_diameter: t.tipDiameter } : {}),
    ...(t.tipAngleDeg !== undefined ? { tip_angle_deg: t.tipAngleDeg } : {}),
    ...(t.dragoff !== undefined ? { dragoff: t.dragoff } : {}),
    flutes: t.flutes,
    speed: t.speed,
    plunge_rate: t.plungeRate,
    feed_rate: t.feedRate,
    coolant: t.coolant,
    ...(t.fluteLengthMm !== undefined ? { flute_length_mm: t.fluteLengthMm } : {}),
    ...(t.lengthMm !== undefined ? { length_mm: t.lengthMm } : {}),
    ...(t.compressionTransitionMm !== undefined
      ? { compression_transition_mm: t.compressionTransitionMm }
      : {}),
    ...(t.threadPitchMm !== undefined ? { thread_pitch_mm: t.threadPitchMm } : {}),
    ...(t.shankDiameterMm !== undefined ? { shank_diameter_mm: t.shankDiameterMm } : {}),
    ...(t.holder !== undefined ? { holder: t.holder } : {}),
    // Per-kind sim/holder metadata so the heightfield simulator can
    // pick the right cutter profile and holder-check uses the right
    // shank dims. Bull-nose collapses to a fillet profile in the sim;
    // form-profile (incl. the folded-in T-slot, z5yw) carves its (z, r)
    // sample list when ≥2 rows are present.
    ...(t.cornerRadiusMm !== undefined ? { corner_radius_mm: t.cornerRadiusMm } : {}),
    ...(t.kind === 'form_profile' && t.formProfileMm !== undefined && t.formProfileMm.length >= 2
      ? { form_profile_mm: t.formProfileMm.map((s) => ({ z_mm: s.zMm, r_mm: s.rMm })) }
      : {}),
    ...(t.zShiftMm !== undefined ? { z_shift_mm: t.zShiftMm } : {}),
  };
}

/// vrrr: `computeFootprint` moved to the THREE-free `./footprint`
/// module so the API layer can resolve the stock box without importing
/// this THREE-heavy driver. Imported for this module's own `build()` use
/// and re-exported for the existing 3D-scene import sites.
export { computeFootprint };

/// 5v1b: the in-browser (`?api=wasm`) trial runs the sim single-threaded
/// on the main thread, so a too-fine heightfield makes `set_toolpath` /
/// `advance` / mesh upload stutter the UI. Cap the grid harder there.
/// This is the single fidelity knob: a lower-res carve PREVIEW is an
/// acceptable trial trade — far better than laggy scrubbing. Native /
/// server / Tauri keep the user's full `maxSimulationCells`. ~250k cells
/// ≈ a 500×500 grid over the footprint — smooth, still legible.
export const WASM_TRIAL_SIM_CELL_CAP = 250_000;

/// Effective sim cell cap for the active transport. In wasm-trial mode
/// it's the tighter of the user's setting and [`WASM_TRIAL_SIM_CELL_CAP`];
/// everywhere else it's the user's setting verbatim. Pure so it's
/// unit-tested without the wasm module.
export function effectiveSimCellCap(userMaxCells: number, isWasm: boolean): number {
  const userCap = Math.max(1, userMaxCells);
  return isWasm ? Math.min(userCap, WASM_TRIAL_SIM_CELL_CAP) : userCap;
}

/// Compute cell size from the active tool diameter when settings is in
/// 'auto' mode. Targets ~tool_diameter/15, clamped 0.05..2.0 mm.
function computeCellSize(toolDiameter: number, settings: AppSettings): number {
  if (settings.cellResolutionMode === 'manual') {
    return Math.max(0.01, settings.cellResolutionMm);
  }
  return Math.max(0.05, Math.min(2.0, toolDiameter / 15));
}

export interface DriverOptions {
  scene: THREE.Scene;
  requestRender: () => void;
}

/// 9c34: module-level reference to the live driver, set by Scene3D on
/// mount / cleared on dispose. Lets file_ops trigger an STL export
/// without circular imports or threading a driver handle through every
/// component. There is only ever one Scene3D in the app.
let currentDriver: HeightfieldDriver | null = null;

/// Active driver, or `null` when no Scene3D is mounted. Used by the
/// "Export simulated stock as STL..." flow in file_ops.
export function getCurrentDriver(): HeightfieldDriver | null {
  return currentDriver;
}

export class HeightfieldDriver {
  readonly group: THREE.Group;
  private sim: SimulatorWasm | null = null;
  private mesh: HeightfieldMeshPyramid | null = null;
  private wasm: WasmHandle | null = null;
  /// Cached buffer view; valid until the next advance() that may grow
  /// WASM linear memory. Re-taken after every advance.
  private heightView: Float32Array | null = null;
  /// Reference to the toolpath array that's currently cached on the
  /// WASM Simulator. Used to detect identity drift (e.g. a stale
  /// driver picking up a new Generate response) and trigger a single
  /// re-cache rather than re-deserializing per frame (audit-9l52).
  private cachedToolpath: ToolpathSegment[] | null = null;
  /// Position in the toolpath that the heightfield is fully carved up
  /// to. Semantics: segments `[0, appliedSeg)` are bulk-carved; segment
  /// `appliedSeg` is carved up to `partialT ∈ [0, 1]` (pi8r). Together
  /// they fully describe the rendered destruction state and let the
  /// driver issue tiny partial carves per render frame.
  private appliedSeg = 0;
  private partialT = 0;
  /// Cumulative sim warnings collected since the last replay. Cleared on
  /// dispose / reset; merged-into on every forward advance() so the UI
  /// can mark the offending segments as the user scrubs.
  private diagnostics: SimDiagnostics = { warnings: [] };
  private onDiagnosticsChange: ((d: SimDiagnostics) => void) | null = null;
  /// Edges rebuild walks every triangle in the active heightfield
  /// (THREE.EdgesGeometry has no incremental API), so it must NOT run
  /// during continuous activity. Pure trailing debounce — every call
  /// to `scheduleEdgeRebuild` resets the timer, the rebuild fires
  /// only after `EDGE_REBUILD_MS` of quiet (no carve + no camera
  /// move). Continuous playback never hits it; idle frames after
  /// playback stops, do.
  private edgeRebuildTimer: ReturnType<typeof setTimeout> | null = null;
  private static readonly EDGE_REBUILD_MS = 400;
  /// Hard cap on active-level triangle count for which edges are
  /// computed at all. Above this, the EdgesGeometry rebuild cost
  /// (~25 ns / triangle in v8) exceeds a frame budget by itself, and
  /// the lines visually clutter the cell field anyway. The LOD
  /// pyramid normally pushes the active level coarser before this
  /// hits, but the cap is a hard ceiling regardless.
  private static readonly EDGE_MAX_TRIANGLES = 400_000;

  constructor(private opts: DriverOptions) {
    this.group = new THREE.Group();
    this.group.visible = false;
    opts.scene.add(this.group);
    // 9c34: register as the live driver so file_ops can reach it.
    // eslint-disable-next-line @typescript-eslint/no-this-alias -- singleton registry
    currentDriver = this;
  }

  async init(): Promise<void> {
    if (this.wasm) return;
    this.wasm = await loadWasm();
  }

  /// Build (or rebuild) the simulator + mesh for the given project
  /// state. Caller must ensure init() has resolved before the first
  /// build. Tearing down a previous sim/mesh is automatic.
  build(input: {
    imported: ImportResponse | null;
    generated: GenerateResponse | null;
    tool: ToolEntry | null;
    stock: {
      mode: 'auto' | 'manual';
      margin: number;
      thickness: number;
      customX: number;
      customY: number;
    };
    settings: AppSettings;
    fixtures?: Fixture[];
  }) {
    if (!this.wasm || !input.imported || !input.generated || !input.tool) {
      this.dispose();
      return;
    }
    const fp = computeFootprint(input.imported, input.stock);
    const cellSize = computeCellSize(input.tool.diameter, input.settings);
    const cols = Math.ceil((fp.maxX - fp.minX) / cellSize) + 1;
    const rows = Math.ceil((fp.maxY - fp.minY) / cellSize) + 1;
    const cellCount = cols * rows;
    // Sim-side cap: `maxSimulationCells` bounds WASM heap allocation
    // (4 bytes / cell). Halve cell density only when the user's
    // setting is exceeded — accurately reflecting their preference.
    // The GPU-side cap is handled separately by the LOD pyramid
    // (9tba) via `maxRenderTriangles`, so high sim accuracy no
    // longer forces a coarse mesh.
    // 5v1b: in the in-browser wasm trial the sim is single-threaded on
    // the main thread, so clamp harder to keep rebuild + scrub smooth.
    const isWasm = isWasmTransport();
    const simCellCap = effectiveSimCellCap(input.settings.maxSimulationCells, isWasm);
    let effectiveCellSize = cellSize;
    let coarsened = false;
    if (cellCount > simCellCap) {
      const scale = Math.sqrt(cellCount / simCellCap);
      effectiveCellSize = cellSize * scale;
      coarsened = true;
    }
    const topZ = 0; // stock surface is z=0; carving descends to negative Z
    // Stepped voxel renderer needs a finite floor — match the physical
    // stock bottom so the rendered boxes have the right thickness when
    // viewed from below. Default to 10 mm so an unconfigured project
    // still has a visible stock height.
    const stockThickness = input.stock.thickness > 0 ? input.stock.thickness : 10.0;
    this.dispose();
    this.sim = new this.wasm.Simulator(fp.minX, fp.minY, fp.maxX, fp.maxY, effectiveCellSize, topZ);
    // wpzm: surface the coarsening as a sim warning so it shows up in
    // the diagnostics panel — silently coarsening the grid hid
    // tool-engagement and small-feature issues from the user.
    // ujs2/5v1b: but ONLY when it's user-config-driven (a low
    // maxSimulationCells the user can raise). The wasm-trial cap
    // coarsens by design and isn't user-actionable — warning there just
    // floods the diagnostics window (it's a sticky warning re-emitted
    // every advance), so stay silent in the trial.
    if (coarsened && !isWasm && typeof this.sim.push_cell_size_coarsened === 'function') {
      this.sim.push_cell_size_coarsened(cellSize, effectiveCellSize, 'max_simulation_cells');
    }
    // 9tba: pick the lowest LOD level whose mesh fits the user's
    // `maxRenderTriangles` budget. Skip building finer (heavier)
    // levels so total GPU memory stays predictable.
    const simCols = this.sim.cols();
    const simRows = this.sim.rows();
    const minLevel = pickMinLodLevelForBudget(simCols, simRows, input.settings.maxRenderTriangles);
    this.mesh = new HeightfieldMeshPyramid(
      {
        cols: simCols,
        rows: simRows,
        cellSize: this.sim.cell_size(),
        originX: this.sim.origin_x(),
        originY: this.sim.origin_y(),
        topZ: this.sim.top_z(),
        floorZ: this.sim.top_z() - stockThickness,
        solidColor: input.settings.solidColor,
        solidOpacity: input.settings.solidOpacity,
        edgeColor: input.settings.edgeColor,
        edgeOpacity: input.settings.edgeOpacity,
      },
      3,
      minLevel,
    );
    this.group.add(this.mesh.group);
    if (input.fixtures && input.fixtures.length > 0) {
      this.sim.set_fixtures(input.fixtures);
    } else {
      this.sim.set_fixtures([]);
    }
    // Cache the toolpath on the WASM side ONCE per Generate so per-frame
    // advance() doesn't re-deserialize the whole segment array
    // (audit-9l52). The cached toolpath is the reference identity tracked
    // by `cachedToolpath` below.
    this.sim.set_toolpath(input.generated.toolpath);
    this.cachedToolpath = input.generated.toolpath;
    this.appliedSeg = 0;
    this.partialT = 0;
    this.diagnostics = { warnings: [] };
    this.notifyDiagnostics();
    this.refreshHeightView();
  }

  /// Replace the simulator's fixture set without rebuilding the mesh.
  /// Triggers a reset so the next advanceTo() call replays from segment
  /// 0 with the new obstacle list.
  setFixtures(fixtures: Fixture[]) {
    if (!this.sim) return;
    this.sim.set_fixtures(fixtures);
    this.sim.reset();
    this.appliedSeg = 0;
    this.partialT = 0;
    this.diagnostics = { warnings: [] };
    this.notifyDiagnostics();
    // 9tba: clear the LOD pyramid's coarse pools so the next
    // updateHeights doesn't pick up stale carved data from before
    // the reset.
    this.mesh?.reset();
    this.refreshHeightView();
  }

  /// Subscribe to diagnostics changes. Called with the current snapshot
  /// after every forward advance() that returns warnings, and after
  /// reset/dispose so listeners can clear stale UI markers.
  onDiagnostics(cb: (d: SimDiagnostics) => void) {
    this.onDiagnosticsChange = cb;
    cb(this.diagnostics);
  }

  /// Latest cumulative diagnostics snapshot. The UI may also subscribe
  /// via `onDiagnostics` for a push-based update.
  getDiagnostics(): SimDiagnostics {
    return this.diagnostics;
  }

  /// Advance the simulation to `headFraction` (a number in [0, 1] —
  /// project.playhead). Returns true if the mesh was modified.
  ///
  /// `cumLen` / `totalLen` are the arc-length cumulative table built
  /// alongside the toolpath; we map `headFraction` to `(segIdx, segT)`
  /// using arc length so the heightfield destruction tracks the 3D
  /// tool mesh and the gcode panel (which also use arc-length mapping).
  ///
  /// Carving happens at sub-segment resolution: segment `appliedSeg` is
  /// carved up to `partialT` and segments `[0, appliedSeg)` are fully
  /// done (pi8r). Forward steps issue a small `partial_advance` to
  /// extend the in-flight segment, finalize it when crossing a
  /// boundary, and bulk-`advance` any skipped segments in between.
  advanceTo(
    headFraction: number,
    segments: ToolpathSegment[],
    /// Resolves the cutting tool for a given toolpath segment index. The
    /// sim must carve each segment with ITS op's tool (a v-bit cuts a V,
    /// an endmill a cylinder) — feeding one tool for the whole program
    /// made multi-op runs carve with the wrong cross-section. A bare
    /// ToolEntry is accepted for single-tool callers / tests.
    toolForSeg: ToolEntry | ((segIdx: number) => ToolEntry),
    cumLen?: Float64Array | null,
    totalLen?: number,
    /// 27ng: when false (the default), a backward scrub leaves the
    /// heightfield untouched — cells retain their deepest-ever
    /// cuts, and the cursor (`appliedSeg` / `partialT`) does not
    /// move backward, so subsequent forward scrubs resume from the
    /// previous max position. When true, the driver runs the
    /// reset + forward-replay path so the heightfield exactly
    /// reflects the cuts up to the new playhead. Replay is O(N)
    /// in segments-replayed and currently has a known visual
    /// artifact on chunked / LOD meshes (5w9z), so it lives behind
    /// a Settings toggle.
    exactRewind: boolean = false,
  ): boolean {
    const resolveTool =
      typeof toolForSeg === 'function' ? toolForSeg : () => toolForSeg as ToolEntry;
    if (!this.sim || !this.mesh) return false;
    const total = segments.length;
    if (total === 0) return false;

    let segIdx: number;
    let segT: number;
    if (cumLen && cumLen.length === total && totalLen && totalLen > 0) {
      const r = playheadToSegment(headFraction, cumLen, totalLen);
      if (r.segIdx < 0) return false;
      segIdx = r.segIdx;
      segT = r.segT;
    } else {
      const clamped = Math.max(0, Math.min(1, headFraction));
      const c = clamped * total;
      segIdx = Math.min(total - 1, Math.floor(c));
      segT = c - segIdx;
    }

    const plan = planAdvance(this.appliedSeg, this.partialT, segIdx, segT, total);
    if (!plan) return false;

    // 27ng: a backward scrub asks the planner to emit `reset: true`
    // plus a forward-replay sequence from t=0 to the new playhead.
    // Default behavior (exactRewind=false) skips the whole advance:
    // the heightmap is forward-monotone (cuts only deepen), so
    // leaving the cells alone shows the deepest-ever state and
    // subsequent forward scrubs correctly resume from the previous
    // max position. exactRewind=true runs the reset + replay so
    // the visible heights track the playhead — slow on long
    // programs and currently has a chunked-mesh artifact (5w9z),
    // which is why it lives behind a Settings toggle.
    if (plan.reset && !exactRewind) {
      // Leave appliedSeg / partialT untouched. The heightmap and
      // mesh stay at the previous max-deep state. The PlaybackBar's
      // hint tells the user this is the cheap path.
      return false;
    }

    // Backward scrub WITH exactRewind: reset the simulator and let
    // the planner's forward ops replay. The mesh has to be
    // refreshed from the clean sim heights BEFORE the forward
    // replay; otherwise cells outside the replay's dirty AABB keep
    // the stale (deeper) heights from the previous playhead.
    if (plan.reset) {
      this.sim.reset();
      this.diagnostics = { warnings: [] };
      this.notifyDiagnostics();
      // 9tba: clear coarse pool data before the forward replay so
      // the active LOD level draws an uncut block to start.
      this.mesh?.reset();
      this.refreshHeightView();
      if (this.heightView && this.mesh) this.mesh.updateHeights(this.heightView);
    }

    // Wire-tool cache (by tool id) so a multi-segment run doesn't
    // re-serialize the same tool spec repeatedly.
    const wireCache = new Map<number, Record<string, unknown>>();
    const wireFor = (segIdx: number): Record<string, unknown> => {
      const t = resolveTool(segIdx);
      let w = wireCache.get(t.id);
      if (!w) {
        w = toWireTool(t);
        wireCache.set(t.id, w);
      }
      return w;
    };
    // Defensive re-cache if the toolpath identity drifts from the
    // build()-time snapshot (e.g. a Generate response replaced
    // `project.generated.toolpath` without going through build()).
    // The common path is a no-op compare (audit-9l52).
    if (segments !== this.cachedToolpath) {
      this.sim.set_toolpath(segments);
      this.cachedToolpath = segments;
    }

    let unionAabb: [number, number, number, number] | null = null;
    const unionWith = (a: Uint32Array | number[]) => {
      if (a.length !== 4) return;
      if (!unionAabb) {
        unionAabb = [a[0], a[1], a[2], a[3]];
      } else {
        if (a[0] < unionAabb[0]) unionAabb[0] = a[0];
        if (a[1] < unionAabb[1]) unionAabb[1] = a[1];
        if (a[2] > unionAabb[2]) unionAabb[2] = a[2];
        if (a[3] > unionAabb[3]) unionAabb[3] = a[3];
      }
    };

    if (plan.finalizePartial) {
      const { segIdx: fIdx, fromT } = plan.finalizePartial;
      unionWith(this.sim.partial_advance(wireFor(fIdx), fIdx, fromT, 1));
      this.collectDiagnostics();
    }
    if (plan.bulkAdvance) {
      // Split the contiguous range into runs of consecutive segments that
      // share a tool (ops are contiguous in the toolpath), advancing each
      // run with its own tool so per-op cutter shapes carve correctly.
      const { from, to } = plan.bulkAdvance;
      let runStart = from;
      while (runStart < to) {
        const runToolId = resolveTool(runStart).id;
        let runEnd = runStart + 1;
        while (runEnd < to && resolveTool(runEnd).id === runToolId) runEnd++;
        unionWith(this.sim.advance(wireFor(runStart), runStart, runEnd));
        runStart = runEnd;
      }
      this.collectDiagnostics();
    }
    if (plan.startPartial) {
      const { segIdx: sIdx, startT, endT } = plan.startPartial;
      unionWith(this.sim.partial_advance(wireFor(sIdx), sIdx, startT, endT));
      this.collectDiagnostics();
    }

    this.appliedSeg = plan.newAppliedSeg;
    this.partialT = plan.newPartialT;

    // Re-take the buffer view: any advance / partial_advance call may
    // have grown WASM linear memory and detached the prior view.
    this.refreshHeightView();
    if (this.heightView) {
      const a = unionAabb as [number, number, number, number] | null;
      // 5w9z: after a reset-driven backstep replay, do a FULL mesh
      // re-upload regardless of how small the forward replay's
      // dirty AABB came out. The partial-AABB path leaves cells
      // outside the AABB at whatever the pre-replay state was —
      // correct ON THE FIRST FRAME after the reset's full upload
      // (those cells are topZ), but defense-in-depth against the
      // chunked / LOD pyramid possibly retaining stale pool data
      // in some configuration. The cost is one extra full upload
      // per backstep, which the user opted into via the
      // Settings exact-rewind toggle.
      if (a && !plan.reset) {
        this.mesh.updateHeights(this.heightView, {
          ix0: a[0],
          iy0: a[1],
          ix1: a[2],
          iy1: a[3],
        });
      } else {
        this.mesh.updateHeights(this.heightView);
      }
    }

    this.scheduleEdgeRebuild();
    this.opts.requestRender();
    return true;
  }

  setVisible(visible: boolean) {
    this.group.visible = visible;
  }

  setSolidVisible(visible: boolean) {
    this.mesh?.setSolidVisible(visible);
  }

  setEdgesVisible(visible: boolean) {
    this.mesh?.setEdgesVisible(visible);
  }

  /// Live-apply settings changes (color / opacity). Resolution / max
  /// cells changes require a full rebuild via build().
  applyStyle(
    settings: Pick<AppSettings, 'solidColor' | 'solidOpacity' | 'edgeColor' | 'edgeOpacity'>,
  ) {
    this.mesh?.setStyle({
      solidColor: settings.solidColor,
      solidOpacity: settings.solidOpacity,
      edgeColor: settings.edgeColor,
      edgeOpacity: settings.edgeOpacity,
    });
    this.opts.requestRender();
  }

  /// 9tba: drive the LOD pyramid's active level. Caller is Scene3D's
  /// render loop, which feeds `pixelsPerL0Cell` from the camera
  /// projection and `maxRenderTriangles` from settings; the pyramid
  /// picks the coarser of the distance- and budget-recommended
  /// levels. Returns the level actually applied so the caller can
  /// debug-log or display it.
  ///
  /// Distance hysteresis: switching to a COARSER level uses the
  /// `1.0 px / cell` threshold; switching back to a FINER level
  /// requires `1.2 px / cell` (20% gap). Without this, a tiny pan
  /// near the threshold would oscillate the active level every
  /// frame.
  setLodHint(pixelsPerL0Cell: number, maxRenderTriangles: number): number {
    if (!this.mesh) return 0;
    const current = this.mesh.getActiveLevel();
    const budgetLevel = this.mesh.recommendBudgetLevel(maxRenderTriangles);
    const coarsenLevel = this.mesh.recommendDistanceLevel(pixelsPerL0Cell, 1.0);
    const finerLevel = this.mesh.recommendDistanceLevel(pixelsPerL0Cell, 1.2);
    const coarsenTarget = Math.max(coarsenLevel, budgetLevel);
    const finerTarget = Math.max(finerLevel, budgetLevel);
    let next = current;
    if (coarsenTarget > current) {
      next = coarsenTarget;
    } else if (finerTarget < current) {
      next = finerTarget;
    }
    if (next !== current) {
      this.mesh.setActiveLevel(next);
      // The new level's EdgesGeometry is stale — schedule a rebuild
      // through the same trailing-debounce path that handles carves
      // so a pan that crosses LOD thresholds doesn't stall on a
      // synchronous edge rebuild.
      this.scheduleEdgeRebuild();
      this.opts.requestRender();
    }
    return this.mesh.getActiveLevel();
  }

  /// Current active LOD level (or `null` when no mesh exists). Used
  /// by Scene3D for the debug overlay.
  getLodLevel(): number | null {
    return this.mesh ? this.mesh.getActiveLevel() : null;
  }

  /// L0 cell-size in mm so the camera-distance LOD heuristic can
  /// project a single cell to screen pixels.
  getCellSize(): number | null {
    return this.sim ? this.sim.cell_size() : null;
  }

  /// 9c34: serialize the carved heightfield as a binary STL. Returns
  /// `null` when there is no live simulator (no project loaded yet, or
  /// the driver was disposed). Walls drop to `stockBottomZ` at every
  /// perimeter sample for a watertight mesh.
  exportStl(stockBottomZ: number): Uint8Array | null {
    return this.sim ? this.sim.export_stl(stockBottomZ) : null;
  }

  dispose() {
    if (this.mesh) {
      this.group.remove(this.mesh.group);
      this.mesh.dispose();
      this.mesh = null;
    }
    if (this.sim) {
      this.sim.free();
      this.sim = null;
    }
    this.heightView = null;
    this.appliedSeg = 0;
    this.partialT = 0;
    this.cachedToolpath = null;
    if (this.diagnostics.warnings.length > 0) {
      this.diagnostics = { warnings: [] };
      this.notifyDiagnostics();
    }
  }

  destroy() {
    if (this.edgeRebuildTimer != null) {
      clearTimeout(this.edgeRebuildTimer);
      this.edgeRebuildTimer = null;
    }
    this.dispose();
    this.opts.scene.remove(this.group);
    // 9c34: deregister so a stale handle can't be reached.
    if (currentDriver === this) currentDriver = null;
  }

  private collectDiagnostics() {
    if (!this.sim) return;
    const fresh = this.sim.take_diagnostics();
    if (!fresh || !Array.isArray(fresh.warnings) || fresh.warnings.length === 0) return;
    // Dedupe against what's already accumulated: the sim re-emits sticky
    // warnings (cell_size_coarsened) every advance and re-fires segment
    // warnings on scrub-back, which otherwise pile up duplicate rows and
    // flood the window (and inflate the critical count).
    const seen = new Set(this.diagnostics.warnings.map(simWarningKey));
    const added = fresh.warnings.filter((w) => {
      const k = simWarningKey(w);
      if (seen.has(k)) return false;
      seen.add(k);
      return true;
    });
    if (added.length === 0) return;
    this.diagnostics = {
      warnings: [...this.diagnostics.warnings, ...added],
    };
    this.notifyDiagnostics();
  }

  private notifyDiagnostics() {
    this.onDiagnosticsChange?.(this.diagnostics);
  }

  private refreshHeightView() {
    if (!this.wasm || !this.sim) {
      this.heightView = null;
      return;
    }
    const cols = this.sim.cols();
    const rows = this.sim.rows();
    this.heightView = new Float32Array(this.wasm.memory.buffer, this.sim.data_ptr(), cols * rows);
  }

  private scheduleEdgeRebuild() {
    if (!this.mesh) return;
    // Bail above the budget — too expensive to rebuild and visually
    // useless at that density. The LOD pyramid normally swaps to a
    // coarser level long before this, but the cap protects against
    // a user-configured `maxRenderTriangles` that pushes L0 over it.
    if (this.mesh.getActiveTriangleCount() > HeightfieldDriver.EDGE_MAX_TRIANGLES) {
      if (this.edgeRebuildTimer != null) {
        clearTimeout(this.edgeRebuildTimer);
        this.edgeRebuildTimer = null;
      }
      return;
    }
    // Pure trailing debounce: reset the timer on every call so the
    // rebuild only fires after EDGE_REBUILD_MS of quiet. Continuous
    // playback at 60 fps never sees it; idle frames after the user
    // stops scrubbing do.
    if (this.edgeRebuildTimer != null) clearTimeout(this.edgeRebuildTimer);
    this.edgeRebuildTimer = setTimeout(() => {
      this.edgeRebuildTimer = null;
      if (!this.mesh) return;
      // Re-check the cap in case the active level changed since the
      // timer was armed.
      if (this.mesh.getActiveTriangleCount() > HeightfieldDriver.EDGE_MAX_TRIANGLES) return;
      this.mesh.rebuildEdges();
      this.opts.requestRender();
    }, HeightfieldDriver.EDGE_REBUILD_MS);
  }
}
