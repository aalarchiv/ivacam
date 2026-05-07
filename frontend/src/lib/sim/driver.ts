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
  ToolpathSegment,
} from '../api/types';
import type { ToolEntry, AppSettings } from '../state/project.svelte';

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
  advance(
    segments: unknown,
    tool: unknown,
    from_idx: number,
    to_idx: number,
  ): Uint32Array;
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
      // The pkg is built by `wasm-pack build crates/wiac-wasm --target web`.
      // The frontend has it linked via package.json so the bare specifier
      // resolves at bundle time.
      const mod = (await import(/* @vite-ignore */ 'wiac-wasm')) as WasmModule;
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
    ...(t.dragoff !== undefined ? { dragoff: t.dragoff } : {}),
    flutes: t.flutes,
    speed: t.speed,
    plunge_rate: t.plungeRate,
    feed_rate: t.feedRate,
    coolant: t.coolant,
  };
}

/// Compute the simulator footprint from the imported geometry + stock
/// config. Defaults to imported bbox plus a small margin; manual mode
/// uses customX/Y centered on the bbox.
function computeFootprint(
  imported: ImportResponse | null,
  stock: {
    mode: 'auto' | 'manual';
    margin: number;
    customX: number;
    customY: number;
  },
): { minX: number; minY: number; maxX: number; maxY: number } {
  if (!imported) {
    return { minX: 0, minY: 0, maxX: 100, maxY: 100 };
  }
  const { min_x, min_y, max_x, max_y } = imported.bbox;
  if (stock.mode === 'manual') {
    const cx = (min_x + max_x) * 0.5;
    const cy = (min_y + max_y) * 0.5;
    return {
      minX: cx - stock.customX * 0.5,
      minY: cy - stock.customY * 0.5,
      maxX: cx + stock.customX * 0.5,
      maxY: cy + stock.customY * 0.5,
    };
  }
  const m = Math.max(0, stock.margin);
  return { minX: min_x - m, minY: min_y - m, maxX: max_x + m, maxY: max_y + m };
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
  private appliedHead = 0;
  /// Edges rebuild is expensive; debounce to avoid stalling on every
  /// playhead frame. Tracks the last time edges were rebuilt; the
  /// driver only triggers a rebuild every EDGE_REBUILD_MS or at the
  /// end of a playback session.
  private lastEdgeRebuild = 0;
  private edgeRebuildScheduled = false;
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
    stock: { mode: 'auto' | 'manual'; margin: number; thickness: number; customX: number; customY: number };
    settings: AppSettings;
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
    this.dispose();
    this.sim = new this.wasm.Simulator(
      fp.minX,
      fp.minY,
      fp.maxX,
      fp.maxY,
      effectiveCellSize,
      topZ,
    );
    this.mesh = new HeightfieldMesh({
      cols: this.sim.cols(),
      rows: this.sim.rows(),
      cellSize: this.sim.cell_size(),
      originX: this.sim.origin_x(),
      originY: this.sim.origin_y(),
      topZ: this.sim.top_z(),
      solidColor: input.settings.solidColor,
      solidOpacity: input.settings.solidOpacity,
      edgeColor: input.settings.edgeColor,
      edgeOpacity: input.settings.edgeOpacity,
    });
    this.group.add(this.mesh.group);
    this.appliedHead = 0;
    this.refreshHeightView();
  }

  /// Advance the simulation to `headFraction` (a number in [0, 1] —
  /// project.playhead). Returns true if the mesh was modified.
  advanceTo(
    headFraction: number,
    segments: ToolpathSegment[],
    tool: ToolEntry,
  ): boolean {
    if (!this.sim || !this.mesh) return false;
    const total = segments.length;
    const newHead = Math.max(0, Math.min(total, Math.round(headFraction * total)));
    if (newHead === this.appliedHead) return false;
    if (newHead < this.appliedHead) {
      // Backward scrub: can't undo cuts, replay from zero.
      this.sim.reset();
      this.appliedHead = 0;
    }
    if (newHead > this.appliedHead) {
      const wireTool = toWireTool(tool);
      const aabb = this.sim.advance(segments, wireTool, this.appliedHead, newHead);
      this.appliedHead = newHead;
      // Memory may have grown — re-take the view before reading cells.
      this.refreshHeightView();
      if (this.heightView && aabb.length === 4) {
        const [ix0, iy0, ix1, iy1] = aabb;
        this.mesh.updateHeights(this.heightView, { ix0, iy0, ix1, iy1 });
      } else if (this.heightView) {
        this.mesh.updateHeights(this.heightView);
      }
      this.scheduleEdgeRebuild();
      this.opts.requestRender();
      return true;
    }
    // Backward scrub fell through: replay 0..newHead in one shot.
    if (newHead > 0) {
      const wireTool = toWireTool(tool);
      this.sim.advance(segments, wireTool, 0, newHead);
      this.appliedHead = newHead;
    }
    this.refreshHeightView();
    if (this.heightView) this.mesh.updateHeights(this.heightView);
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
  applyStyle(settings: Pick<AppSettings, 'solidColor' | 'solidOpacity' | 'edgeColor' | 'edgeOpacity'>) {
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
  }

  destroy() {
    this.dispose();
    this.opts.scene.remove(this.group);
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
      this.edgeRebuildScheduled = false;
      return;
    }
    if (this.edgeRebuildScheduled) return;
    this.edgeRebuildScheduled = true;
    setTimeout(() => {
      this.edgeRebuildScheduled = false;
      this.lastEdgeRebuild = performance.now();
      this.mesh?.rebuildEdges();
      this.opts.requestRender();
    }, HeightfieldDriver.EDGE_REBUILD_MS);
  }
}
