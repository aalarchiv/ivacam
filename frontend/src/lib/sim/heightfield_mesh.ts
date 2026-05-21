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
  /// Translucent-mode depth pre-pass. Without it, transparent
  /// fragments blend in geometry-emit order (TOP face first, walls,
  /// floor last), so top-down views end up seeing TOO MUCH of the
  /// floor below and bottom-up views see TOO MUCH of the top — an
  /// asymmetric "more translucent from above" artifact. The pre-pass
  /// writes depth for the front-most face only; the main mesh's
  /// depthTest then keeps just the visible surface (which is what
  /// CAM users actually want to see — the carved top is geometric,
  /// not visible-through-translucency). `setStyle` toggles its
  /// `.visible` so live opacity changes work.
  private readonly depthMesh: THREE.Mesh;
  private readonly depthMaterial: THREE.Material;
  /// Edge overlay: a `LineSegments` over `THREE.EdgesGeometry`
  /// derived from the current heightfield. Rebuilt by
  /// `rebuildEdges()` on the existing 120ms driver debounce so
  /// fast carve sequences don't trigger a per-frame rebuild
  /// (EdgesGeometry is O(triangles)). Highlights the per-cell
  /// step transitions + outer stock boundary so carved features
  /// pop visually against the lit solid.
  private edgeGeometry: THREE.EdgesGeometry;
  private readonly edgeMaterial: THREE.LineBasicMaterial;
  private readonly edgeLines: THREE.LineSegments;
  private readonly edgeThresholdDeg: number;

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

    // Depth pre-pass: a colorless draw of the same geometry that
    // populates the depth buffer with the front-most surface. When
    // the main material is translucent, the main mesh's depthTest
    // then culls back faces so the user sees ONE tinted surface
    // rather than the alpha-blended layer stack. Built unconditionally
    // and toggled via `.visible` so live opacity changes (setStyle)
    // work without rebuilding meshes. Redundant when opaque (the main
    // mesh writes depth itself), so kept hidden in that case.
    this.depthMaterial = new THREE.MeshBasicMaterial({
      colorWrite: false,
      depthWrite: true,
      depthTest: true,
      side: THREE.DoubleSide,
    });
    this.depthMesh = new THREE.Mesh(this.geometry, this.depthMaterial);
    this.depthMesh.frustumCulled = false;
    // Lower renderOrder → drawn first.
    this.depthMesh.renderOrder = -1;
    this.mesh.renderOrder = 0;
    this.depthMesh.visible = isTransparent;
    this.group.add(this.depthMesh);
    this.group.add(this.mesh);

    // Edge overlay. ThresholdDeg = 1 catches every wall→top transition
    // (walls are vertical, exactly 90° from the top face) without
    // emitting noise for coplanar same-height cell boundaries. Lines
    // ride at renderOrder=2 so they sit on top of both the depth
    // pre-pass (renderOrder=-1) and the lit solid (renderOrder=0).
    this.edgeThresholdDeg = opts.edgeThresholdDeg ?? 1;
    this.edgeGeometry = new THREE.EdgesGeometry(this.geometry, this.edgeThresholdDeg);
    this.edgeMaterial = new THREE.LineBasicMaterial({
      color: new THREE.Color(opts.edgeColor),
      opacity: opts.edgeOpacity,
      transparent: opts.edgeOpacity < 1,
      depthTest: true,
      depthWrite: false,
    });
    this.edgeLines = new THREE.LineSegments(this.edgeGeometry, this.edgeMaterial);
    this.edgeLines.frustumCulled = false;
    this.edgeLines.renderOrder = 2;
    this.group.add(this.edgeLines);

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

  /// Rebuild the edge overlay from the current heightfield positions.
  /// `THREE.EdgesGeometry` is O(triangles) and doesn't support partial
  /// updates, so this is on the driver's 120ms debounce — fast carve
  /// sequences won't thrash it. The edge color/opacity stay on
  /// `edgeMaterial` across rebuilds; only the geometry is swapped.
  rebuildEdges(): void {
    const old = this.edgeGeometry;
    this.edgeGeometry = new THREE.EdgesGeometry(this.geometry, this.edgeThresholdDeg);
    this.edgeLines.geometry = this.edgeGeometry;
    old.dispose();
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
      // Depth pre-pass is only needed in translucent mode. Opaque
      // mode writes depth in the main pass so the pre-pass would be
      // redundant work.
      this.depthMesh.visible = transparent;
    }
    if (opts.edgeColor !== undefined) {
      this.edgeMaterial.color.set(opts.edgeColor);
    }
    if (opts.edgeOpacity !== undefined) {
      this.edgeMaterial.opacity = opts.edgeOpacity;
      this.edgeMaterial.transparent = opts.edgeOpacity < 1;
      this.edgeMaterial.needsUpdate = true;
    }
    this.material.needsUpdate = true;
  }

  setVisible(visible: boolean): void {
    this.group.visible = visible;
  }

  setEdgesVisible(visible: boolean): void {
    this.edgeLines.visible = visible;
  }

  setSolidVisible(visible: boolean): void {
    this.mesh.visible = visible;
    // Depth pre-pass writes depth ONLY when the solid is visible —
    // otherwise it would occlude other scene geometry behind an
    // invisible mesh.
    this.depthMesh.visible = visible && this.material.transparent;
  }

  dispose(): void {
    this.geometry.dispose();
    this.material.dispose();
    this.group.remove(this.mesh);
    this.group.remove(this.depthMesh);
    this.depthMaterial.dispose();
    this.group.remove(this.edgeLines);
    this.edgeGeometry.dispose();
    this.edgeMaterial.dispose();
  }
}

/// LOD pyramid of HeightfieldMesh instances (9tba). Builds N parallel
/// meshes at successively coarser resolution (L0 = full, L1 = 2×2-pooled,
/// L2 = 4×4, …) and exposes the same `updateHeights` surface as a single
/// HeightfieldMesh so the driver can swap in a pyramid without touching
/// its dispatch logic.
///
/// Only the active level's mesh is attached to the scene group; every
/// other level's group is detached until selected. Memory cost over a
/// single mesh is `sum_{k≥1} 1/4^k = 1/3` for an infinite pyramid; with
/// the default 4 levels it's ~33%.
///
/// **MIN-pool semantics.** Heights drop monotonically as the sim
/// carves, so we pool each LOD block as `min(L0 cells in block)`. This
/// keeps the deepest cut visible at any LOD; a MAX-pool would hide
/// cuts at coarse levels (the LOD would over-report uncarved
/// material). A small-trace-loss caveat: cuts narrower than the LOD
/// cell width may disappear at that level, but at the camera distance
/// triggering that LOD the trace was sub-pixel anyway.
///
/// **Dirty-AABB flow.** The driver hands the WASM L0 view + an L0-coord
/// AABB to `updateHeights`. If the active level is L0, that forwards
/// directly to mesh.updateHeights. For Lk > 0 the pyramid re-pools the
/// L0 AABB span (padded out to whole pool blocks) into its own Lk
/// buffer, then forwards a translated LOD-coord AABB to `levels[k]`.
export class HeightfieldMeshPyramid {
  readonly group: THREE.Group;
  /// `levels[k]` is the HeightfieldMesh for LOD level k, or `null` when
  /// `k < minLevel` (we skip building unaffordable fine levels — the
  /// L0 mesh alone is ~280 MB at 1 M cells, so the budget-driven
  /// `minLevel` keeps us inside the GPU ceiling regardless of sim
  /// cell count).
  private readonly levels: Array<HeightfieldMesh | null>;
  /// `pools[0]` is the WASM-backed Float32Array stored on the latest
  /// updateHeights call so a level-swap can re-pool from it. `pools[k>0]`
  /// is owned by the pyramid — one Float32Array of cols_k * rows_k
  /// floats per coarse level. `pools[k]` for `k < minLevel` is an
  /// empty placeholder (no mesh attached, no buffer needed).
  private readonly pools: Float32Array[];
  private readonly levelCols: number[];
  private readonly levelRows: number[];
  /// Source-grid dimensions (L0 cols/rows). Mirrored on each level via
  /// `levelCols[0]`/`levelRows[0]` for symmetry.
  private readonly cols: number;
  private readonly rows: number;
  private readonly topZ: number;
  private activeLevel: number;

  /// Lowest pyramid level the user can render at. Levels below this
  /// were skipped at construction because the budget said their mesh
  /// would exceed the render-triangle ceiling.
  readonly minLevel: number;
  /// Maximum LOD level index — the deepest pool index (so total levels
  /// = `maxLevel + 1`). Default 3 → 4 levels (L0..L3 with 1×, 4×, 16×,
  /// 64× area pooling) before `minLevel` trims the bottom.
  readonly maxLevel: number;

  /// `minLevel` is the lowest LOD index whose mesh is actually built.
  /// Callers pass the budget-driven floor (see
  /// `pickMinLodLevelForBudget`) so unaffordable fine levels never
  /// allocate. Defaults to 0 (build all levels including L0) for tests
  /// and small grids.
  constructor(opts: HeightfieldOptions, maxLevel = 3, minLevel = 0) {
    this.group = new THREE.Group();
    this.cols = opts.cols;
    this.rows = opts.rows;
    this.topZ = opts.topZ;
    this.maxLevel = Math.max(0, maxLevel);
    this.minLevel = Math.max(0, Math.min(this.maxLevel, minLevel));
    this.levels = [];
    this.pools = [];
    this.levelCols = [];
    this.levelRows = [];
    // Build each level k in [minLevel, maxLevel]. Cell dimensions halve
    // per step, rounded up so a grid with a residual partial cell still
    // gets one LOD cell that covers it. Levels below minLevel are kept
    // as null placeholders so `levels[k]` indexing stays one-to-one
    // with k regardless of skipped fine levels.
    for (let k = 0; k <= this.maxLevel; k++) {
      const factor = 1 << k;
      const cols_k = Math.max(1, Math.ceil(this.cols / factor));
      const rows_k = Math.max(1, Math.ceil(this.rows / factor));
      this.levelCols.push(cols_k);
      this.levelRows.push(rows_k);
      if (k < this.minLevel) {
        this.levels.push(null);
        this.pools.push(new Float32Array(0));
        continue;
      }
      const cellSize_k = opts.cellSize * factor;
      const mesh = new HeightfieldMesh({
        ...opts,
        cols: cols_k,
        rows: rows_k,
        cellSize: cellSize_k,
      });
      this.levels.push(mesh);
      if (k === 0) {
        // L0's pool view is plugged in by updateHeights.
        this.pools.push(new Float32Array(0));
      } else {
        const pool = new Float32Array(cols_k * rows_k);
        pool.fill(this.topZ);
        this.pools.push(pool);
      }
    }
    // Active level starts at the floor.
    this.activeLevel = this.minLevel;
    const initialMesh = this.levels[this.activeLevel];
    if (initialMesh) this.group.add(initialMesh.group);
  }

  /// Swap the active mesh in the scene. Clamps to `[minLevel, maxLevel]`.
  /// If switching to a non-L0 level, fully re-pool from the stored L0
  /// view so the new mesh shows the current carved state from frame
  /// one. Cheap: only one mesh is in the scene at a time so the GPU
  /// draw set doesn't grow.
  setActiveLevel(k: number): void {
    const clamped = Math.max(this.minLevel, Math.min(this.maxLevel, k));
    if (clamped === this.activeLevel) return;
    const oldMesh = this.levels[this.activeLevel];
    const newMesh = this.levels[clamped];
    if (oldMesh) this.group.remove(oldMesh.group);
    if (newMesh) this.group.add(newMesh.group);
    this.activeLevel = clamped;
    if (!newMesh) return;
    if (this.pools[0].length === 0) {
      // No L0 view yet — driver hasn't called updateHeights. Leave the
      // new level at its initial-stock state until the next update.
      return;
    }
    if (clamped === 0) {
      newMesh.updateHeights(this.pools[0]);
    } else {
      this.poolRange(clamped, 0, 0, this.cols, this.rows);
      newMesh.updateHeights(this.pools[clamped]);
    }
    // The new level's EdgesGeometry is stale (positions were just
    // updated), but the rebuild is O(triangles) — call out to the
    // driver's trailing-debounce scheduler instead of running it
    // synchronously here so an LOD swap during a pan doesn't stall
    // the frame. Edges will catch up on the next idle tick.
  }

  /// Recommend an LOD level from the rendered cell-pixel-size + the
  /// configured triangle budget. The caller picks the coarser of the
  /// two (i.e. `Math.max(distLevel, budgetLevel)`), then optionally
  /// applies hysteresis before calling `setActiveLevel`.
  ///
  /// `pixelsPerL0Cell` is the apparent screen pixel size of a single
  /// L0 cell at the current camera distance. `minPixelsPerCell` is the
  /// target floor (≈1 keeps cells at sub-pixel size before promoting
  /// to a coarser LOD).
  recommendDistanceLevel(pixelsPerL0Cell: number, minPixelsPerCell: number): number {
    if (pixelsPerL0Cell <= 0 || minPixelsPerCell <= 0) return this.minLevel;
    if (pixelsPerL0Cell >= minPixelsPerCell) return this.minLevel;
    const ratio = minPixelsPerCell / pixelsPerL0Cell;
    // log2 floor: e.g. ratio 1.4 → 0 (stay); 2.1 → 1; 4.5 → 2; 9.0 → 3.
    const k = Math.floor(Math.log2(ratio));
    return Math.min(this.maxLevel, Math.max(this.minLevel, k));
  }

  recommendBudgetLevel(maxRenderTriangles: number): number {
    if (maxRenderTriangles <= 0) return this.minLevel;
    for (let k = this.minLevel; k <= this.maxLevel; k++) {
      const tris = this.levelCols[k] * this.levelRows[k] * 6;
      if (tris <= maxRenderTriangles) return k;
    }
    return this.maxLevel;
  }

  /// Active level index. Useful for debug overlays / tests.
  getActiveLevel(): number {
    return this.activeLevel;
  }

  /// Triangle count of the currently-active level's mesh. Excludes the
  /// constant fringe / floor quads (negligible vs cell × 6).
  getActiveTriangleCount(): number {
    return this.levelCols[this.activeLevel] * this.levelRows[this.activeLevel] * 6;
  }

  /// Drop-in for HeightfieldMesh.updateHeights. Stores the L0 view so a
  /// later level-swap can re-pool from it, then forwards the dirty
  /// span to the active level (with min-pooling for Lk > 0).
  updateHeights(
    dataView: Float32Array,
    aabb?: { ix0: number; iy0: number; ix1: number; iy1: number },
  ): void {
    this.pools[0] = dataView;
    const k = this.activeLevel;
    const activeMesh = this.levels[k];
    if (!activeMesh) return;
    if (k === 0) {
      activeMesh.updateHeights(dataView, aabb);
      return;
    }
    const f = 1 << k;
    if (!aabb) {
      this.poolRange(k, 0, 0, this.cols, this.rows);
      activeMesh.updateHeights(this.pools[k]);
      return;
    }
    this.poolRange(k, aabb.ix0, aabb.iy0, aabb.ix1, aabb.iy1);
    const lod_ix0 = Math.max(0, Math.floor(aabb.ix0 / f));
    const lod_iy0 = Math.max(0, Math.floor(aabb.iy0 / f));
    const lod_ix1 = Math.min(this.levelCols[k], Math.ceil(aabb.ix1 / f));
    const lod_iy1 = Math.min(this.levelRows[k], Math.ceil(aabb.iy1 / f));
    if (lod_ix1 > lod_ix0 && lod_iy1 > lod_iy0) {
      activeMesh.updateHeights(this.pools[k], {
        ix0: lod_ix0,
        iy0: lod_iy0,
        ix1: lod_ix1,
        iy1: lod_iy1,
      });
    }
  }

  /// MIN-pool L0 cells in `[ix0, ix1) × [iy0, iy1)` into the
  /// corresponding LOD-k cells of `pools[k]`. The LOD-cell range is the
  /// AABB ceiling-divided by `2^k`. Exposed-as-private only.
  private poolRange(
    k: number,
    ix0: number,
    iy0: number,
    ix1: number,
    iy1: number,
  ): void {
    const f = 1 << k;
    const cols_k = this.levelCols[k];
    const rows_k = this.levelRows[k];
    const lod_ix0 = Math.max(0, Math.floor(ix0 / f));
    const lod_iy0 = Math.max(0, Math.floor(iy0 / f));
    const lod_ix1 = Math.min(cols_k, Math.ceil(ix1 / f));
    const lod_iy1 = Math.min(rows_k, Math.ceil(iy1 / f));
    const L0 = this.pools[0];
    const pool = this.pools[k];
    const cols = this.cols;
    const rows = this.rows;
    for (let py = lod_iy0; py < lod_iy1; py++) {
      const blockY0 = py * f;
      const blockY1 = Math.min(rows, blockY0 + f);
      for (let px = lod_ix0; px < lod_ix1; px++) {
        const blockX0 = px * f;
        const blockX1 = Math.min(cols, blockX0 + f);
        let m = L0[blockY0 * cols + blockX0];
        for (let iy = blockY0; iy < blockY1; iy++) {
          const row = iy * cols;
          for (let ix = blockX0; ix < blockX1; ix++) {
            const v = L0[row + ix];
            if (v < m) m = v;
          }
        }
        pool[py * cols_k + px] = m;
      }
    }
  }

  /// Reset every level back to uncut stock state. Mirrors the
  /// `HeightfieldMesh` post-`sim.reset()` flow: L0's WASM data is back
  /// at topZ (the driver will re-feed its view); coarse pools must be
  /// re-filled and uploaded so the active LOD's mesh shows topZ instead
  /// of the previous frame's carved state.
  reset(): void {
    for (let k = this.minLevel; k <= this.maxLevel; k++) {
      if (k > 0) this.pools[k].fill(this.topZ);
    }
    // Don't re-upload here: the driver calls updateHeights right after
    // its own refreshHeightView, and that's the right time to push the
    // reset pool data to the GPU.
  }

  rebuildEdges(): void {
    this.levels[this.activeLevel]?.rebuildEdges();
  }

  setStyle(opts: Partial<HeightfieldOptions>): void {
    for (const m of this.levels) m?.setStyle(opts);
  }

  setVisible(visible: boolean): void {
    this.group.visible = visible;
  }

  setEdgesVisible(visible: boolean): void {
    for (const m of this.levels) m?.setEdgesVisible(visible);
  }

  setSolidVisible(visible: boolean): void {
    for (const m of this.levels) m?.setSolidVisible(visible);
  }

  dispose(): void {
    for (const m of this.levels) m?.dispose();
    while (this.group.children.length > 0) {
      this.group.remove(this.group.children[0]);
    }
  }
}

/// 9tba helper: smallest LOD level `k ∈ [0, maxLevel]` whose mesh fits
/// the render-triangle budget for a `cols × rows` source heightmap.
/// Used by callers to decide the pyramid's `minLevel` BEFORE the
/// constructor allocates any HeightfieldMesh — skipping unaffordable
/// fine levels keeps total GPU memory predictable regardless of the
/// user's `maxSimulationCells` setting.
export function pickMinLodLevelForBudget(
  cols: number,
  rows: number,
  maxRenderTriangles: number,
  maxLevel = 3,
): number {
  if (maxRenderTriangles <= 0) return 0;
  for (let k = 0; k <= maxLevel; k++) {
    const cols_k = Math.max(1, Math.ceil(cols / (1 << k)));
    const rows_k = Math.max(1, Math.ceil(rows / (1 << k)));
    if (cols_k * rows_k * 6 <= maxRenderTriangles) return k;
  }
  return maxLevel;
}
