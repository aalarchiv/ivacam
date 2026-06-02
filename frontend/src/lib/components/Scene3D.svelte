<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as THREE from 'three';
  import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
  // Fat lines: WebGL caps LineBasicMaterial.linewidth to 1px, so the
  // preview-line-width setting (68ab) drives Line2/LineMaterial instead,
  // which renders width in screen pixels via a resolution uniform.
  import { LineSegments2 } from 'three/addons/lines/LineSegments2.js';
  import { LineSegmentsGeometry } from 'three/addons/lines/LineSegmentsGeometry.js';
  import { LineMaterial } from 'three/addons/lines/LineMaterial.js';
  import {
    project,
    playheadToSegment,
    simWarningSeverity,
    simWarningSegmentIdx,
    isContourOp,
  } from '../state/project.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { opHue, opSourceHsl } from '../state/op-color';
  import { HeightfieldDriver, computeFootprint } from '../sim/driver';
  import { autoTabTs, buildObjectPolylines, polylineAtT } from '../cam/tabs';
  import { tessellate } from '../scene3d/tessellate';
  import { buildToolMesh, disposeMesh } from '../scene3d/tool_mesh';
  import { pixelsPerCell } from '../scene3d/lod';
  import { warningPosition } from '../scene3d/warning_position';
  import { opIncludesObject } from '../state/op_source';
  import type { SimWarning, ToolpathSegment } from '../api/types';
  import type { ToolEntry } from '../state/project.svelte';
  import { previewSegmentsFor, previewVersion, requestPreview } from '../state/text_preview.svelte';
  import OpKindPicker, { PICKER_LABEL, type PickerKind } from './OpKindPicker.svelte';
  import { LONG_PRESS_MS, LONG_PRESS_MOVE_TOL_PX } from '../canvas/touch-gestures';
  import { computeArrowChevron, arrowSpacingMm } from '../scene3d/toolpath_buffers';
  import { resolveAci } from '../canvas/aci-color';
  import { unpackFixtureColor, DEFAULT_FIXTURE_COLOR } from '../canvas/fixture-color';

  interface Props {
    /// w5wx: mirrors EntityCanvas2D — after the right-click menu creates
    /// an op from the selection, bounce the sidebar to Operations so the
    /// new row is visible.
    onActivateSidebarPane?: (pane: 'stock' | 'layers' | 'text' | 'operations') => void;
  }
  let { onActivateSidebarPane }: Props = $props();

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
    // 9tba: re-evaluate the heightfield LOD level whenever the camera
    // moves. setLodHint is a no-op when the recommended level matches
    // the current one (the common case during smooth orbiting), so the
    // per-event cost is just a few divisions.
    updateHeightfieldLod();
  }

  /// 9tba: ask the heightfield driver to pick an LOD level for the
  /// current camera distance + the user's render-triangle budget.
  /// Cheap; safe to call on every camera-change tick.
  function updateHeightfieldLod() {
    if (!driver || !camera || !controls || !renderer) return;
    const cellSize = driver.getCellSize();
    if (cellSize == null) return;
    const ppc = pixelsPerCell({
      cellSizeMm: cellSize,
      cameraDistance: camera.position.distanceTo(controls.target),
      fovDeg: camera.fov,
      renderHeightPx: renderer.domElement.clientHeight,
    });
    if (ppc == null) return;
    driver.setLodHint(ppc, project.settings.maxRenderTriangles);
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
      // 68ab: refresh fat-line resolution from the LIVE renderer size
      // before every render. The 3D pane is `display:none` while the 2D
      // tab is active, so geometry built then has clientWidth 0 and bakes
      // a (1,1) resolution → hairline lines. Re-deriving it here makes the
      // width correct the moment the pane becomes visible, regardless of
      // build timing.
      renderer.getSize(resVec);
      if (resVec.x > 0 && resVec.y > 0) updateLineResolution(resVec.x, resVec.y);
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

  // Pickable line meshes — split into two so editing operations or moving
  // the playhead does NOT teardown + reupload the imported-geometry buffer
  // (and vice versa). Each LineSegments owns its own positions / colors /
  // owner array; raycaster.intersectObjects([…]) queries both at once.
  //
  //   importedLinesObject — imported drawing segments + text-layer
  //     previews. Rebuilds on transformedImport / visibleLayers /
  //     textLayers / previewVersion changes. Selection-color toggles
  //     mutate its color attribute in place.
  //
  //   toolpathLinesObject — generated toolpath wireframe. Rebuilds on
  //     generated / operations changes (op enable/disable filter is
  //     reapplied). Playhead fade + sim-warning tints mutate its color
  //     attribute in place.
  type LineOwner = { kind: 'object'; objectId: number } | { kind: 'toolpath'; segIdx: number };
  let importedLinesObject: LineSegments2 | undefined;
  let importedLineOwners: LineOwner[] = [];
  let toolpathLinesObject: LineSegments2 | undefined;
  /// Direction-indicator chevrons drawn on top of the toolpath
  /// wireframe. One pair of short line segments per qualifying
  /// toolpath segment (cut / plunge / retract / arc; rapids omitted
  /// — the user doesn't care about feed direction on positioning
  /// moves). Decluttered by a min-length threshold + a cumulative
  /// spacing rule so a dense raster pocket doesn't drown the scene
  /// in arrowheads.
  let toolpathArrowsObject: LineSegments2 | undefined;
  let toolpathLineOwners: LineOwner[] = [];
  let sceneRadius = 100;

  /// Per-object color ranges into importedLinesObject's color attribute.
  /// Each entry is `{ start, count, base: [r,g,b] }` — start is the
  /// vertex index (not floats) where this object's first vertex lives,
  /// count is how many vertices belong to it, base is the original
  /// (non-selected) color the object should revert to. Filled during
  /// rebuildImportedGeometry so the selection-only $effect can mutate
  /// the color attribute in-place.
  type ColorRange = { start: number; count: number; base: [number, number, number] };
  let objectColorRanges = new Map<number, ColorRange[]>();
  /// Selection set the color attribute currently reflects. Compared
  /// against project.selectedObjects to compute the symmetric diff.
  let appliedSelection = new Set<number>();

  /// Per-toolpath-segment color ranges into toolpathLinesObject's color
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
  // w5wx: right-click context menu. `rightStart` records the right-button
  // press so the `contextmenu` handler can tell a tap (open the menu)
  // from an OrbitControls right-drag pan (don't). `ctxMenu` is the menu's
  // host-relative position when open.
  let rightStart: { x: number; y: number } | null = null;
  let ctxMenu = $state<{ x: number; y: number } | null>(null);

  /// bwt7: touch long-press → context menu (parity with the 2D pane and
  /// with mouse right-click). OrbitControls already handles one-finger
  /// rotate / two-finger pan+zoom on touch; this only adds the
  /// hold-to-open-menu path. `lpPointers` counts live touches so a
  /// second finger (a pinch) cancels the pending hold. `lpStart` is the
  /// press position in client coords, used both for the move-tolerance
  /// check and to anchor the menu.
  const lpPointers = new Set<number>();
  let lpTimer: ReturnType<typeof setTimeout> | null = null;
  let lpStart: { x: number; y: number } | null = null;
  function cancelLongPress() {
    if (lpTimer != null) {
      clearTimeout(lpTimer);
      lpTimer = null;
    }
    lpStart = null;
  }
  /// Open the op-picker context menu at a viewport-clamped position
  /// derived from client coords. Shared by mouse right-click and the
  /// touch long-press.
  function openCtxMenuAt(clientX: number, clientY: number) {
    if (!host) return;
    const rect = host.getBoundingClientRect();
    const x = Math.max(4, Math.min(clientX - rect.left, host.clientWidth - 260));
    const y = Math.max(4, Math.min(clientY - rect.top, host.clientHeight - 220));
    ctxMenu = { x, y };
  }
  const raycaster = new THREE.Raycaster();
  const ndc = new THREE.Vector2();
  const resVec = new THREE.Vector2();
  /// w5wx/68ab: per-op dashed overlays revealing multi-op source
  /// assignments. One dashed Line2 per (multi-op object, op) — the dashes
  /// of each op tile the object's path in distinct phases so all op
  /// colors show as interleaved bands (a thin solid line can only show
  /// one color). Decorative: not picked, not selection-recolored.
  let assignmentOverlayObjects: LineSegments2[] = [];

  function cssVar(name: string, fallback: string): string {
    if (!host) return fallback;
    const v = getComputedStyle(host).getPropertyValue(name).trim();
    return v || fallback;
  }
  function cssColor(name: string, fallback: number): THREE.Color {
    return new THREE.Color(cssVar(name, '') || fallback);
  }
  /// Per-toolpath-kind tip colors for the playhead glyph (the small
  /// cone/sphere at the cutter tip). Recomputed by `applyTheme` so
  /// theme switches don't leave the tip a stale color while the rest
  /// of the toolpath restyles.
  let tipColorByKind: Record<string, number> = {
    rapid: 0x35a2ff,
    cut: 0xff5555,
    plunge: 0xffd23a,
    retract: 0x5fd06e,
    arc: 0xff8a3a,
  };
  function refreshTipColors() {
    tipColorByKind = {
      rapid: cssColor('--toolpath-rapid', 0x35a2ff).getHex(),
      cut: cssColor('--toolpath-cut', 0xff5555).getHex(),
      plunge: cssColor('--toolpath-plunge', 0xffd23a).getHex(),
      retract: cssColor('--toolpath-retract', 0x5fd06e).getHex(),
      arc: cssColor('--toolpath-arc', 0xff8a3a).getHex(),
    };
  }

  /// Deterministic hue in [0, 1) per op id. Delegates to the shared
  /// `opHue` so the toolpath, the 3D source tint, and the 2D canvas all
  /// land on the SAME color for a given op.
  const opPalette = opHue;

  /// Build a fat-line (Line2) object from flat per-segment position +
  /// color arrays (6 floats per segment — the same layout the old
  /// LineSegments buffers used, which is also exactly how
  /// LineSegmentsGeometry stores its interleaved instance buffers, so
  /// the playhead-fade / selection recolor offset math is unchanged).
  /// `linewidth` is in screen pixels (worldUnits off); the `resolution`
  /// uniform must track the canvas size — set here and on every resize.
  function buildFatLines(positions: number[], colors: number[]): LineSegments2 {
    const geom = new LineSegmentsGeometry();
    geom.setPositions(new Float32Array(positions));
    geom.setColors(new Float32Array(colors));
    const mat = new LineMaterial({
      vertexColors: true,
      linewidth: Math.max(0.5, project.settings.previewLineWidth),
      worldUnits: false,
    });
    mat.resolution.set(host?.clientWidth || 1, host?.clientHeight || 1);
    return new LineSegments2(geom, mat);
  }

  /// Push the canvas pixel size into every fat-line material's
  /// `resolution` uniform (they render wrong / invisible otherwise).
  function updateLineResolution(w: number, h: number) {
    for (const o of [importedLinesObject, toolpathLinesObject, toolpathArrowsObject]) {
      (o?.material as LineMaterial | undefined)?.resolution.set(w, h);
    }
    for (const o of assignmentOverlayObjects) {
      (o.material as LineMaterial).resolution.set(w, h);
    }
  }

  onMount(() => {
    scene = new THREE.Scene();
    scene.background = cssColor('--bg-app', 0x0d0d0d);

    camera = new THREE.PerspectiveCamera(45, 1, 0.1, 5000);
    camera.position.set(150, -150, 150);
    camera.up.set(0, 0, 1);

    renderer = new THREE.WebGLRenderer({ antialias: true });
    renderer.setPixelRatio(window.devicePixelRatio);
    // The WebGL canvas is a THREE-owned child of `host`; Svelte's
    // reconciler doesn't touch it (no template renders into host beyond
    // the bind:this div itself), so appending it directly is safe.
    // eslint-disable-next-line svelte/no-dom-manipulating
    host.appendChild(renderer.domElement);
    renderer.domElement.addEventListener('pointerdown', onPointerDown);
    renderer.domElement.addEventListener('pointerup', onPointerUp);
    renderer.domElement.addEventListener('pointercancel', onPointerCancel);
    renderer.domElement.addEventListener('pointermove', onPointerMoveLongPress);
    renderer.domElement.addEventListener('contextmenu', onContextMenu);

    controls = new OrbitControls(camera, renderer.domElement);
    // Damping defaults to true on OrbitControls, which produced a ~30-
    // frame ease-out drift after every drag/zoom/pan release. Users
    // read that as lag, not smoothness — disable it so motion stops on
    // release. tickFrame() still calls controls.update() each frame; it's
    // a no-op when nothing changed.
    controls.enableDamping = false;
    // OrbitControls dispatches 'change' on user drag, zoom, pan.
    // Hooking it is enough to keep the scene rendering and to persist
    // the camera pose to the workspace.
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

    // Defer the resize-driven fit() to the next animation frame.
    // `fit()` calls `renderer.setSize(w, h)` which adjusts the
    // observed canvas, retriggering the observer in the same layout
    // pass → "ResizeObserver loop completed with undelivered
    // notifications". Coalescing into one rAF eliminates the
    // warning and avoids duplicate `setSize` calls during multi-
    // event resizes.
    let fitFrame = 0;
    observer = new ResizeObserver(() => {
      if (fitFrame !== 0) return;
      fitFrame = requestAnimationFrame(() => {
        fitFrame = 0;
        fit();
      });
    });
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
    // Populate the tip-color cache with the live tokens — without this
    // the first frame after mount would draw the playhead in the
    // dark-theme fallback even when the user's on light theme.
    refreshTipColors();
  });

  function applyTheme() {
    if (!scene) return;
    refreshTipColors();
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
    // After grid swap, re-emit both line buffers so the imported drawing
    // + toolpath wireframe sit cleanly on top of the new grid.
    rebuildImportedGeometry();
    rebuildToolpathGeometry();
  }

  onDestroy(() => {
    stopTick();
    observer?.disconnect();
    intersectObserver?.disconnect();
    document.removeEventListener('visibilitychange', onVisibilityChange);
    if (renderer) {
      renderer.domElement.removeEventListener('pointerdown', onPointerDown);
      renderer.domElement.removeEventListener('pointerup', onPointerUp);
      renderer.domElement.removeEventListener('pointercancel', onPointerCancel);
      renderer.domElement.removeEventListener('pointermove', onPointerMoveLongPress);
      renderer.domElement.removeEventListener('contextmenu', onContextMenu);
    }
    controls?.removeEventListener('change', requestRender);
    controls?.removeEventListener('change', onCameraChanged);
    if (cameraSaveTimer) {
      clearTimeout(cameraSaveTimer);
      cameraSaveTimer = null;
    }
    if (simRebuildTimer) {
      clearTimeout(simRebuildTimer);
      simRebuildTimer = null;
    }
    controls?.dispose();
    if (toolMesh) {
      disposeMesh(toolMesh);
      toolMesh = undefined;
    }
    disposeStockGroup();
    if (workAreaGroup) {
      disposeGroup(workAreaGroup);
      scene?.remove(workAreaGroup);
      workAreaGroup = undefined;
    }
    // 7iej.4: renderer.dispose() frees the GL context but does NOT walk
    // the scene graph, so every owned group's geometry/material must be
    // disposed explicitly. geometryGroup holds the imported wireframe,
    // toolpath lines, direction arrows, and assignment overlays — the
    // largest buffers — and these leaked a full toolpath on every 2D↔3D
    // pane swap (Scene3D unmounts on each swap).
    if (geometryGroup) {
      disposeGroup(geometryGroup);
      scene?.remove(geometryGroup);
      geometryGroup = undefined;
      importedLinesObject = undefined;
      toolpathLinesObject = undefined;
      toolpathArrowsObject = undefined;
      assignmentOverlayObjects = [];
    }
    for (const g of [tabsGroup, approachGroup, fixturesGroup, warningGroup]) {
      if (g) {
        disposeGroup(g);
        scene?.remove(g);
      }
    }
    tabsGroup = undefined;
    approachGroup = undefined;
    fixturesGroup = undefined;
    warningGroup = undefined;
    driver?.destroy();
    driver = undefined;
    renderer?.dispose();
    if (themeMql && onThemeChange) {
      themeMql.removeEventListener('change', onThemeChange);
    }
    onThemeChange = undefined;
    themeMo?.disconnect();
    if (renderer && host?.contains(renderer.domElement)) {
      // Counterpart to the appendChild above — Svelte didn't render this
      // child, so we own its removal too. See the mount comment.
      // eslint-disable-next-line svelte/no-dom-manipulating
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
    updateLineResolution(w, h);
    requestRender();
  }

  // Mirror imported geometry into the 3D scene as flat polylines on Z=0.
  // When a /generate response is also available, draw the 3D toolpath on
  // top with depth + color coded by move kind (rapid/cut/plunge/retract).
  // Per-concern effects (audit gk8). The previous mega-effect read
  // seven project.* fields and rebuilt geometry+tabs+stock+fixtures
  // on every change — toggling a fixture's color used to rebuild the
  // toolpath wireframe. Split so each builder only refires when its
  // own inputs change.

  // Geometry wireframe: imported drawing, layer toggles, generated
  // toolpath, and the op set (color stamps follow op_id).
  // Two effects, two buffers (see LineSegments declaration above for
  // rationale). Each rebuild is independent — editing an op no longer
  // tears down the imported-geometry buffer, and toggling a layer no
  // longer teardowns the toolpath buffer.

  // Imported drawing + text-layer previews.
  $effect(() => {
    void project.transformedImport;
    void project.visibleLayers;
    void project.textLayers;
    void previewVersion.v;
    void project.generated; // affects fade for non-selected imports
    void project.settings.previewMode; // affects contrast-against-stock color
    void project.operations; // op-source assignments drive the per-op tint
    void project.selectedOpId; // selected op renders emphasized
    rebuildImportedGeometry();
    requestRender();
  });

  // Generated toolpath wireframe (re-emitted when a new pipeline run
  // resolves or the user toggles an op enable / disable).
  $effect(() => {
    void project.generated;
    void project.operations;
    void project.settings.toolMoveArrowDensity; // arrow spacing
    rebuildToolpathGeometry();
    requestRender();
  });

  // Fat-line thickness (68ab): update the live materials in place rather
  // than rebuilding geometry, so dragging the slider is cheap.
  $effect(() => {
    const lw = Math.max(0.5, project.settings.previewLineWidth);
    for (const o of [importedLinesObject, toolpathLinesObject, toolpathArrowsObject]) {
      const m = o?.material as LineMaterial | undefined;
      if (m) m.linewidth = lw;
    }
    // Overlays render a touch wider so the colored dashes sit proud of
    // the base wireframe.
    for (const o of assignmentOverlayObjects) {
      (o.material as LineMaterial).linewidth = lw + 1;
    }
    requestRender();
  });

  // Keep the text-preview cache warm independent of the 2D canvas — the
  // user might never visit 2D and still expects text to show in 3D.
  $effect(() => {
    for (const layer of project.textLayers) {
      requestPreview(layer);
    }
  });

  // Tab markers: per-op tab placements + tabMode.
  $effect(() => {
    void project.transformedImport;
    void project.operations;
    updateTabs();
    requestRender();
  });

  // n79: approach-point needle for the currently selected op. The
  // marker shows up only when the user is looking at the op that
  // carries it (driven by selectedOpId) — otherwise the 3D view
  // stays uncluttered.
  $effect(() => {
    void project.operations;
    void project.selectedOpId;
    void project.machine.fastMoveZ;
    updateApproach();
    requestRender();
  });

  // Stock bbox visual: stock config + toggle. Doesn't touch the
  // toolpath wireframe.
  $effect(() => {
    void project.stock;
    void project.settings.showStockBox;
    updateStock();
    requestRender();
  });

  // Machine work-area wireframe — the always-visible envelope the user
  // can't move the cutter outside of. Dotted-style edges so it reads
  // as "limit, not solid", and dim opacity so it sits in the back of
  // the scene without competing with the toolpath.
  $effect(() => {
    void project.machine.workArea;
    updateWorkArea();
    requestRender();
  });

  // Fixture meshes: fixtures themselves + selection / playback flash.
  // No reason to rebuild the toolpath when the user clicks a fixture.
  $effect(() => {
    void project.fixtures;
    void project.selectedFixtureId;
    updateFixtures();
    requestRender();
  });

  /// Fit-to-view fires ONLY when the count of imports changes — i.e.
  /// the user added or removed a drawing. fileTransform tweaks, layer
  /// toggles, op edits, and Generates all derive a new
  /// `transformedImport` reference but must NOT overrule the user's
  /// chosen camera angle (user feedback this session). Tracking the
  /// length directly (rather than the derived reference) gives the
  /// right invalidation profile: add file → fit; tweak transform → no
  /// touch.
  $effect(() => {
    void project.imports.length;
    fitCameraToScene();
  });

  /// Selection-only fast path: mutate the color attribute in place
  /// instead of rebuilding the entire LineSegments mesh on every click.
  /// Falls through to a full rebuild only if the geometry is missing
  /// (e.g. before the first rebuild has run).
  $effect(() => {
    const sel = project.selectedObjects;
    if (!importedLinesObject) {
      // Geometry hasn't been built yet; the next rebuildImportedGeometry
      // will pick up the current selection naturally.
      return;
    }
    applySelectionDelta(sel);
    requestRender();
  });

  $effect(() => {
    void project.playhead;
    void project.generated;
    void project.tools;
    void project.operations; // op→tool assignment drives the cutter mesh
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
  ///
  /// DEBOUNCED: the sim driver rebuild (heightfield reconstruction +
  /// WASM cell-grid build + toolpath replay) is heavy. Without a
  /// debounce, every keystroke in a Stock-thickness / margin /
  /// cell-resolution input fires a full rebuild and freezes the UI
  /// while the user is typing. Wait for values to settle before
  /// kicking the driver. The cheap bbox-box visual (`updateStock`)
  /// still updates instantly via its own effect.
  let simRebuildTimer: ReturnType<typeof setTimeout> | null = null;
  // 1 s lets the user finish typing a multi-digit stock value (e.g.
  // 12.50) before the WASM heightfield re-bakes. Cheap visuals (the
  // bbox box) still update every keystroke via `updateStock`.
  const SIM_REBUILD_DEBOUNCE_MS = 1000;

  /// Wireframe visibility — wireframe/both modes show lines, solid mode
  /// hides them. Used by both the preview-mode effect and the rebuild
  /// functions so freshly-created LineSegments start with the right
  /// visibility (otherwise toggling solid → wireframe would only affect
  /// the buffer that was alive at the toggle moment).
  const wireVisible = $derived(project.settings.previewMode !== 'solid');

  /// Build a per-segment tool resolver for the sim: each toolpath segment
  /// is carved with ITS op's tool (looked up by op_id), so a multi-op
  /// program (e.g. endmill profile then v-bit chamfer) carves each part
  /// with the correct cutter cross-section instead of one tool for all.
  function toolForSegment(segs: ToolpathSegment[]): (i: number) => ToolEntry {
    const byOp = new Map<number, ToolEntry>();
    for (const op of project.operations) {
      const t = project.tools.find((tt) => tt.id === op.toolId);
      if (t) byOp.set(op.id, t);
    }
    const fallback = project.tools[0];
    return (i) => byOp.get(segs[i]?.op_id ?? -1) ?? fallback;
  }

  $effect(() => {
    if (!scene) return;
    const settings = project.settings;
    // Wire-mesh visibility tracks the preview mode: wireframe / both
    // show the toolpath + imported lines; solid hides them in favor of
    // the heightfield carved-stock mesh. wireVisible is a $derived at
    // module scope so the rebuild functions see the same value.
    if (importedLinesObject) importedLinesObject.visible = wireVisible;
    if (toolpathLinesObject) toolpathLinesObject.visible = wireVisible;
    if (toolpathArrowsObject) toolpathArrowsObject.visible = wireVisible;
    for (const o of assignmentOverlayObjects) o.visible = wireVisible;
    if (settings.previewMode === 'wireframe') {
      driver?.setVisible(false);
      requestRender();
      return;
    }
    const imported = project.transformedImport;
    const generated = project.generated;
    const firstOp = project.operations[0];
    const tool = project.tools.find((t) => t.id === (firstOp?.toolId ?? 0)) ?? project.tools[0];
    if (!imported || !generated || !tool) {
      driver?.setVisible(false);
      requestRender();
      return;
    }
    const cellRes = settings.cellResolutionMode === 'manual' ? settings.cellResolutionMm : -1;
    // Sim-rebuild key uses ONLY the fields the heightfield cares about:
    // the bbox + stock bounds + tool diameter + cell resolution +
    // fixture POSITIONS / GEOMETRY (not color or name). Hashing the full
    // fixture array re-rendered the sim every time the user tweaked a
    // fixture's color, which is a cosmetic-only change.
    const fixturesKey = project.fixtures
      .map((f) => {
        const k = f.kind;
        let shape: string;
        if (k.shape === 'box') shape = `b:${k.width}:${k.depth}`;
        else if (k.shape === 'cylinder') shape = `c:${k.radius}`;
        else shape = `p:${k.vertices.map((v) => `${v[0]},${v[1]}`).join('|')}`;
        return `${f.id}:${f.origin[0]},${f.origin[1]}:${f.z_bottom},${f.z_top}:${shape}`;
      })
      .join(';');
    const key = JSON.stringify({
      bbox: imported.bbox,
      stock: project.stock,
      tool_id: tool.id,
      tool_dia: tool.diameter,
      cellRes,
      maxCells: settings.maxSimulationCells,
      gen_id: project.generatedVersion,
      fixturesKey,
    });
    if (key === lastSimKey) {
      driver?.setVisible(true);
      driver?.setSolidVisible(settings.previewMode === 'solid' || settings.previewMode === 'both');
      driver?.setEdgesVisible(settings.previewMode === 'solid' || settings.previewMode === 'both');
      requestRender();
      return;
    }
    // Cancel any pending rebuild from a prior keystroke. The effect
    // re-runs synchronously on every reactive change, so this debounce
    // collapses a burst of stock edits into a single rebuild after
    // the user pauses.
    if (simRebuildTimer != null) {
      clearTimeout(simRebuildTimer);
    }
    simRebuildTimer = setTimeout(() => {
      simRebuildTimer = null;
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
          driver.setSolidVisible(
            settings.previewMode === 'solid' || settings.previewMode === 'both',
          );
          driver.setEdgesVisible(
            settings.previewMode === 'solid' || settings.previewMode === 'both',
          );
          lastSimKey = key;
          // Replay 0..head so we see the carved state immediately (not
          // an unmilled stock we have to scrub forward through).
          driver.advanceTo(
            project.playhead,
            generated.toolpath,
            toolForSegment(generated.toolpath),
            project.toolpathCumLen,
            project.toolpathTotalLen,
          );
          // 9tba: select the right LOD level for the current camera
          // distance once the new pyramid exists, so the first paint
          // uses the affordable level instead of L0.
          updateHeightfieldLod();
          requestRender();
        })
        .catch((e) => {
          project.setError(`solid preview: ${e instanceof Error ? e.message : String(e)}`);
        });
    }, SIM_REBUILD_DEBOUNCE_MS);
  });

  /// Advance the simulation on every playhead change. Falls through
  /// silently if the driver isn't built yet (preview mode = wireframe
  /// or no generated yet).
  $effect(() => {
    void project.playhead;
    if (!driver) return;
    const generated = project.generated;
    if (!generated || project.tools.length === 0) return;
    driver.advanceTo(
      project.playhead,
      generated.toolpath,
      toolForSegment(generated.toolpath),
      project.toolpathCumLen,
      project.toolpathTotalLen,
      // 27ng: pass the user's exact-rewind preference through to the
      // driver. Default false leaves the heightfield untouched on
      // backstep (deepest-ever state retained); true triggers the
      // reset + forward-replay path.
      project.settings.exactSimRewind,
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
    if (!toolpathLinesObject || toolpathColors.length === 0) return;
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
    const head =
      segIdx < 0
        ? Math.max(0, Math.min(total, Math.round(project.playhead * total)))
        : Math.max(0, Math.min(total, segIdx + 1));
    if (head === appliedHead) return;
    // LineSegmentsGeometry stores colors as one interleaved instance
    // buffer (6 floats / segment: start-rgb, end-rgb) — the same layout
    // the old flat color array used, so the `start * 3` offset math below
    // is unchanged; only the buffer handle + dirty flag differ.
    const colorAttr = toolpathLinesObject.geometry.getAttribute(
      'instanceColorStart',
    ) as THREE.InterleavedBufferAttribute;
    const arr = colorAttr.array as Float32Array;
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
    colorAttr.data.needsUpdate = true;
    appliedHead = head;
  }

  function applySelectionDelta(next: Set<number>) {
    if (!importedLinesObject) return;
    // Interleaved instance color buffer (6 floats / segment); the
    // ColorRange offsets (start = first vertex index = 2·firstSegment)
    // index it identically to the old flat color attribute.
    const colorAttr = importedLinesObject.geometry.getAttribute(
      'instanceColorStart',
    ) as THREE.InterleavedBufferAttribute;
    const arr = colorAttr.array as Float32Array;
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
    if (touched) colorAttr.data.needsUpdate = true;
    appliedSelection = new Set(next);
  }

  let tabsGroup: THREE.Group | undefined;
  let stockGroup: THREE.Group | undefined;
  let workAreaGroup: THREE.Group | undefined;
  let fixturesGroup: THREE.Group | undefined;
  /// n79: a vertical needle (line) + tiny dot at the selected op's
  /// approach point. Rebuilt on every relevant project change so
  /// drag updates show live.
  let approachGroup: THREE.Group | undefined;
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

  function warningMarkerColor(w: SimWarning): THREE.Color {
    return simWarningSeverity(w) === 'critical'
      ? cssColor('--error', 0xe54848)
      : cssColor('--warn', 0xf0c020);
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
      const pos = warningPosition(w, project.generated?.toolpath);
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
      for (const id of next)
        if (!flashingFixtures.has(id)) {
          changed = true;
          break;
        }
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
    const data = project.transformedImport;
    // Stock-first: render the stock even without a drawing (falls back
    // to machine work-area inside computeFootprint).
    const fp = computeFootprint(data, cfg, project.machine.workArea);
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
      // Theme-tracking neutral so the stock fill is visible against both
      // the dark and light backdrops. `--stock-edge` is the matching
      // outline token (used a few lines below).
      color: cssColor('--stock-edge', 0xcccccc),
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

  /// Build/refresh the machine work-area wireframe. A dashed box from
  /// (0, 0, 0) to (workArea.x, workArea.y, workArea.z) so the user
  /// sees the machinable envelope. Rebuilt whenever the user edits the
  /// work area in MachineDialog.
  function updateWorkArea() {
    if (!scene) return;
    if (!workAreaGroup) {
      workAreaGroup = new THREE.Group();
      workAreaGroup.name = 'work-area';
      scene.add(workAreaGroup);
    }
    disposeGroup(workAreaGroup);
    const wa = project.machine.workArea;
    if (!wa || wa.x <= 0 || wa.y <= 0 || wa.z <= 0) return;
    // Center the box on (wa.x/2, wa.y/2, wa.z/2) since BoxGeometry is
    // centered on its local origin. The work area corner sits at (0, 0, 0).
    const cx = wa.x * 0.5;
    const cy = wa.y * 0.5;
    const cz = wa.z * 0.5;
    const box = new THREE.BoxGeometry(wa.x, wa.y, wa.z);
    const edges = new THREE.EdgesGeometry(box);
    const lineMat = new THREE.LineDashedMaterial({
      color: cssColor('--text-muted', 0x888888),
      dashSize: 3,
      gapSize: 2,
      transparent: true,
      opacity: 0.45,
    });
    const wire = new THREE.LineSegments(edges, lineMat);
    wire.computeLineDistances();
    wire.position.set(cx, cy, cz);
    workAreaGroup.add(wire);
    box.dispose();
  }

  /// Generic group disposer — frees geometry + materials for every
  /// LineSegments / Mesh child before removing them. Shared by the
  /// stock + work-area cleanup paths.
  function disposeGroup(g: THREE.Group) {
    while (g.children.length > 0) {
      const child = g.children[0];
      g.remove(child);
      // `THREE.Line` covers plain lines AND `LineSegments` (which extends
      // it); `THREE.Mesh` covers meshes AND the fat-line `LineSegments2`
      // (which extends Mesh). Both carry `.geometry` + `.material`.
      // Disposing a geometry/material shared across several children
      // (e.g. updateTabs reuses one SphereGeometry) more than once is a
      // safe no-op in three.js.
      if (child instanceof THREE.Mesh || child instanceof THREE.Line) {
        child.geometry.dispose();
        const m = (child as THREE.Mesh | THREE.Line).material as THREE.Material | THREE.Material[];
        if (Array.isArray(m)) m.forEach((mm) => mm.dispose());
        else m.dispose();
      }
    }
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
      // 7iej.12: shared unpack with the 2D canvas. Default alpha ~0.5 when
      // the wire color omits it; the 3D opacity treatment stays here.
      const { a, hex } = unpackFixtureColor(f.color);
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
        geom = new THREE.BoxGeometry(
          Math.max(0.01, f.kind.width),
          Math.max(0.01, f.kind.depth),
          sizeZ,
        );
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
        const shape = new THREE.Shape(f.kind.vertices.map(([x, y]) => new THREE.Vector2(x, y)));
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
    const flashColor = cssColor('--error', 0xe54848);
    for (const [id, mats] of fixtureMaterials) {
      const flash = flashingFixtures.has(id);
      const base = fixtureBaseColors.get(id) ?? DEFAULT_FIXTURE_COLOR;
      for (const m of mats) {
        if (flash) m.color.copy(flashColor);
        else m.color.set(base);
      }
    }
  }

  function updateTabs() {
    if (!scene) return;
    if (!tabsGroup) {
      tabsGroup = new THREE.Group();
      scene.add(tabsGroup);
    }
    // 7iej.4: dispose the previous run's geometry/material (the shared
    // sphere geom+mat from the last call) — `.clear()` only detaches
    // children and would leak a geom+mat pair on every op/transform edit.
    disposeGroup(tabsGroup);
    const imp = project.transformedImport;
    if (!imp) return;
    const color = cssColor('--tab-marker', 0xffd23a);
    const radius = Math.max(0.5, (imp.bbox.max_x - imp.bbox.min_x || 100) * 0.008);
    const geom = new THREE.SphereGeometry(radius, 12, 8);
    const mat = new THREE.MeshBasicMaterial({ color });
    // rt1.10 + hr5: tabs are per-op. Manual placements get resolved
    // directly via (objectId, t); Auto / Mixed modes additionally
    // walk every object the op covers and emit auto-spaced t values
    // there. Same arc-length math as the 2D canvas + backend.
    //
    // Performance (90j): build the object-polyline cache ONCE and
    // resolve placements inline against this local cache. The prior
    // code called resolveTabPlacementToWorld(imp, tp) per manual
    // placement, which internally re-ran buildObjectPolylines —
    // O(N_placements × N_segments) on a multi-thousand-segment DXF.
    const objects = buildObjectPolylines(imp);
    const objectById = new Map(objects.map((o) => [o.objectId, o]));
    for (const op of project.operations) {
      if (!isContourOp(op)) continue;
      const mode = op.tabMode;
      if (!mode || mode.kind === 'off') continue;
      // Manual placements (Manual + Mixed).
      if (mode.kind === 'manual' || mode.kind === 'mixed') {
        for (const tp of op.tabPlacements ?? []) {
          const obj = objectById.get(tp.objectId);
          if (!obj) continue;
          const { point } = polylineAtT(obj.pts, tp.t, obj.closed);
          const sphere = new THREE.Mesh(geom, mat);
          sphere.position.set(point.x, point.y, 0);
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

  /// n79: render a small vertical needle from z=0 up to `fast_move_z`
  /// at the selected op's `approachPoint`. Optional small sphere at
  /// the base so the marker reads even when the camera is top-down.
  /// The marker only appears when the active op carries one — same
  /// data the 2D canvas paints from.
  function updateApproach() {
    if (!scene) return;
    if (!approachGroup) {
      approachGroup = new THREE.Group();
      scene.add(approachGroup);
    }
    // 7iej.4: dispose the previous needle + dot geom/material — `.clear()`
    // alone leaks them on every approach-point drag / op selection.
    disposeGroup(approachGroup);
    const opId = project.selectedOpId;
    if (opId == null) return;
    const op = project.operations.find((o) => o.id === opId);
    if (!op) return;
    if (op.kind !== 'profile' && op.kind !== 'pocket') return;
    const ap = op.approachPoint;
    if (!ap) return;
    const [x, y] = ap;
    const topZ = Math.max(1, project.machine.fastMoveZ);
    const color = cssColor('--accent', 0x44aaaa);
    // Vertical needle from (x, y, 0) up to (x, y, topZ).
    const geom = new THREE.BufferGeometry().setFromPoints([
      new THREE.Vector3(x, y, 0),
      new THREE.Vector3(x, y, topZ),
    ]);
    const mat = new THREE.LineBasicMaterial({ color, linewidth: 2 });
    approachGroup.add(new THREE.Line(geom, mat));
    // Base dot — tiny sphere at z=0 to anchor the needle visually
    // when the camera is overhead.
    const dotR = Math.max(0.4, topZ * 0.04);
    const dotGeom = new THREE.SphereGeometry(dotR, 12, 8);
    const dotMat = new THREE.MeshBasicMaterial({ color });
    const dot = new THREE.Mesh(dotGeom, dotMat);
    dot.position.set(x, y, 0);
    approachGroup.add(dot);
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
    const headIdx =
      mapped.segIdx >= 0
        ? Math.max(0, Math.min(total - 1, mapped.segIdx))
        : Math.max(0, Math.min(total - 1, Math.round(project.playhead * total) - 1));
    const seg = gen.toolpath[headIdx];
    if (!seg) return;
    const t =
      mapped.segIdx >= 0
        ? Math.max(0, Math.min(1, mapped.segT))
        : Math.max(0, Math.min(1, project.playhead * total - headIdx));
    const px = seg.from.x + (seg.to.x - seg.from.x) * t;
    const py = seg.from.y + (seg.to.y - seg.from.y) * t;
    const pz = seg.from.z + (seg.to.z - seg.from.z) * t;

    const colorHex = tipColorByKind[seg.kind] ?? tipColorByKind.cut;

    // Pick the tool by the op actually cutting at the playhead (the
    // segment's op), so the displayed cutter changes as the playhead
    // crosses op boundaries. Fall back to the selected op, then the
    // first op. (Previously this preferred the SELECTED op, which showed
    // that op's tool throughout the whole program.)
    const segOp = project.operations.find((o) => o.id === seg.op_id);
    const selOp =
      project.selectedOpId == null
        ? null
        : (project.operations.find((o) => o.id === project.selectedOpId) ?? null);
    const opForTool = segOp ?? selOp ?? project.operations[0];
    const tool = project.tools.find((t) => t.id === (opForTool?.toolId ?? 0)) ?? project.tools[0];
    const diameter = Math.max(0.2, tool?.diameter ?? 3);
    const mode = project.machine.mode;
    const dragoff = tool?.dragoff;
    const tipDiameter = tool?.tipDiameter;
    const tipAngleDeg = tool?.tipAngleDeg;
    const kind = tool?.kind ?? 'endmill';
    const fluteLen = tool?.fluteLengthMm;
    const shankDia = tool?.shankDiameterMm;
    const holder = tool?.holder;
    const lengthMm = tool?.lengthMm;

    // Cache key — anything that changes the geometry shape. Color is NOT
    // part of the key; we only mutate material.color on the cached mesh
    // for that. Holder fields are JSON-stringified so the key updates
    // whenever any part of the holder spec changes.
    const key = `${kind}|${mode}|${diameter}|${tipDiameter ?? ''}|${tipAngleDeg ?? ''}|${dragoff ?? ''}|${fluteLen ?? ''}|${shankDia ?? ''}|${holder ? JSON.stringify(holder) : ''}|${lengthMm ?? ''}`;
    if (key !== toolMeshKey || !toolMesh) {
      if (toolMesh) {
        toolGroup.remove(toolMesh);
        disposeMesh(toolMesh);
      }
      toolMesh = buildToolMesh(
        kind,
        mode,
        diameter,
        tipDiameter,
        dragoff,
        colorHex,
        fluteLen,
        shankDia,
        holder,
        tipAngleDeg,
        lengthMm,
      );
      toolGroup.add(toolMesh);
      toolMeshKey = key;
    } else {
      // Cached mesh — just retint the material to match the active move.
      const m = toolMesh.material as THREE.MeshBasicMaterial;
      if (m.color.getHex() !== colorHex) m.color.setHex(colorHex);
    }
    toolMesh.position.set(px, py, pz);
  }

  /// Imported drawing + text-layer previews. Independent of toolpath /
  /// op enable state — re-runs only on transformedImport / visibleLayers /
  /// textLayers / previewVersion changes (plus `generated` to switch
  /// imported geometry to faded-color when a toolpath exists).
  function rebuildImportedGeometry() {
    if (!geometryGroup || !scene) return;
    // Tear down only the imported half; toolpath stays put.
    if (importedLinesObject) {
      geometryGroup.remove(importedLinesObject);
      importedLinesObject.geometry.dispose();
      (importedLinesObject.material as THREE.Material).dispose();
      importedLinesObject = undefined;
    }
    for (const o of assignmentOverlayObjects) {
      geometryGroup.remove(o);
      o.geometry.dispose();
      (o.material as THREE.Material).dispose();
    }
    assignmentOverlayObjects = [];
    importedLineOwners = [];
    objectColorRanges = new Map();
    const data = project.transformedImport;
    if (!data) {
      updateSceneRadius();
      return;
    }

    const positions: number[] = [];
    const colors: number[] = [];
    const c = new THREE.Color();
    const fadedColor = cssColor('--imported-faded', 0x444444);
    const selectedColor = cssColor('--accent', 0x4a8df0);
    // When the stock heightfield is visible as a solid surface, the
    // tan / configured stock color drowns out the ACI / faded
    // wireframe colors. Use the user-configured EDGE color (already
    // chosen for contrast against the stock material) as the line
    // tint. Falls back to ACI when no solid is showing.
    const previewMode = project.settings.previewMode;
    const solidVisible = previewMode === 'solid' || previewMode === 'both';
    const contrastOverStock = solidVisible ? new THREE.Color(project.settings.edgeColor) : null;
    // Lift the wireframe slightly above the stock top surface so it
    // doesn't Z-fight with the heightfield mesh (top_z = 0 in the
    // stock coord system). 0.1 mm is below the smallest carve step
    // but enough to win the depth test.
    const lineZ = solidVisible ? 0.1 : 0;
    const flat = !!project.generated;
    // Source-assignment tint: objectId → op ids that reference it (mirror
    // of EntityCanvas2D.objectToOps). An assigned object is drawn in its
    // op's color — overriding the ACI / faded base so the assignment is
    // visible even after Generate (when the wireframe otherwise fades to
    // near-black). The base wireframe carries the PRIMARY op's solid
    // color (selected op if assigned, else the first); objects in several
    // ops additionally get phase-staggered DASHED overlays (built below)
    // so every assigned op's color shows as interleaved bands — a single
    // thin/thick line can only carry one color at a time.
    const objectToOps3d = new Map<number, number[]>();
    for (const op of project.operations) {
      const refs = op.sourceObjects;
      if (!refs) continue;
      for (const id of refs) {
        if (id <= 0) continue;
        const list = objectToOps3d.get(id);
        if (list) list.push(op.id);
        else objectToOps3d.set(id, [op.id]);
      }
    }
    const selOpId = project.selectedOpId;
    // Per-object path points for the multi-op dashed overlays. Only
    // populated for objects in ≥2 ops; each object's pairs are pushed in
    // buffer (path) order so LineSegments2.computeLineDistances gives a
    // cumulative distance → the dashes tile continuously along the path.
    const overlayPosByObj = new Map<number, number[]>();
    let segIdx = 0;
    for (const seg of data.segments) {
      if (!project.visibleLayers.has(seg.layer)) {
        segIdx++;
        continue;
      }
      const objectId = data.objects?.[segIdx] ?? 0;
      const isSelected = objectId > 0 && project.selectedObjects.has(objectId);
      const points = tessellate(seg);
      const assignedOps = objectId > 0 ? objectToOps3d.get(objectId) : undefined;
      let baseR: number;
      let baseG: number;
      let baseB: number;
      if (assignedOps && assignedOps.length > 0) {
        // Primary op: the selected one if this object is among its
        // sources, otherwise the first-assigned op.
        const primaryOp =
          selOpId != null && assignedOps.includes(selOpId) ? selOpId : assignedOps[0];
        const [hh, ss, ll] = opSourceHsl(primaryOp, primaryOp === selOpId);
        c.setHSL(hh, ss, ll);
        baseR = c.r;
        baseG = c.g;
        baseB = c.b;
      } else if (contrastOverStock) {
        baseR = contrastOverStock.r;
        baseG = contrastOverStock.g;
        baseB = contrastOverStock.b;
      } else if (flat) {
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
      let overlayBuf: number[] | null = null;
      if (assignedOps && assignedOps.length >= 2) {
        overlayBuf = overlayPosByObj.get(objectId) ?? null;
        if (!overlayBuf) {
          overlayBuf = [];
          overlayPosByObj.set(objectId, overlayBuf);
        }
      }
      for (let i = 0; i < points.length - 1; i++) {
        const [ax, ay] = points[i];
        const [bx, by] = points[i + 1];
        positions.push(ax, ay, lineZ, bx, by, lineZ);
        colors.push(r, g, b, r, g, b);
        importedLineOwners.push({ kind: 'object', objectId });
        if (overlayBuf) overlayBuf.push(ax, ay, lineZ, bx, by, lineZ);
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

    // Text-layer previews. Each TextLayer renders client-side into a
    // segment list cached by `text_preview`; the 2D canvas reads the
    // same cache. Drawn in the accent color so they read as "live
    // preview, not yet baked into the toolpath".
    if (project.textLayers.length > 0) {
      const previewC = cssColor('--accent', 0x4a8df0);
      for (const layer of project.textLayers) {
        const segs = previewSegmentsFor(layer.id);
        if (!segs || segs.length === 0) continue;
        for (const seg of segs) {
          const points = tessellate(seg);
          for (let i = 0; i < points.length - 1; i++) {
            const [ax, ay] = points[i];
            const [bx, by] = points[i + 1];
            positions.push(ax, ay, lineZ, bx, by, lineZ);
            colors.push(previewC.r, previewC.g, previewC.b, previewC.r, previewC.g, previewC.b);
            importedLineOwners.push({ kind: 'object', objectId: 0 });
          }
        }
      }
    }

    if (positions.length > 0) {
      importedLinesObject = buildFatLines(positions, colors);
      importedLinesObject.visible = wireVisible;
      geometryGroup.add(importedLinesObject);
    }
    // Selection set is now baked into the imported color attribute.
    appliedSelection = new Set(project.selectedObjects);
    updateSceneRadius(); // refresh sceneRadius before sizing dashes

    // Multi-op dashed overlays. For an object in N ops we lay N dashed
    // copies of its path, each in one op's color, with dashSize = L and
    // gapSize = (N-1)·L so op i's dashes occupy slot i of an N·L period
    // (dashOffset = -i·L). The slots tile the whole path → it reads as
    // consecutive colored bands A B C A B C, every assigned op visible.
    const lw = Math.max(0.5, project.settings.previewLineWidth);
    const dash = Math.max(0.3, sceneRadius * 0.04);
    const w0 = host?.clientWidth || 1;
    const h0 = host?.clientHeight || 1;
    for (const [objectId, pos] of overlayPosByObj) {
      if (pos.length === 0) continue;
      const ops = (objectToOps3d.get(objectId) ?? []).slice().sort((a, b) => a - b);
      const n = ops.length;
      if (n < 2) continue;
      for (let i = 0; i < n; i++) {
        const opId = ops[i];
        const [hh, ss, ll] = opSourceHsl(opId, opId === selOpId);
        const mat = new LineMaterial({
          color: new THREE.Color().setHSL(hh, ss, ll).getHex(),
          worldUnits: false,
          linewidth: lw + 1,
          dashed: true,
          dashSize: dash,
          gapSize: dash * (n - 1),
        });
        mat.dashOffset = -i * dash;
        mat.resolution.set(w0, h0);
        const geom = new LineSegmentsGeometry();
        geom.setPositions(new Float32Array(pos));
        const obj = new LineSegments2(geom, mat);
        obj.computeLineDistances();
        obj.renderOrder = 2; // sit on top of the base wireframe
        obj.visible = wireVisible;
        geometryGroup.add(obj);
        assignmentOverlayObjects.push(obj);
      }
    }
  }

  /// Generated toolpath wireframe. Rebuilds on `generated` /
  /// `operations` (op enable filter) only. Playhead fade + sim-warning
  /// tints mutate the color attribute in place after this.
  function rebuildToolpathGeometry() {
    if (!geometryGroup || !scene) return;
    if (toolpathLinesObject) {
      geometryGroup.remove(toolpathLinesObject);
      toolpathLinesObject.geometry.dispose();
      (toolpathLinesObject.material as THREE.Material).dispose();
      toolpathLinesObject = undefined;
    }
    if (toolpathArrowsObject) {
      geometryGroup.remove(toolpathArrowsObject);
      toolpathArrowsObject.geometry.dispose();
      (toolpathArrowsObject.material as THREE.Material).dispose();
      toolpathArrowsObject = undefined;
    }
    toolpathLineOwners = [];
    toolpathColors = [];
    appliedHead = -1;
    const gen = project.generated;
    if (!gen) {
      // Toolpath went away — the imported $effect also tracks
      // `project.generated` and will rebuild with the un-faded baseline.
      updateSceneRadius();
      return;
    }

    const positions: number[] = [];
    const colors: number[] = [];
    // Direction-arrow geometry — separate buffer so it doesn't
    // interfere with selectionDelta / playhead-fade range math on
    // the main toolpath buffer.
    const arrowPositions: number[] = [];
    const arrowColors: number[] = [];
    // 7iej.17: chevron geometry lives in scene3d/toolpath_buffers.ts (pure +
    // unit-tested); the buffer assembly + spacing bookkeeping stay here.
    const ARROW_PARAMS = {
      minLen: 1.0, // mm; shorter segments never get an arrow
      maxSize: 4.0, // mm; absolute cap on arrow size
      sizeFrac: 0.2, // arrow size relative to segment length
      halfWing: Math.tan((30 * Math.PI) / 180), // ±30° wings
    };
    // Arrow spacing is user-tunable (Settings → arrow density): higher
    // density packs arrows closer. density 0 ⇒ Infinity spacing ⇒ no
    // segment ever qualifies, so arrows are disabled.
    const ARROW_MIN_SPACING = arrowSpacingMm(project.settings.toolMoveArrowDensity);
    let lenSinceLastArrow = ARROW_MIN_SPACING; // emit on first qualifying segment
    const moveTints: Record<string, THREE.Color> = {
      rapid: cssColor('--toolpath-rapid', 0x35a2ff),
      cut: cssColor('--toolpath-cut', 0xff5555),
      plunge: cssColor('--toolpath-plunge', 0xffd23a),
      retract: cssColor('--toolpath-retract', 0x5fd06e),
      arc: cssColor('--toolpath-arc', 0xff8a3a),
    };
    // Per-op enable filter: disabling an op via OperationsList hides its
    // segments without forcing a re-Generate (matches the gcode panel's
    // commented-out chapter view).
    const disabledOpIds = new Set<number>();
    for (const o of project.operations) {
      if (!o.enabled) disabledOpIds.add(o.id);
    }
    const total = gen.toolpath.length;
    for (let i = 0; i < total; i++) {
      const seg = gen.toolpath[i];
      const opId = seg.op_id ?? 0;
      if (opId > 0 && disabledOpIds.has(opId)) continue;
      const moveTint = moveTints[seg.kind] ?? moveTints.cut;
      const opHue = opId === 0 ? 0.0 : opPalette(opId);
      const opCol = new THREE.Color().setHSL(opHue, 0.55, 0.5);
      const moveBoost =
        seg.kind === 'rapid' ? 0.5 : seg.kind === 'plunge' || seg.kind === 'retract' ? 0.85 : 1.15;
      const r = opId === 0 ? moveTint.r : opCol.r * moveBoost;
      const g = opId === 0 ? moveTint.g : opCol.g * moveBoost;
      const b = opId === 0 ? moveTint.b : opCol.b * moveBoost;
      const startVertex = positions.length / 3;
      positions.push(seg.from.x, seg.from.y, seg.from.z, seg.to.x, seg.to.y, seg.to.z);
      colors.push(r, g, b, r, g, b);
      toolpathLineOwners.push({ kind: 'toolpath', segIdx: i });
      toolpathColors.push({ start: startVertex, base: [r, g, b] });

      // Direction-arrow chevron at the segment midpoint when
      // qualifying. Rapids skip — feed direction matters only for
      // material-cutting moves. The cumulative-spacing guard
      // prevents arrow noise on dense raster pockets.
      const dx = seg.to.x - seg.from.x;
      const dy = seg.to.y - seg.from.y;
      const dz = seg.to.z - seg.from.z;
      const len = Math.sqrt(dx * dx + dy * dy + dz * dz);
      if (len > 0) lenSinceLastArrow += len;
      // Spacing + move-kind eligibility stays here (caller state); the
      // chevron geometry (incl. the per-segment minLen gate) is pure.
      const spacingOk = lenSinceLastArrow >= ARROW_MIN_SPACING && seg.kind !== 'rapid';
      const chev = spacingOk ? computeArrowChevron(seg.from, seg.to, ARROW_PARAMS) : null;
      if (chev) {
        const { mid, wing1, wing2 } = chev;
        arrowPositions.push(mid[0], mid[1], mid[2], wing1[0], wing1[1], wing1[2]);
        arrowPositions.push(mid[0], mid[1], mid[2], wing2[0], wing2[1], wing2[2]);
        // Slight brightness boost so arrows pop on top of the
        // base line.
        const ar = Math.min(1, r * 1.25);
        const ag = Math.min(1, g * 1.25);
        const ab = Math.min(1, b * 1.25);
        arrowColors.push(ar, ag, ab, ar, ag, ab, ar, ag, ab, ar, ag, ab);
        lenSinceLastArrow = 0;
      }
    }

    if (positions.length > 0) {
      toolpathLinesObject = buildFatLines(positions, colors);
      toolpathLinesObject.visible = wireVisible;
      geometryGroup.add(toolpathLinesObject);
    }
    if (arrowPositions.length > 0) {
      toolpathArrowsObject = buildFatLines(arrowPositions, arrowColors);
      toolpathArrowsObject.visible = wireVisible;
      // Render after the base line so the chevron sits on top.
      toolpathArrowsObject.renderOrder = 1;
      geometryGroup.add(toolpathArrowsObject);
    }
    updateSceneRadius();
    // Re-apply the past/future fade to the freshly-baked colors so the
    // playhead tint is correct even when no playhead change triggered
    // this rebuild.
    applyToolpathFade();
  }

  /// Bounding sphere across whichever line buffers exist. Used by
  /// fit-to-view and raycaster threshold scaling. Returns null when
  /// nothing's rendered yet.
  function combinedBoundingSphere(): THREE.Sphere | null {
    const spheres: THREE.Sphere[] = [];
    for (const obj of [importedLinesObject, toolpathLinesObject]) {
      if (!obj) continue;
      obj.geometry.computeBoundingSphere();
      if (obj.geometry.boundingSphere) spheres.push(obj.geometry.boundingSphere);
    }
    if (spheres.length === 0) return null;
    if (spheres.length === 1) return spheres[0].clone();
    // Two spheres: take a sphere covering both. Cheap approximation
    // (axis-aligned containment) — adequate for camera framing.
    const out = spheres[0].clone();
    for (let i = 1; i < spheres.length; i++) out.union(spheres[i]);
    return out;
  }

  /// Refresh `sceneRadius` (used for raycaster threshold) without
  /// touching the camera. Called from both rebuilds.
  function updateSceneRadius() {
    const sphere = combinedBoundingSphere();
    if (sphere) sceneRadius = Math.max(sphere.radius, 1);
  }

  /// Manual "Fit view" entry point (1ei2). Resets the one-shot workspace
  /// restore so the auto-fit isn't immediately overruled, then refits.
  /// Called by the toolbar button and (planned) the numpad-'.' shortcut.
  function requestFitView() {
    restoredFromWorkspace = true; // suppress the auto-restore inside fit
    fitCameraToScene();
  }

  /// Camera fit-to-view, run once when a new geometry source appears.
  /// Layer toggles / generates / op edits no longer reset the user's view.
  function fitCameraToScene() {
    if (!camera || !controls) return;
    const sphere = combinedBoundingSphere();
    if (!sphere) return;
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
    // bwt7: touch long-press arming. A single finger held still opens
    // the context menu; a second finger (pinch) cancels it and lets
    // OrbitControls zoom/pan.
    if (e.pointerType === 'touch') {
      lpPointers.add(e.pointerId);
      if (lpPointers.size >= 2) {
        cancelLongPress();
      } else {
        cancelLongPress();
        lpStart = { x: e.clientX, y: e.clientY };
        const sx = e.clientX;
        const sy = e.clientY;
        lpTimer = setTimeout(() => {
          lpTimer = null;
          lpStart = null;
          if (lpPointers.size === 1) openCtxMenuAt(sx, sy);
        }, LONG_PRESS_MS);
      }
    }
    // w5wx: remember the right-button press position so `onContextMenu`
    // can distinguish a tap (open the menu) from a right-drag pan.
    if (e.button === 2) {
      rightStart = { x: e.clientX, y: e.clientY };
      return;
    }
    // Any other press dismisses an open context menu (click-away).
    if (ctxMenu) ctxMenu = null;
    if (e.button !== 0) return;
    pointerStart = { x: e.clientX, y: e.clientY, t: performance.now() };
  }

  /// bwt7: cancel a pending long-press once the finger wanders (a
  /// one-finger rotate is a drag, not a hold). Registered as a passive
  /// pointermove listener alongside OrbitControls' own.
  function onPointerMoveLongPress(e: PointerEvent) {
    if (
      lpStart &&
      Math.hypot(e.clientX - lpStart.x, e.clientY - lpStart.y) > LONG_PRESS_MOVE_TOL_PX
    ) {
      cancelLongPress();
    }
  }

  /// bwt7: a cancelled pointer (e.g. the OS captured the gesture) just
  /// tears down transient press state — no pick, no menu.
  function onPointerCancel(e: PointerEvent) {
    if (e.pointerType === 'touch') lpPointers.delete(e.pointerId);
    cancelLongPress();
    pointerStart = null;
    rightStart = null;
  }

  /// w5wx: right-click → "New operation from selection" menu (parity with
  /// the 2D pane). Only opens on a right-click TAP; a right-drag (which
  /// OrbitControls uses to pan) moves past the 3px threshold and is left
  /// to pan without popping the menu. Always preventDefault so the
  /// browser's native menu never shows over the canvas.
  function onContextMenu(e: MouseEvent) {
    e.preventDefault();
    if (rightStart) {
      const moved = Math.hypot(e.clientX - rightStart.x, e.clientY - rightStart.y);
      rightStart = null;
      if (moved > 3) {
        ctxMenu = null;
        return;
      }
    }
    // Clamp so the menu (≈16rem wide) stays inside the viewport even on a
    // right-click near the right/bottom edge.
    openCtxMenuAt(e.clientX, e.clientY);
  }

  /// Mirror of EntityCanvas2D.pickFromCtx: build a new op whose source is
  /// the current 3D selection. `pocket_outside` gets the same frame +
  /// difference-combine pre-fill as the 2D path.
  function pickFromCtx(kind: PickerKind) {
    const sel = [...project.selectedObjects];
    if (sel.length === 0) {
      ctxMenu = null;
      return;
    }
    const label = `New ${PICKER_LABEL[kind]} from selection`;
    project.history.beginTransaction(label);
    try {
      if (kind === 'pocket_outside') {
        const endmill = project.tools.find((t) => t.kind === 'endmill') ?? project.tools[0];
        const toolDiameter = endmill?.diameter ?? 3;
        const op = project.addOperation('pocket');
        project.updateOperation(op.id, {
          name: 'Pocket Outside',
          toolId: endmill?.id ?? op.toolId,
          sourceLayers: null,
          sourceObjects: sel,
          sourceCombine: 'difference',
          frameShape: 'rectangle',
          framePaddingMm: 3 * toolDiameter,
          frameCornerRadiusMm: undefined,
        });
      } else {
        const op = project.addOperation(kind);
        project.updateOperation(op.id, {
          name: `${PICKER_LABEL[kind]} from selection`,
          sourceLayers: null,
          sourceObjects: sel,
        });
      }
      project.history.commitTransaction();
    } catch (err) {
      project.cancelTransaction();
      throw err;
    }
    onActivateSidebarPane?.('operations');
    ctxMenu = null;
  }

  function onPointerUp(e: PointerEvent) {
    // bwt7: release touch tracking + cancel a pending long-press (a
    // quick tap is not a hold).
    if (e.pointerType === 'touch') {
      lpPointers.delete(e.pointerId);
    }
    cancelLongPress();
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
    if (!camera || !renderer) return;
    const targets: LineSegments2[] = [];
    if (importedLinesObject) targets.push(importedLinesObject);
    if (toolpathLinesObject) targets.push(toolpathLinesObject);
    if (targets.length === 0) return;
    const rect = renderer.domElement.getBoundingClientRect();
    ndc.x = ((e.clientX - rect.left) / rect.width) * 2 - 1;
    ndc.y = -((e.clientY - rect.top) / rect.height) * 2 + 1;
    raycaster.setFromCamera(ndc, camera);
    // LineSegments2 raycasts in screen space against the line width;
    // the threshold (px) widens the pick corridor so thin lines stay
    // clickable.
    raycaster.params.Line2 = { threshold: 8 };
    const hits = raycaster.intersectObjects(targets, false);
    if (hits.length === 0) {
      if (!e.shiftKey) project.clearSelection();
      return;
    }
    const hit = hits[0];
    // LineSegments2 reports the picked segment as `faceIndex`; the owner
    // arrays hold one entry per segment, so it maps directly.
    const segIndex = hit.faceIndex ?? (hit.index != null ? Math.floor(hit.index / 2) : null);
    if (segIndex == null) return;
    // Resolve which owner array to consult based on which line object
    // produced the hit. Both are pickable; closer wins (Three's
    // intersectObjects sorts by distance).
    const owners = hit.object === importedLinesObject ? importedLineOwners : toolpathLineOwners;
    const owner = owners[segIndex];
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

  function aciColor(c: number): THREE.Color {
    // 7iej.12: shared palette + classification with the 2D canvas; the 3D
    // copy previously omitted ACI 9.
    const r = resolveAci(c);
    return r.kind === 'fixed' ? new THREE.Color(r.hex) : cssColor(r.token, r.fallback);
  }
</script>

<div class="scene" bind:this={host}>
  <button
    type="button"
    class="fit-btn"
    onclick={requestFitView}
    title="Fit view to scene"
    aria-label="Fit view to scene"
  >
    ⌖
  </button>
  {#if !project.transformedImport}
    <!-- Empty-state mirror of EntityCanvas2D's "Open a file" overlay.
         Before this, switching to 3D before loading anything showed a
         blank grid + axes with no affordance. The pointer-events:none
         keeps OrbitControls fully usable around it. -->
    <div class="empty-state" aria-hidden="true">
      <div class="empty-card">
        <div class="empty-glyph">⌗</div>
        <div class="empty-title">No drawing loaded</div>
        <div class="empty-sub">
          Open a DXF / SVG, drop a file onto the window, or pick a sample.
        </div>
      </div>
    </div>
  {/if}
  {#if ctxMenu}
    {@const hasObjsSelected = project.selectedObjects.size > 0}
    {#if hasObjsSelected}
      <div
        class="ctx-menu"
        style:left={`${ctxMenu.x}px`}
        style:top={`${ctxMenu.y}px`}
        role="menu"
        tabindex="-1"
        onkeydown={(e) => {
          if (e.key === 'Escape') ctxMenu = null;
        }}
      >
        <div class="ctx-header">New operation from selection</div>
        <OpKindPicker onPick={pickFromCtx} />
      </div>
    {:else}
      <div
        class="ctx-menu empty"
        style:left={`${ctxMenu.x}px`}
        style:top={`${ctxMenu.y}px`}
        role="menu"
      >
        <p class="ctx-hint">Click geometry to select objects, then add an operation from them.</p>
        <button type="button" onclick={() => (ctxMenu = null)}>Dismiss</button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .scene {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    background: var(--bg-app);
  }
  /* w5wx: right-click "New operation from selection" menu — visual twin
     of EntityCanvas2D's .ctx-menu so the two panes match. */
  .ctx-menu {
    position: absolute;
    min-width: 16rem;
    max-width: 22rem;
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px var(--shadow-modal);
    z-index: var(--z-floating);
    padding: 0.25rem;
  }
  .ctx-header {
    font-size: 0.68rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 0.25rem 0.45rem 0.3rem;
  }
  .ctx-menu.empty {
    padding: 0.4rem 0.55rem;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    min-width: 14rem;
  }
  .ctx-hint {
    margin: 0;
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .ctx-menu.empty button {
    align-self: flex-end;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.15rem 0.6rem;
    font-size: 0.74rem;
    cursor: pointer;
  }
  /* 1ei2: manual fit-view trigger. Same overlay style as EntityCanvas2D's
     help-btn so the two stack visually consistently across the 2D / 3D
     panes. */
  .fit-btn {
    position: absolute;
    top: 0.5rem;
    right: 0.5rem;
    width: 1.6rem;
    height: 1.6rem;
    border-radius: 50%;
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text-muted);
    cursor: pointer;
    font-size: 1rem;
    line-height: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    opacity: 0.7;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
    z-index: var(--z-anchor);
  }
  .fit-btn:hover,
  .fit-btn:focus-visible {
    opacity: 1;
    color: var(--text-strong);
  }
  /* Empty-state overlay shown when no drawing is loaded. Mirrors the
     2D pane's empty hint so the user gets the same affordance in
     either view. pointer-events:none lets OrbitControls keep working. */
  .empty-state {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    pointer-events: none;
  }
  .empty-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.4rem;
    padding: 1.2rem 2rem;
    color: var(--text-muted);
    text-align: center;
  }
  .empty-glyph {
    font-size: 2.4rem;
    color: var(--canvas-empty);
    line-height: 1;
  }
  .empty-title {
    font-size: 1.05rem;
    color: var(--text);
  }
  .empty-sub {
    font-size: 0.82rem;
    max-width: 22rem;
  }
</style>
