<script lang="ts">
  import { onMount, onDestroy, untrack } from 'svelte';
  import * as THREE from 'three';
  import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
  import { project, playheadToSegment } from '../state/project.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { HeightfieldDriver } from '../sim/driver';
  import { pixelsPerCell } from '../scene3d/lod';
  import type { BuilderContext, CssColor } from '../scene3d/builder';
  import { StockBoxBuilder } from '../scene3d/stock_box';
  import { WorkAreaBuilder } from '../scene3d/work_area';
  import { TabsBuilder } from '../scene3d/tabs';
  import { ApproachBuilder } from '../scene3d/approach';
  import { WarningMarkersBuilder } from '../scene3d/warning_markers';
  import { FixturesBuilder } from '../scene3d/fixtures';
  import { ToolGlyphBuilder } from '../scene3d/tool_glyph';
  import { ImportedGeometryBuilder } from '../scene3d/imported_geometry';
  import { ToolpathBuilder } from '../scene3d/toolpath';
  import { Picker } from '../scene3d/picker';
  import type { PickableLineBuilder } from '../scene3d/builder';
  import type { ToolpathSegment } from '../api/types';
  import type { ToolEntry } from '../state/project.svelte';
  import { previewVersion, requestPreview } from '../state/text_preview.svelte';
  import { consumeSelectHint } from '../state/ui-hints';
  import OpKindPicker, { PICKER_LABEL, type PickerKind } from './OpKindPicker.svelte';
  import { LONG_PRESS_MS, LONG_PRESS_MOVE_TOL_PX } from '../canvas/touch-gestures';

  interface Props {
    /// Mirrors EntityCanvas2D — after the right-click menu creates
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
  /// away IDEs still left the loop running.
  let pageVisible = true;
  let hostVisible = true;

  /// Render-on-demand flag. The animation loop calls `renderer.render()`
  /// only when something has visibly changed since the last frame —
  /// otherwise it just ticks `controls.update()` (cheap, needed for
  /// damping) and bails. Without this we burned 60 fps of GPU + CPU
  /// drawing the same static scene whenever the 3D pane was open.
  /// Anything that mutates the scene must call `requestRender()`.
  let needsRender = true;
  /// Bumped by applyTheme to re-trigger the line-buffer build effects (so a
  /// theme switch re-emits the imported + toolpath wireframes in the new
  /// tokens) WITHOUT making tabs/stock/fixtures/tool re-skin — matching the
  /// the behavior where applyTheme rebuilt only those two buffers.
  let themeVersion = $state(0);
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
    // Re-evaluate the heightfield LOD level whenever the camera
    // moves. setLodHint is a no-op when the recommended level matches
    // the current one (the common case during smooth orbiting), so the
    // per-event cost is just a few divisions.
    updateHeightfieldLod();
  }

  /// Ask the heightfield driver to pick an LOD level for the
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
    driver.setLodHint(ppc, project.data.settings.maxRenderTriangles);
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
      // Refresh fat-line resolution from the LIVE renderer size
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

  // The imported wireframe + the generated toolpath wireframe each live in
  // their own builder (ImportedGeometryBuilder / ToolpathBuilder);
  // both expose a pickable LineSegments2 + owner array for handlePick and a
  // bounding sphere for fit-to-view.
  let sceneRadius = 100;

  /// Heightfield-based cutting simulator. Lazy-loaded on first need
  /// (the WASM module is async). Owns its own group inside `scene`;
  /// shows / hides per project.data.settings.previewMode.
  let driver: HeightfieldDriver | undefined;
  let driverInitPromise: Promise<void> | undefined;
  /// Cache the inputs that trigger a sim rebuild (footprint or grid
  /// resolution change) so we don't tear it down for cosmetic changes.
  let lastSimKey = '';
  // Click vs. drag: OrbitControls owns pointermove so we only treat a
  // pointerup as a click when the user barely moved the cursor between
  // down and up. 3px / 400ms is the same threshold the 2D pane uses.
  let pointerStart: { x: number; y: number; t: number } | null = null;
  // Right-click context menu. `rightStart` records the right-button
  // press so the `contextmenu` handler can tell a tap (open the menu)
  // from an OrbitControls right-drag pan (don't). `ctxMenu` is the menu's
  // host-relative position when open.
  let rightStart: { x: number; y: number } | null = null;
  import { clampPopup } from '../canvas/clamp-popup';
  let ctxMenu = $state<{ x: number; y: number } | null>(null);

  /// Touch long-press → context menu (parity with the 2D pane and
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
    // With nothing selected the menu is just the "click geometry to
    // select" hint — show it at most once per session (shared with the
    // 2D pane) instead of nagging on every empty right-click.
    if (project.sel.selectedObjects.size === 0 && !consumeSelectHint()) return;
    const rect = host.getBoundingClientRect();
    const x = Math.max(4, Math.min(clientX - rect.left, host.clientWidth - 260));
    const y = Math.max(4, Math.min(clientY - rect.top, host.clientHeight - 220));
    ctxMenu = { x, y };
  }
  const picker = new Picker();
  const resVec = new THREE.Vector2();

  function cssVar(name: string, fallback: string): string {
    if (!host) return fallback;
    const v = getComputedStyle(host).getPropertyValue(name).trim();
    return v || fallback;
  }
  function cssColor(name: string, fallback: number): THREE.Color {
    return new THREE.Color(cssVar(name, '') || fallback);
  }
  /// Push the canvas pixel size into the line builders' fat-line materials'
  /// `resolution` uniform (they render wrong / invisible otherwise).
  function updateLineResolution(w: number, h: number) {
    importedBuilder?.setResolution(w, h);
    toolpathBuilder?.setResolution(w, h);
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

    // Builders own their own groups inside the scene. The host's
    // $effects read project fields and call builder.build(...).
    const builderCtx: BuilderContext = { scene, requestRender };
    const css: CssColor = cssColor;
    importedBuilder = new ImportedGeometryBuilder(builderCtx, css);
    toolpathBuilder = new ToolpathBuilder(builderCtx, css);
    toolGlyphBuilder = new ToolGlyphBuilder(builderCtx, css);
    stockBuilder = new StockBoxBuilder(builderCtx, css);
    workAreaBuilder = new WorkAreaBuilder(builderCtx, css);
    tabsBuilder = new TabsBuilder(builderCtx, css);
    approachBuilder = new ApproachBuilder(builderCtx, css);
    warningMarkersBuilder = new WarningMarkersBuilder(builderCtx, css);
    fixturesBuilder = new FixturesBuilder(builderCtx, css);

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
    toolGlyphBuilder?.refreshTipColors();
  });

  function applyTheme() {
    if (!scene) return;
    toolGlyphBuilder?.refreshTipColors();
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
    // After grid swap, re-emit both line buffers so the imported drawing +
    // toolpath wireframe sit cleanly on top of the new grid. Both build
    // effects depend on themeVersion, so bumping it re-runs them with the
    // new tokens (without re-skinning tabs/stock/fixtures/tool).
    themeVersion++;
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
    // Builders own + free their groups.
    toolGlyphBuilder?.dispose();
    stockBuilder?.dispose();
    workAreaBuilder?.dispose();
    tabsBuilder?.dispose();
    approachBuilder?.dispose();
    warningMarkersBuilder?.dispose();
    fixturesBuilder?.dispose();
    // renderer.dispose() frees the GL context but does NOT walk the
    // scene graph, so each builder frees its own group's geometry/material
    // (the largest buffers — imported wireframe + toolpath lines + arrows)
    // or they leak a full toolpath on every 2D↔3D pane swap (Scene3D
    // unmounts on each swap).
    importedBuilder?.dispose();
    toolpathBuilder?.dispose();
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
  // Per-concern effects. A single mega-effect over all seven project.*
  // fields would rebuild geometry+tabs+stock+fixtures on every change —
  // e.g. toggling a fixture's color would rebuild the toolpath
  // wireframe. Split so each builder only refires when its own inputs
  // change.

  // Geometry wireframe: imported drawing, layer toggles, generated
  // toolpath, and the op set (color stamps follow op_id).
  // Two effects, two buffers (see LineSegments declaration above for
  // rationale). Each rebuild is independent — editing an op does not
  // tear down the imported-geometry buffer, and toggling a layer does
  // not tear down the toolpath buffer.

  // Only the SET of text layers (add / remove) changes what this rebuild
  // draws — glyph segments are origin-baked + cached and read by id, so a
  // text-origin drag hands `textLayers` a new array reference without
  // changing any output (the cache stays stale until the debounced render
  // bumps `previewVersion`). Keying on the id set instead of the raw array
  // stops a full imported-geometry teardown/rebuild on every pointermove
  // of a text-origin drag.
  const textLayerIdKey = $derived(project.data.textLayers.map((l) => l.id).join(','));

  // Imported drawing + text-layer previews. textLayerIdKey (not the raw
  // textLayers array) keys the text-layer dep so a text-origin drag doesn't
  // teardown/rebuild the whole buffer every pointermove. themeVersion
  // re-runs this on theme switch.
  $effect(() => {
    void textLayerIdKey;
    void previewVersion.v;
    void themeVersion;
    importedBuilder?.build({
      data: project.transformedImport,
      visibleLayers: project.data.visibleLayers,
      operations: project.data.operations, // op-source assignments drive the tint
      selectedOpId: project.sel.selectedOpId, // selected op renders emphasized
      selectedObjects: project.sel.selectedObjects,
      textLayers: project.data.textLayers,
      hasGenerated: !!project.gen.generated, // affects fade for non-selected imports
      previewMode: project.data.settings.previewMode, // contrast-against-stock color
      edgeColor: project.data.settings.edgeColor,
      lineWidth: project.data.settings.previewLineWidth,
      wireVisible,
      width: host?.clientWidth || 1,
      height: host?.clientHeight || 1,
      sceneRadius,
    });
    updateSceneRadius(); // refresh combined radius now that imported is rebuilt
    requestRender();
  });

  // Generated toolpath wireframe (re-emitted when a new pipeline run
  // resolves or the user toggles an op enable / disable). themeVersion
  // re-runs it on a theme switch.
  $effect(() => {
    void themeVersion;
    // build() seeds the playhead fade on the freshly-baked colors,
    // but the fade-seed (playhead + the arc-length tables it maps through)
    // must NOT make this build effect depend on the playhead — otherwise
    // every scrub/playback tick rebuilds the entire toolpath geometry,
    // defeating the in-place applyFade() fast path. cum/total only change
    // when `generated` changes (still tracked below), so reading all three
    // untracked is safe; the separate playhead + warning effects drive the
    // in-place fade during a scrub.
    const fade = untrack(() => ({
      playhead: project.playhead,
      cumLen: project.gen.toolpathCumLen,
      totalLen: project.gen.toolpathTotalLen,
    }));
    // The raster heatmap re-derives S from the source
    // brightness + placement, so refresh when a source changes.
    toolpathBuilder?.build({
      generated: project.gen.generated,
      operations: project.data.operations,
      reliefSources: project.data.reliefSources,
      arrowDensity: project.data.settings.toolMoveArrowDensity,
      lineWidth: project.data.settings.previewLineWidth,
      width: host?.clientWidth || 1,
      height: host?.clientHeight || 1,
      wireVisible,
      playhead: fade.playhead,
      cumLen: fade.cumLen,
      totalLen: fade.totalLen,
    });
    updateSceneRadius();
    requestRender();
  });

  // Fat-line thickness: update the live materials in place rather
  // than rebuilding geometry, so dragging the slider is cheap.
  $effect(() => {
    const lw = Math.max(0.5, project.data.settings.previewLineWidth);
    importedBuilder?.setLineWidth(lw);
    toolpathBuilder?.setLineWidth(lw);
    requestRender();
  });

  // Keep the text-preview cache warm independent of the 2D canvas — the
  // user might never visit 2D and still expects text to show in 3D.
  $effect(() => {
    for (const layer of project.data.textLayers) {
      requestPreview(layer);
    }
  });

  // Tab markers: per-op tab placements + tabMode.
  $effect(() => {
    tabsBuilder?.build({
      imported: project.transformedImport,
      operations: project.data.operations,
    });
    requestRender();
  });

  // Approach-point needle for the currently selected op. The
  // marker shows up only when the user is looking at the op that
  // carries it (driven by selectedOpId) — otherwise the 3D view
  // stays uncluttered.
  $effect(() => {
    approachBuilder?.build({
      selectedOpId: project.sel.selectedOpId,
      operations: project.data.operations,
      fastMoveZ: project.data.machine.fastMoveZ,
    });
    requestRender();
  });

  // Stock bbox visual: stock config + toggle. Doesn't touch the
  // toolpath wireframe.
  $effect(() => {
    stockBuilder?.build({
      stock: project.data.stock,
      showStockBox: project.data.settings.showStockBox,
      imported: project.transformedImport,
      workArea: project.data.machine.workArea,
    });
    requestRender();
  });

  // Machine work-area wireframe — the always-visible envelope the user
  // can't move the cutter outside of. Dotted-style edges so it reads
  // as "limit, not solid", and dim opacity so it sits in the back of
  // the scene without competing with the toolpath.
  $effect(() => {
    workAreaBuilder?.build({ workArea: project.data.machine.workArea });
    requestRender();
  });

  // Fixture meshes: fixtures themselves + selection / playback flash.
  // No reason to rebuild the toolpath when the user clicks a fixture.
  $effect(() => {
    fixturesBuilder?.build({
      fixtures: project.data.fixtures,
      selectedFixtureId: project.sel.selectedFixtureId,
    });
    requestRender();
  });

  /// Auto fit-to-view. Refit the camera to frame the whole scene when
  /// the visible content GROWS or a fresh toolpath is generated:
  ///   • a drawing is imported (imports.length grows), or
  ///   • the total layer set grows — imported layers ∪ text layers — so
  ///     newly-added geometry frames itself, while toggling an EXISTING
  ///     layer's visibility leaves the count unchanged and so does NOT
  ///     refit (preserves the earlier "don't fight my camera" feedback);
  ///   • every Generate (`generatedVersion` increments only on a real
  ///     generate, never on a clear — see project.svelte.ts).
  /// fileTransform tweaks, op edits, and selection derive new references
  /// but leave these counts/version alone, so they never override the
  /// user's chosen camera. Removing content (fewer imports/layers) also
  /// keeps the camera put. Counts (not the derived `transformedImport`
  /// reference) give the right invalidation profile.
  ///
  /// Defined AFTER the geometry-rebuild effects above so
  /// combinedBoundingSphere() sees the freshly-rebuilt line buffers.
  /// On (re)mount the lasts start at -1 → one fit runs, which
  /// maybeRestoreSavedCamera() then supersedes with the saved workspace
  /// pose (one-shot), so 2D↔3D pane swaps don't jar the user's view.
  let lastImportCount = -1;
  let lastLayerCount = -1;
  let lastGenVersion = -1;
  $effect(() => {
    const importCount = project.data.imports.length;
    const layerCount =
      (project.transformedImport?.layers.length ?? 0) + project.data.textLayers.length;
    const genVersion = project.gen.generatedVersion;
    const grew = importCount > lastImportCount || layerCount > lastLayerCount;
    const regenerated = genVersion !== lastGenVersion;
    lastImportCount = importCount;
    lastLayerCount = layerCount;
    lastGenVersion = genVersion;
    if (grew || regenerated) fitCameraToScene();
  });

  /// Selection-only fast path: mutate the color attribute in place
  /// instead of rebuilding the entire LineSegments mesh on every click.
  /// Falls through to a full rebuild only if the geometry is missing
  /// (e.g. before the first rebuild has run).
  $effect(() => {
    const sel = project.sel.selectedObjects;
    if (!importedBuilder?.pickable) {
      // Geometry hasn't been built yet; the next build picks up the current
      // selection naturally.
      return;
    }
    importedBuilder.applySelection(sel);
    requestRender();
  });

  $effect(() => {
    void project.data.machine; // mode drives the cutter shape; whole-machine dep
    toolGlyphBuilder?.build({
      generated: project.gen.generated,
      playhead: project.playhead,
      cumLen: project.gen.toolpathCumLen,
      totalLen: project.gen.toolpathTotalLen,
      operations: project.data.operations, // op→tool assignment drives the cutter
      selectedOpId: project.sel.selectedOpId,
      tools: project.data.tools,
      machineMode: project.data.machine.mode,
    });
    toolpathBuilder?.applyFade(
      project.playhead,
      project.gen.toolpathCumLen,
      project.gen.toolpathTotalLen,
    );
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
  const wireVisible = $derived(project.data.settings.previewMode !== 'solid');

  /// Build a per-segment tool resolver for the sim: each toolpath segment
  /// is carved with ITS op's tool (looked up by op_id), so a multi-op
  /// program (e.g. endmill profile then v-bit chamfer) carves each part
  /// with the correct cutter cross-section instead of one tool for all.
  function toolForSegment(segs: ToolpathSegment[]): (i: number) => ToolEntry {
    const byOp = new Map<number, ToolEntry>();
    for (const op of project.data.operations) {
      const t = project.data.tools.find((tt) => tt.id === op.toolId);
      if (t) byOp.set(op.id, t);
    }
    const fallback = project.data.tools[0];
    return (i) => byOp.get(segs[i]?.op_id ?? -1) ?? fallback;
  }

  $effect(() => {
    if (!scene) return;
    const settings = project.data.settings;
    // Wire-mesh visibility tracks the preview mode: wireframe / both
    // show the toolpath + imported lines; solid hides them in favor of
    // the heightfield carved-stock mesh. wireVisible is a $derived at
    // module scope so the rebuild functions see the same value.
    importedBuilder?.setWireVisible(wireVisible);
    toolpathBuilder?.setWireVisible(wireVisible);
    if (settings.previewMode === 'wireframe') {
      driver?.setVisible(false);
      requestRender();
      return;
    }
    const imported = project.transformedImport;
    const generated = project.gen.generated;
    const firstOp = project.data.operations[0];
    const tool =
      project.data.tools.find((t) => t.id === (firstOp?.toolId ?? 0)) ?? project.data.tools[0];
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
    const fixturesKey = project.data.fixtures
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
      stock: project.data.stock,
      tool_id: tool.id,
      tool_dia: tool.diameter,
      cellRes,
      maxCells: settings.maxSimulationCells,
      gen_id: project.gen.generatedVersion,
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
            stock: project.data.stock,
            settings,
            fixtures: project.data.fixtures,
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
            project.gen.toolpathCumLen,
            project.gen.toolpathTotalLen,
          );
          // Select the right LOD level for the current camera
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
    const generated = project.gen.generated;
    if (!generated || project.data.tools.length === 0) return;
    driver.advanceTo(
      project.playhead,
      generated.toolpath,
      toolForSegment(generated.toolpath),
      project.gen.toolpathCumLen,
      project.gen.toolpathTotalLen,
      // Pass the user's exact-rewind preference through to the
      // driver. Default false leaves the heightfield untouched on
      // backstep (deepest-ever state retained); true triggers the
      // reset + forward-replay path.
      project.data.settings.exactSimRewind,
    );
  });

  /// Live-apply cosmetic settings (color / opacity).
  $effect(() => {
    void project.data.settings.solidColor;
    void project.data.settings.solidOpacity;
    void project.data.settings.edgeColor;
    void project.data.settings.edgeOpacity;
    driver?.applyStyle({
      solidColor: project.data.settings.solidColor,
      solidOpacity: project.data.settings.solidOpacity,
      edgeColor: project.data.settings.edgeColor,
      edgeOpacity: project.data.settings.edgeOpacity,
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

  // Sim-warning tints: rebuild the toolpath's per-segment override map and
  // re-apply the fade (warnings can repaint any past/future segment).
  $effect(() => {
    toolpathBuilder?.setWarnings(project.gen.simDiagnostics?.warnings ?? []);
    toolpathBuilder?.applyFade(
      project.playhead,
      project.gen.toolpathCumLen,
      project.gen.toolpathTotalLen,
    );
    requestRender();
  });

  // Marker builders: each owns its THREE.Group and rebuilds from
  // plain data the effects below hand it. Instantiated in onMount once the
  // scene exists.
  let stockBuilder: StockBoxBuilder | undefined;
  let workAreaBuilder: WorkAreaBuilder | undefined;
  let tabsBuilder: TabsBuilder | undefined;
  let approachBuilder: ApproachBuilder | undefined;
  let warningMarkersBuilder: WarningMarkersBuilder | undefined;
  let fixturesBuilder: FixturesBuilder | undefined;
  let toolGlyphBuilder: ToolGlyphBuilder | undefined;
  let importedBuilder: ImportedGeometryBuilder | undefined;
  let toolpathBuilder: ToolpathBuilder | undefined;

  $effect(() => {
    void project.gen.simDiagnostics;
    warningMarkersBuilder?.build({
      warnings: project.gen.simDiagnostics?.warnings ?? [],
      toolpath: project.gen.generated?.toolpath,
      sceneRadius,
    });
    requestRender();
  });

  /// Flash any fixture whose collision warning's segment is within +-2
  /// segments of the current playhead position. Re-applies the in-place
  /// color override on every playhead tick — cheap (one .color.set per
  /// fixture).
  $effect(() => {
    void project.playhead;
    void project.gen.simDiagnostics;
    void project.data.fixtures;
    const warnings = project.gen.simDiagnostics?.warnings ?? [];
    const collisions = warnings.filter((w) => w.kind === 'fixture_collision');
    const next = new Set<number>();
    if (collisions.length > 0) {
      const { segIdx } = playheadToSegment(
        project.playhead,
        project.gen.toolpathCumLen,
        project.gen.toolpathTotalLen,
      );
      const window = 2;
      for (const w of collisions) {
        if (w.kind !== 'fixture_collision') continue;
        if (Math.abs(w.segment_idx - segIdx) <= window) {
          next.add(w.fixture_id);
        }
      }
    }
    // FixturesBuilder owns the flashing set + materials; it returns true
    // only when the set actually changed, so we render only then.
    if (fixturesBuilder?.flash(next)) requestRender();
  });

  /// Bounding sphere across whichever line buffers exist. Used by
  /// fit-to-view and raycaster threshold scaling. Returns null when
  /// nothing's rendered yet.
  function combinedBoundingSphere(): THREE.Sphere | null {
    const spheres: THREE.Sphere[] = [];
    const imp = importedBuilder?.boundingSphere();
    if (imp) spheres.push(imp);
    const tp = toolpathBuilder?.boundingSphere();
    if (tp) spheres.push(tp);
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

  /// Manual "Fit view" entry point. Resets the one-shot workspace
  /// restore so the auto-fit isn't immediately overruled, then refits.
  /// Called by the toolbar button and (planned) the numpad-'.' shortcut.
  function requestFitView() {
    restoredFromWorkspace = true; // suppress the auto-restore inside fit
    fitCameraToScene();
  }

  /// Camera fit-to-view. Driven by the auto-fit effect above (content
  /// grew or a fresh Generate) and the manual "Fit view" button. Layer
  /// visibility toggles, fileTransform tweaks, and op edits do NOT call
  /// this, so they never reset the user's chosen camera.
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
    // Touch long-press arming. A single finger held still opens
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
    // Remember the right-button press position so `onContextMenu`
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

  /// Cancel a pending long-press once the finger wanders (a
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

  /// A cancelled pointer (e.g. the OS captured the gesture) just
  /// tears down transient press state — no pick, no menu.
  function onPointerCancel(e: PointerEvent) {
    if (e.pointerType === 'touch') lpPointers.delete(e.pointerId);
    cancelLongPress();
    pointerStart = null;
    rightStart = null;
  }

  /// Right-click → "New operation from selection" menu (parity with
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
    const sel = [...project.sel.selectedObjects];
    if (sel.length === 0) {
      ctxMenu = null;
      return;
    }
    const label = `New ${PICKER_LABEL[kind]} from selection`;
    project.history.beginTransaction(label);
    try {
      if (kind === 'pocket_outside') {
        const endmill =
          project.data.tools.find((t) => t.kind === 'endmill') ?? project.data.tools[0];
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
    // Release touch tracking + cancel a pending long-press (a
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

  /// Single-click pick: cast a ray through the cursor against the line
  /// builders' buffers and act on the closest hit. Imported geometry hits
  /// drive object selection (mirrors the 2D pane); toolpath hits scrub the
  /// playhead so the gcode panel scrolls + highlights the matching line.
  /// The raycast + owner resolution live in the Picker; the store-side
  /// actions (selection / playhead) stay here.
  function handlePick(e: PointerEvent) {
    if (!camera || !renderer) return;
    const builders: PickableLineBuilder[] = [];
    if (importedBuilder) builders.push(importedBuilder);
    if (toolpathBuilder) builders.push(toolpathBuilder);
    const result = picker.pick({
      clientX: e.clientX,
      clientY: e.clientY,
      rect: renderer.domElement.getBoundingClientRect(),
      camera,
      builders,
    });
    if (result.kind === 'ignore') return;
    if (result.kind === 'clear') {
      if (!e.shiftKey) project.clearSelection();
      return;
    }
    const owner = result.owner;
    if (owner.kind === 'object') {
      if (owner.objectId > 0) project.toggleObject(owner.objectId, e.shiftKey);
      else if (!e.shiftKey) project.clearSelection();
    } else {
      // Set playhead so the arc-length mapping lands at the end of the
      // picked segment (so the cutter sits there and gcode-panel scrolls
      // to the matching line).
      const cum = project.gen.toolpathCumLen;
      const total = project.gen.toolpathTotalLen;
      const segs = project.gen.generated?.toolpath.length ?? 0;
      if (cum && total > 0 && owner.segIdx >= 0 && owner.segIdx < cum.length) {
        project.playhead = Math.min(1, cum[owner.segIdx] / total);
      } else if (segs > 0) {
        project.playhead = (owner.segIdx + 1) / segs;
      }
    }
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
        <div class="empty-sub">Open a DXF / SVG, drop a file onto the window, or add text.</div>
      </div>
    </div>
  {/if}
  {#if ctxMenu}
    {@const hasObjsSelected = project.sel.selectedObjects.size > 0}
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
        use:clampPopup={ctxMenu}
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
  /* Right-click "New operation from selection" menu — visual twin
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
  /* Manual fit-view trigger. Same overlay style as EntityCanvas2D's
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
    /* Above the gesture catch layers (pull-to-refresh strip + EdgeSwipeNav
       edge zones at --z-floating) so the tap reaches the button, matching
       the 2D pane's fix. */
    z-index: calc(var(--z-floating) + 2);
  }
  .fit-btn:hover,
  .fit-btn:focus-visible {
    opacity: 1;
    color: var(--text-strong);
  }
  /* Touch: enlarge to a ~40px target and move clear of the pull-refresh
     strip (top 28px) and the 22px EdgeSwipeNav right edge zone, so the
     whole button is tappable (matches EntityCanvas2D). */
  @media (pointer: coarse) {
    .fit-btn {
      top: 2.4rem;
      right: 1.75rem;
      width: 2.5rem;
      height: 2.5rem;
      opacity: 0.85;
    }
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
