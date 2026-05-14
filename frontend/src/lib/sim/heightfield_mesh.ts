import * as THREE from 'three';

/// Options for constructing a HeightfieldMesh. `cols`/`rows` are the
/// heightmap grid dimensions; `cellSize` is the spacing in mm between
/// adjacent samples. `originX`/`originY` place the heightmap's
/// (ix=0, iy=0) corner in world XY. `topZ` is the unmilled stock surface
/// height every cell starts at; `floorZ` is the stock bottom — wall
/// quads on the grid boundary drop from each cell's Z down to floorZ
/// when they face the outside, and any cell carved all the way through
/// to `floorZ` (or below) is rendered as a flat hole. The four
/// color/opacity fields drive the solid faces and can be live-updated
/// via `setStyle`. `edgeColor`/`edgeOpacity`/`edgeThresholdDeg` are
/// accepted for backwards compatibility with the previous
/// PlaneGeometry implementation; the stepped renderer has no separate
/// edge geometry.
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
/// `data[iy * cols + ix]`) as an indexed BufferGeometry with stepped
/// per-cell top faces + vertical wall quads. Each cell owns:
///   * 4 top-face vertices (always at the cell's Z) → 2 triangles.
///   * 4 vertices for its +X (right) wall, between this cell and the
///     ix+1 neighbor (or `topZ` if on the grid edge) → 2 triangles.
///   * 4 vertices for its +Y (top) wall, between this cell and the
///     iy+1 neighbor (or `topZ`) → 2 triangles.
/// Plus a fringe of -X and -Y walls for cells on the ix=0 / iy=0
/// edges so the stock's outer wall is visible from any angle.
///
/// Walls between cells that share a Z value collapse to zero-area
/// (degenerate) triangles which the rasterizer drops at no fragment
/// cost. Only the active wall discontinuities consume fill — the same
/// triangle count as the old PlaneGeometry for the smooth case, half
/// of the boxes-per-cell InstancedMesh for the dense case, and
/// vertical (not interpolated) for the so70 cylindrical-tool fix.
///
/// `updateHeights(view, aabb?)` rewrites only the dirty AABB's
/// vertex Z values + the wall Z values on the immediate −X and −Y
/// neighbors (since those walls reference this cell's Z on their
/// far side) and sets `position.updateRange` so Three.js uploads
/// only the touched sub-range to the GPU.
export class HeightfieldMesh {
  readonly group: THREE.Group;

  private readonly cols: number;
  private readonly rows: number;
  private readonly cellSize: number;
  private readonly originX: number;
  private readonly originY: number;
  private readonly topZ: number;
  private readonly floorZ: number;

  // Vertex region offsets — in VERTICES, not floats. Multiply by 3 to
  // get into the positions/normals arrays.
  private readonly TOP_BASE: number; // 4 * N
  private readonly RIGHT_BASE: number; // 4 * N
  private readonly UP_BASE: number; // 4 * N
  private readonly LEFT_BASE: number; // 4 * rows
  private readonly BOTTOM_BASE: number; // 4 * cols
  /// Single flat quad at floorZ closing the underside of the stock so
  /// the user sees a solid block from any camera angle (regression
  /// fix euxi). 4 static verts, never updated after init.
  private readonly FLOOR_BASE: number; // 4
  private readonly TOTAL_VERTS: number;

  private readonly positions: Float32Array;
  private readonly positionAttr: THREE.BufferAttribute;
  private readonly geometry: THREE.BufferGeometry;
  private readonly material: THREE.MeshStandardMaterial;
  private readonly mesh: THREE.Mesh;

  constructor(opts: HeightfieldOptions) {
    this.cols = opts.cols;
    this.rows = opts.rows;
    this.cellSize = opts.cellSize;
    this.originX = opts.originX;
    this.originY = opts.originY;
    this.topZ = opts.topZ;
    this.floorZ = opts.floorZ < opts.topZ - 1e-3 ? opts.floorZ : opts.topZ - 10.0;

    const n = this.cols * this.rows;
    this.TOP_BASE = 0;
    this.RIGHT_BASE = 4 * n;
    this.UP_BASE = 8 * n;
    this.LEFT_BASE = 12 * n;
    this.BOTTOM_BASE = this.LEFT_BASE + 4 * this.rows;
    this.FLOOR_BASE = this.BOTTOM_BASE + 4 * this.cols;
    this.TOTAL_VERTS = this.FLOOR_BASE + 4;

    this.positions = new Float32Array(this.TOTAL_VERTS * 3);
    const normals = new Float32Array(this.TOTAL_VERTS * 3);
    // Per cell: top(2) + right(2) + up(2) = 6 triangles × 3 indices
    // = 18 indices. Per fringe wall: 2 triangles = 6 indices. Plus 6
    // for the floor quad's 2 triangles. The old `6 * n` allocation
    // (audit-euxi) was sized as triangle COUNT not index count, so
    // writes past index 6*N silently no-op'd — explained the
    // half-missing-mesh regression.
    const indices = new Uint32Array(18 * n + 6 * this.rows + 6 * this.cols + 6);

    this.initStaticBuffers(normals, indices);

    this.geometry = new THREE.BufferGeometry();
    this.positionAttr = new THREE.BufferAttribute(this.positions, 3);
    this.positionAttr.setUsage(THREE.DynamicDrawUsage);
    this.geometry.setAttribute('position', this.positionAttr);
    this.geometry.setAttribute('normal', new THREE.BufferAttribute(normals, 3));
    this.geometry.setIndex(new THREE.BufferAttribute(indices, 1));
    this.geometry.boundingBox = new THREE.Box3(
      new THREE.Vector3(this.originX, this.originY, this.floorZ),
      new THREE.Vector3(
        this.originX + this.cols * this.cellSize,
        this.originY + this.rows * this.cellSize,
        this.topZ,
      ),
    );
    this.geometry.boundingSphere = this.geometry.boundingBox.getBoundingSphere(
      new THREE.Sphere(),
    );

    const isTransparent = opts.solidOpacity < 1;
    this.material = new THREE.MeshStandardMaterial({
      color: new THREE.Color(opts.solidColor),
      opacity: opts.solidOpacity,
      transparent: isTransparent,
      // For the translucent default (opacity 0.5) we must NOT write
      // depth — the stepped mesh emits TOP + WALL triangles in
      // geometry order, not back-to-front, so depthWrite=true causes
      // earlier-drawn faces to occlude later same-pixel faces and the
      // user sees random chunks missing. depthWrite=false lets every
      // visible fragment blend; fully opaque still writes depth as
      // normal.
      depthWrite: !isTransparent,
      side: THREE.DoubleSide,
      roughness: 0.8,
      metalness: 0.0,
    });

    this.mesh = new THREE.Mesh(this.geometry, this.material);
    // Defensive: with the manually-set boundingBox / boundingSphere a
    // tilted camera at the wrong distance occasionally culled the
    // whole mesh on the previous voxel-box renderer; the stepped mesh
    // is large enough that a stale sphere is the obvious regression
    // culprit, so just opt out of frustum culling entirely.
    this.mesh.frustumCulled = false;
    this.group = new THREE.Group();
    this.group.add(this.mesh);

    // Initial state: every cell at topZ (uncut stock). Walls between
    // INTERIOR cells collapse to degenerate triangles automatically
    // (both sides at topZ). Boundary walls — the outward-facing sides
    // of the stock — need their "outside" verts set to floorZ so the
    // uncarved block shows complete vertical sides from frame zero;
    // otherwise the sides look open until the cell first carves.
    for (let i = 0; i < this.TOTAL_VERTS; i++) {
      this.positions[i * 3 + 2] = this.topZ;
    }
    // RIGHT wall outside verts (v2/v3) for the rightmost column drop
    // to floorZ; interior right walls stay at topZ (degenerate).
    for (let iy = 0; iy < this.rows; iy++) {
      const cellIdx = iy * this.cols + (this.cols - 1);
      const p = (this.RIGHT_BASE + cellIdx * 4) * 3;
      this.positions[p + 8] = this.floorZ;
      this.positions[p + 11] = this.floorZ;
    }
    // TOP wall outside verts (v2/v3) for the topmost row drop to floorZ.
    for (let ix = 0; ix < this.cols; ix++) {
      const cellIdx = (this.rows - 1) * this.cols + ix;
      const p = (this.UP_BASE + cellIdx * 4) * 3;
      this.positions[p + 8] = this.floorZ;
      this.positions[p + 11] = this.floorZ;
    }
    // LEFT and BOTTOM fringes: v0/v1 = outside (floorZ), v2/v3 stay at
    // topZ (this cell's top, which equals topZ until a carve lands).
    for (let iy = 0; iy < this.rows; iy++) {
      const p = (this.LEFT_BASE + iy * 4) * 3;
      this.positions[p + 2] = this.floorZ;
      this.positions[p + 5] = this.floorZ;
    }
    for (let ix = 0; ix < this.cols; ix++) {
      const p = (this.BOTTOM_BASE + ix * 4) * 3;
      this.positions[p + 2] = this.floorZ;
      this.positions[p + 5] = this.floorZ;
    }
    // FLOOR quad is fixed at floorZ regardless of carve state.
    for (let k = 0; k < 4; k++) {
      this.positions[(this.FLOOR_BASE + k) * 3 + 2] = this.floorZ;
    }
    this.positionAttr.needsUpdate = true;
  }

  /// Pre-fill the static XY coordinates + normals + index buffer. Z
  /// values get written by updateHeights / the initial uncut-stock
  /// pass in the constructor.
  private initStaticBuffers(normals: Float32Array, indices: Uint32Array): void {
    const cell = this.cellSize;
    const ox = this.originX;
    const oy = this.originY;
    const cols = this.cols;
    const rows = this.rows;

    // Helpers
    const writeVertex = (vIdx: number, x: number, y: number) => {
      const p = vIdx * 3;
      this.positions[p] = x;
      this.positions[p + 1] = y;
      // Z written later by updateHeights.
    };
    const writeNormal = (vIdx: number, nx: number, ny: number, nz: number) => {
      const p = vIdx * 3;
      normals[p] = nx;
      normals[p + 1] = ny;
      normals[p + 2] = nz;
    };
    const pushQuad = (idxOff: number, v0: number, v1: number, v2: number, v3: number) => {
      indices[idxOff] = v0;
      indices[idxOff + 1] = v1;
      indices[idxOff + 2] = v2;
      indices[idxOff + 3] = v1;
      indices[idxOff + 4] = v3;
      indices[idxOff + 5] = v2;
    };

    let indexOff = 0;
    for (let iy = 0; iy < rows; iy++) {
      const yB = oy + iy * cell;
      const yT = yB + cell;
      for (let ix = 0; ix < cols; ix++) {
        const xL = ox + ix * cell;
        const xR = xL + cell;
        const cellIdx = iy * cols + ix;

        // TOP face: 4 corners (CCW from above)
        const tBase = this.TOP_BASE + cellIdx * 4;
        writeVertex(tBase + 0, xL, yB);
        writeVertex(tBase + 1, xR, yB);
        writeVertex(tBase + 2, xR, yT);
        writeVertex(tBase + 3, xL, yT);
        writeNormal(tBase + 0, 0, 0, 1);
        writeNormal(tBase + 1, 0, 0, 1);
        writeNormal(tBase + 2, 0, 0, 1);
        writeNormal(tBase + 3, 0, 0, 1);
        pushQuad(indexOff, tBase + 0, tBase + 1, tBase + 3, tBase + 2);
        indexOff += 6;

        // RIGHT wall: at x = xR, y span [yB, yT]. v0/v1 sit on this
        // cell's edge (zA), v2/v3 on the neighbor's (zB).
        const rBase = this.RIGHT_BASE + cellIdx * 4;
        writeVertex(rBase + 0, xR, yB);
        writeVertex(rBase + 1, xR, yT);
        writeVertex(rBase + 2, xR, yB);
        writeVertex(rBase + 3, xR, yT);
        writeNormal(rBase + 0, 1, 0, 0);
        writeNormal(rBase + 1, 1, 0, 0);
        writeNormal(rBase + 2, 1, 0, 0);
        writeNormal(rBase + 3, 1, 0, 0);
        pushQuad(indexOff, rBase + 0, rBase + 1, rBase + 2, rBase + 3);
        indexOff += 6;

        // TOP wall: at y = yT, x span [xL, xR]. v0/v1 on this cell
        // (zA), v2/v3 on the iy+1 neighbor (zB).
        const uBase = this.UP_BASE + cellIdx * 4;
        writeVertex(uBase + 0, xL, yT);
        writeVertex(uBase + 1, xR, yT);
        writeVertex(uBase + 2, xL, yT);
        writeVertex(uBase + 3, xR, yT);
        writeNormal(uBase + 0, 0, 1, 0);
        writeNormal(uBase + 1, 0, 1, 0);
        writeNormal(uBase + 2, 0, 1, 0);
        writeNormal(uBase + 3, 0, 1, 0);
        pushQuad(indexOff, uBase + 0, uBase + 1, uBase + 2, uBase + 3);
        indexOff += 6;
      }
    }

    // LEFT fringe: one wall per row, at x = originX. v0/v1 sit at the
    // outside (zB = topZ), v2/v3 sit on cell (0, iy)'s edge (zA).
    for (let iy = 0; iy < rows; iy++) {
      const yB = oy + iy * cell;
      const yT = yB + cell;
      const lBase = this.LEFT_BASE + iy * 4;
      writeVertex(lBase + 0, ox, yB);
      writeVertex(lBase + 1, ox, yT);
      writeVertex(lBase + 2, ox, yB);
      writeVertex(lBase + 3, ox, yT);
      writeNormal(lBase + 0, -1, 0, 0);
      writeNormal(lBase + 1, -1, 0, 0);
      writeNormal(lBase + 2, -1, 0, 0);
      writeNormal(lBase + 3, -1, 0, 0);
      pushQuad(indexOff, lBase + 0, lBase + 1, lBase + 2, lBase + 3);
      indexOff += 6;
    }
    // BOTTOM fringe: one wall per column, at y = originY.
    for (let ix = 0; ix < cols; ix++) {
      const xL = ox + ix * cell;
      const xR = xL + cell;
      const bBase = this.BOTTOM_BASE + ix * 4;
      writeVertex(bBase + 0, xL, oy);
      writeVertex(bBase + 1, xR, oy);
      writeVertex(bBase + 2, xL, oy);
      writeVertex(bBase + 3, xR, oy);
      writeNormal(bBase + 0, 0, -1, 0);
      writeNormal(bBase + 1, 0, -1, 0);
      writeNormal(bBase + 2, 0, -1, 0);
      writeNormal(bBase + 3, 0, -1, 0);
      pushQuad(indexOff, bBase + 0, bBase + 1, bBase + 2, bBase + 3);
      indexOff += 6;
    }
    // FLOOR: single quad closing the underside of the stock so the
    // mesh looks solid from any camera angle (regression euxi). Z is
    // floorZ and gets written by the constructor's initial-state
    // loop along with everything else.
    const fxR = ox + cols * cell;
    const fyT = oy + rows * cell;
    const f = this.FLOOR_BASE;
    writeVertex(f + 0, ox, oy);
    writeVertex(f + 1, fxR, oy);
    writeVertex(f + 2, fxR, fyT);
    writeVertex(f + 3, ox, fyT);
    writeNormal(f + 0, 0, 0, -1);
    writeNormal(f + 1, 0, 0, -1);
    writeNormal(f + 2, 0, 0, -1);
    writeNormal(f + 3, 0, 0, -1);
    // CCW from below = (v0, v3, v1) + (v1, v3, v2) so the normal
    // computed from the winding agrees with the stored (-Z) normal.
    pushQuad(indexOff, f + 0, f + 3, f + 1, f + 2);
    indexOff += 6;
  }

  /// Clamp a cell's Z to [floorZ, topZ]. Cells carved below floorZ
  /// render as a flat hole at the floor — no negative-thickness boxes.
  private clampZ(z: number): number {
    if (z > this.topZ) return this.topZ;
    if (z < this.floorZ) return this.floorZ;
    return z;
  }

  /// Read a cell's heightfield value, clamped, with topZ for
  /// out-of-bounds indices (used by walls that face the outside).
  private cellZ(view: Float32Array, ix: number, iy: number): number {
    if (ix < 0 || ix >= this.cols || iy < 0 || iy >= this.rows) {
      return this.topZ;
    }
    return this.clampZ(view[iy * this.cols + ix]);
  }

  /// Rewrite the four top-face vertex Z values for cell (ix, iy).
  private writeTop(ix: number, iy: number, z: number): void {
    const cellIdx = iy * this.cols + ix;
    const p = (this.TOP_BASE + cellIdx * 4) * 3;
    this.positions[p + 2] = z;
    this.positions[p + 5] = z;
    this.positions[p + 8] = z;
    this.positions[p + 11] = z;
  }

  /// Rewrite the four +X wall vertex Z values for cell (ix, iy)'s
  /// right wall. v0/v1 ride on this cell (zA); v2/v3 on the ix+1
  /// neighbor (zB). At the grid's right edge (ix == cols-1) the
  /// "neighbor" is open air — the wall must drop from this cell's
  /// top down to floorZ to close the side of the stock, not up to
  /// topZ (which left the side looking open).
  private writeRightWall(ix: number, iy: number, zA: number, view: Float32Array): void {
    const cellIdx = iy * this.cols + ix;
    const zB = ix + 1 < this.cols ? this.cellZ(view, ix + 1, iy) : this.floorZ;
    const p = (this.RIGHT_BASE + cellIdx * 4) * 3;
    this.positions[p + 2] = zA;
    this.positions[p + 5] = zA;
    this.positions[p + 8] = zB;
    this.positions[p + 11] = zB;
  }

  /// Rewrite the four +Y wall vertex Z values for cell (ix, iy)'s
  /// top wall. Same outside-of-grid handling as the right wall.
  private writeTopWall(ix: number, iy: number, zA: number, view: Float32Array): void {
    const cellIdx = iy * this.cols + ix;
    const zB = iy + 1 < this.rows ? this.cellZ(view, ix, iy + 1) : this.floorZ;
    const p = (this.UP_BASE + cellIdx * 4) * 3;
    this.positions[p + 2] = zA;
    this.positions[p + 5] = zA;
    this.positions[p + 8] = zB;
    this.positions[p + 11] = zB;
  }

  /// LEFT fringe: vertex Zs for cell (0, iy)'s outside-facing wall.
  /// v0/v1 = floorZ (open air outside the stock — nothing material
  /// above floorZ on that side), v2/v3 = this cell's Z (top of the
  /// remaining material in this column).
  private writeLeftFringe(iy: number, zA: number): void {
    const p = (this.LEFT_BASE + iy * 4) * 3;
    this.positions[p + 2] = this.floorZ;
    this.positions[p + 5] = this.floorZ;
    this.positions[p + 8] = zA;
    this.positions[p + 11] = zA;
  }

  /// BOTTOM fringe: vertex Zs for cell (ix, 0)'s outside-facing wall.
  private writeBottomFringe(ix: number, zA: number): void {
    const p = (this.BOTTOM_BASE + ix * 4) * 3;
    this.positions[p + 2] = this.floorZ;
    this.positions[p + 5] = this.floorZ;
    this.positions[p + 8] = zA;
    this.positions[p + 11] = zA;
  }

  updateHeights(
    dataView: Float32Array,
    aabb?: { ix0: number; iy0: number; ix1: number; iy1: number },
  ): void {
    // Expand the dirty rect by 1 cell on −X and −Y so the left/bottom
    // neighbors' right/top walls (which reference this cell's Z on
    // their far side) get re-derived too. Note `ix1`/`iy1` are
    // half-open upper bounds in the sim's AABB convention.
    const ix0 = aabb ? Math.max(0, aabb.ix0 - 1) : 0;
    const iy0 = aabb ? Math.max(0, aabb.iy0 - 1) : 0;
    const ix1 = aabb ? Math.min(this.cols, aabb.ix1) : this.cols;
    const iy1 = aabb ? Math.min(this.rows, aabb.iy1) : this.rows;

    for (let iy = iy0; iy < iy1; iy++) {
      const dataRow = iy * this.cols;
      for (let ix = ix0; ix < ix1; ix++) {
        const z = this.clampZ(dataView[dataRow + ix]);
        // Only rewrite the top face when this cell is actually inside
        // the original (non-expanded) AABB — the −X/−Y expansion is
        // there to pick up neighbor walls, not extra top-face writes.
        const inOriginal =
          (!aabb || (ix >= aabb.ix0 && ix < aabb.ix1 && iy >= aabb.iy0 && iy < aabb.iy1));
        if (inOriginal) {
          this.writeTop(ix, iy, z);
        }
        // Both walls always need refresh: their far-side Z could have
        // moved even if this cell didn't change.
        this.writeRightWall(ix, iy, z, dataView);
        this.writeTopWall(ix, iy, z, dataView);
        if (ix === 0) this.writeLeftFringe(iy, z);
        if (iy === 0) this.writeBottomFringe(ix, z);
      }
    }

    // Partial buffer upload (audit-6tmz): tell Three.js to upload
    // only the float ranges we touched, not the whole buffer. For
    // typical per-segment AABBs this is tens of kB instead of MBs.
    // Three.js's `addUpdateRange(start, count)` (>= r158) lets us
    // post multiple ranges so the LEFT/BOTTOM fringe writes don't
    // force a full upload.
    this.positionAttr.clearUpdateRanges();
    const lowCellIdx = iy0 * this.cols + ix0;
    const highCellIdx = (iy1 - 1) * this.cols + (ix1 - 1);
    const cellsMinVert = this.TOP_BASE + lowCellIdx * 4;
    const cellsMaxVert = this.UP_BASE + highCellIdx * 4 + 4;
    this.positionAttr.addUpdateRange(cellsMinVert * 3, (cellsMaxVert - cellsMinVert) * 3);
    if (ix0 === 0) {
      this.positionAttr.addUpdateRange(
        (this.LEFT_BASE + iy0 * 4) * 3,
        (iy1 - iy0) * 4 * 3,
      );
    }
    if (iy0 === 0) {
      this.positionAttr.addUpdateRange(
        (this.BOTTOM_BASE + ix0 * 4) * 3,
        (ix1 - ix0) * 4 * 3,
      );
    }
    this.positionAttr.needsUpdate = true;
  }

  /// No-op in the stepped renderer; kept for API compatibility with
  /// the previous PlaneGeometry implementation.
  rebuildEdges(): void {
    // Intentionally empty.
  }

  setStyle(opts: Partial<HeightfieldOptions>): void {
    if (opts.solidColor !== undefined) {
      this.material.color.set(opts.solidColor);
    }
    if (opts.solidOpacity !== undefined) {
      this.material.opacity = opts.solidOpacity;
      const transparent = opts.solidOpacity < 1;
      this.material.transparent = transparent;
      // Mirror the depthWrite policy from the constructor — see the
      // comment there for why transparent + depthWrite=true hides
      // chunks of the stepped mesh.
      this.material.depthWrite = !transparent;
    }
    this.material.needsUpdate = true;
  }

  setVisible(visible: boolean): void {
    this.group.visible = visible;
  }

  setEdgesVisible(_visible: boolean): void {
    // Stepped renderer has no separate edge mesh.
  }

  setSolidVisible(visible: boolean): void {
    this.mesh.visible = visible;
  }

  dispose(): void {
    this.geometry.dispose();
    this.material.dispose();
    this.group.remove(this.mesh);
  }
}
