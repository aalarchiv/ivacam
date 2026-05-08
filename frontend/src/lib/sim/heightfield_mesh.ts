import * as THREE from 'three';

/// Options for constructing a HeightfieldMesh. `cols`/`rows` are the
/// heightmap grid dimensions; `cellSize` is the spacing in mm between
/// adjacent samples. `originX`/`originY` place the heightmap's
/// (ix=0, iy=0) corner in world XY. `topZ` is the unmilled stock surface
/// height every vertex starts at. The four color/opacity fields drive the
/// solid faces and the overlay edges and can be live-updated via
/// `setStyle`.
export interface HeightfieldOptions {
  cols: number;
  rows: number;
  cellSize: number;
  originX: number;
  originY: number;
  topZ: number;
  solidColor: string;
  solidOpacity: number;
  edgeColor: string;
  edgeOpacity: number;
  edgeThresholdDeg?: number;
}

/// Renders a Float32Array heightmap (cols × rows, row-major bottom-up,
/// `data[iy * cols + ix]`) as a Three.js heightfield with two coordinated
/// passes: semi-transparent solid faces plus an opaque LineSegments edge
/// overlay. Add `mesh.group` to a scene, call `updateHeights` whenever
/// the heightmap data changes, and `rebuildEdges` after the topology
/// settles (debounced — `THREE.EdgesGeometry` does a full topology pass
/// and is too expensive for every frame).
///
/// PlaneGeometry vertex layout: with widthSegments = cols-1 and
/// heightSegments = rows-1 we get exactly cols × rows vertices. Three.js
/// emits them in nested loops with iy outermost (iy=0 → y=-heightHalf,
/// the bottom row) and ix innermost, so the vertex index iy * cols + ix
/// matches the heightmap's row-major bottom-up layout 1:1.
export class HeightfieldMesh {
  readonly group: THREE.Group;

  private readonly cols: number;
  private readonly rows: number;
  private edgeThresholdDeg: number;
  private readonly cellSize: number;

  private readonly solidGeometry: THREE.PlaneGeometry;
  private readonly solidMaterial: THREE.MeshStandardMaterial;
  private readonly solidMesh: THREE.Mesh;

  private edgesGeometry: THREE.EdgesGeometry;
  private readonly edgesMaterial: THREE.LineBasicMaterial;
  private readonly edgesMesh: THREE.LineSegments;

  constructor(opts: HeightfieldOptions) {
    this.cols = opts.cols;
    this.rows = opts.rows;
    this.cellSize = opts.cellSize;
    this.edgeThresholdDeg = opts.edgeThresholdDeg ?? 30;

    // PlaneGeometry width = (cols - 1) * cellSize so the vertex-to-vertex
    // spacing equals cellSize exactly. (cols * cellSize would put one
    // extra cell-worth of space between the outer vertices.)
    this.solidGeometry = new THREE.PlaneGeometry(
      (opts.cols - 1) * opts.cellSize,
      (opts.rows - 1) * opts.cellSize,
      opts.cols - 1,
      opts.rows - 1,
    );
    // Lift every vertex to the unmilled stock surface so an empty
    // heightmap renders as a flat plane at topZ rather than at z=0.
    const positions = this.solidGeometry.attributes.position.array as Float32Array;
    const vertexCount = opts.cols * opts.rows;
    for (let i = 0; i < vertexCount; i++) {
      positions[i * 3 + 2] = opts.topZ;
    }
    this.solidGeometry.attributes.position.needsUpdate = true;
    this.solidGeometry.computeVertexNormals();

    this.solidMaterial = new THREE.MeshStandardMaterial({
      color: new THREE.Color(opts.solidColor),
      opacity: opts.solidOpacity,
      transparent: true,
      // depthWrite is safe here: a heightfield is 2.5D so faces never
      // overlap themselves in screen space; back-faces don't sort badly
      // and we want them visible from below to read the surface shape.
      depthWrite: true,
      side: THREE.DoubleSide,
      roughness: 0.8,
      metalness: 0.0,
    });
    this.solidMesh = new THREE.Mesh(this.solidGeometry, this.solidMaterial);

    this.edgesGeometry = new THREE.EdgesGeometry(this.solidGeometry, this.edgeThresholdDeg);
    this.edgesMaterial = new THREE.LineBasicMaterial({
      color: new THREE.Color(opts.edgeColor),
      opacity: opts.edgeOpacity,
      transparent: opts.edgeOpacity < 1,
      depthTest: true,
    });
    this.edgesMesh = new THREE.LineSegments(this.edgesGeometry, this.edgesMaterial);

    this.group = new THREE.Group();
    this.group.add(this.solidMesh);
    this.group.add(this.edgesMesh);
    // PlaneGeometry is centered at the local origin. Translate so the
    // heightmap's (originX, originY) corner lands on vertex (ix=0, iy=0).
    this.group.position.set(
      opts.originX + ((opts.cols - 1) * opts.cellSize) / 2,
      opts.originY + ((opts.rows - 1) * opts.cellSize) / 2,
      0,
    );
  }

  updateHeights(
    dataView: Float32Array,
    aabb?: { ix0: number; iy0: number; ix1: number; iy1: number },
  ): void {
    // PlaneGeometry's vertex layout is row-major TOP-DOWN — Three.js
    // emits each vertex as (x, -y, 0), so vertex with index `iy*cols+ix`
    // sits at local Y = +halfHeight - iy*step. The heightmap, on the
    // other hand, is row-major BOTTOM-UP (cell (ix, iy=0) lives at the
    // smallest world Y). Without an iy flip on upload, the carved
    // surface appeared mirrored on Y — the user reported it as the
    // simulator being "rotated 180° around Z".
    const positions = this.solidGeometry.attributes.position.array as Float32Array;
    const ix0 = aabb ? Math.max(0, aabb.ix0) : 0;
    const iy0 = aabb ? Math.max(0, aabb.iy0) : 0;
    const ix1 = aabb ? Math.min(this.cols, aabb.ix1) : this.cols;
    const iy1 = aabb ? Math.min(this.rows, aabb.iy1) : this.rows;
    const lastRow = this.rows - 1;
    for (let iy = iy0; iy < iy1; iy++) {
      const dataRow = iy * this.cols;
      const vertRow = (lastRow - iy) * this.cols;
      for (let ix = ix0; ix < ix1; ix++) {
        positions[(vertRow + ix) * 3 + 2] = dataView[dataRow + ix];
      }
    }
    this.solidGeometry.attributes.position.needsUpdate = true;
    // Partial-AABB normal recompute: re-derive normals only for the
    // dirty vertex range plus a 1-cell halo. Full computeVertexNormals
    // on a 1k×1k grid was ~40 ms / frame; this is O(AABB area).
    //
    // Heightfield normal at (ix, iy): finite-difference gradient of Z.
    //   dz/dx = (z(ix+1, iy) - z(ix-1, iy)) / (2 * cellSize)
    //   dz/dy = (z(ix, iy+1) - z(ix, iy-1)) / (2 * cellSize)
    //   normal = normalize(-dz/dx, -dz/dy, 1)
    //
    // The Y component is in PLANE-LOCAL coords. PlaneGeometry's iy axis
    // is flipped (vertex iy=0 at +halfHeight = top), but we wrote data
    // with the flip already applied (vertRow = (rows-1-iy)*cols), so
    // the local-Y gradient direction matches the heightmap's iy
    // direction by construction — no extra negation here.
    this.refreshNormalsAabb(ix0, iy0, ix1, iy1);
  }

  private refreshNormalsAabb(ix0: number, iy0: number, ix1: number, iy1: number): void {
    const positions = this.solidGeometry.attributes.position.array as Float32Array;
    const normalAttr = this.solidGeometry.attributes.normal as THREE.BufferAttribute;
    const normals = normalAttr.array as Float32Array;
    const cols = this.cols;
    const rows = this.rows;
    const lastRow = rows - 1;
    const inv2cell = 1.0 / (2.0 * this.cellSize);
    // Halo of 1 in vertex index terms — neighbors needed for central
    // differences. Clamp to [0, n].
    const lo_ix = Math.max(0, ix0 - 1);
    const lo_iy = Math.max(0, iy0 - 1);
    const hi_ix = Math.min(cols, ix1 + 1);
    const hi_iy = Math.min(rows, iy1 + 1);
    // Geometry vertex index for heightmap cell (ix, iy_data) is
    // (lastRow - iy_data) * cols + ix (the same flip the upload uses).
    // We iterate in heightmap (iy_data) coords so the gradient sign is
    // straightforward: +iy_data → +Y_world.
    const z = (ix: number, iy: number): number => {
      const v = (lastRow - iy) * cols + ix;
      return positions[v * 3 + 2];
    };
    for (let iy = lo_iy; iy < hi_iy; iy++) {
      const ip1 = Math.min(rows - 1, iy + 1);
      const im1 = Math.max(0, iy - 1);
      for (let ix = lo_ix; ix < hi_ix; ix++) {
        const jp1 = Math.min(cols - 1, ix + 1);
        const jm1 = Math.max(0, ix - 1);
        const dzdx = (z(jp1, iy) - z(jm1, iy)) * inv2cell;
        const dzdy = (z(ix, ip1) - z(ix, im1)) * inv2cell;
        const nx = -dzdx;
        const ny = -dzdy;
        const nz = 1.0;
        const inv_len = 1.0 / Math.sqrt(nx * nx + ny * ny + nz * nz);
        const v = (lastRow - iy) * cols + ix;
        const off = v * 3;
        normals[off] = nx * inv_len;
        normals[off + 1] = ny * inv_len;
        normals[off + 2] = nz * inv_len;
      }
    }
    normalAttr.needsUpdate = true;
  }

  rebuildEdges(): void {
    const next = new THREE.EdgesGeometry(this.solidGeometry, this.edgeThresholdDeg);
    this.edgesGeometry.dispose();
    this.edgesGeometry = next;
    this.edgesMesh.geometry = next;
  }

  setStyle(opts: Partial<HeightfieldOptions>): void {
    if (opts.solidColor !== undefined) {
      this.solidMaterial.color.set(opts.solidColor);
    }
    if (opts.solidOpacity !== undefined) {
      this.solidMaterial.opacity = opts.solidOpacity;
      this.solidMaterial.transparent = opts.solidOpacity < 1;
    }
    if (opts.edgeColor !== undefined) {
      this.edgesMaterial.color.set(opts.edgeColor);
    }
    if (opts.edgeOpacity !== undefined) {
      this.edgesMaterial.opacity = opts.edgeOpacity;
      this.edgesMaterial.transparent = opts.edgeOpacity < 1;
    }
    if (opts.edgeThresholdDeg !== undefined) {
      this.edgeThresholdDeg = opts.edgeThresholdDeg;
      // Threshold change requires re-deriving edge topology.
      this.rebuildEdges();
    }
    this.solidMaterial.needsUpdate = true;
    this.edgesMaterial.needsUpdate = true;
  }

  setVisible(visible: boolean): void {
    this.group.visible = visible;
  }

  setEdgesVisible(visible: boolean): void {
    this.edgesMesh.visible = visible;
  }

  setSolidVisible(visible: boolean): void {
    this.solidMesh.visible = visible;
  }

  dispose(): void {
    this.solidGeometry.dispose();
    this.solidMaterial.dispose();
    this.edgesGeometry.dispose();
    this.edgesMaterial.dispose();
    this.group.remove(this.solidMesh);
    this.group.remove(this.edgesMesh);
  }
}
