import * as THREE from 'three';

/// Options for constructing a HeightfieldMesh. `cols`/`rows` are the
/// heightmap grid dimensions; `cellSize` is the spacing in mm between
/// adjacent samples. `originX`/`originY` place the heightmap's
/// (ix=0, iy=0) corner in world XY. `topZ` is the unmilled stock surface
/// height every cell starts at; `floorZ` is the stock bottom (every
/// cell's box extends from `floorZ` up to its current Z). The four
/// color/opacity fields drive the solid faces and can be live-updated
/// via `setStyle`. `edgeColor`/`edgeOpacity`/`edgeThresholdDeg` are
/// accepted for backwards compatibility with the previous PlaneGeometry
/// implementation but no longer drive any geometry (see notes below).
export interface HeightfieldOptions {
  cols: number;
  rows: number;
  cellSize: number;
  originX: number;
  originY: number;
  topZ: number;
  floorZ: number;
  solidColor: string;
  solidOpacity: number;
  edgeColor: string;
  edgeOpacity: number;
  edgeThresholdDeg?: number;
}

/// Renders a Float32Array heightmap (cols × rows, row-major bottom-up,
/// `data[iy * cols + ix]`) as a Three.js stepped voxel field using one
/// InstancedMesh of boxes — one box per cell. Each box's top is the
/// cell's Z value; the side walls are exactly vertical so a flat-bottom
/// endmill leaves vertical walls in the preview (not the linear-Z slope
/// PlaneGeometry produced by interpolating between adjacent vertices).
///
/// The bottom of every box is `floorZ` (the stock bottom), so the
/// rendered solid matches the physical stock thickness. Carving lowers
/// the cell's top; cells carved all the way through (Z ≤ floorZ) are
/// hidden by collapsing the instance matrix.
///
/// Edges: with stepped geometry every cell-to-cell boundary is a 90°
/// face pair, so `THREE.EdgesGeometry` would draw the entire grid as
/// a wireframe — not what the user wants. The `rebuildEdges` /
/// `setEdgesVisible` / `edgeColor` / `edgeOpacity` API is kept as a
/// no-op so callers don't have to special-case the new renderer.
export class HeightfieldMesh {
  readonly group: THREE.Group;

  private readonly cols: number;
  private readonly rows: number;
  private readonly cellSize: number;
  private readonly originX: number;
  private readonly originY: number;
  private readonly topZ: number;
  private readonly floorZ: number;

  private readonly boxGeometry: THREE.BoxGeometry;
  private readonly solidMaterial: THREE.MeshStandardMaterial;
  private readonly solidMesh: THREE.InstancedMesh;

  private readonly tmpMatrix: THREE.Matrix4;
  private readonly hiddenMatrix: THREE.Matrix4;

  constructor(opts: HeightfieldOptions) {
    this.cols = opts.cols;
    this.rows = opts.rows;
    this.cellSize = opts.cellSize;
    this.originX = opts.originX;
    this.originY = opts.originY;
    this.topZ = opts.topZ;
    // Guard against floorZ >= topZ (would produce zero-thickness or
    // inverted boxes). Fall back to a 10mm-deep stock if the caller
    // forgot to pass a thickness.
    this.floorZ = opts.floorZ < opts.topZ - 1e-3 ? opts.floorZ : opts.topZ - 10.0;

    this.tmpMatrix = new THREE.Matrix4();
    this.hiddenMatrix = new THREE.Matrix4().makeScale(0, 0, 0);

    // Unit cube centered at the origin. Per-instance matrix scales it
    // to (cellSize, cellSize, height) and translates to the cell's
    // center.
    this.boxGeometry = new THREE.BoxGeometry(1, 1, 1);

    this.solidMaterial = new THREE.MeshStandardMaterial({
      color: new THREE.Color(opts.solidColor),
      opacity: opts.solidOpacity,
      transparent: opts.solidOpacity < 1,
      // depthWrite stays on: an instanced voxel field has no
      // self-overlap, so back-to-front sorting is unnecessary.
      depthWrite: true,
      side: THREE.DoubleSide,
      roughness: 0.8,
      metalness: 0.0,
    });

    const count = opts.cols * opts.rows;
    this.solidMesh = new THREE.InstancedMesh(this.boxGeometry, this.solidMaterial, count);
    this.solidMesh.frustumCulled = false;

    for (let iy = 0; iy < this.rows; iy++) {
      for (let ix = 0; ix < this.cols; ix++) {
        const idx = iy * this.cols + ix;
        this.writeInstance(idx, ix, iy, this.topZ);
      }
    }
    this.solidMesh.instanceMatrix.needsUpdate = true;

    this.group = new THREE.Group();
    this.group.add(this.solidMesh);
  }

  /// Write the instance matrix for a single cell. Boxes with their top
  /// at or below `floorZ` are hidden (zero-scale matrix) so the user
  /// sees a through-cut as an empty cell rather than a sliver of stock.
  private writeInstance(idx: number, ix: number, iy: number, cellZ: number): void {
    const topZ = Math.min(cellZ, this.topZ);
    if (topZ <= this.floorZ + 1e-6) {
      this.solidMesh.setMatrixAt(idx, this.hiddenMatrix);
      return;
    }
    const height = topZ - this.floorZ;
    const centerX = this.originX + (ix + 0.5) * this.cellSize;
    const centerY = this.originY + (iy + 0.5) * this.cellSize;
    const centerZ = this.floorZ + height * 0.5;
    this.tmpMatrix.makeScale(this.cellSize, this.cellSize, height);
    this.tmpMatrix.setPosition(centerX, centerY, centerZ);
    this.solidMesh.setMatrixAt(idx, this.tmpMatrix);
  }

  updateHeights(
    dataView: Float32Array,
    aabb?: { ix0: number; iy0: number; ix1: number; iy1: number },
  ): void {
    const ix0 = aabb ? Math.max(0, aabb.ix0) : 0;
    const iy0 = aabb ? Math.max(0, aabb.iy0) : 0;
    const ix1 = aabb ? Math.min(this.cols, aabb.ix1) : this.cols;
    const iy1 = aabb ? Math.min(this.rows, aabb.iy1) : this.rows;
    for (let iy = iy0; iy < iy1; iy++) {
      const dataRow = iy * this.cols;
      for (let ix = ix0; ix < ix1; ix++) {
        const idx = iy * this.cols + ix;
        this.writeInstance(idx, ix, iy, dataView[dataRow + ix]);
      }
    }
    this.solidMesh.instanceMatrix.needsUpdate = true;
  }

  /// No-op in the stepped renderer; kept for API compatibility with
  /// the previous PlaneGeometry implementation.
  rebuildEdges(): void {
    // Intentionally empty.
  }

  setStyle(opts: Partial<HeightfieldOptions>): void {
    if (opts.solidColor !== undefined) {
      this.solidMaterial.color.set(opts.solidColor);
    }
    if (opts.solidOpacity !== undefined) {
      this.solidMaterial.opacity = opts.solidOpacity;
      this.solidMaterial.transparent = opts.solidOpacity < 1;
    }
    this.solidMaterial.needsUpdate = true;
  }

  setVisible(visible: boolean): void {
    this.group.visible = visible;
  }

  setEdgesVisible(_visible: boolean): void {
    // Stepped renderer has no edge mesh.
  }

  setSolidVisible(visible: boolean): void {
    this.solidMesh.visible = visible;
  }

  dispose(): void {
    this.boxGeometry.dispose();
    this.solidMaterial.dispose();
    this.solidMesh.dispose();
    this.group.remove(this.solidMesh);
  }
}
