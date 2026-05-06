<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as THREE from 'three';
  import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
  import { project } from '../state/project.svelte';

  let host: HTMLDivElement;
  let renderer: THREE.WebGLRenderer | undefined;
  let scene: THREE.Scene | undefined;
  let camera: THREE.PerspectiveCamera | undefined;
  let controls: OrbitControls | undefined;
  let geometryGroup: THREE.Group | undefined;
  let toolGroup: THREE.Group | undefined;
  let raf = 0;
  let observer: ResizeObserver | undefined;
  let themeMql: MediaQueryList | undefined;
  let themeMo: MutationObserver | undefined;

  // Pickable line mesh + owner map. Each entry in `lineOwners` describes
  // the source of one *line pair* (two consecutive vertices in the
  // BufferAttribute) so a Raycaster.intersectObject hit can be mapped
  // back to either an imported object id or a toolpath segment index.
  type LineOwner =
    | { kind: 'object'; objectId: number }
    | { kind: 'toolpath'; segIdx: number };
  let linesObject: THREE.LineSegments | undefined;
  let lineOwners: LineOwner[] = [];
  let sceneRadius = 100;
  // Click vs. drag: OrbitControls owns pointermove so we only treat a
  // pointerup as a click when the user barely moved the cursor between
  // down and up. 3px / 400ms is the same threshold the 2D pane uses.
  let pointerStart: { x: number; y: number; t: number } | null = null;
  const raycaster = new THREE.Raycaster();
  const ndc = new THREE.Vector2();

  function cssVar(name: string, fallback: string): string {
    if (!host) return fallback;
    const v = getComputedStyle(host).getPropertyValue(name).trim();
    return v || fallback;
  }
  function cssColor(name: string, fallback: number): THREE.Color {
    return new THREE.Color(cssVar(name, '') || fallback);
  }

  /// Deterministic hue in [0, 1) per op id. Spread by the golden-ratio
  /// conjugate so even close ids land far apart on the wheel.
  function opPalette(opId: number): number {
    const phi = 0.6180339887498949;
    return ((opId * phi) % 1 + 1) % 1;
  }

  onMount(() => {
    scene = new THREE.Scene();
    scene.background = cssColor('--bg-app', 0x0d0d0d);

    camera = new THREE.PerspectiveCamera(45, 1, 0.1, 5000);
    camera.position.set(150, -150, 150);
    camera.up.set(0, 0, 1);

    renderer = new THREE.WebGLRenderer({ antialias: true });
    renderer.setPixelRatio(window.devicePixelRatio);
    host.appendChild(renderer.domElement);
    renderer.domElement.addEventListener('pointerdown', onPointerDown);
    renderer.domElement.addEventListener('pointerup', onPointerUp);

    controls = new OrbitControls(camera, renderer.domElement);
    controls.enableDamping = true;

    // Z-up grid on the XY plane. Colors track the active theme.
    const gridMajor = cssColor('--grid-major', 0x262626);
    const gridMinor = cssColor('--grid-minor', 0x1a1a1a);
    const grid = new THREE.GridHelper(400, 40, gridMajor, gridMinor);
    grid.rotation.x = Math.PI / 2;
    grid.name = 'theme-grid';
    scene.add(grid);

    const axes = new THREE.AxesHelper(50);
    scene.add(axes);

    scene.add(new THREE.AmbientLight(0xffffff, 0.7));
    const dir = new THREE.DirectionalLight(0xffffff, 0.8);
    dir.position.set(100, -100, 200);
    scene.add(dir);

    geometryGroup = new THREE.Group();
    scene.add(geometryGroup);

    toolGroup = new THREE.Group();
    scene.add(toolGroup);

    observer = new ResizeObserver(() => fit());
    observer.observe(host);
    fit();

    const tick = () => {
      controls?.update();
      if (renderer && scene && camera) renderer.render(scene, camera);
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);

    // Re-skin background + grid + (re-trigger) toolpath colors when the
    // OS theme changes OR the user toggles a manual theme. The toolpath
    // group rebuilds via the $effect below since we touch project.imported
    // as a Svelte dep.
    themeMql = window.matchMedia('(prefers-color-scheme: light)');
    const onTheme = () => applyTheme();
    themeMql.addEventListener('change', onTheme);
    themeMo = new MutationObserver(() => applyTheme());
    themeMo.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
  });

  function applyTheme() {
    if (!scene) return;
    scene.background = cssColor('--bg-app', 0x0d0d0d);
    const grid = scene.getObjectByName('theme-grid') as THREE.GridHelper | undefined;
    if (grid) {
      const newGrid = new THREE.GridHelper(
        400,
        40,
        cssColor('--grid-major', 0x262626),
        cssColor('--grid-minor', 0x1a1a1a),
      );
      newGrid.rotation.x = Math.PI / 2;
      newGrid.name = 'theme-grid';
      scene.remove(grid);
      grid.geometry.dispose();
      (grid.material as THREE.Material).dispose();
      scene.add(newGrid);
    }
    rebuildGeometry();
  }

  onDestroy(() => {
    cancelAnimationFrame(raf);
    observer?.disconnect();
    if (renderer) {
      renderer.domElement.removeEventListener('pointerdown', onPointerDown);
      renderer.domElement.removeEventListener('pointerup', onPointerUp);
    }
    controls?.dispose();
    renderer?.dispose();
    if (themeMql) {
      const handler = () => applyTheme();
      themeMql.removeEventListener('change', handler);
    }
    themeMo?.disconnect();
    if (renderer && host?.contains(renderer.domElement)) {
      host.removeChild(renderer.domElement);
    }
  });

  function fit() {
    if (!renderer || !camera || !host) return;
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    renderer.setSize(w, h);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
  }

  // Mirror imported geometry into the 3D scene as flat polylines on Z=0.
  // When a /generate response is also available, draw the 3D toolpath on
  // top with depth + color coded by move kind (rapid/cut/plunge/retract).
  // Splitting these effects keeps OrbitControls responsive during playback —
  // every frame `project.playhead` ticks at 60 Hz, and rebuildGeometry +
  // updateStock + updateTabs are far too expensive to run that often. Only
  // updateTool needs to follow the playhead.
  $effect(() => {
    void project.imported;
    void project.visibleLayers;
    void project.generated;
    void project.tabs;
    void project.stock;
    void project.operations;
    void project.selectedObjects;
    rebuildGeometry();
    updateTabs();
    updateStock();
  });

  $effect(() => {
    void project.playhead;
    void project.generated;
    void project.tools;
    void project.machine;
    void project.selectedOpId;
    updateTool();
  });

  let tabsGroup: THREE.Group | undefined;
  let stockGroup: THREE.Group | undefined;

  /// Translucent stock box + its wireframe. The Z extents go from
  /// setup.mill.depth (or stock.thickness for `manual` mode) up to 0.
  /// In `auto` mode the XY footprint is derived from the imported bbox
  /// plus a margin; otherwise the user supplies customX/customY centered
  /// on the bbox center.
  function updateStock() {
    if (!scene) return;
    if (!stockGroup) {
      stockGroup = new THREE.Group();
      scene.add(stockGroup);
    }
    stockGroup.clear();
    const cfg = project.stock;
    if (!cfg.visible) return;
    const data = project.imported;
    if (!data) return;

    // Stock thickness in auto mode is the deepest enabled-op depth so
    // the box sized to the actual cut volume.
    const opDepth = project.operations
      .filter((o) => o.enabled)
      .reduce((min, o) => Math.min(min, o.depth), 0);
    const cx = (data.bbox.min_x + data.bbox.max_x) * 0.5;
    const cy = (data.bbox.min_y + data.bbox.max_y) * 0.5;
    let sizeX: number;
    let sizeY: number;
    let z0: number;
    if (cfg.mode === 'manual') {
      sizeX = Math.max(0.1, cfg.customX);
      sizeY = Math.max(0.1, cfg.customY);
      z0 = -Math.max(0.1, cfg.thickness);
    } else {
      const margin = Math.max(0, cfg.margin);
      sizeX = (data.bbox.max_x - data.bbox.min_x) + 2 * margin;
      sizeY = (data.bbox.max_y - data.bbox.min_y) + 2 * margin;
      // Default to a 2 mm sheet when no ops are configured yet so the
      // user still sees a sensibly-sized stock outline.
      const depth = Math.abs(opDepth < 0 ? opDepth : -2);
      z0 = -Math.max(0.5, depth);
    }
    if (sizeX <= 0.1 || sizeY <= 0.1) return;

    const sizeZ = -z0;
    const cz = z0 / 2;
    const box = new THREE.BoxGeometry(sizeX, sizeY, sizeZ);
    const fillMat = new THREE.MeshBasicMaterial({
      color: cssColor('--accent', 0x4a8df0),
      transparent: true,
      opacity: 0.07,
      depthWrite: false,
      side: THREE.DoubleSide,
    });
    const fill = new THREE.Mesh(box, fillMat);
    fill.position.set(cx, cy, cz);
    stockGroup.add(fill);

    const edges = new THREE.EdgesGeometry(box);
    const lineMat = new THREE.LineBasicMaterial({
      color: cssColor('--text-muted', 0xa0a0a0),
      transparent: true,
      opacity: 0.55,
    });
    const wire = new THREE.LineSegments(edges, lineMat);
    wire.position.set(cx, cy, cz);
    stockGroup.add(wire);
  }

  function updateTabs() {
    if (!scene) return;
    if (!tabsGroup) {
      tabsGroup = new THREE.Group();
      scene.add(tabsGroup);
    }
    tabsGroup.clear();
    const color = cssColor('--tab-marker', 0xffd23a);
    const radius = Math.max(
      0.5,
      ((project.imported?.bbox.max_x ?? 100) - (project.imported?.bbox.min_x ?? 0)) * 0.008,
    );
    const geom = new THREE.SphereGeometry(radius, 12, 8);
    const mat = new THREE.MeshBasicMaterial({ color });
    for (const list of Object.values(project.tabs)) {
      for (const t of list) {
        const sphere = new THREE.Mesh(geom, mat);
        sphere.position.set(t.x, t.y, 0);
        tabsGroup.add(sphere);
      }
    }
  }

  /// Tool-tip cone: a small inverted cone whose apex sits at the current
  /// toolpath position. Color is the active move kind (cut/plunge/etc.) so
  /// the user can see at a glance what the tool is doing right now.
  /// Tool tip indicator: a real-scale endmill (cylinder), V-bit (cone),
  /// or drag-knife (small blade) sitting above the work with the cutting
  /// tip planted at the current toolpath point. The shape comes from
  /// setup.machine.mode + setup.tool.{diameter,dragoff}; size matches the
  /// configured tool diameter so it visibly differs between a 1 mm engraver
  /// and a 6 mm endmill.
  function updateTool() {
    if (!toolGroup) return;
    toolGroup.clear();
    const gen = project.generated;
    if (!gen || gen.toolpath.length === 0) return;
    const total = gen.toolpath.length;
    const headIdx = Math.max(0, Math.min(total - 1, Math.round(project.playhead * total) - 1));
    const seg = gen.toolpath[headIdx];
    if (!seg) return;
    const subT = project.playhead * total - headIdx;
    const t = Math.max(0, Math.min(1, subT));
    const px = seg.from.x + (seg.to.x - seg.from.x) * t;
    const py = seg.from.y + (seg.to.y - seg.from.y) * t;
    const pz = seg.from.z + (seg.to.z - seg.from.z) * t;

    const tipColor: Record<string, number> = {
      rapid: 0x35a2ff,
      cut: 0xff5555,
      plunge: 0xffd23a,
      retract: 0x5fd06e,
      arc: 0xff8a3a,
    };
    const colorHex = tipColor[seg.kind] ?? 0xff5555;

    // Pick the tool: prefer the selected op's tool, else the active
    // segment's op, else the first tool entry, else fallback.
    const segOp = project.operations.find((o) => o.id === seg.op_id);
    const selOp =
      project.selectedOpId == null
        ? null
        : project.operations.find((o) => o.id === project.selectedOpId) ?? null;
    const opForTool = selOp ?? segOp ?? project.operations[0];
    const tool =
      project.tools.find((t) => t.id === (opForTool?.toolId ?? 0)) ?? project.tools[0];
    const diameter = Math.max(0.2, tool?.diameter ?? 3);
    const radius = diameter * 0.5;
    const mode = project.machine.mode;
    const dragoff = tool?.dragoff;

    // Body color matches the move kind so users see what the tool's doing.
    const mat = new THREE.MeshBasicMaterial({
      color: colorHex,
      transparent: true,
      opacity: 0.85,
    });

    // Tool shape derived from the tool kind first, machine mode second.
    // Each piece is built with its axis along +Z (Z-up world) and the
    // cutting tip at z=0 so mesh.position.set(px, py, pz) lands the tip
    // exactly on the toolpath point.
    const kind = tool?.kind ?? 'endmill';
    let mesh: THREE.Mesh;
    if (mode === 'drag' || kind === 'drag_knife') {
      const off = dragoff ?? 0;
      const bladeLen = Math.max(diameter * 4, 4);
      const bladeT = Math.max(0.4, diameter * 0.4);
      const geom = new THREE.BoxGeometry(bladeT, off > 0 ? off * 2 : bladeT, bladeLen);
      geom.translate(0, 0, bladeLen / 2);
      mesh = new THREE.Mesh(geom, mat);
    } else if (mode === 'laser' || kind === 'laser_beam') {
      const beamLen = Math.max(8, diameter * 6);
      const geom = new THREE.CylinderGeometry(0.3, 0.3, beamLen, 12);
      geom.rotateX(Math.PI / 2); // CylinderGeometry's axis is +Y → put on +Z
      geom.translate(0, 0, beamLen / 2);
      mesh = new THREE.Mesh(geom, mat);
    } else if (kind === 'v_bit' || kind === 'engraver' || kind === 'drill') {
      // Tapered cutter: cone with apex at the cutting tip.
      const len = Math.max(diameter * 4, 8);
      const tipR = (tool?.tipDiameter ?? 0) * 0.5;
      const geom = new THREE.CylinderGeometry(radius, Math.max(tipR, 0.05), len, 24);
      // CylinderGeometry's axis is +Y; the *first* radius is the +Y end and
      // the second is the -Y end. After rotateX(π/2), +Y → -Z so the small
      // (tip) end lands at -Z. Then translate so the tip sits at z=0.
      geom.rotateX(Math.PI / 2);
      geom.translate(0, 0, len / 2);
      mesh = new THREE.Mesh(geom, mat);
    } else if (kind === 'ball_nose') {
      // Cylinder body with a hemisphere at the cutting end.
      const bodyLen = Math.max(diameter * 5, 8);
      const body = new THREE.CylinderGeometry(radius, radius, bodyLen, 24);
      body.rotateX(Math.PI / 2);
      body.translate(0, 0, radius + bodyLen / 2);
      const ball = new THREE.SphereGeometry(radius, 24, 12, 0, Math.PI * 2, 0, Math.PI / 2);
      // Default sphere: top half is +Y. After rotateX(π) it's -Y; we want
      // -Z, so rotateX(-π/2) puts the dome face at -Z. Translate so the
      // pole sits at z=0.
      ball.rotateX(-Math.PI / 2);
      ball.translate(0, 0, radius);
      const merged = mergeBufferGeometries([body, ball]);
      mesh = new THREE.Mesh(merged, mat);
    } else {
      // Endmill: a flat-ended cylinder. No cone — the cutting edge is the
      // bottom face. Tip lands at z=0; body extends +Z.
      const bodyLen = Math.max(diameter * 6, 8);
      const body = new THREE.CylinderGeometry(radius, radius, bodyLen, 24);
      body.rotateX(Math.PI / 2);
      body.translate(0, 0, bodyLen / 2);
      mesh = new THREE.Mesh(body, mat);
    }
    mesh.position.set(px, py, pz);
    toolGroup.add(mesh);
  }

  /// Tiny helper because three.js's BufferGeometryUtils requires a separate
  /// import. Manually splice positions/indices for our two-piece tool.
  function mergeBufferGeometries(geometries: THREE.BufferGeometry[]): THREE.BufferGeometry {
    const out = new THREE.BufferGeometry();
    let posCount = 0;
    let idxCount = 0;
    for (const g of geometries) {
      posCount += g.attributes.position.count;
      idxCount += g.index ? g.index.count : g.attributes.position.count;
    }
    const positions = new Float32Array(posCount * 3);
    const indices = new Uint32Array(idxCount);
    let posOffset = 0;
    let idxOffset = 0;
    let vertexBase = 0;
    for (const g of geometries) {
      const p = g.attributes.position.array as Float32Array;
      positions.set(p, posOffset * 3);
      const idx = g.index ? (g.index.array as ArrayLike<number>) : null;
      const n = g.attributes.position.count;
      if (idx) {
        for (let i = 0; i < idx.length; i++) {
          indices[idxOffset + i] = idx[i] + vertexBase;
        }
        idxOffset += idx.length;
      } else {
        for (let i = 0; i < n; i++) {
          indices[idxOffset + i] = i + vertexBase;
        }
        idxOffset += n;
      }
      vertexBase += n;
      posOffset += n;
    }
    out.setAttribute('position', new THREE.BufferAttribute(positions, 3));
    out.setIndex(new THREE.BufferAttribute(indices, 1));
    out.computeVertexNormals();
    return out;
  }

  function rebuildGeometry() {
    if (!geometryGroup || !scene) return;
    geometryGroup.clear();
    linesObject = undefined;
    lineOwners = [];
    const data = project.imported;
    const gen = project.generated;
    if (!data && !gen) return;

    const positions: number[] = [];
    const colors: number[] = [];
    const c = new THREE.Color();

    const fadedColor = cssColor('--imported-faded', 0x444444);
    const selectedColor = cssColor('--accent', 0x4a8df0);
    if (data) {
      const flat = !!gen;
      let segIdx = 0;
      for (const seg of data.segments) {
        if (!project.visibleLayers.has(seg.layer)) {
          segIdx++;
          continue;
        }
        const objectId = data.objects?.[segIdx] ?? 0;
        const isSelected = objectId > 0 && project.selectedObjects.has(objectId);
        const points = tessellate(seg);
        if (isSelected) {
          c.copy(selectedColor);
        } else if (flat) {
          c.copy(fadedColor);
        } else {
          c.copy(aciColor(seg.color));
        }
        for (let i = 0; i < points.length - 1; i++) {
          const [ax, ay] = points[i];
          const [bx, by] = points[i + 1];
          positions.push(ax, ay, 0, bx, by, 0);
          colors.push(c.r, c.g, c.b, c.r, c.g, c.b);
          lineOwners.push({ kind: 'object', objectId });
        }
        segIdx++;
      }
    }

    if (gen) {
      const moveTints: Record<string, THREE.Color> = {
        rapid: cssColor('--toolpath-rapid', 0x35a2ff),
        cut: cssColor('--toolpath-cut', 0xff5555),
        plunge: cssColor('--toolpath-plunge', 0xffd23a),
        retract: cssColor('--toolpath-retract', 0x5fd06e),
        arc: cssColor('--toolpath-arc', 0xff8a3a),
      };
      const total = gen.toolpath.length;
      const head = Math.max(0, Math.min(total, Math.round(project.playhead * total)));
      for (let i = 0; i < total; i++) {
        const seg = gen.toolpath[i];
        const moveTint = moveTints[seg.kind] ?? moveTints.cut;
        // Per-op base hue from a stable hash of op_id; the move tint
        // brightens it 1.5× for cuts, halves it for rapids — so the eye
        // can pick out each operation while still distinguishing
        // rapid/cut/plunge/retract within an op.
        const opId = seg.op_id ?? 0;
        const opHue = opId === 0 ? 0.0 : opPalette(opId);
        const opCol = new THREE.Color().setHSL(opHue, 0.55, 0.5);
        const moveBoost = seg.kind === 'rapid' ? 0.5 : seg.kind === 'plunge' || seg.kind === 'retract' ? 0.85 : 1.15;
        let r = opId === 0 ? moveTint.r : opCol.r * moveBoost;
        let g = opId === 0 ? moveTint.g : opCol.g * moveBoost;
        let b = opId === 0 ? moveTint.b : opCol.b * moveBoost;
        // Future moves (after the playhead) faded so the user can see
        // what's come and what's coming next.
        if (i >= head) {
          const f = 0.25;
          r = r * f + 0.05;
          g = g * f + 0.05;
          b = b * f + 0.05;
        }
        positions.push(seg.from.x, seg.from.y, seg.from.z, seg.to.x, seg.to.y, seg.to.z);
        colors.push(r, g, b, r, g, b);
        lineOwners.push({ kind: 'toolpath', segIdx: i });
      }
    }
    if (positions.length === 0) return;
    const geom = new THREE.BufferGeometry();
    geom.setAttribute('position', new THREE.Float32BufferAttribute(positions, 3));
    geom.setAttribute('color', new THREE.Float32BufferAttribute(colors, 3));
    const mat = new THREE.LineBasicMaterial({ vertexColors: true });
    const lines = new THREE.LineSegments(geom, mat);
    geometryGroup.add(lines);
    linesObject = lines;

    if (camera && controls) {
      const sphere = new THREE.Sphere();
      lines.geometry.computeBoundingSphere();
      if (lines.geometry.boundingSphere) sphere.copy(lines.geometry.boundingSphere);
      const radius = Math.max(sphere.radius, 1);
      sceneRadius = radius;
      const fov = (camera.fov * Math.PI) / 180;
      const distance = (radius * 1.4) / Math.sin(fov / 2);
      const dir = new THREE.Vector3(0.6, -0.9, 0.9).normalize();
      controls.target.copy(sphere.center);
      camera.position.copy(sphere.center).addScaledVector(dir, distance);
      camera.near = Math.max(distance / 1000, 0.01);
      camera.far = distance * 10;
      camera.updateProjectionMatrix();
      controls.update();
    }
  }

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    pointerStart = { x: e.clientX, y: e.clientY, t: performance.now() };
  }

  function onPointerUp(e: PointerEvent) {
    if (!pointerStart) return;
    const dx = e.clientX - pointerStart.x;
    const dy = e.clientY - pointerStart.y;
    const dt = performance.now() - pointerStart.t;
    pointerStart = null;
    if (Math.hypot(dx, dy) > 3 || dt > 400) return;
    handlePick(e);
  }

  /// Single-click pick: cast a ray through the cursor against the merged
  /// LineSegments mesh and act on the closest hit. Imported geometry hits
  /// drive object selection (mirrors the 2D pane); toolpath hits scrub
  /// the playhead so the gcode panel scrolls + highlights the matching
  /// line.
  function handlePick(e: PointerEvent) {
    if (!camera || !renderer || !linesObject) return;
    const rect = renderer.domElement.getBoundingClientRect();
    ndc.x = ((e.clientX - rect.left) / rect.width) * 2 - 1;
    ndc.y = -((e.clientY - rect.top) / rect.height) * 2 + 1;
    raycaster.setFromCamera(ndc, camera);
    raycaster.params.Line = { threshold: Math.max(0.5, sceneRadius * 0.01) };
    const hits = raycaster.intersectObject(linesObject, false);
    if (hits.length === 0) {
      if (!e.shiftKey) project.clearSelection();
      return;
    }
    const hit = hits[0];
    if (hit.index == null) return;
    const owner = lineOwners[Math.floor(hit.index / 2)];
    if (!owner) return;
    if (owner.kind === 'object') {
      if (owner.objectId > 0) project.toggleObject(owner.objectId, e.shiftKey);
      else if (!e.shiftKey) project.clearSelection();
    } else {
      const total = project.generated?.toolpath.length ?? 0;
      if (total > 0) project.playhead = (owner.segIdx + 1) / total;
    }
  }

  function tessellate(seg: { start: { x: number; y: number }; end: { x: number; y: number }; bulge: number; type: string }): [number, number][] {
    if (seg.type === 'POINT') return [[seg.start.x, seg.start.y]];
    if (Math.abs(seg.bulge) < 1e-9) {
      return [
        [seg.start.x, seg.start.y],
        [seg.end.x, seg.end.y],
      ];
    }
    // Recompute arc center from start/end/bulge (canonical formula).
    const dx = seg.end.x - seg.start.x;
    const dy = seg.end.y - seg.start.y;
    const chord = Math.hypot(dx, dy);
    if (chord < 1e-9) return [[seg.start.x, seg.start.y]];
    const sagitta = (seg.bulge * chord) / 2;
    const r = (chord / 2) ** 2 / (2 * Math.abs(sagitta)) + Math.abs(sagitta) / 2;
    const mx = (seg.start.x + seg.end.x) / 2;
    const my = (seg.start.y + seg.end.y) / 2;
    const ux = -dy / chord;
    const uy = dx / chord;
    const h = r - Math.abs(sagitta);
    const sign = seg.bulge > 0 ? 1 : -1;
    const cx = mx + ux * h * sign;
    const cy = my + uy * h * sign;
    const a0 = Math.atan2(seg.start.y - cy, seg.start.x - cx);
    const a1 = Math.atan2(seg.end.y - cy, seg.end.x - cx);
    let sweep = a1 - a0;
    if (seg.bulge > 0 && sweep < 0) sweep += Math.PI * 2;
    if (seg.bulge < 0 && sweep > 0) sweep -= Math.PI * 2;
    const steps = Math.max(8, Math.ceil(Math.abs(sweep) / (Math.PI / 18))); // ≤10° per step
    const pts: [number, number][] = [];
    for (let i = 0; i <= steps; i++) {
      const t = a0 + (sweep * i) / steps;
      pts.push([cx + r * Math.cos(t), cy + r * Math.sin(t)]);
    }
    return pts;
  }

  function aciColor(c: number): THREE.Color {
    const fixed: Record<number, number> = {
      1: 0xff0000,
      2: 0xffff00,
      3: 0x00ff00,
      4: 0x00ffff,
      5: 0x0000ff,
      6: 0xff00ff,
    };
    if (c === 7 || c === 256) return cssColor('--text-strong', 0xe6e6e6);
    if (c === 8) return cssColor('--text-muted', 0x888888);
    if (fixed[c] !== undefined) return new THREE.Color(fixed[c]);
    return cssColor('--text-faint', 0xbbbbbb);
  }
</script>

<div class="scene" bind:this={host}></div>

<style>
  .scene {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    background: var(--bg-app);
  }
</style>
