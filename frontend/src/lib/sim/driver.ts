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
import { HeightfieldMesh } from './heightfield_mesh';
import type {
  GenerateResponse,
  ImportResponse,
  SimDiagnostics,
  ToolpathSegment,
} from '../api/types';
import type { AppSettings, Fixture, ToolEntry } from '../state/project.svelte';

interface SimulatorWasm {
  new (
    minX: number,
    minY: number,
    maxX: number,
    maxY: number,
    cellSize: number,
    topZ: number,
  ): SimulatorWasm;
  reset(): void;
  advance(tool: unknown, from_idx: number, to_idx: number): Uint32Array;
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
function toWireTool(t: ToolEntry): Record<string, unknown> {
  return {
    id: t.id,
    name: t.name,
    kind: t.kind,
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
    ...(t.shankDiameterMm !== undefined ? { shank_diameter_mm: t.shankDiameterMm } : {}),
    ...(t.holder !== undefined ? { holder: t.holder } : {}),
    // Per-kind sim/holder metadata so the heightfield simulator can
    // pick the right cutter profile and holder-check uses the right
    // shank dims. Bull-nose / T-slot collapse to Endmill in the sim
    // for now (see ToolProfile::from_tool), but ship the fields so
    // the data is there when the sim grows fillet / undercut support.
    ...(t.cornerRadiusMm !== undefined ? { corner_radius_mm: t.cornerRadiusMm } : {}),
    ...(t.tslotNeckDiameterMm !== undefined
      ? { tslot_neck_diameter_mm: t.tslotNeckDiameterMm }
      : {}),
    ...(t.tslotNeckLengthMm !== undefined ? { tslot_neck_length_mm: t.tslotNeckLengthMm } : {}),
    ...(t.zShiftMm !== undefined ? { z_shift_mm: t.zShiftMm } : {}),
  };
}

/// Compute the simulator footprint from the imported geometry + stock
/// config. Defaults to imported bbox plus a small margin; manual mode
/// uses customX/Y centered on the bbox.
export function computeFootprint(
  imported: ImportResponse | null,
  stock: {
    mode: 'auto' | 'manual';
    margin: number;
    customX: number;
    customY: number;
    offsetX?: number;
    offsetY?: number;
  },
  workArea?: { x: number; y: number } | null,
): { minX: number; minY: number; maxX: number; maxY: number } {
  const ox = stock.offsetX ?? 0;
  const oy = stock.offsetY ?? 0;
  // Manual mode: footprint is exactly customX × customY centered on
  // the imported geometry's bbox center (or origin when none).
  if (stock.mode === 'manual') {
    let cx = 0;
    let cy = 0;
    if (imported) {
      const { min_x, min_y, max_x, max_y } = imported.bbox;
      cx = (min_x + max_x) * 0.5;
      cy = (min_y + max_y) * 0.5;
    }
    return {
      minX: cx - stock.customX * 0.5 + ox,
      minY: cy - stock.customY * 0.5 + oy,
      maxX: cx + stock.customX * 0.5 + ox,
      maxY: cy + stock.customY * 0.5 + oy,
    };
  }
  // Auto mode WITH geometry: bbox + margin (the legacy behavior).
  if (imported) {
    const { min_x, min_y, max_x, max_y } = imported.bbox;
    const m = Math.max(0, stock.margin);
    return {
      minX: min_x - m + ox,
      minY: min_y - m + oy,
      maxX: max_x + m + ox,
      maxY: max_y + m + oy,
    };
  }
  // Auto mode WITHOUT geometry: default to the machine work-area
  // footprint anchored at the origin.
  if (workArea && workArea.x > 0 && workArea.y > 0) {
    return { minX: ox, minY: oy, maxX: workArea.x + ox, maxY: workArea.y + oy };
  }
  // Final fallback for clients that don't pass a work area.
  return { minX: ox, minY: oy, maxX: 100 + ox, maxY: 100 + oy };
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

export class HeightfieldDriver {
  readonly group: THREE.Group;
  private sim: SimulatorWasm | null = null;
  private mesh: HeightfieldMesh | null = null;
  private wasm: WasmHandle | null = null;
  /// Cached buffer view; valid until the next advance() that may grow
  /// WASM linear memory. Re-taken after every advance.
  private heightView: Float32Array | null = null;
  /// Reference to the toolpath array that's currently cached on the
  /// WASM Simulator. Used to detect identity drift (e.g. a stale
  /// driver picking up a new Generate response) and trigger a single
  /// re-cache rather than re-deserializing per frame (audit-9l52).
  private cachedToolpath: ToolpathSegment[] | null = null;
  private appliedHead = 0;
  /// Cumulative sim warnings collected since the last replay. Cleared on
  /// dispose / reset; merged-into on every forward advance() so the UI
  /// can mark the offending segments as the user scrubs.
  private diagnostics: SimDiagnostics = { warnings: [] };
  private onDiagnosticsChange: ((d: SimDiagnostics) => void) | null = null;
  /// Edges rebuild is expensive; debounce to avoid stalling on every
  /// playhead frame. Tracks the last time edges were rebuilt; the
  /// driver only triggers a rebuild every EDGE_REBUILD_MS or at the
  /// end of a playback session.
  private lastEdgeRebuild = 0;
  private edgeRebuildTimer: ReturnType<typeof setTimeout> | null = null;
  private static readonly EDGE_REBUILD_MS = 120;

  constructor(private opts: DriverOptions) {
    this.group = new THREE.Group();
    this.group.visible = false;
    opts.scene.add(this.group);
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
    // Cap the resolution so a tiny tool on a big stock doesn't OOM.
    const cols = Math.ceil((fp.maxX - fp.minX) / cellSize) + 1;
    const rows = Math.ceil((fp.maxY - fp.minY) / cellSize) + 1;
    const cellCount = cols * rows;
    let effectiveCellSize = cellSize;
    if (cellCount > input.settings.maxSimulationCells) {
      const scale = Math.sqrt(cellCount / input.settings.maxSimulationCells);
      effectiveCellSize = cellSize * scale;
    }
    const topZ = 0; // stock surface is z=0; carving descends to negative Z
    // Stepped voxel renderer needs a finite floor — match the physical
    // stock bottom so the rendered boxes have the right thickness when
    // viewed from below. Default to 10 mm so an unconfigured project
    // still has a visible stock height.
    const stockThickness = input.stock.thickness > 0 ? input.stock.thickness : 10.0;
    this.dispose();
    this.sim = new this.wasm.Simulator(fp.minX, fp.minY, fp.maxX, fp.maxY, effectiveCellSize, topZ);
    this.mesh = new HeightfieldMesh({
      cols: this.sim.cols(),
      rows: this.sim.rows(),
      cellSize: this.sim.cell_size(),
      originX: this.sim.origin_x(),
      originY: this.sim.origin_y(),
      topZ: this.sim.top_z(),
      floorZ: this.sim.top_z() - stockThickness,
      solidColor: input.settings.solidColor,
      solidOpacity: input.settings.solidOpacity,
      edgeColor: input.settings.edgeColor,
      edgeOpacity: input.settings.edgeOpacity,
    });
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
    this.appliedHead = 0;
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
    this.appliedHead = 0;
    this.diagnostics = { warnings: [] };
    this.notifyDiagnostics();
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
  /// alongside the toolpath. Without them this falls back to plain
  /// index-fraction mapping (round(playhead × N segments)), but the
  /// 3D tool mesh and the gcode panel both use arc-length mapping —
  /// when the heightfield uses index-fraction here, mixed long-cut /
  /// short-rapid programs show a sync gap between the tool tip and
  /// the carved surface (the tool floats over un-carved material, or
  /// the carve runs ahead of the tip). With cumLen/totalLen they
  /// agree.
  advanceTo(
    headFraction: number,
    segments: ToolpathSegment[],
    tool: ToolEntry,
    cumLen?: Float64Array | null,
    totalLen?: number,
  ): boolean {
    if (!this.sim || !this.mesh) return false;
    const total = segments.length;
    let newHead: number;
    if (cumLen && cumLen.length === total && totalLen && totalLen > 0) {
      const target = Math.max(0, Math.min(1, headFraction)) * totalLen;
      // Binary search for smallest i where cumLen[i] >= target.
      let lo = 0;
      let hi = cumLen.length - 1;
      while (lo < hi) {
        const mid = (lo + hi) >>> 1;
        if (cumLen[mid] < target) lo = mid + 1;
        else hi = mid;
      }
      const segIdx = lo;
      const segEndLen = cumLen[segIdx];
      const segStartLen = segIdx === 0 ? 0 : cumLen[segIdx - 1];
      const segLen = segEndLen - segStartLen;
      const segT = segLen > 1e-12 ? (target - segStartLen) / segLen : 0;
      // Include the in-progress segment when the tool is more than
      // halfway through it. With segT ∈ [0, 0.5) the carve stops at
      // segIdx (matching the tool sitting near segment start); with
      // segT ∈ [0.5, 1] the carve advances to segIdx+1 (matching the
      // tool sitting near segment end). Worst-case carve-vs-tool
      // mismatch is then ½ segment of arc length, regardless of
      // segment count distribution.
      newHead = Math.max(0, Math.min(total, segIdx + Math.round(segT)));
    } else {
      newHead = Math.max(0, Math.min(total, Math.round(headFraction * total)));
    }
    if (newHead === this.appliedHead) return false;
    if (newHead < this.appliedHead) {
      // Backward scrub: the heightfield is monotone — cuts can only
      // deepen — so we reset the simulator and replay forward. The mesh
      // also has to be refreshed from the now-clean sim heights BEFORE
      // the forward replay; otherwise cells outside the replay's dirty
      // AABB keep the stale (deeper) heights from the previous playhead
      // position, and the user sees material that's still "removed"
      // ahead of where the tool actually is.
      this.sim.reset();
      this.appliedHead = 0;
      this.diagnostics = { warnings: [] };
      this.notifyDiagnostics();
      this.refreshHeightView();
      if (this.heightView && this.mesh) this.mesh.updateHeights(this.heightView);
    }
    if (newHead > this.appliedHead) {
      const wireTool = toWireTool(tool);
      // Defensive re-cache if the toolpath identity drifts from the
      // build()-time snapshot (e.g. a Generate response replaced
      // `project.generated.toolpath` without going through build()).
      // The common path is a no-op compare (audit-9l52).
      if (segments !== this.cachedToolpath) {
        this.sim.set_toolpath(segments);
        this.cachedToolpath = segments;
      }
      const aabb = this.sim.advance(wireTool, this.appliedHead, newHead);
      this.appliedHead = newHead;
      this.collectDiagnostics();
      // Memory may have grown — re-take the view before reading cells.
      this.refreshHeightView();
      if (this.heightView && aabb.length === 4) {
        const [ix0, iy0, ix1, iy1] = aabb;
        this.mesh.updateHeights(this.heightView, { ix0, iy0, ix1, iy1 });
      } else if (this.heightView) {
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
    this.appliedHead = 0;
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
  }

  private collectDiagnostics() {
    if (!this.sim) return;
    const fresh = this.sim.take_diagnostics();
    if (!fresh || !Array.isArray(fresh.warnings) || fresh.warnings.length === 0) return;
    this.diagnostics = {
      warnings: [...this.diagnostics.warnings, ...fresh.warnings],
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
    const now = performance.now();
    if (now - this.lastEdgeRebuild >= HeightfieldDriver.EDGE_REBUILD_MS) {
      this.mesh.rebuildEdges();
      this.lastEdgeRebuild = now;
      if (this.edgeRebuildTimer != null) {
        clearTimeout(this.edgeRebuildTimer);
        this.edgeRebuildTimer = null;
      }
      return;
    }
    if (this.edgeRebuildTimer != null) return;
    this.edgeRebuildTimer = setTimeout(() => {
      this.edgeRebuildTimer = null;
      this.lastEdgeRebuild = performance.now();
      // Guard the dispose race — by the time the debounced rebuild
      // fires, dispose() / destroy() may have cleared the mesh.
      if (!this.mesh) return;
      this.mesh.rebuildEdges();
      this.opts.requestRender();
    }, HeightfieldDriver.EDGE_REBUILD_MS);
  }
}
