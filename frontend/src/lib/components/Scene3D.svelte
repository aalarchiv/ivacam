<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as THREE from 'three';
  import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
  import {
    project,
    playheadToSegment,
    simWarningSeverity,
    simWarningSegmentIdx,
  } from '../state/project.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { HeightfieldDriver, computeFootprint } from '../sim/driver';
  import {
    autoTabTs,
    buildObjectPolylines,
    polylineAtT,
    resolveTabPlacementToWorld,
  } from '../cam/tabs';
  import type { SimWarning } from '../api/types';

  let host: HTMLDivElement;
  let renderer: THREE.WebGLRenderer | undefined;
  let scene: THREE.Scene | undefined;
  let camera: THREE.PerspectiveCamera | undefined;
  let controls: OrbitControls | undefined;
  let geometryGroup: THREE.Group | undefined;
  let toolGroup: THREE.Group | undefined;
  let raf = 0;
  let observer: ResizeObserver | undefined;
  let intersectObserver: IntersectionObserver | undefined;
  let themeMql: MediaQueryList | undefined;
  let themeMo: MutationObserver | undefined;
  // Hoisted so onDestroy's removeEventListener can pass the SAME
  // function reference that onMount's addEventListener used — a
  // fresh closure would silently fail to detach (audit C12).
  let onThemeChange: (() => void) | undefined;
  /// RAF gating: stop the loop entirely when the page is hidden OR the
  /// host element is fully off-screen. Pane swaps unmount Scene3D
  /// already (so onDestroy stops RAF), but minimised windows / tabbed-
  /// away IDEs still left the loop running pre-9js.
  let pageVisible = true;
  let hostVisible = true;

  /// Render-on-demand flag. The animation loop calls `renderer.render()`
  /// only when something has visibly changed since the last frame —
  /// otherwise it just ticks `controls.update()` (cheap, needed for
  /// damping) and bails. Without this we burned 60 fps of GPU + CPU
  /// drawing the same static scene whenever the 3D pane was open.
  /// Anything that mutates the scene must call `requestRender()`.
  let needsRender = true;
  function requestRender() {
    needsRender = true;
  }

  /// Camera persistence. The OrbitControls 'change' event fires every
  /// damping tick (60 Hz during a drag, then ~30 frames after release as
  /// damping settles), so we coalesce writes to the workspace store with
  /// a 500 ms tail. Workspace state is global (not per-project) — if the
  /// user wants different angles per project, they can save it as part
  /// of a project file on top.
  let cameraSaveTimer: ReturnType<typeof setTimeout> | null = null;
  function onCameraChanged() {
    if (cameraSaveTimer) clearTimeout(cameraSaveTimer);
    cameraSaveTimer = setTimeout(() => {
      cameraSaveTimer = null;
      if (!camera || !controls) return;
      workspace.update({
        camera: {
          px: camera.position.x,
          py: camera.position.y,
          pz: camera.position.z,
          tx: controls.target.x,
          ty: controls.target.y,
          tz: controls.target.z,
        },
      });
    }, 500);
  }

  /// Apply the saved camera pose, if any. Run once after the initial
  /// fit-to-view so the saved view supersedes the auto-fit; subsequent
  /// 'change' events overwrite the saved value.
  let restoredFromWorkspace = false;
  function maybeRestoreSavedCamera() {
    if (restoredFromWorkspace) return;
    if (!camera || !controls) return;
    const saved = workspace.get().camera;
    if (!saved) return;
    camera.position.set(saved.px, saved.py, saved.pz);
    controls.target.set(saved.tx, saved.ty, saved.tz);
    controls.update();
    requestRender();
    restoredFromWorkspace = true;
  }

  function tickFrame() {
    // Damping needs controls.update() every frame to advance, but the
    // call itself is cheap (~50 µs) and dispatches 'change' (and thus
    // requestRender) when anything actually moved. The expensive call
    // is renderer.render — we gate it on needsRender.
    controls?.update();
    if (needsRender && renderer && scene && camera) {
      renderer.render(scene, camera);
      needsRender = false;
    }
    raf = requestAnimationFrame(tickFrame);
  }

  function maybeStartTick() {
    if (raf) return;
    if (!pageVisible || !hostVisible) return;
    raf = requestAnimationFrame(tickFrame);
  }

  function stopTick() {
    if (raf) {
      cancelAnimationFrame(raf);
      raf = 0;
    }
  }

  function onVisibilityChange() {
    const visible = document.visibilityState === 'visible';
    if (visible && !pageVisible) requestRender();
    pageVisible = visible;
    if (visible) maybeStartTick();
    else stopTick();
  }

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

  /// Per-object color ranges into linesObject.geometry.attributes.color.
  /// Each entry is `{ start, count, base: [r,g,b] }` — start is the
  /// vertex index (not floats) where this object's first vertex lives,
  /// count is how many vertices belong to it, base is the original
  /// (non-selected) color the object should revert to. Filled during
  /// rebuildGeometry so the selection-only $effect can mutate the
  /// color attribute in-place instead of rebuilding the whole mesh.
  type ColorRange = { start: number; count: number; base: [number, number, number] };
  let objectColorRanges = new Map<number, ColorRange[]>();
  /// Selection set the color attribute currently reflects. Compared
  /// against project.selectedObjects to compute the symmetric diff.
  let appliedSelection = new Set<number>();

  /// Per-toolpath-segment color ranges into linesObject's color
  /// attribute, baked at rebuild time. Each entry covers exactly two
  /// vertices (one line segment), records the segment's BASE color
  /// (un-faded), and lets the playhead $effect mutate the attribute in
  /// place on each tick instead of rebuilding the entire geometry —
  /// which previously also reset the camera, killing user pan/zoom
  /// during playback.
  type ToolpathColor = { start: number; base: [number, number, number] };
  let toolpathColors: ToolpathColor[] = [];
  /// Head index the toolpath fade currently reflects.
  let appliedHead = -1;
  /// Per-segment override colors driven by sim warnings. Consumed by
  /// applyToolpathFade so the affected segment is painted in the
  /// severity color regardless of past/future state.
  let warningSegmentColors = new Map<number, [number, number, number]>();

  /// Persistent tool-tip mesh + the spec it was built for. updateTool()
  /// fires every playhead change (60×/sec while playing), so creating a
  /// fresh BufferGeometry + Mesh per tick churned ~60 GPU uploads / GC
  /// cycles per second. We cache the mesh keyed by the tool/mode spec
  /// and only rebuild when the spec changes; per-tick updates are now
  /// position.set + material.color.setHex.
  let toolMesh: THREE.Mesh | undefined;
  let toolMeshKey = '';

  /// Heightfield-based cutting simulator. Lazy-loaded on first need
  /// (the WASM module is async). Owns its own group inside `scene`;
  /// shows / hides per project.settings.previewMode.
  let driver: HeightfieldDriver | undefined;
  let driverInitPromise: Promise<void> | undefined;
  /// Cache the inputs that trigger a sim rebuild (footprint or grid
  /// resolution change) so we don't tear it down for cosmetic changes.
  let lastSimKey = '';
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
    // OrbitControls dispatches 'change' whenever the camera moves —
    // user drag, zoom, pan, AND each damping tick after release. Hooking
    // it is enough to keep the scene rendering until damping settles.
    controls.addEventListener('change', requestRender);
    controls.addEventListener('change', onCameraChanged);

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

    intersectObserver = new IntersectionObserver((entries) => {
      const isect = entries[0]?.isIntersecting ?? true;
      if (isect && !hostVisible) requestRender();
      hostVisible = isect;
      maybeStartTick();
    });
    intersectObserver.observe(host);

    document.addEventListener('visibilitychange', onVisibilityChange);
    pageVisible = document.visibilityState === 'visible';
    maybeStartTick();

    // Re-skin background + grid + (re-trigger) toolpath colors when the
    // OS theme changes OR the user toggles a manual theme. The toolpath
    // group rebuilds via the $effect below since we touch project.imported
    // as a Svelte dep.
    themeMql = window.matchMedia('(prefers-color-scheme: light)');
    onThemeChange = () => applyTheme();
    themeMql.addEventListener('change', onThemeChange);
    // MutationObserver fires on every attribute *write*, even when the
    // value didn't change — track the last seen value so we only do the
    // work when the theme actually flipped. applyTheme rebuilds the grid
    // and re-runs rebuildGeometry, which is non-trivial on big imports.
    let lastTheme = document.documentElement.dataset.theme ?? '';
    themeMo = new MutationObserver(() => {
      const cur = document.documentElement.dataset.theme ?? '';
      if (cur === lastTheme) return;
      lastTheme = cur;
      applyTheme();
    });
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
    stopTick();
    observer?.disconnect();
    intersectObserver?.disconnect();
    document.removeEventListener('visibilitychange', onVisibilityChange);
    if (renderer) {
      renderer.domElement.removeEventListener('pointerdown', onPointerDown);
      renderer.domElement.removeEventListener('pointerup', onPointerUp);
    }
    controls?.removeEventListener('change', requestRender);
    controls?.removeEventListener('change', onCameraChanged);
    if (cameraSaveTimer) {
      clearTimeout(cameraSaveTimer);
      cameraSaveTimer = null;
    }
    controls?.dispose();
    if (toolMesh) {
      disposeMesh(toolMesh);
      toolMesh = undefined;
    }
    disposeStockGroup();
    driver?.destroy();
    driver = undefined;
    renderer?.dispose();
    if (themeMql && onThemeChange) {
      themeMql.removeEventListener('change', onThemeChange);
    }
    onThemeChange = undefined;
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
    requestRender();
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
    void project.stock;
    void project.operations;
    void project.fixtures;
    void project.selectedFixtureId;
    void project.settings.showStockBox;
    rebuildGeometry();
    updateTabs();
    updateStock();
    updateFixtures();
    requestRender();
  });

  /// Fit-to-view fires only when a *new* geometry source appears (the
  /// reference identity of project.imported changes). Previously this
  /// ran inside rebuildGeometry, which made every layer toggle / op
  /// edit / Generate snap the camera back to the default angle.
  $effect(() => {
    void project.imported;
    fitCameraToScene();
  });

  /// Selection-only fast path: mutate the color attribute in place
  /// instead of rebuilding the entire LineSegments mesh on every click.
  /// Falls through to a full rebuild only if the geometry is missing
  /// (e.g. before the first rebuild has run).
  $effect(() => {
    const sel = project.selectedObjects;
    if (!linesObject) {
      // Geometry hasn't been built yet; the next rebuildGeometry will
      // pick up the current selection naturally.
      return;
    }
    applySelectionDelta(sel);
    requestRender();
  });

  $effect(() => {
    void project.playhead;
    void project.generated;
    void project.tools;
    void project.machine;
    void project.selectedOpId;
    updateTool();
    applyToolpathFade();
    requestRender();
  });

  /// Build (or rebuild) the heightfield simulator + mesh whenever the
  /// stock footprint, grid resolution, or active generated toolpath
  /// changes. Cosmetic settings (color / opacity) are NOT in this key —
  /// those flow through applyStyle without rebuilding.
  $effect(() => {
    if (!scene) return;
    const settings = project.settings;
    // Wire-mesh visibility tracks the preview mode: wireframe / both
    // show the toolpath + imported lines; solid hides them in favor of
    // the heightfield carved-stock mesh.
    const wireVisible = settings.previewMode !== 'solid';
    if (linesObject) linesObject.visible = wireVisible;
    if (settings.previewMode === 'wireframe') {
      driver?.setVisible(false);
      requestRender();
      return;
    }
    const imported = project.imported;
    const generated = project.generated;
    const firstOp = project.operations[0];
    const tool =
      project.tools.find((t) => t.id === (firstOp?.toolId ?? 0)) ?? project.tools[0];
    if (!imported || !generated || !tool) {
      driver?.setVisible(false);
      requestRender();
      return;
    }
    const cellRes = settings.cellResolutionMode === 'manual' ? settings.cellResolutionMm : -1;
    const key = JSON.stringify({
      bbox: imported.bbox,
      stock: project.stock,
      tool_id: tool.id,
      tool_dia: tool.diameter,
      cellRes,
      maxCells: settings.maxSimulationCells,
      // Monotonic version counter bumped on every setGenerated — two
      // runs whose gcode happens to have the same length but different
      // content now correctly invalidate the sim cache (was: gcode.length
      // as a cheap-but-unreliable stand-in).
      gen_id: project.generatedVersion,
      fixtures: project.fixtures,
    });
    if (key !== lastSimKey) {
      ensureDriver()
        .then(() => {
          if (!driver) return;
          driver.build({
            imported,
            generated,
            tool,
            stock: project.stock,
            settings,
            fixtures: project.fixtures,
          });
          driver.setVisible(true);
          driver.setSolidVisible(settings.previewMode === 'solid' || settings.previewMode === 'both');
          driver.setEdgesVisible(settings.previewMode === 'solid' || settings.previewMode === 'both');
          lastSimKey = key;
          // Replay 0..head so we see the carved state immediately (not
          // an unmilled stock we have to scrub forward through).
          driver.advanceTo(
            project.playhead,
            generated.toolpath,
            tool,
            project.toolpathCumLen,
            project.toolpathTotalLen,
          );
          requestRender();
        })
        .catch((e) => {
          // Surface async failures the user couldn't otherwise see.
          // Driver init failures (wasm load) and build failures
          // (Simulator construction throwing) used to swallow silently.
          project.setError(`solid preview: ${e instanceof Error ? e.message : String(e)}`);
        });
    } else {
      driver?.setVisible(true);
      driver?.setSolidVisible(settings.previewMode === 'solid' || settings.previewMode === 'both');
      driver?.setEdgesVisible(settings.previewMode === 'solid' || settings.previewMode === 'both');
      requestRender();
    }
  });

  /// Advance the simulation on every playhead change. Falls through
  /// silently if the driver isn't built yet (preview mode = wireframe
  /// or no generated yet).
  $effect(() => {
    void project.playhead;
    if (!driver) return;
    const generated = project.generated;
    const firstOp = project.operations[0];
    const tool =
      project.tools.find((t) => t.id === (firstOp?.toolId ?? 0)) ?? project.tools[0];
    if (!generated || !tool) return;
    driver.advanceTo(
      project.playhead,
      generated.toolpath,
      tool,
      project.toolpathCumLen,
      project.toolpathTotalLen,
    );
  });

  /// Live-apply cosmetic settings (color / opacity).
  $effect(() => {
    void project.settings.solidColor;
    void project.settings.solidOpacity;
    void project.settings.edgeColor;
    void project.settings.edgeOpacity;
    driver?.applyStyle({
      solidColor: project.settings.solidColor,
      solidOpacity: project.settings.solidOpacity,
      edgeColor: project.settings.edgeColor,
      edgeOpacity: project.settings.edgeOpacity,
    });
  });

  async function ensureDriver(): Promise<void> {
    if (driver) return;
    if (!driverInitPromise) {
      driverInitPromise = (async () => {
        if (!scene) return;
        const d = new HeightfieldDriver({ scene, requestRender });
        await d.init();
        d.onDiagnostics((diag) => project.setSimDiagnostics(diag));
        driver = d;
      })();
    }
    return driverInitPromise;
  }

  /// Mutate the color attribute in place to reflect the current
  /// playhead — segments before the head get their base color, segments
  /// after get faded. Walks only the slice between appliedHead and the
  /// new head so a 60fps playback is O(playhead delta) per tick, not
  /// O(toolpath length). Replaces the per-frame full rebuild that
  /// previously also reset the camera and broke pan/zoom during play.
  function rebuildWarningSegmentColors() {
    warningSegmentColors = new Map();
    const warnings = project.simDiagnostics?.warnings ?? [];
    for (const w of warnings) {
      const idx = simWarningSegmentIdx(w);
      const sev = simWarningSeverity(w);
      // Critical wins over warning if both fired on the same segment.
      const existing = warningSegmentColors.get(idx);
      if (existing && sev !== 'critical') continue;
      const tint: [number, number, number] =
        sev === 'critical' ? [0.9, 0.28, 0.28] : [0.94, 0.75, 0.13];
      warningSegmentColors.set(idx, tint);
      if (w.kind === 'dragging_rapids') {
        for (let i = 1; i < w.count; i++) {
          if (!warningSegmentColors.has(idx + i)) {
            warningSegmentColors.set(idx + i, tint);
          }
        }
      }
    }
  }

  $effect(() => {
    void project.simDiagnostics;
    rebuildWarningSegmentColors();
    appliedHead = -1;
    applyToolpathFade();
    requestRender();
  });

  function applyToolpathFade() {
    if (!linesObject || toolpathColors.length === 0) return;
    const total = toolpathColors.length;
    // Arc-length mapping: head = segIdx + 1 so the segment currently
    // under the cutter (segIdx) is rendered as "past" (fully colored)
    // and everything strictly after is faded — matches the count-based
    // behavior at segment boundaries while keeping playback uniform
    // across short connectors and long edges.
    const { segIdx } = playheadToSegment(
      project.playhead,
      project.toolpathCumLen,
      project.toolpathTotalLen,
    );
    const head = segIdx < 0
      ? Math.max(0, Math.min(total, Math.round(project.playhead * total)))
      : Math.max(0, Math.min(total, segIdx + 1));
    if (head === appliedHead) return;
    const attr = linesObject.geometry.attributes.color as THREE.BufferAttribute;
    const arr = attr.array as Float32Array;
    const f = 0.25; // fade factor for future moves
    const fade_offset = 0.05;
    const lo = appliedHead < 0 ? 0 : Math.min(appliedHead, head);
    const hi = appliedHead < 0 ? total : Math.max(appliedHead, head);
    for (let i = lo; i < hi; i++) {
      const tc = toolpathColors[i];
      const past = i < head;
      const tint = warningSegmentColors.get(i);
      let r: number;
      let g: number;
      let b: number;
      if (tint) {
        r = past ? tint[0] : tint[0] * f + fade_offset;
        g = past ? tint[1] : tint[1] * f + fade_offset;
        b = past ? tint[2] : tint[2] * f + fade_offset;
      } else {
        r = past ? tc.base[0] : tc.base[0] * f + fade_offset;
        g = past ? tc.base[1] : tc.base[1] * f + fade_offset;
        b = past ? tc.base[2] : tc.base[2] * f + fade_offset;
      }
      const off = tc.start * 3;
      arr[off] = r;
      arr[off + 1] = g;
      arr[off + 2] = b;
      arr[off + 3] = r;
      arr[off + 4] = g;
      arr[off + 5] = b;
    }
    attr.needsUpdate = true;
    appliedHead = head;
  }

  function applySelectionDelta(next: Set<number>) {
    if (!linesObject) return;
    const attr = linesObject.geometry.attributes.color as THREE.BufferAttribute;
    const arr = attr.array as Float32Array;
    const selectedColor = cssColor('--accent', 0x4a8df0);
    let touched = false;
    // Newly-selected objects: paint accent over their ranges.
    for (const id of next) {
      if (appliedSelection.has(id)) continue;
      const ranges = objectColorRanges.get(id);
      if (!ranges) continue;
      for (const r of ranges) {
        for (let v = 0; v < r.count; v++) {
          const off = (r.start + v) * 3;
          arr[off] = selectedColor.r;
          arr[off + 1] = selectedColor.g;
          arr[off + 2] = selectedColor.b;
        }
      }
      touched = true;
    }
    // Newly-deselected objects: restore base color from the recorded
    // ranges so the wireframe goes back to ACI / faded.
    for (const id of appliedSelection) {
      if (next.has(id)) continue;
      const ranges = objectColorRanges.get(id);
      if (!ranges) continue;
      for (const r of ranges) {
        const [br, bg, bb] = r.base;
        for (let v = 0; v < r.count; v++) {
          const off = (r.start + v) * 3;
          arr[off] = br;
          arr[off + 1] = bg;
          arr[off + 2] = bb;
        }
      }
      touched = true;
    }
    if (touched) attr.needsUpdate = true;
    appliedSelection = new Set(next);
  }

  let tabsGroup: THREE.Group | undefined;
  let stockGroup: THREE.Group | undefined;
  let fixturesGroup: THREE.Group | undefined;
  /// Sim-warning markers (one mesh per critical / holder warning). Lazy
  /// rebuilt whenever project.simDiagnostics changes.
  let warningGroup: THREE.Group | undefined;
  /// Per-fixture-id → list of THREE.Material whose color we flip when
  /// the playhead crosses a `fixture_collision` warning's segment.
  let fixtureMaterials = new Map<number, THREE.MeshBasicMaterial[]>();
  /// Recorded base colors so we can restore on un-flash.
  let fixtureBaseColors = new Map<number, number>();
  /// Fixture ids currently flashing red (set by the playhead $effect).
  let flashingFixtures = new Set<number>();

  function warningMarkerColor(w: SimWarning): number {
    return simWarningSeverity(w) === 'critical' ? 0xe54848 : 0xf0c020;
  }

  function warningPosition(w: SimWarning): { x: number; y: number; z: number } | null {
    if (w.kind === 'rapid_through_material') {
      return { x: w.worst_x, y: w.worst_y, z: w.worst_cell_z };
    }
    if (w.kind === 'fixture_collision') {
      return { x: w.nearest_x, y: w.nearest_y, z: 0 };
    }
    if (w.kind === 'holder_collision') {
      return { x: w.worst_x, y: w.worst_y, z: w.wall_z };
    }
    // Engagement / dragging are span-shaped, not point-shaped — fall
    // back to the toolpath segment endpoint so the marker still appears.
    const segIdx = simWarningSegmentIdx(w);
    const tp = project.generated?.toolpath;
    const seg = tp ? tp[segIdx] : undefined;
    if (!seg) return null;
    return { x: seg.from.x, y: seg.from.y, z: seg.from.z };
  }

  function rebuildWarningMarkers() {
    if (!scene) return;
    if (!warningGroup) {
      warningGroup = new THREE.Group();
      scene.add(warningGroup);
    }
    while (warningGroup.children.length > 0) {
      const child = warningGroup.children[0];
      warningGroup.remove(child);
      if (child instanceof THREE.Mesh) {
        child.geometry.dispose();
        const m = child.material as THREE.Material | THREE.Material[];
        if (Array.isArray(m)) m.forEach((mm) => mm.dispose());
        else m.dispose();
      }
    }
    const warnings = project.simDiagnostics?.warnings ?? [];
    if (warnings.length === 0) return;
    const radius = Math.max(0.5, sceneRadius * 0.012);
    const geom = new THREE.TetrahedronGeometry(radius, 0);
    for (const w of warnings) {
      const pos = warningPosition(w);
      if (!pos) continue;
      const mat = new THREE.MeshBasicMaterial({
        color: warningMarkerColor(w),
        transparent: true,
        opacity: 0.9,
      });
      const mesh = new THREE.Mesh(geom, mat);
      mesh.position.set(pos.x, pos.y, pos.z + radius);
      warningGroup.add(mesh);
    }
  }

  $effect(() => {
    void project.simDiagnostics;
    rebuildWarningMarkers();
    requestRender();
  });

  /// Flash any fixture whose collision warning's segment is within +-2
  /// segments of the current playhead position. Re-applies the in-place
  /// color override on every playhead tick — cheap (one .color.set per
  /// fixture).
  $effect(() => {
    void project.playhead;
    void project.simDiagnostics;
    void project.fixtures;
    const warnings = project.simDiagnostics?.warnings ?? [];
    const collisions = warnings.filter((w) => w.kind === 'fixture_collision');
    if (collisions.length === 0) {
      if (flashingFixtures.size > 0) {
        flashingFixtures = new Set();
        applyFixtureFlash();
        requestRender();
      }
      return;
    }
    const { segIdx } = playheadToSegment(
      project.playhead,
      project.toolpathCumLen,
      project.toolpathTotalLen,
    );
    const next = new Set<number>();
    const window = 2;
    for (const w of collisions) {
      if (w.kind !== 'fixture_collision') continue;
      if (Math.abs(w.segment_idx - segIdx) <= window) {
        next.add(w.fixture_id);
      }
    }
    let changed = next.size !== flashingFixtures.size;
    if (!changed) {
      for (const id of next) if (!flashingFixtures.has(id)) { changed = true; break; }
    }
    if (changed) {
      flashingFixtures = next;
      applyFixtureFlash();
      requestRender();
    }
  });

  /// Translucent stock box + its wireframe. Always visible (not only in
  /// sim mode) whenever an import is loaded and both `stock.visible` and
  /// `settings.showStockBox` are on. The XY footprint comes from the
  /// shared `computeFootprint` (auto = bbox + margin; manual = customX/Y
  /// centered on the bbox); Z extents are `-stock.thickness..0`.
  function updateStock() {
    if (!scene) return;
    if (!stockGroup) {
      stockGroup = new THREE.Group();
      scene.add(stockGroup);
    }
    disposeStockGroup();
    const cfg = project.stock;
    if (!cfg.visible || !project.settings.showStockBox) return;
    const data = project.imported;
    if (!data) return;

    const fp = computeFootprint(data, cfg);
    const sizeX = fp.maxX - fp.minX;
    const sizeY = fp.maxY - fp.minY;
    const thickness = Math.max(0.1, cfg.thickness);
    if (sizeX <= 0.1 || sizeY <= 0.1) return;

    const cx = (fp.minX + fp.maxX) * 0.5;
    const cy = (fp.minY + fp.maxY) * 0.5;
    const cz = -thickness * 0.5;
    const box = new THREE.BoxGeometry(sizeX, sizeY, thickness);
    const fillMat = new THREE.MeshBasicMaterial({
      transparent: true,
      opacity: 0.05,
      color: 0xcccccc,
      side: THREE.DoubleSide,
      depthWrite: false,
    });
    const fill = new THREE.Mesh(box, fillMat);
    fill.position.set(cx, cy, cz);
    stockGroup.add(fill);

    const edges = new THREE.EdgesGeometry(box);
    const lineMat = new THREE.LineBasicMaterial({
      color: cssColor('--stock-edge', 0x888888),
      transparent: true,
      opacity: 0.4,
    });
    const wire = new THREE.LineSegments(edges, lineMat);
    wire.position.set(cx, cy, cz);
    stockGroup.add(wire);
  }

  /// Dispose all geometry + materials inside stockGroup before clearing.
  /// THREE.Group.clear() only removes the children; without explicit
  /// disposal the GPU buffers leak on every stock-config tweak.
  function disposeStockGroup() {
    if (!stockGroup) return;
    while (stockGroup.children.length > 0) {
      const child = stockGroup.children[0];
      stockGroup.remove(child);
      if (child instanceof THREE.Mesh || child instanceof THREE.LineSegments) {
        child.geometry.dispose();
        const m = (child as THREE.Mesh | THREE.LineSegments).material as
          | THREE.Material
          | THREE.Material[];
        if (Array.isArray(m)) m.forEach((mm) => mm.dispose());
        else m.dispose();
      }
    }
  }

  /// Build/refresh the 3D fixture group. Each fixture extrudes between
  /// `z_bottom..z_top` in its declared color; selected fixtures get an
  /// accented outline. Lazily-rebuilt on every fixture-set change.
  function updateFixtures() {
    if (!scene) return;
    if (!fixturesGroup) {
      fixturesGroup = new THREE.Group();
      scene.add(fixturesGroup);
    }
    while (fixturesGroup.children.length > 0) {
      const child = fixturesGroup.children[0];
      fixturesGroup.remove(child);
      if (child instanceof THREE.Mesh || child instanceof THREE.LineSegments) {
        child.geometry.dispose();
        const m = (child as THREE.Mesh | THREE.LineSegments).material as
          | THREE.Material
          | THREE.Material[];
        if (Array.isArray(m)) m.forEach((mm) => mm.dispose());
        else m.dispose();
      }
    }
    fixtureMaterials = new Map();
    fixtureBaseColors = new Map();
    const accent = cssColor('--accent', 0x4a8df0);
    for (const f of project.fixtures) {
      const colorPacked = f.color ?? 0xffa050c0;
      // Packed RGBA → hex 0xRRGGBB + alpha [0,1]. Default alpha ~0.5
      // when the wire color omits it.
      const r = (colorPacked >>> 24) & 0xff;
      const g = (colorPacked >>> 16) & 0xff;
      const b = (colorPacked >>> 8) & 0xff;
      const a = colorPacked & 0xff;
      const hex = (r << 16) | (g << 8) | b;
      const opacity = Math.max(0.2, Math.min(1.0, a > 0 ? a / 255 : 0.5));
      fixtureBaseColors.set(f.id, hex);

      const mat = new THREE.MeshBasicMaterial({
        color: hex,
        transparent: true,
        opacity,
        depthWrite: false,
        side: THREE.DoubleSide,
      });
      const matsForFix: THREE.MeshBasicMaterial[] = [mat];
      const sizeZ = Math.max(0.05, f.z_top - f.z_bottom);
      const cz = (f.z_top + f.z_bottom) * 0.5;

      let geom: THREE.BufferGeometry | undefined;
      if (f.kind.shape === 'box') {
        geom = new THREE.BoxGeometry(Math.max(0.01, f.kind.width), Math.max(0.01, f.kind.depth), sizeZ);
      } else if (f.kind.shape === 'cylinder') {
        geom = new THREE.CylinderGeometry(
          Math.max(0.01, f.kind.radius),
          Math.max(0.01, f.kind.radius),
          sizeZ,
          24,
        );
        // CylinderGeometry's axis is +Y; rotate so it stands on +Z.
        geom.rotateX(Math.PI / 2);
      } else if (f.kind.shape === 'polygon') {
        const shape = new THREE.Shape(
          f.kind.vertices.map(([x, y]) => new THREE.Vector2(x, y)),
        );
        geom = new THREE.ExtrudeGeometry(shape, { depth: sizeZ, bevelEnabled: false });
      }
      if (!geom) continue;
      const mesh = new THREE.Mesh(geom, mat);
      if (f.kind.shape === 'polygon') {
        // ExtrudeGeometry extrudes along +Z from the shape plane (Z=0).
        // Translate so the extrusion sits in [z_bottom, z_top].
        mesh.position.set(f.origin[0], f.origin[1], f.z_bottom);
      } else {
        mesh.position.set(f.origin[0], f.origin[1], cz);
      }
      fixturesGroup.add(mesh);

      const isSelected = project.selectedFixtureId === f.id;
      const edgeColor = isSelected ? accent : new THREE.Color(hex);
      const edgesGeom = new THREE.EdgesGeometry(geom);
      const edgeMat = new THREE.LineBasicMaterial({
        color: edgeColor,
        transparent: true,
        opacity: isSelected ? 0.95 : 0.7,
      });
      const wire = new THREE.LineSegments(edgesGeom, edgeMat);
      wire.position.copy(mesh.position);
      fixturesGroup.add(wire);
      fixtureMaterials.set(f.id, matsForFix);
    }
    applyFixtureFlash();
  }

  /// Re-apply the flashingFixtures color override. Called whenever the
  /// flashing set changes (playhead crosses a fixture_collision segment).
  function applyFixtureFlash() {
    for (const [id, mats] of fixtureMaterials) {
      const flash = flashingFixtures.has(id);
      const base = fixtureBaseColors.get(id) ?? 0xffa050c0;
      for (const m of mats) {
        m.color.set(flash ? 0xe54848 : base);
      }
    }
  }

  function updateTabs() {
    if (!scene) return;
    if (!tabsGroup) {
      tabsGroup = new THREE.Group();
      scene.add(tabsGroup);
    }
    tabsGroup.clear();
    const imp = project.imported;
    if (!imp) return;
    const color = cssColor('--tab-marker', 0xffd23a);
    const radius = Math.max(
      0.5,
      ((imp.bbox.max_x - imp.bbox.min_x) || 100) * 0.008,
    );
    const geom = new THREE.SphereGeometry(radius, 12, 8);
    const mat = new THREE.MeshBasicMaterial({ color });
    // rt1.10 + hr5: tabs are per-op. Manual placements get resolved
    // directly via (objectId, t); Auto / Mixed modes additionally
    // walk every object the op covers and emit auto-spaced t values
    // there. Same arc-length math as the 2D canvas + backend.
    const objects = buildObjectPolylines(imp);
    for (const op of project.operations) {
      const mode = op.tabMode;
      if (!mode || mode.kind === 'off') continue;
      // Manual placements (Manual + Mixed).
      if (mode.kind === 'manual' || mode.kind === 'mixed') {
        for (const tp of op.tabPlacements ?? []) {
          const pt = resolveTabPlacementToWorld(imp, tp);
          if (!pt) continue;
          const sphere = new THREE.Mesh(geom, mat);
          sphere.position.set(pt[0], pt[1], 0);
          tabsGroup.add(sphere);
        }
      }
      // Auto-spaced placements (Auto + Mixed).
      if (mode.kind === 'auto' || mode.kind === 'mixed') {
        const count = mode.kind === 'auto' ? mode.count : mode.auto_count;
        if (count <= 0) continue;
        for (const obj of objects) {
          if (!opIncludesObject(op, obj.objectId, imp)) continue;
          const ts = autoTabTs(count, obj.closed);
          for (const t of ts) {
            const { point } = polylineAtT(obj.pts, t, obj.closed);
            const sphere = new THREE.Mesh(geom, mat);
            sphere.position.set(point.x, point.y, 0);
            tabsGroup.add(sphere);
          }
        }
      }
    }
  }

  /// Mirror of `wiac_core::pipeline::op_includes_object`. Tells us
  /// whether an op's source filter (All / Layers / Objects) selects
  /// the given 1-based object id, so the 3D scene's auto-tab walk
  /// honors the same rules as the backend.
  function opIncludesObject(
    op: { sourceLayers: string[] | null; sourceObjects?: number[] },
    objectId: number,
    imp: import('../api/types').ImportResponse,
  ): boolean {
    if (op.sourceObjects && op.sourceObjects.length > 0) {
      return op.sourceObjects.includes(objectId);
    }
    if (op.sourceLayers && op.sourceLayers.length > 0) {
      // Look up this object's layer via the first segment that maps
      // to it (objects[] is per-segment; layers come from segments[]).
      for (let i = 0; i < (imp.objects?.length ?? 0); i++) {
        if (imp.objects?.[i] === objectId) {
          const layer = imp.segments[i]?.layer ?? '';
          return op.sourceLayers.includes(layer);
        }
      }
      return false;
    }
    return true;
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
    const gen = project.generated;
    if (!gen || gen.toolpath.length === 0) {
      // No toolpath → drop the cached mesh so a future regenerate starts
      // clean instead of orbiting the previous program's tip.
      if (toolMesh) {
        toolGroup.remove(toolMesh);
        disposeMesh(toolMesh);
        toolMesh = undefined;
        toolMeshKey = '';
      }
      return;
    }
    const total = gen.toolpath.length;
    const mapped = playheadToSegment(
      project.playhead,
      project.toolpathCumLen,
      project.toolpathTotalLen,
    );
    // Fall back to the count-based mapping only if the cum-length table
    // hasn't been built yet (race between setGenerated and the first
    // updateTool tick).
    const headIdx = mapped.segIdx >= 0
      ? Math.max(0, Math.min(total - 1, mapped.segIdx))
      : Math.max(0, Math.min(total - 1, Math.round(project.playhead * total) - 1));
    const seg = gen.toolpath[headIdx];
    if (!seg) return;
    const t = mapped.segIdx >= 0
      ? Math.max(0, Math.min(1, mapped.segT))
      : Math.max(0, Math.min(1, project.playhead * total - headIdx));
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
    const mode = project.machine.mode;
    const dragoff = tool?.dragoff;
    const tipDiameter = tool?.tipDiameter;
    const kind = tool?.kind ?? 'endmill';
    const fluteLen = tool?.fluteLengthMm;
    const shankDia = tool?.shankDiameterMm;
    const holder = tool?.holder;

    // Cache key — anything that changes the geometry shape. Color is NOT
    // part of the key; we only mutate material.color on the cached mesh
    // for that. Holder fields are JSON-stringified so the key updates
    // whenever any part of the holder spec changes.
    const key = `${kind}|${mode}|${diameter}|${tipDiameter ?? ''}|${dragoff ?? ''}|${fluteLen ?? ''}|${shankDia ?? ''}|${holder ? JSON.stringify(holder) : ''}`;
    if (key !== toolMeshKey || !toolMesh) {
      if (toolMesh) {
        toolGroup.remove(toolMesh);
        disposeMesh(toolMesh);
      }
      toolMesh = buildToolMesh(kind, mode, diameter, tipDiameter, dragoff, colorHex, fluteLen, shankDia, holder);
      toolGroup.add(toolMesh);
      toolMeshKey = key;
    } else {
      // Cached mesh — just retint the material to match the active move.
      const m = toolMesh.material as THREE.MeshBasicMaterial;
      if (m.color.getHex() !== colorHex) m.color.setHex(colorHex);
    }
    toolMesh.position.set(px, py, pz);
  }

  function disposeMesh(mesh: THREE.Mesh) {
    mesh.geometry.dispose();
    const m = mesh.material as THREE.Material | THREE.Material[];
    if (Array.isArray(m)) m.forEach((mm) => mm.dispose());
    else m.dispose();
  }

  /// Build the tool-tip mesh for the given spec. Each piece is built
  /// with its axis along +Z (Z-up world) and the cutting tip at z=0 so
  /// mesh.position.set(px, py, pz) lands the tip exactly on the
  /// toolpath point.
  function buildToolMesh(
    kind: string,
    mode: string,
    diameter: number,
    tipDiameter: number | undefined,
    dragoff: number | undefined,
    colorHex: number,
    fluteLen?: number,
    shankDia?: number,
    holder?: import('../state/project.svelte').HolderShape,
  ): THREE.Mesh {
    const radius = diameter * 0.5;
    const mat = new THREE.MeshBasicMaterial({
      color: colorHex,
      transparent: true,
      opacity: 0.85,
    });
    if (mode === 'drag' || kind === 'drag_knife') {
      const off = dragoff ?? 0;
      const bladeLen = Math.max(diameter * 4, 4);
      const bladeT = Math.max(0.4, diameter * 0.4);
      const geom = new THREE.BoxGeometry(bladeT, off > 0 ? off * 2 : bladeT, bladeLen);
      geom.translate(0, 0, bladeLen / 2);
      return new THREE.Mesh(geom, mat);
    }
    if (mode === 'laser' || kind === 'laser_beam') {
      const beamLen = Math.max(8, diameter * 6);
      const geom = new THREE.CylinderGeometry(0.3, 0.3, beamLen, 12);
      geom.rotateX(Math.PI / 2); // CylinderGeometry's axis is +Y → put on +Z
      geom.translate(0, 0, beamLen / 2);
      return new THREE.Mesh(geom, mat);
    }
    if (kind === 'v_bit' || kind === 'engraver' || kind === 'drill') {
      // Tapered cutter: cone with apex at the cutting tip.
      const len = Math.max(diameter * 4, 8);
      const tipR = (tipDiameter ?? 0) * 0.5;
      const geom = new THREE.CylinderGeometry(radius, Math.max(tipR, 0.05), len, 24);
      // CylinderGeometry's axis is +Y; the *first* radius is the +Y end and
      // the second is the -Y end. After rotateX(π/2), +Y → -Z so the small
      // (tip) end lands at -Z. Then translate so the tip sits at z=0.
      geom.rotateX(Math.PI / 2);
      geom.translate(0, 0, len / 2);
      return new THREE.Mesh(geom, mat);
    }
    if (kind === 'ball_nose') {
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
      return new THREE.Mesh(merged, mat);
    }
    // Endmill / generic: stack flutes + (optional) shank + (optional) holder.
    return buildEndmillStack(diameter, mat, fluteLen, shankDia, holder);
  }

  /// Build a stacked tool envelope for endmill-like cutters. When the
  /// tool entry has flute length / shank / holder fields set, render
  /// each region as a distinct cylinder/cone segment so the user sees
  /// the same envelope the holder-collision sweep uses. When everything
  /// is default, falls back to the legacy single-cylinder body.
  function buildEndmillStack(
    diameter: number,
    mat: THREE.MeshBasicMaterial,
    fluteLen?: number,
    shankDia?: number,
    holder?: import('../state/project.svelte').HolderShape,
  ): THREE.Mesh {
    const radius = diameter * 0.5;
    if (fluteLen === undefined && shankDia === undefined && !holder) {
      const bodyLen = Math.max(diameter * 6, 8);
      const body = new THREE.CylinderGeometry(radius, radius, bodyLen, 24);
      body.rotateX(Math.PI / 2);
      body.translate(0, 0, bodyLen / 2);
      return new THREE.Mesh(body, mat);
    }
    const pieces: THREE.BufferGeometry[] = [];
    const shankR = ((shankDia ?? diameter) * 0.5);
    let zCursor = 0;
    const fLen = Math.max(0, fluteLen ?? Math.max(diameter * 4, 6));
    if (fLen > 0) {
      const flutes = new THREE.CylinderGeometry(radius, radius, fLen, 24);
      flutes.rotateX(Math.PI / 2);
      flutes.translate(0, 0, zCursor + fLen / 2);
      pieces.push(flutes);
      zCursor += fLen;
    }
    // Shank: from top of flutes up to the bottom of the holder. When the
    // holder is undefined, give the shank a sensible default length so
    // the user can still see "this is the non-cutting part of the tool"
    // sticking out.
    const shankLen = holder ? Math.max(diameter * 2, 4) : Math.max(diameter * 4, 6);
    if (shankR > 0 && shankLen > 0) {
      const shank = new THREE.CylinderGeometry(shankR, shankR, shankLen, 18);
      shank.rotateX(Math.PI / 2);
      shank.translate(0, 0, zCursor + shankLen / 2);
      pieces.push(shank);
      zCursor += shankLen;
    }
    if (holder) {
      if (holder.kind === 'cylinder') {
        const r = holder.diameter_mm * 0.5;
        const len = holder.length_mm;
        if (r > 0 && len > 0) {
          const g = new THREE.CylinderGeometry(r, r, len, 18);
          g.rotateX(Math.PI / 2);
          g.translate(0, 0, zCursor + len / 2);
          pieces.push(g);
        }
      } else if (holder.kind === 'cone') {
        const rb = holder.bottom_diameter_mm * 0.5;
        const rt = holder.top_diameter_mm * 0.5;
        const len = holder.length_mm;
        if (Math.max(rb, rt) > 0 && len > 0) {
          // CylinderGeometry: first arg is +Y (upper) radius, second arg is
          // -Y (lower) radius. After rotateX(π/2): +Y → -Z, -Y → +Z. So we
          // pass (top, bottom) swapped to keep the bottom at z = zCursor.
          const g = new THREE.CylinderGeometry(rt, rb, len, 18);
          g.rotateX(Math.PI / 2);
          g.translate(0, 0, zCursor + len / 2);
          pieces.push(g);
        }
      } else if (holder.kind === 'stepped') {
        const cylR = holder.cylinder_diameter_mm * 0.5;
        const cylLen = holder.cylinder_length_mm;
        const coneTopR = holder.cone_top_diameter_mm * 0.5;
        const coneLen = holder.cone_length_mm;
        if (cylR > 0 && cylLen > 0) {
          const g = new THREE.CylinderGeometry(cylR, cylR, cylLen, 18);
          g.rotateX(Math.PI / 2);
          g.translate(0, 0, zCursor + cylLen / 2);
          pieces.push(g);
          zCursor += cylLen;
        }
        if (Math.max(cylR, coneTopR) > 0 && coneLen > 0) {
          const g = new THREE.CylinderGeometry(coneTopR, cylR, coneLen, 18);
          g.rotateX(Math.PI / 2);
          g.translate(0, 0, zCursor + coneLen / 2);
          pieces.push(g);
        }
      }
    }
    const merged = pieces.length === 1 ? pieces[0] : mergeBufferGeometries(pieces);
    return new THREE.Mesh(merged, mat);
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
    objectColorRanges = new Map();
    toolpathColors = [];
    appliedHead = -1;
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
        // Base color (non-selected) so selection toggles can revert without
        // recomputing aciColor / fadedColor / etc.
        let baseR: number;
        let baseG: number;
        let baseB: number;
        if (flat) {
          baseR = fadedColor.r;
          baseG = fadedColor.g;
          baseB = fadedColor.b;
        } else {
          c.copy(aciColor(seg.color));
          baseR = c.r;
          baseG = c.g;
          baseB = c.b;
        }
        const r = isSelected ? selectedColor.r : baseR;
        const g = isSelected ? selectedColor.g : baseG;
        const b = isSelected ? selectedColor.b : baseB;
        const startVertex = positions.length / 3;
        let pairCount = 0;
        for (let i = 0; i < points.length - 1; i++) {
          const [ax, ay] = points[i];
          const [bx, by] = points[i + 1];
          positions.push(ax, ay, 0, bx, by, 0);
          colors.push(r, g, b, r, g, b);
          lineOwners.push({ kind: 'object', objectId });
          pairCount++;
        }
        if (objectId > 0 && pairCount > 0) {
          const range: ColorRange = {
            start: startVertex,
            count: pairCount * 2,
            base: [baseR, baseG, baseB],
          };
          const list = objectColorRanges.get(objectId);
          if (list) list.push(range);
          else objectColorRanges.set(objectId, [range]);
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
      // Bake colors at full intensity here; the playhead $effect below
      // applies the past/future fade in-place by mutating the color
      // attribute for the affected slice. Reading project.playhead in
      // rebuildGeometry would auto-track it as a dep, causing 60fps
      // re-tessellation + camera reset during playback (which fought
      // OrbitControls and made pan/zoom unusable).
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
        const r = opId === 0 ? moveTint.r : opCol.r * moveBoost;
        const g = opId === 0 ? moveTint.g : opCol.g * moveBoost;
        const b = opId === 0 ? moveTint.b : opCol.b * moveBoost;
        const startVertex = positions.length / 3;
        positions.push(seg.from.x, seg.from.y, seg.from.z, seg.to.x, seg.to.y, seg.to.z);
        colors.push(r, g, b, r, g, b);
        lineOwners.push({ kind: 'toolpath', segIdx: i });
        toolpathColors.push({ start: startVertex, base: [r, g, b] });
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
    // Snapshot the selection that's now baked into the color attribute,
    // so the selection-only $effect can compute deltas against it.
    appliedSelection = new Set(project.selectedObjects);

    // Update sceneRadius for raycaster threshold scaling, but do NOT
    // touch the camera here — fit-to-view is moved to a dedicated
    // $effect that only fires when project.imported actually changes.
    // Resetting the camera on every rebuild fought OrbitControls: any
    // layer toggle / Generate / op edit cancelled the user's view.
    lines.geometry.computeBoundingSphere();
    if (lines.geometry.boundingSphere) {
      sceneRadius = Math.max(lines.geometry.boundingSphere.radius, 1);
    }
    // Re-apply the past/future fade to the freshly-baked colors so the
    // playhead tint is correct even when no playhead change triggered
    // the rebuild (e.g. layer toggle).
    applyToolpathFade();
  }

  /// Camera fit-to-view, run once when a new geometry source appears.
  /// Manual fit (e.g. a "frame" button) would call this directly. Layer
  /// toggles / generates / op edits no longer reset the user's view.
  function fitCameraToScene() {
    if (!camera || !controls || !linesObject) return;
    const sphere = new THREE.Sphere();
    linesObject.geometry.computeBoundingSphere();
    if (linesObject.geometry.boundingSphere) {
      sphere.copy(linesObject.geometry.boundingSphere);
    }
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
    // Workspace-saved camera (if any) overrides the auto-fit on first
    // load. Subsequent project switches still snap to the new bbox
    // since `restoredFromWorkspace` is one-shot.
    maybeRestoreSavedCamera();
    requestRender();
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
      // Set playhead so the arc-length mapping lands at the end of the
      // picked segment (so the cutter sits there and gcode-panel
      // scrolls to the matching line).
      const cum = project.toolpathCumLen;
      const total = project.toolpathTotalLen;
      const segs = project.generated?.toolpath.length ?? 0;
      if (cum && total > 0 && owner.segIdx >= 0 && owner.segIdx < cum.length) {
        project.playhead = Math.min(1, cum[owner.segIdx] / total);
      } else if (segs > 0) {
        project.playhead = (owner.segIdx + 1) / segs;
      }
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
