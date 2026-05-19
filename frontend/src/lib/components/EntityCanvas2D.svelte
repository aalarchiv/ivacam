<script lang="ts">
  import { onMount } from 'svelte';
  import { project, isContourOp } from '../state/project.svelte';
  import {
    buildObjectPolylines,
    polylineAtT,
    polylineProject,
    vertexAndMidpointTs,
    type ObjectPolyline,
  } from '../cam/tabs';
  import type { Segment } from '../api/types';
  import {
    bboxOfSegments,
    clamp,
    distanceToSegment,
    pointInPolygon,
    projectOntoSegment,
  } from '../canvas/selection-geometry';
  import {
    buildHitIndex as buildHitIndexPure,
    queryHit,
    type HitIndex,
  } from '../canvas/spatial-index';
  import { fixtureAt } from '../canvas/fixture-hit';
  import {
    DEFAULT_OSNAP_SETTINGS,
    findOSnap,
    precomputeOSnapTargets,
    type OSnapCandidate,
    type OSnapTargets,
  } from '../canvas/osnap';
  import OpKindPicker, { PICKER_LABEL, type PickerKind } from './OpKindPicker.svelte';
  import {
    previewSegmentsFor,
    previewVersion,
    requestPreview,
  } from '../state/text_preview.svelte';

  interface Props {
    onShowHelp?: () => void;
  }
  let { onShowHelp }: Props = $props();

  // AutoCAD ACI palette. ACI 7 means "white in dark mode, black in light" —
  // this is exactly how AutoCAD itself renders it. We resolve it at draw
  // time from the active theme.
  const ACI_FIXED: Record<number, string> = {
    1: '#ff0000',
    2: '#ffff00',
    3: '#00ff00',
    4: '#00ffff',
    5: '#0000ff',
    6: '#ff00ff',
    9: '#808080',
  };

  let canvas: HTMLCanvasElement;
  /// Stacked overlay canvas for state-bearing repaints (selection halos,
  /// hover halo, ghost tab, approach point, box-select rect, fixtures,
  /// tabs, OSnap glyph). pointer-events: none in CSS so the bg canvas
  /// keeps receiving input. Splits the per-frame work so hover and
  /// selection don't repaint the (often huge) imported geometry layer.
  let canvasOverlay: HTMLCanvasElement;
  let container: HTMLDivElement;

  /// Cached resolved theme colors. `themeVar` was previously calling
  /// `getComputedStyle(container).getPropertyValue(name)` on every
  /// lookup, which fires a synchronous style recalc — and `draw()`
  /// invokes it 15-20× per frame. We memoise per CSS var until the
  /// theme observer (onMount) bumps `themeCacheToken` to invalidate.
  let themeCache = new Map<string, string>();
  let themeCacheToken = 0;
  function themeVar(name: string, fallback: string): string {
    if (!container) return fallback;
    const cached = themeCache.get(name);
    if (cached !== undefined) return cached;
    const v = getComputedStyle(container).getPropertyValue(name).trim() || fallback;
    themeCache.set(name, v);
    return v;
  }
  function resetThemeCache() {
    themeCache = new Map();
    themeCacheToken++;
  }

  /// Trigger both canvas layers — used by resize / theme / mount paths
  /// where the right answer is "repaint everything". The $effect blocks
  /// drive per-state-change repaints; this helper is for non-reactive
  /// triggers.
  function drawBoth() {
    drawBackground();
    drawOverlay();
  }

  onMount(() => {
    const ro = new ResizeObserver(() => drawBoth());
    ro.observe(container);
    drawBoth();
    // Re-paint when the user toggles their OS theme or picks a manual one.
    const mql = window.matchMedia('(prefers-color-scheme: light)');
    const onChange = () => {
      resetThemeCache();
      drawBoth();
    };
    mql.addEventListener('change', onChange);
    // Diff the data-theme value before redrawing — MutationObserver fires
    // on every attribute write, including same-value writes. draw() is
    // non-trivial for big imports.
    let lastTheme = document.documentElement.dataset.theme ?? '';
    const themeMo = new MutationObserver(() => {
      const cur = document.documentElement.dataset.theme ?? '';
      if (cur === lastTheme) return;
      lastTheme = cur;
      resetThemeCache();
      drawBoth();
    });
    themeMo.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
    return () => {
      ro.disconnect();
      mql.removeEventListener('change', onChange);
      themeMo.disconnect();
    };
  });

  // Two-effect canvas paint (split bd: 'EntityCanvas2D draw effect').
  //
  // The bg canvas paints the heavy static layer: imported geometry in
  // base layer color, text-layer previews in base color, regions, grid,
  // axes, work-area. Repaints only on data / layout / zoom / pan / theme
  // changes — NEVER on hover or selection.
  //
  // The overlay canvas paints state-bearing items on top: selection /
  // hover / op-assignment halos, ghost tab, approach point, fixtures,
  // tab markers, box-select rect, selected-text-layer highlight, OSnap
  // glyphs. Hover and selection only retouch this layer.

  $effect(() => {
    void project.transformedImport;
    void project.visibleLayers;
    void project.regionsVisible;
    void project.generated;
    void project.textLayers;
    void project.selectedTextLayerId;
    void previewVersion.v;
    void project.machine.workArea;
    void project.stock;
    void userZoom;
    void userPanX;
    void userPanY;
    drawBackground();
  });

  $effect(() => {
    void project.transformedImport;
    void project.visibleLayers;
    void project.selectedObjects;
    void project.operations;
    void project.selectedOpId;
    void project.fixtures;
    void project.selectedFixtureId;
    void project.selectedTextLayerId;
    void hoverIdx;
    void ghostTab;
    void boxSelect;
    void userZoom;
    void userPanX;
    void userPanY;
    drawOverlay();
  });

  // Keep the live-preview cache warm. Loops every text layer and asks
  // for a render — the helper deduplicates by content hash and
  // debounces, so this is cheap when nothing changed.
  $effect(() => {
    for (const layer of project.textLayers) {
      requestPreview(layer);
    }
  });

  /// Selected-op-driven tab placement mode (rt1.10). When the user
  /// has a profile / pocket op selected with `tabMode` === manual or
  /// mixed, the canvas behaves as a tab-placement surface: hover
  /// shows a ghost tab; click toggles a placement.
  const selectedOp = $derived(
    project.selectedOpId == null
      ? null
      : (project.operations.find((o) => o.id === project.selectedOpId) ?? null),
  );
  const tabPlacementActive = $derived(
    !!selectedOp &&
      (selectedOp.kind === 'profile' || selectedOp.kind === 'pocket') &&
      (selectedOp.tabMode?.kind === 'manual' || selectedOp.tabMode?.kind === 'mixed'),
  );
  /// Ghost tab while hovering the contour in placement mode. The
  /// `snap` field describes which secondary snap target the cursor
  /// landed on so the renderer can flash a small dot.
  let ghostTab = $state<{
    x: number;
    y: number;
    objectId: number;
    t: number;
    snap: 'contour' | 'vertex' | 'midpoint' | 'existing';
  } | null>(null);
  /// Track Alt-held state across the gesture (1q3) — when true, snap
  /// to anything except the bare contour projection is disabled,
  /// matching the CAD-convention escape hatch.
  let altDown = $state(false);

  /// Track Shift-held state for the approach-point picker (n79). When
  /// true, snap-to-vertex is disabled — the user is asking for a
  /// free-form pick anywhere in the canvas.
  let shiftDown = $state(false);

  /// Approach-point picker (n79). Active when project.pickMode is
  /// `{ kind: 'approach-point', opId: <selected op id> }`. Cursor
  /// becomes a crosshair, a preview marker tracks the mouse (snapped
  /// to source-object vertices unless Shift is held), and a click
  /// commits the point to `op.approachPoint` while staying in pick
  /// mode (sticky — ESC exits).
  const approachPickActive = $derived(
    project.pickMode?.kind === 'approach-point' &&
      project.pickMode.opId === selectedOp?.id,
  );

  /// Live preview state while picking. `lastTransform`-relative data
  /// coords; `snap` carries the snap-kind classification so the
  /// renderer can paint the matching glyph (square / triangle / X / +).
  let approachPreview = $state<{
    x: number;
    y: number;
    snap: OSnapCandidate['kind'] | null;
  } | null>(null);

  /// Drag state for repositioning an already-placed approach marker
  /// (Option C: hybrid pick + draggable). Captured on pointerdown
  /// inside the marker's hit circle; released on pointerup.
  let approachDrag = $state<{ opId: number; pointerId: number } | null>(null);

  /// Precomputed OSnap target collection. Rebuilt only when the
  /// imported geometry changes — never per pointermove. (64p.)
  const osnapTargets = $derived<OSnapTargets>(
    approachPickActive || approachDrag != null
      ? precomputeOSnapTargets(project.transformedImport)
      : { endpoints: [], midpoints: [], intersections: [], centers: [] },
  );

  /// OSnap settings. TODO: thread through `project.settings` so users
  /// can toggle per-kind from the Settings dialog. Today the defaults
  /// (all CAD-feature kinds on, grid off) match what most users want.
  const osnapSettings = DEFAULT_OSNAP_SETTINGS;

  // Mouse → segment hit testing. We project each segment to canvas space
  // and pick the nearest one within `HIT_PIXEL_TOL`.
  const HIT_PIXEL_TOL = 8;
  let hoverIdx = $state<number | null>(null);
  /// 7tp5: cursor world coordinates for the on-canvas HUD. Updated on
  /// every pointermove (regardless of modal mode); cleared on
  /// pointerleave. null until the first import + first move.
  let cursorXY = $state<{ x: number; y: number } | null>(null);
  let lastTransform: { scale: number; offX: number; offY: number } | null = null;
  /// Last-computed AUTO-FIT (base) transform — the scale/offset the
  /// canvas would use with zoom=1 and no pan. Stored separately so the
  /// wheel-zoom math can solve for a user pan that keeps the cursor
  /// over the same data-space point as the zoom multiplier changes.
  let lastBaseTransform: { scale: number; offX: number; offY: number } | null = null;

  /// User-applied pan + zoom on top of the auto-fit transform. zoom = 1
  /// + panX/panY = 0 → auto-fit (the default after every new import).
  /// Wheel zooms around the cursor; middle-button drag pans; double-
  /// click empty space resets both to default.
  let userZoom = $state(1);
  let userPanX = $state(0);
  let userPanY = $state(0);
  /// Active pan drag — started on middle-button down, ended on pointer up.
  let panDrag = $state<{ startX: number; startY: number; pointerId: number } | null>(null);

  /// Reset pan + zoom when the imported file changes (different filename
  /// or going from no-import to imported). Keeps mid-session zooms
  /// intact across normal redraws.
  let _lastImportedKey: string | null = null;
  $effect(() => {
    const key = project.imported?.filename ?? null;
    if (key !== _lastImportedKey) {
      _lastImportedKey = key;
      userZoom = 1;
      userPanX = 0;
      userPanY = 0;
    }
  });

  /// FreeCAD-style box-select state. Captured on pointerdown over
  /// empty canvas; commits to a box drag once the cursor has moved
  /// `BOX_DRAG_THRESHOLD` px (so a sloppy click on empty space still
  /// just clears the selection).
  let boxSelect = $state<{
    startX: number;
    startY: number;
    curX: number;
    curY: number;
    mode: 'replace' | 'add' | 'toggle';
    armed: boolean; // pointer is down but we haven't crossed the threshold yet
  } | null>(null);
  const BOX_DRAG_THRESHOLD = 4;

  /// Inverted index: objectId → opIds that reference it via
  /// `op.sourceObjects`. Drives the green / dim-green tinting on the
  /// canvas and the "click an assigned object activates its op"
  /// behaviour.
  const objectToOps = $derived.by<Map<number, number[]>>(() => {
    const out = new Map<number, number[]>();
    for (const op of project.operations) {
      const refs = op.sourceObjects;
      if (!refs) continue;
      for (const id of refs) {
        if (id <= 0) continue;
        const list = out.get(id);
        if (list) list.push(op.id);
        else out.set(id, [op.id]);
      }
    }
    return out;
  });
  /// Object ids the currently-selected op references (highlighted
  /// brighter than other-op assignments).
  const activeOpObjects = $derived<Set<number>>(
    selectedOp?.sourceObjects ? new Set(selectedOp.sourceObjects) : new Set<number>(),
  );

  /// Uniform-grid spatial index for segment hit testing. Without it,
  /// pixelHit() ran an O(N) scan on every pointermove — fine for tiny
  /// DXFs but a million distance computations per second of idle hover
  /// on a 10k-segment file. The grid is rebuilt when project.imported
  /// changes (rare); each query inspects only the cells overlapping the
  /// cursor + tolerance and bails early past that.
  // Spatial index (HitIndex type + buildHitIndex + the cell-walk
  // query loop) extracted to lib/canvas/spatial-index.ts so vitest
  // can exercise them without mounting the canvas (y0ez).
  let hitIndex: HitIndex | null = null;

  $effect(() => {
    void project.transformedImport;
    hitIndex = buildHitIndexPure(project.transformedImport);
  });

  function pixelHit(canvasX: number, canvasY: number): number | null {
    const data = project.transformedImport;
    if (!data || !lastTransform) return null;
    const { scale, offX, offY } = lastTransform;
    const dataX = (canvasX - offX) / scale;
    const dataY = (offY - canvasY) / scale;
    const tolData = HIT_PIXEL_TOL / scale;
    return queryHit(data, hitIndex, dataX, dataY, tolData, (l) =>
      project.visibleLayers.has(l),
    );
  }

  /// Convert canvas-pixel coords to data-space (mm) using the last
  /// emitted transform. Returns `null` when no transform is staged
  /// (project hasn't rendered yet).
  function pxToData(cx: number, cy: number): { x: number; y: number } | null {
    if (!lastTransform) return null;
    const t = lastTransform;
    // Canvas Y axis is inverted vs data Y (canvas grows downward).
    return { x: (cx - t.offX) / t.scale, y: -(cy - t.offY) / t.scale };
  }

  /// Tolerance for the approach-point snap, in data units. Mirrors
  /// the existing pixel-tolerance pattern used by `pixelHit` so the
  /// snap radius stays constant in screen pixels across zoom levels.
  function approachSnapToleranceData(): number {
    if (!lastTransform) return 0;
    // ~6 px feels right — matches the marker hit radius below.
    return 6 / Math.max(Math.abs(lastTransform.scale), 1e-6);
  }

  /// Snap radius for the placed marker's drag handle, in data units.
  function approachMarkerHitRadiusData(): number {
    if (!lastTransform) return 0;
    return 7 / Math.max(Math.abs(lastTransform.scale), 1e-6);
  }

  function onPointerMove(e: PointerEvent) {
    const rect = canvas.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    // 7tp5: cursor coordinate HUD. Track the world (data) position on
    // every move regardless of modal mode — users want to read X/Y
    // while pan/zoom/select/picking. pxToData returns null if the
    // transform isn't staged yet (no imported drawing).
    cursorXY = pxToData(cx, cy);
    // n79: in approach-pick mode, the cursor IS the picker — update
    // the preview marker on every move and short-circuit the
    // hover-hit / box-select paths below.
    if (approachPickActive) {
      const data = pxToData(cx, cy);
      if (data) {
        const tol = approachSnapToleranceData();
        const snap = shiftDown
          ? null
          : findOSnap(osnapTargets, data.x, data.y, tol, osnapSettings);
        approachPreview = snap
          ? { x: snap.x, y: snap.y, snap: snap.kind }
          : { x: data.x, y: data.y, snap: null };
      } else {
        approachPreview = null;
      }
      canvas.style.cursor = 'crosshair';
      return;
    } else if (approachPreview) {
      approachPreview = null;
    }

    // Live drag of an already-placed approach marker (n79 hybrid).
    if (approachDrag && e.pointerId === approachDrag.pointerId) {
      const data = pxToData(cx, cy);
      if (data) {
        const tol = approachSnapToleranceData();
        const snap = shiftDown
          ? null
          : findOSnap(osnapTargets, data.x, data.y, tol, osnapSettings);
        const x = snap ? snap.x : data.x;
        const y = snap ? snap.y : data.y;
        project.updateOperation(approachDrag.opId, { approachPoint: [x, y] });
        approachPreview = { x, y, snap: snap?.kind ?? null };
      }
      canvas.style.cursor = 'grabbing';
      return;
    }

    // Active pan drag: translate the user-pan offsets by the cursor
    // delta. Each move is RELATIVE so we anchor on the previous frame's
    // screen position, then update the anchor for the next frame.
    if (panDrag) {
      const dx = e.clientX - panDrag.startX;
      const dy = e.clientY - panDrag.startY;
      userPanX += dx;
      userPanY += dy;
      panDrag = { ...panDrag, startX: e.clientX, startY: e.clientY };
      return;
    }
    // Box-select drag: once the cursor crosses BOX_DRAG_THRESHOLD px
    // from the arm point, commit to a box drag. While dragging,
    // suppress hover hit-testing so the cursor stays a crosshair.
    if (boxSelect) {
      const dx = cx - boxSelect.startX;
      const dy = cy - boxSelect.startY;
      if (!boxSelect.armed || Math.hypot(dx, dy) >= BOX_DRAG_THRESHOLD) {
        boxSelect = { ...boxSelect, curX: cx, curY: cy, armed: false };
        canvas.style.cursor = 'crosshair';
        return;
      }
    }
    const idx = pixelHit(cx, cy);
    if (idx !== hoverIdx) hoverIdx = idx;
    // rt1.10: tab-placement mode — project cursor to the op's
    // closest source contour and stage a ghost tab. The ghost only
    // renders when the projection is within ~6 px of the cursor
    // (screen-space) so we don't spam ghosts the user wasn't aiming at.
    if (tabPlacementActive && lastTransform) {
      const ghost = projectGhostTab(cx, cy);
      if (
        !ghost ||
        !ghostTab ||
        ghost.objectId !== ghostTab.objectId ||
        Math.abs(ghost.t - ghostTab.t) > 1e-5
      ) {
        ghostTab = ghost;
      }
    } else if (ghostTab) {
      ghostTab = null;
    }
    const baseCursor = tabPlacementActive ? 'crosshair' : 'default';
    canvas.style.cursor = idx == null ? baseCursor : tabPlacementActive ? 'cell' : 'pointer';
  }

  function onPointerUp(e: PointerEvent) {
    // n79: end an active approach-marker drag.
    if (approachDrag && e.pointerId === approachDrag.pointerId) {
      approachDrag = null;
      canvas.style.cursor = 'default';
      approachPreview = null;
      try {
        canvas.releasePointerCapture(e.pointerId);
      } catch {}
      return;
    }
    // End any active pan drag.
    if (panDrag && e.pointerId === panDrag.pointerId) {
      panDrag = null;
      canvas.style.cursor = 'default';
      try {
        canvas.releasePointerCapture(e.pointerId);
      } catch {}
      return;
    }
    // Commit any pending box-select. A box-select that never crossed
    // the threshold collapses to a plain "click on empty" — handled
    // already in onPointerDown's empty-hit branch — so here we only
    // act when we've committed to a box drag.
    if (boxSelect && !boxSelect.armed) {
      const { startX, startY, curX, curY, mode } = boxSelect;
      const ids = objectsInBox(startX, startY, curX, curY);
      project.selectObjects(ids, mode);
    }
    boxSelect = null;
    canvas.style.cursor = tabPlacementActive ? 'crosshair' : 'default';
    try {
      canvas.releasePointerCapture(e.pointerId);
    } catch {
      /* may already be released; harmless */
    }
  }

  /// Wheel = cursor-pivot zoom. deltaY < 0 (scroll up) zooms in; > 0
  /// zooms out. The pan offsets are adjusted so the data-space point
  /// under the cursor stays under the cursor across the zoom.
  function onWheel(e: WheelEvent) {
    if (!lastBaseTransform) return;
    e.preventDefault();
    const rect = canvas.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    const { scale: baseScale, offX: baseOffX, offY: baseOffY } = lastBaseTransform;
    const oldScale = baseScale * userZoom;
    const oldOffX = baseOffX + userPanX;
    const oldOffY = baseOffY + userPanY;
    // Data-space point under the cursor right now.
    const dataX = (cx - oldOffX) / oldScale;
    const dataY = (oldOffY - cy) / oldScale;
    const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
    const nextZoom = Math.max(0.05, Math.min(80, userZoom * factor));
    const newScale = baseScale * nextZoom;
    // Solve for offset that keeps (dataX, dataY) under the cursor.
    const newOffX = cx - dataX * newScale;
    const newOffY = cy + dataY * newScale;
    userZoom = nextZoom;
    userPanX = newOffX - baseOffX;
    userPanY = newOffY - baseOffY;
  }

  /// Double-click on empty space = reset pan + zoom to auto-fit.
  function onDblClick(e: MouseEvent) {
    const rect = canvas.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    if (pixelHit(cx, cy) != null) return; // hit something — don't reset
    userZoom = 1;
    userPanX = 0;
    userPanY = 0;
  }

  /// Return the set of object ids whose bbox lies fully INSIDE the
  /// screen rectangle drawn between (x0,y0) and (x1,y1) — Illustrator /
  /// Inkscape style containment select, so dragging the rubber-band
  /// across part of an object does NOT pick it. Works in DATA
  /// coordinates: we transform the rectangle once into data space and
  /// containment-test each object's bbox (audit-1dqh).
  function objectsInBox(x0: number, y0: number, x1: number, y1: number): number[] {
    const data = project.transformedImport;
    if (!data || !lastTransform) return [];
    const { scale, offX, offY } = lastTransform;
    const px2dx = (x: number) => (x - offX) / scale;
    const px2dy = (y: number) => (offY - y) / scale;
    const minX = Math.min(px2dx(x0), px2dx(x1));
    const maxX = Math.max(px2dx(x0), px2dx(x1));
    // Canvas Y is inverted relative to data Y, so the data-space min
    // comes from the LOWER pixel y.
    const minY = Math.min(px2dy(y0), px2dy(y1));
    const maxY = Math.max(px2dy(y0), px2dy(y1));
    const meta = data.object_meta ?? [];
    const visibleLayers = project.visibleLayers;
    const out: number[] = [];
    for (const m of meta) {
      // Layer-visibility filter so the user can't accidentally pick
      // hidden chains.
      if (!visibleLayers.has(m.layer)) continue;
      const b = m.bbox;
      // Containment: every corner of the object's bbox must lie inside
      // the selection rectangle.
      if (b.min_x < minX || b.max_x > maxX || b.min_y < minY || b.max_y > maxY) continue;
      out.push(m.id);
    }
    return out;
  }
  function onPointerLeave() {
    hoverIdx = null;
    ghostTab = null;
    cursorXY = null;
    canvas.style.cursor = tabPlacementActive ? 'crosshair' : 'default';
  }

  /// Cache of the per-object polylines for the current import. Cleared
  /// when the import changes; the projection helpers reuse it.
  let objectPolylinesCache: ObjectPolyline[] | null = null;
  let objectPolylinesCacheKey: unknown = null;
  function getObjectPolylines(): ObjectPolyline[] {
    const imp = project.transformedImport;
    if (!imp) return [];
    if (objectPolylinesCacheKey !== imp) {
      objectPolylinesCache = buildObjectPolylines(imp);
      objectPolylinesCacheKey = imp;
    }
    return objectPolylinesCache ?? [];
  }

  /// Project canvas-space (cx, cy) onto the closest source contour of
  /// the selected op. Returns the ghost-tab position or null when no
  /// contour is within 6 screen-px / the op has no closed source.
  /// Snap precedence (1q3): vertex within 4 screen-px > midpoint
  /// within 4 screen-px > existing tab on this op within 2 mm
  /// data-space > raw contour projection within 6 screen-px. Alt
  /// disables secondary snaps.
  function projectGhostTab(
    cx: number,
    cy: number,
  ): {
    x: number;
    y: number;
    objectId: number;
    t: number;
    snap: 'contour' | 'vertex' | 'midpoint' | 'existing';
  } | null {
    const op = selectedOp;
    if (!op || !isContourOp(op) || !lastTransform) return null;
    const { scale, offX, offY } = lastTransform;
    // Canvas → data XY (mirror of the draw transform).
    const dataX = (cx - offX) / scale;
    const dataY = (offY - cy) / scale;
    const tolPx = 6;
    const snapPx = 4;
    const existingTabTolMm = 2;
    const tolData = tolPx / scale;
    const snapTolData = snapPx / scale;
    // Op-source filter: only project onto contours the op actually consumes.
    const allow = (id: number) => {
      const so = op.sourceObjects;
      if (so && so.length > 0) return so.includes(id);
      // 'all' or layer-source: every chained object qualifies.
      return true;
    };
    let best: {
      x: number;
      y: number;
      objectId: number;
      t: number;
      d2: number;
      snap: 'contour' | 'vertex' | 'midpoint' | 'existing';
    } | null = null;
    for (const obj of getObjectPolylines()) {
      if (!allow(obj.objectId)) continue;
      const { t, snap, d2 } = polylineProject(obj.pts, { x: dataX, y: dataY }, obj.closed);
      if (d2 > tolData * tolData) continue;
      if (best && d2 >= best.d2) continue;
      best = {
        x: snap.x,
        y: snap.y,
        objectId: obj.objectId,
        t,
        d2,
        snap: 'contour',
      };
    }
    if (!best) return null;
    if (altDown) {
      // CAD-style escape hatch: bare contour projection only.
      return best;
    }
    // Promote to vertex / midpoint when the cursor is close enough.
    const obj = getObjectPolylines().find((o) => o.objectId === best!.objectId);
    if (obj) {
      let promoted: {
        t: number;
        x: number;
        y: number;
        snap: 'vertex' | 'midpoint' | 'existing';
        d2: number;
      } | null = null;
      for (const cand of vertexAndMidpointTs(obj.pts, obj.closed)) {
        const dx = cand.point.x - dataX;
        const dy = cand.point.y - dataY;
        const d2 = dx * dx + dy * dy;
        if (d2 > snapTolData * snapTolData) continue;
        if (promoted && d2 >= promoted.d2) continue;
        promoted = { t: cand.t, x: cand.point.x, y: cand.point.y, snap: cand.kind, d2 };
      }
      // Existing-tab snap on the SAME op + object, within 2mm data-space.
      for (const tp of op.tabPlacements ?? []) {
        if (tp.objectId !== best.objectId) continue;
        const wp = polylineAtT(obj.pts, tp.t, obj.closed).point;
        const dx = wp.x - dataX;
        const dy = wp.y - dataY;
        const d2 = dx * dx + dy * dy;
        if (d2 > existingTabTolMm * existingTabTolMm) continue;
        if (promoted && d2 >= promoted.d2) continue;
        promoted = { t: tp.t, x: wp.x, y: wp.y, snap: 'existing', d2 };
      }
      if (promoted) {
        return {
          x: promoted.x,
          y: promoted.y,
          objectId: best.objectId,
          t: promoted.t,
          snap: promoted.snap,
        };
      }
    }
    return best;
  }
  /// Right-click context menu. `null` = closed. Open menu lists the
  /// same op kinds as the Add-operation picker; clicking an entry
  /// creates an op whose source is the current canvas selection, all
  /// wrapped in one undoable transaction.
  let ctxMenu = $state<{ x: number; y: number; dataX: number; dataY: number } | null>(null);

  /// Per-tab popover (8rd). Opens on right-click over an existing
  /// tab; carries the canvas-space position to anchor the popover
  /// + the (opId, placementIdx) it edits. Clamped to canvas bounds
  /// at render time so a tab near the edge doesn't open off-screen.
  let tabPopover = $state<{
    x: number;
    y: number;
    opId: number;
    placementIdx: number;
  } | null>(null);

  function onContextMenu(e: MouseEvent) {
    e.preventDefault();
    const rect = canvas.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    // 8rd: right-click over an existing tab opens the per-tab
    // popover BEFORE falling through to the op-picker context menu.
    const hit = findTabAtPixel(cx, cy);
    if (hit) {
      // Clamp the popover anchor so it stays inside the canvas
      // even when the user right-clicks near the right / bottom
      // edge. Popover footprint ≈ 200×160 px.
      const cw = container?.clientWidth ?? 800;
      const ch = container?.clientHeight ?? 600;
      const px = Math.max(8, Math.min(cx, cw - 200));
      const py = Math.max(8, Math.min(cy, ch - 160));
      tabPopover = { x: px, y: py, opId: hit.opId, placementIdx: hit.placementIdx };
      ctxMenu = null;
      return;
    }
    // Convert canvas pixels → data-space mm so menu actions (like
    // "Set text origin here") can plant their target at the cursor
    // without redoing the projection math.
    const t = lastTransform;
    const dataX = t ? (cx - t.offX) / t.scale : 0;
    const dataY = t ? (t.offY - cy) / t.scale : 0;
    ctxMenu = { x: cx, y: cy, dataX, dataY };
  }

  /// Set the currently-selected text layer's origin to the position
  /// the user right-clicked at. No-op when no text layer is selected.
  function setTextOriginHere() {
    if (!ctxMenu) return;
    const id = project.selectedTextLayerId;
    if (id == null) {
      ctxMenu = null;
      return;
    }
    const x = ctxMenu.dataX;
    const y = ctxMenu.dataY;
    ctxMenu = null;
    project.updateTextLayer(id, { origin: { x, y } });
  }

  function closeCtxMenu() {
    ctxMenu = null;
  }

  function closeTabPopover() {
    tabPopover = null;
  }

  /// Find an op's tab placement under the cursor (canvas-space).
  /// Walks every op (not just the selected) so right-click works
  /// regardless of which op is active — matches CAD intuition
  /// ('that tab right there').
  function findTabAtPixel(cx: number, cy: number): { opId: number; placementIdx: number } | null {
    if (!lastTransform) return null;
    const { scale, offX, offY } = lastTransform;
    const tolPx = 10;
    const objects = getObjectPolylines();
    let best: { opId: number; placementIdx: number; d2: number } | null = null;
    for (const op of project.operations) {
      if (!isContourOp(op)) continue;
      const mode = op.tabMode?.kind ?? 'off';
      if (mode !== 'manual' && mode !== 'mixed') continue;
      const placements = op.tabPlacements ?? [];
      for (let i = 0; i < placements.length; i++) {
        const tp = placements[i];
        const obj = objects.find((o) => o.objectId === tp.objectId);
        if (!obj) continue;
        const { point } = polylineAtT(obj.pts, tp.t, obj.closed);
        const sx = point.x * scale + offX;
        const sy = offY - point.y * scale;
        const d2 = (cx - sx) * (cx - sx) + (cy - sy) * (cy - sy);
        if (d2 > tolPx * tolPx) continue;
        if (best && d2 >= best.d2) continue;
        best = { opId: op.id, placementIdx: i, d2 };
      }
    }
    return best ? { opId: best.opId, placementIdx: best.placementIdx } : null;
  }

  /// Update one tab placement's width / height override. Routes
  /// through updateOperation so it's a single undoable history entry.
  function patchTabOverride(
    opId: number,
    placementIdx: number,
    patch: { widthOverrideMm?: number | undefined; heightOverrideMm?: number | undefined },
  ) {
    const op = project.operations.find((o) => o.id === opId);
    if (!op || !isContourOp(op)) return;
    const cur = op.tabPlacements ?? [];
    if (placementIdx < 0 || placementIdx >= cur.length) return;
    const next = cur.map((p, i) => (i === placementIdx ? { ...p, ...patch } : p));
    project.updateOperation(opId, { tabPlacements: next });
  }

  /// Delete one tab placement (via toggleTabPlacement — its remove
  /// branch fires when the target is within tolerance).
  function deleteTabPlacement(opId: number, placementIdx: number) {
    const op = project.operations.find((o) => o.id === opId);
    if (!op || !isContourOp(op)) return;
    const cur = op.tabPlacements ?? [];
    if (placementIdx < 0 || placementIdx >= cur.length) return;
    const next = cur.filter((_, i) => i !== placementIdx);
    project.updateOperation(opId, { tabPlacements: next });
    tabPopover = null;
  }

  function onCtxKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && tabPopover) {
      tabPopover = null;
      e.preventDefault();
      return;
    }
    if (e.key === 'Escape' && ctxMenu) {
      ctxMenu = null;
      e.preventDefault();
    }
    // n79: ESC finalizes the approach-point picker (sticky mode exit).
    if (e.key === 'Escape' && approachPickActive) {
      project.pickMode = null;
      approachPreview = null;
      canvas.style.cursor = 'default';
      e.preventDefault();
    }
    // Escape mid-drag cancels the box-select without changing the
    // current selection — FreeCAD-style.
    if (e.key === 'Escape' && boxSelect) {
      boxSelect = null;
      canvas.style.cursor = tabPlacementActive ? 'crosshair' : 'default';
      e.preventDefault();
    }
  }

  function onCtxDocClick(e: MouseEvent) {
    // Cheap bail when neither the context menu nor the tab popover
    // is open — the global onclick from <svelte:window> fires on
    // every document click and we don't want to walk the DOM with
    // `closest` per click when there's nothing to dismiss (audit-pgxb).
    if (!ctxMenu && !tabPopover) return;
    const target = e.target as HTMLElement | null;
    if (tabPopover) {
      if (!(target && target.closest('.tab-popover'))) {
        tabPopover = null;
      }
    }
    if (!ctxMenu) return;
    if (target && target.closest('.ctx-menu')) return;
    ctxMenu = null;
  }

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
    } catch (e) {
      project.cancelTransaction();
      throw e;
    }
    ctxMenu = null;
  }

  function onPointerDown(e: PointerEvent) {
    const rect = canvas.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;

    // Middle-button drag = pan. Capture the pointer so the drag
    // continues if the cursor leaves the canvas.
    if (e.button === 1) {
      e.preventDefault();
      panDrag = { startX: e.clientX, startY: e.clientY, pointerId: e.pointerId };
      try {
        canvas.setPointerCapture(e.pointerId);
      } catch {}
      canvas.style.cursor = 'grabbing';
      return;
    }

    // n79: approach-point pick mode. Left-click commits the snapped
    // (or free, if Shift) cursor position into op.approachPoint and
    // STAYS in pick mode (sticky — ESC exits). Right-click bails
    // out without committing.
    if (approachPickActive && selectedOp && e.button === 0) {
      const data = pxToData(cx, cy);
      if (data) {
        const tol = approachSnapToleranceData();
        const snap = shiftDown
          ? null
          : findOSnap(osnapTargets, data.x, data.y, tol, osnapSettings);
        const x = snap ? snap.x : data.x;
        const y = snap ? snap.y : data.y;
        project.updateOperation(selectedOp.id, { approachPoint: [x, y] });
        approachPreview = { x, y, snap: snap?.kind ?? null };
      }
      e.preventDefault();
      return;
    }
    if (approachPickActive && e.button === 2) {
      project.pickMode = null;
      approachPreview = null;
      e.preventDefault();
      return;
    }

    // n79: dragging an already-placed approach marker. Only allowed
    // when the selected op has one and we're NOT in pick mode.
    if (
      !approachPickActive
      && selectedOp
      && (selectedOp.kind === 'profile' || selectedOp.kind === 'pocket')
      && selectedOp.approachPoint
      && e.button === 0
    ) {
      const data = pxToData(cx, cy);
      const hitR = approachMarkerHitRadiusData();
      if (data) {
        const [ax, ay] = selectedOp.approachPoint;
        const dx = data.x - ax;
        const dy = data.y - ay;
        if (dx * dx + dy * dy <= hitR * hitR) {
          approachDrag = { opId: selectedOp.id, pointerId: e.pointerId };
          try {
            canvas.setPointerCapture(e.pointerId);
          } catch {}
          canvas.style.cursor = 'grabbing';
          e.preventDefault();
          return;
        }
      }
    }

    // rt1.10: tab-placement mode (selected op has Manual / Mixed
    // tab_mode). Click toggles a placement at the contour projection
    // — Estlcam-style. ToleranceT picks the "is this near an existing
    // tab" threshold: ~3 px of contour length.
    if (tabPlacementActive && selectedOp) {
      const ghost = projectGhostTab(cx, cy);
      if (!ghost) return;
      // Tolerance in t-units: ~3 px of contour length. Without an
      // exact polyline length we conservatively use 0.01 (1% of contour).
      project.toggleTabPlacement(selectedOp.id, { objectId: ghost.objectId, t: ghost.t }, 0.01);
      return;
    }

    // Fixture hit-test runs before segment selection so clicking a fixture
    // outline snaps the right-hand panel's edit form to it.
    const fixId = fixtureHit(cx, cy);
    if (fixId != null) {
      project.selectFixture(fixId);
      return;
    }

    const idx = pixelHit(cx, cy);
    // Modifier semantics (audit-eqxd):
    //   * Shift+click  → SERIES select — extend the selection from the
    //                    anchor object (last single-clicked) to the
    //                    clicked one, sweeping every object whose bbox
    //                    is crossed by the imaginary line between them.
    //                    Falls back to plain replace when no anchor.
    //   * Ctrl/Cmd+click → TOGGLE in selection (add or deselect).
    //   * plain click  → REPLACE selection.
    const mode: 'replace' | 'add' | 'toggle' | 'series' = e.shiftKey
      ? 'series'
      : e.ctrlKey || e.metaKey
        ? 'toggle'
        : 'replace';
    if (idx == null) {
      // Clicked empty space — arm a potential box-select. If the
      // pointer comes back up without ever moving past
      // BOX_DRAG_THRESHOLD, this collapses to a "click on empty"
      // which clears the selection for `replace` mode and is a
      // no-op for `add` / `toggle` / `series` (so the user can't
      // accidentally drop their selection mid-modifier).
      if (mode === 'replace') {
        project.clearSelection();
        project.selectFixture(null);
      }
      // Series-select needs an object target — on empty space we fall
      // back to additive box-select so Shift+drag stays useful.
      const boxMode: 'replace' | 'add' | 'toggle' = mode === 'series' ? 'add' : mode;
      boxSelect = { startX: cx, startY: cy, curX: cx, curY: cy, mode: boxMode, armed: true };
      // Capture so pointermove keeps firing if the user drags past the
      // canvas edge — otherwise the box-select would freeze at the
      // last point inside the canvas.
      try {
        canvas.setPointerCapture(e.pointerId);
      } catch {
        /* not all browsers / older versions; harmless */
      }
      return;
    }
    // Map segment index → its 1-based object id from the chaining pass.
    const objId = project.transformedImport?.objects?.[idx] ?? 0;
    if (objId === 0) return;
    if (mode === 'series') {
      project.seriesSelectTo(objId);
    } else {
      project.selectObjects([objId], mode);
    }
    // Clicking an object that's already wired into an operation makes
    // that op the active one — surfaces the right edit form on the
    // right-hand panel without a separate trip to the operations list.
    // Only fires for plain clicks; modifier-clicks are about building
    // selections, not switching the active op.
    if (mode === 'replace') {
      const ops = objectToOps.get(objId);
      if (ops && ops.length > 0 && project.selectedOpId !== ops[0]) {
        project.selectedOpId = ops[0];
      }
    }
  }

  /// Returns the id of the fixture under the cursor, or null. Hit-test
  /// runs in data coordinates: a click is "inside" a Box / Cylinder if
  /// the point is inside their AABB / disc, and inside a Polygon by
  /// Hit-test fixtures in canvas pixel space. Pure shape-inclusion
  /// logic delegated to `lib/canvas/fixture-hit.ts` (audit y0ez);
  /// the component just converts canvas-pixel to data-space and
  /// passes the current fixture list.
  function fixtureHit(canvasX: number, canvasY: number): number | null {
    if (!lastTransform) return null;
    const { scale, offX, offY } = lastTransform;
    const dataX = (canvasX - offX) / scale;
    const dataY = (offY - canvasY) / scale;
    return fixtureAt(project.fixtures, dataX, dataY);
  }

  function closestPointOnSegment(
    segmentIdx: number,
    canvasX: number,
    canvasY: number,
  ): { x: number; y: number } | null {
    const data = project.transformedImport;
    if (!data || !lastTransform) return null;
    const { scale, offX, offY } = lastTransform;
    const dataX = (canvasX - offX) / scale;
    const dataY = (offY - canvasY) / scale;
    const s = data.segments[segmentIdx];
    if (!s) return null;
    return projectOntoSegment(s.start, s.end, dataX, dataY);
  }

  function colorFor(c: number): string {
    if (c === 7 || c === 256) return themeVar('--text-strong', '#e6e6e6');
    if (c === 8) return themeVar('--text-muted', '#888');
    return ACI_FIXED[c] ?? themeVar('--text-faint', '#bbbbbb');
  }

  /// Idempotent canvas-size + DPR sync. Returns the painting context +
  /// CSS-pixel dimensions, or null when the host isn't mounted yet.
  function setupCanvas(
    c: HTMLCanvasElement | undefined,
  ): { ctx: CanvasRenderingContext2D; w: number; h: number } | null {
    if (!c || !container) return null;
    const ctx = c.getContext('2d');
    if (!ctx) return null;
    const dpr = window.devicePixelRatio || 1;
    const w = container.clientWidth;
    const h = container.clientHeight;
    // Only reallocate the backing store on real size changes — setting
    // canvas.width on every redraw allocates a fresh GPU buffer and
    // clears it.
    const targetW = w * dpr;
    const targetH = h * dpr;
    if (c.width !== targetW) c.width = targetW;
    if (c.height !== targetH) c.height = targetH;
    if (c.style.width !== `${w}px`) c.style.width = `${w}px`;
    if (c.style.height !== `${h}px`) c.style.height = `${h}px`;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return { ctx, w, h };
  }

  /// Auto-fit transform compute. Reads bbox + user pan/zoom and writes
  /// `lastTransform` / `lastBaseTransform` so the pointer handlers + the
  /// overlay layer see the same projection as the bg layer.
  function computeTransform(
    data: import('../api/types').ImportResponse,
    w: number,
    h: number,
  ): { scale: number; offX: number; offY: number; project2: (x: number, y: number) => [number, number] } {
    const { min_x, min_y, max_x, max_y } = data.bbox;
    const dataW = Math.max(max_x - min_x, 1e-6);
    const dataH = Math.max(max_y - min_y, 1e-6);
    const margin = 32;
    const baseScale = Math.min((w - 2 * margin) / dataW, (h - 2 * margin) / dataH);
    const baseOffX = margin - min_x * baseScale + (w - 2 * margin - dataW * baseScale) / 2;
    // Y flipped: DXF y-up, canvas y-down.
    const baseOffY = h - margin - -min_y * baseScale - (h - 2 * margin - dataH * baseScale) / 2;
    lastBaseTransform = { scale: baseScale, offX: baseOffX, offY: baseOffY };
    const scale = baseScale * userZoom;
    const offX = baseOffX + userPanX;
    const offY = baseOffY + userPanY;
    lastTransform = { scale, offX, offY };
    const project2 = (px: number, py: number): [number, number] => [
      px * scale + offX,
      offY - py * scale,
    ];
    return { scale, offX, offY, project2 };
  }

  /// Heavy static layer — repaints only on geometry / layout / theme /
  /// zoom / pan changes. State-bearing repaints (hover / selection /
  /// ghost tab / box-select / approach point) happen on the overlay
  /// canvas via drawOverlay() and do NOT invalidate this layer.
  function drawBackground() {
    const setup = setupCanvas(canvas);
    if (!setup) return;
    const { ctx, w, h } = setup;
    ctx.fillStyle = themeVar('--bg-app', '#0d0d0d');
    ctx.fillRect(0, 0, w, h);

    const data = project.transformedImport;
    if (!data || data.segments.length === 0) {
      ctx.fillStyle = themeVar('--canvas-empty', '#555');
      ctx.font = '13px system-ui, sans-serif';
      ctx.fillText('Open a file to view geometry', 16, 24);
      return;
    }

    const { scale, offX, offY, project2 } = computeTransform(data, w, h);

    drawGrid(ctx, w, h, scale, offX, offY);
    drawAxes(ctx, w, h, offX, offY);
    drawWorkArea(ctx, project2);

    // Filled-region preview painted under the wireframe so contours stay
    // legible. Regions come from the backend (pipeline.rs
    // build_region_previews).
    const regions = project.generated?.regions ?? [];
    if (regions.length > 0 && project.regionsVisible) {
      drawRegions(ctx, regions, scale, offX, offY);
    }

    // Imported segments — paint in BASE layer color only. State-bearing
    // overlays (selection / hover / op-assignment halos) go on the
    // overlay canvas, so editing those does NOT invalidate this layer.
    const visibleLayersSnap = new Set(project.visibleLayers);
    ctx.lineWidth = 1.25;
    for (let i = 0; i < data.segments.length; i++) {
      const seg = data.segments[i];
      if (!visibleLayersSnap.has(seg.layer)) continue;
      ctx.strokeStyle = colorFor(seg.color);
      drawSegment(ctx, seg, project2);
    }

    // Text-layer previews. The cache is filled by requestPreview() in
    // the top-of-file effect. drawTextPreview also reads
    // selectedTextLayerId for the highlight; selecting a text layer is
    // rare enough that retainting bg is acceptable.
    if (project.textLayers.length > 0) {
      const accent = themeVar('--accent', '#2d6cdf');
      const haloColor = themeVar('--text-strong', '#ffffff');
      drawTextPreview(ctx, project2, accent, '', haloColor);
    }
  }

  /// State-bearing overlay — selection halos + accent strokes, hover
  /// halo, op-assignment tints, fixtures, tab markers + ghost tab,
  /// approach-point marker + OSnap glyph, box-select rect. Cleared and
  /// repainted on every interaction-state change; the bg canvas stays
  /// put.
  function drawOverlay() {
    const setup = setupCanvas(canvasOverlay);
    if (!setup) return;
    const { ctx, w, h } = setup;
    // Transparent clear — bg shows through.
    ctx.clearRect(0, 0, w, h);

    const data = project.transformedImport;
    if (!data || data.segments.length === 0) return;
    const { scale, project2 } = computeTransform(data, w, h);

    const accent = themeVar('--accent', '#2d6cdf');
    const hoverColor = themeVar('--accent-strong', '#6e9ce6');
    const activeAssignColor = themeVar('--obj-assigned-active', '#39c75c');
    const otherAssignColor = themeVar('--obj-assigned-other', '#2a6f3b');
    // Halo color = a high-contrast outline drawn UNDER selected /
    // hovered / op-assigned objects so the state stays visible even
    // when the underlying layer's ACI color happens to match the state
    // color. Uses --text-strong so it inverts automatically in light
    // theme.
    const haloColor = themeVar('--text-strong', '#ffffff');
    const hoverObj = hoverIdx == null ? 0 : (data.objects?.[hoverIdx] ?? 0);
    const visibleLayersSnap = new Set(project.visibleLayers);
    const selectedObjectsSnap = new Set(project.selectedObjects);
    for (let i = 0; i < data.segments.length; i++) {
      const seg = data.segments[i];
      if (!visibleLayersSnap.has(seg.layer)) continue;
      const objId = data.objects?.[i] ?? 0;
      if (objId === 0) continue;
      const selected = selectedObjectsSnap.has(objId);
      const hovered = objId === hoverObj;
      const inActiveOp = activeOpObjects.has(objId);
      const inAnyOp = !inActiveOp && objectToOps.has(objId);
      if (!selected && !hovered && !inActiveOp && !inAnyOp) continue;
      // Assignment-tint precedence (top wins):
      //   selected → accent
      //   hovered → hoverColor
      //   in active op → bright green
      //   in any other op → dim green
      const baseWidth = selected ? 2.4 : hovered ? 1.8 : inActiveOp ? 1.6 : 1.4;
      const haloAlpha = selected ? 0.6 : hovered ? 0.55 : inActiveOp ? 0.5 : 0.3;
      const prevAlpha = ctx.globalAlpha;
      ctx.globalAlpha = haloAlpha;
      ctx.lineWidth = baseWidth + 3;
      ctx.strokeStyle = haloColor;
      drawSegment(ctx, seg, project2);
      ctx.globalAlpha = prevAlpha;
      ctx.lineWidth = baseWidth;
      ctx.strokeStyle = selected
        ? accent
        : hovered
          ? hoverColor
          : inActiveOp
            ? activeAssignColor
            : otherAssignColor;
      drawSegment(ctx, seg, project2);
    }

    drawFixtures(ctx, project2);
    drawTabs(ctx, project2, scale);
    drawApproachPoint(ctx, project2);
    if (boxSelect && !boxSelect.armed) {
      drawBoxSelect(ctx, accent);
    }
  }

  /// Paint the approach-point marker (n79) for the currently selected
  /// op when it has one set, plus the live preview while in pick mode
  /// or actively dragging.
  function drawApproachPoint(
    ctx: CanvasRenderingContext2D,
    project2: (x: number, y: number) => [number, number],
  ): void {
    const op = selectedOp;
    if (!op) return;
    // approachPoint lives on ContourFields, currently shared only by
    // Profile + Pocket on the FE type side. (The BE accepts it on
    // Engrave / DragKnife too; expanding the FE types is a follow-up.)
    if (op.kind !== 'profile' && op.kind !== 'pocket') return;

    const markerColor = themeVar('--accent', '#3aa');
    const snapColor = '#3c3'; // green = locked-to-vertex (matches EstlCam)
    const ringColor = themeVar('--text', '#000');

    // The committed point, when present.
    if (op.approachPoint) {
      const [sx, sy] = project2(op.approachPoint[0], op.approachPoint[1]);
      ctx.beginPath();
      ctx.arc(sx, sy, 6, 0, Math.PI * 2);
      ctx.fillStyle = markerColor;
      ctx.fill();
      ctx.lineWidth = 1.5;
      ctx.strokeStyle = ringColor;
      ctx.stroke();
      // Inner dot for precision feel.
      ctx.beginPath();
      ctx.arc(sx, sy, 1.5, 0, Math.PI * 2);
      ctx.fillStyle = ringColor;
      ctx.fill();
    }

    // Live preview during pick / drag.
    if ((approachPickActive || approachDrag != null) && approachPreview) {
      const [sx, sy] = project2(approachPreview.x, approachPreview.y);
      const color = approachPreview.snap ? snapColor : markerColor;
      // Dashed ring while picking (vs solid for the committed point)
      // so the user sees clearly which is provisional.
      ctx.save();
      if (!op.approachPoint) {
        // No committed point yet — make the preview the focal element.
        ctx.beginPath();
        ctx.arc(sx, sy, 6, 0, Math.PI * 2);
        ctx.fillStyle = color;
        ctx.globalAlpha = 0.5;
        ctx.fill();
        ctx.globalAlpha = 1;
      }
      ctx.setLineDash([3, 3]);
      ctx.lineWidth = 1.5;
      ctx.strokeStyle = color;
      ctx.beginPath();
      ctx.arc(sx, sy, 9, 0, Math.PI * 2);
      ctx.stroke();
      ctx.setLineDash([]);
      // Snap glyph by kind (64p):
      //   endpoint     → ■ filled square
      //   midpoint     → ▲ filled triangle
      //   intersection → ✕ diagonal cross
      //   center       → ◯ ring
      //   grid         → + plus sign
      if (approachPreview.snap) {
        drawOSnapGlyph(ctx, sx, sy, approachPreview.snap, snapColor);
      }
      ctx.restore();
    }
  }

  /// Paint the OSnap classification glyph (64p) at canvas position
  /// (sx, sy). The glyph reads at a glance which CAD feature the
  /// cursor latched onto.
  function drawOSnapGlyph(
    ctx: CanvasRenderingContext2D,
    sx: number,
    sy: number,
    kind: OSnapCandidate['kind'],
    color: string,
  ): void {
    ctx.strokeStyle = color;
    ctx.fillStyle = color;
    ctx.lineWidth = 1.5;
    const r = 7;
    switch (kind) {
      case 'endpoint': {
        // Filled square outline.
        ctx.beginPath();
        ctx.rect(sx - r, sy - r, r * 2, r * 2);
        ctx.stroke();
        break;
      }
      case 'midpoint': {
        // Triangle pointing up, outline only.
        ctx.beginPath();
        ctx.moveTo(sx, sy - r);
        ctx.lineTo(sx + r, sy + r * 0.8);
        ctx.lineTo(sx - r, sy + r * 0.8);
        ctx.closePath();
        ctx.stroke();
        break;
      }
      case 'intersection': {
        // Diagonal cross.
        ctx.beginPath();
        ctx.moveTo(sx - r, sy - r);
        ctx.lineTo(sx + r, sy + r);
        ctx.moveTo(sx - r, sy + r);
        ctx.lineTo(sx + r, sy - r);
        ctx.stroke();
        break;
      }
      case 'center': {
        // Ring (concentric with the preview ring, slightly smaller).
        ctx.beginPath();
        ctx.arc(sx, sy, r * 0.7, 0, Math.PI * 2);
        ctx.stroke();
        break;
      }
      case 'grid': {
        // Axis-aligned plus.
        ctx.beginPath();
        ctx.moveTo(sx - r, sy);
        ctx.lineTo(sx + r, sy);
        ctx.moveTo(sx, sy - r);
        ctx.lineTo(sx, sy + r);
        ctx.stroke();
        break;
      }
    }
  }

  /// Render every TextLayer's cached preview segments. Each layer's
  /// segments live on the synthetic layer `__text_<id>`; selection
  /// state is the text-list's `selectedTextLayerId`. The active layer
  /// gets a bright halo + accent stroke; idle layers render in the
  /// muted assigned-other tint so they're visible but don't draw the
  /// eye.
  function drawTextPreview(
    ctx: CanvasRenderingContext2D,
    p: (x: number, y: number) => [number, number],
    accent: string,
    _hoverColor: string,
    haloColor: string,
  ) {
    const activeColor = themeVar('--obj-assigned-active', '#39c75c');
    const idleColor = themeVar('--obj-assigned-other', '#2a6f3b');
    for (const layer of project.textLayers) {
      const segs = previewSegmentsFor(layer.id);
      if (!segs || segs.length === 0) continue;
      const isActive = project.selectedTextLayerId === layer.id;
      const baseWidth = isActive ? 1.8 : 1.4;
      const haloAlpha = isActive ? 0.55 : 0.3;
      for (const seg of segs) {
        const prevAlpha = ctx.globalAlpha;
        ctx.globalAlpha = haloAlpha;
        ctx.lineWidth = baseWidth + 2.5;
        ctx.strokeStyle = haloColor;
        drawSegment(ctx, seg, p);
        ctx.globalAlpha = prevAlpha;
        ctx.lineWidth = baseWidth;
        ctx.strokeStyle = isActive ? accent : idleColor;
        drawSegment(ctx, seg, p);
      }
    }
  }

  /// Translucent rectangle for the active box-select drag (canvas
  /// coords). Drawn last so it sits above everything else.
  function drawBoxSelect(ctx: CanvasRenderingContext2D, accent: string) {
    if (!boxSelect) return;
    const x = Math.min(boxSelect.startX, boxSelect.curX);
    const y = Math.min(boxSelect.startY, boxSelect.curY);
    const w = Math.abs(boxSelect.curX - boxSelect.startX);
    const h = Math.abs(boxSelect.curY - boxSelect.startY);
    ctx.save();
    ctx.fillStyle = `${accent}22`;
    ctx.strokeStyle = accent;
    ctx.lineWidth = 1;
    ctx.setLineDash([4, 3]);
    ctx.fillRect(x, y, w, h);
    ctx.strokeRect(x, y, w, h);
    ctx.restore();
  }

  /// Paint each fixture as a translucent filled outline in its declared
  /// color. Selected fixture gets a thicker accent stroke so it's
  /// obvious which one the sidebar is editing.
  function drawFixtures(
    ctx: CanvasRenderingContext2D,
    p: (x: number, y: number) => [number, number],
  ) {
    if (!project.fixtures || project.fixtures.length === 0) return;
    const accent = themeVar('--accent', '#2d6cdf');
    for (const f of project.fixtures) {
      const colorPacked = f.color ?? 0xffa050c0;
      const r = (colorPacked >>> 24) & 0xff;
      const g = (colorPacked >>> 16) & 0xff;
      const b = (colorPacked >>> 8) & 0xff;
      const a = colorPacked & 0xff;
      const fill = `rgba(${r}, ${g}, ${b}, ${Math.max(0.15, (a / 255) * 0.5)})`;
      const stroke = `rgb(${r}, ${g}, ${b})`;
      const isSel = project.selectedFixtureId === f.id;
      ctx.fillStyle = fill;
      ctx.strokeStyle = isSel ? accent : stroke;
      ctx.lineWidth = isSel ? 2.4 : 1.4;
      const [ox, oy] = f.origin;
      if (f.kind.shape === 'box') {
        const hw = f.kind.width / 2;
        const hd = f.kind.depth / 2;
        const [x0, y0] = p(ox - hw, oy - hd);
        const [x1, y1] = p(ox + hw, oy + hd);
        const xMin = Math.min(x0, x1);
        const yMin = Math.min(y0, y1);
        const w = Math.abs(x1 - x0);
        const h = Math.abs(y1 - y0);
        ctx.fillRect(xMin, yMin, w, h);
        ctx.strokeRect(xMin, yMin, w, h);
      } else if (f.kind.shape === 'cylinder') {
        const [cx, cy] = p(ox, oy);
        const [edgeX] = p(ox + f.kind.radius, oy);
        const rPx = Math.abs(edgeX - cx);
        ctx.beginPath();
        ctx.arc(cx, cy, rPx, 0, Math.PI * 2);
        ctx.fill();
        ctx.stroke();
      } else if (f.kind.shape === 'polygon') {
        if (f.kind.vertices.length < 2) continue;
        ctx.beginPath();
        const [vx0, vy0] = p(ox + f.kind.vertices[0][0], oy + f.kind.vertices[0][1]);
        ctx.moveTo(vx0, vy0);
        for (let i = 1; i < f.kind.vertices.length; i++) {
          const [vx, vy] = p(ox + f.kind.vertices[i][0], oy + f.kind.vertices[i][1]);
          ctx.lineTo(vx, vy);
        }
        ctx.closePath();
        ctx.fill();
        ctx.stroke();
      }
    }
  }

  /// Path2D cache for region previews. Tracing each region's polygons by
  /// hand on every redraw was O(total tessellated points) per draw, which
  /// fires on hover, selection, layer toggle, etc. We build the Path2D
  /// objects once in *data space* (no canvas transform applied) and
  /// stamp them with ctx.setTransform during draw — re-rebuilt only when
  /// project.generated.regions actually changes.
  type RegionPath = {
    op_id: number;
    path: Path2D;
  };
  let regionPathCache: { regionsRef: unknown; paths: RegionPath[] } | null = null;

  function regionPaths(regions: NonNullable<typeof project.generated>['regions']): RegionPath[] {
    if (regionPathCache && regionPathCache.regionsRef === regions) {
      return regionPathCache.paths;
    }
    const paths: RegionPath[] = (regions ?? []).map((r) => {
      const path = new Path2D();
      tracePolygonInto(path, r.outer);
      for (const hole of r.holes ?? []) tracePolygonInto(path, hole);
      return { op_id: r.op_id, path };
    });
    regionPathCache = { regionsRef: regions, paths };
    return paths;
  }

  /// Paint each region's outer polygon and punch its holes via the
  /// even-odd fill rule. The selected op's region is drawn in accent so
  /// the user can spot it; others fade so the canvas doesn't get loud.
  function drawRegions(
    ctx: CanvasRenderingContext2D,
    regions: NonNullable<typeof project.generated>['regions'],
    scale: number,
    offX: number,
    offY: number,
  ) {
    const accent = themeVar('--accent', '#2d6cdf');
    const muted = themeVar('--text-muted', '#9aa0aa');
    const paths = regionPaths(regions);
    // Compose data → canvas transform on top of the existing dpr scale.
    // Y is flipped (canvas y-down vs DXF y-up) so we use -scale on Y +
    // offY as the canvas-space origin of data-y=0.
    ctx.save();
    ctx.transform(scale, 0, 0, -scale, offX, offY);
    for (const rp of paths) {
      const isSelected = project.selectedOpId === rp.op_id;
      ctx.fillStyle = isSelected
        ? `${accent}33` // ~20% alpha
        : `${muted}1a`; // ~10% alpha
      ctx.fill(rp.path, 'evenodd');
    }
    ctx.restore();
  }

  function tracePolygonInto(path: Path2D, pts: Array<{ x: number; y: number }>) {
    if (pts.length < 3) return;
    path.moveTo(pts[0].x, pts[0].y);
    for (let i = 1; i < pts.length; i++) {
      path.lineTo(pts[i].x, pts[i].y);
    }
    path.closePath();
  }

  function drawTabs(
    ctx: CanvasRenderingContext2D,
    p: (x: number, y: number) => [number, number],
    scale: number,
  ) {
    const tabFill = themeVar('--tab-marker', '#ffd23a');
    const tabAuto = themeVar('--tab-auto', '#ffeb88');
    const tabStroke = themeVar('--bg-app', '#0d0d0d');
    const objects = getObjectPolylines();
    // Walk every op with tabs ON: render auto-spaced (per kind),
    // manual placements, and the ghost (if the selected op). Tabs
    // are only meaningful for closed-contour ops (profile + pocket),
    // so narrow first.
    for (const op of project.operations) {
      if (!isContourOp(op)) continue;
      const mode = op.tabMode?.kind ?? 'off';
      const tabsActive = op.tabsActive ?? false;
      // Skip ops with no tabs to draw.
      if (mode === 'off' && (op.tabPlacements?.length ?? 0) === 0 && !tabsActive) continue;
      const allowedObjects = op.sourceObjects;
      const objFilter = (id: number) =>
        !allowedObjects || allowedObjects.length === 0 || allowedObjects.includes(id);
      // Manual / Mixed placements.
      if (mode === 'manual' || mode === 'mixed') {
        for (const tp of op.tabPlacements ?? []) {
          const obj = objects.find((o) => o.objectId === tp.objectId);
          if (!obj || !objFilter(obj.objectId)) continue;
          const { point, tangent } = polylineAtT(obj.pts, tp.t, obj.closed);
          drawTabMarker(
            ctx,
            p,
            scale,
            point.x,
            point.y,
            tangent.x,
            tangent.y,
            tp.widthOverrideMm ?? op.tabWidth ?? 10,
            tp.heightOverrideMm ?? op.tabHeight ?? 1,
            tabFill,
            tabStroke,
            'manual',
          );
        }
      }
      // Auto / Mixed: N evenly spaced tabs per allowed object.
      if (op.tabMode?.kind === 'auto' || op.tabMode?.kind === 'mixed') {
        const count = op.tabMode.kind === 'auto' ? op.tabMode.count : op.tabMode.auto_count;
        if (count > 0) {
          for (const obj of objects) {
            if (!objFilter(obj.objectId)) continue;
            const ts = obj.closed
              ? Array.from({ length: count }, (_, i) => i / count)
              : Array.from({ length: count }, (_, i) => (i + 0.5) / count);
            for (const t of ts) {
              const { point, tangent } = polylineAtT(obj.pts, t, obj.closed);
              drawTabMarker(
                ctx,
                p,
                scale,
                point.x,
                point.y,
                tangent.x,
                tangent.y,
                op.tabWidth ?? 10,
                op.tabHeight ?? 1,
                tabAuto,
                tabStroke,
                'auto',
              );
            }
          }
        }
      }
    }
    // Ghost (selected op + manual/mixed mode + cursor over contour).
    if (ghostTab && tabPlacementActive && selectedOp && isContourOp(selectedOp)) {
      const obj = objects.find((o) => o.objectId === ghostTab!.objectId);
      if (obj) {
        const { tangent } = polylineAtT(obj.pts, ghostTab.t, obj.closed);
        ctx.save();
        ctx.globalAlpha = 0.4;
        ctx.setLineDash([4, 3]);
        drawTabMarker(
          ctx,
          p,
          scale,
          ghostTab.x,
          ghostTab.y,
          tangent.x,
          tangent.y,
          selectedOp.tabWidth ?? 10,
          selectedOp.tabHeight ?? 1,
          tabFill,
          tabStroke,
          'manual',
        );
        ctx.restore();
        // Snap indicator (1q3): a small accent dot next to the
        // ghost when the cursor caught a secondary snap target
        // (vertex / midpoint / existing tab).
        if (ghostTab.snap !== 'contour') {
          const [gx, gy] = p(ghostTab.x, ghostTab.y);
          const accent = themeVar('--accent', '#2d6cdf');
          ctx.beginPath();
          ctx.arc(gx, gy, 3.5, 0, Math.PI * 2);
          ctx.fillStyle = accent;
          ctx.fill();
          ctx.lineWidth = 1;
          ctx.strokeStyle = themeVar('--bg-app', '#0d0d0d');
          ctx.stroke();
        }
      }
    }
  }

  /// Draw one tab marker oriented along the contour tangent. Falls
  /// back to a 6-px pill when the data-space size collapses too small
  /// on screen so the marker stays visible at extreme zoom-out.
  function drawTabMarker(
    ctx: CanvasRenderingContext2D,
    p: (x: number, y: number) => [number, number],
    scale: number,
    dataX: number,
    dataY: number,
    tanX: number,
    tanY: number,
    widthMm: number,
    heightMm: number,
    fill: string,
    stroke: string,
    _kind: 'auto' | 'manual',
  ) {
    const [cx, cy] = p(dataX, dataY);
    const halfLenPx = Math.max(3, widthMm * 0.5 * scale);
    const halfThickPx = Math.max(2, heightMm * scale);
    // Canvas Y is flipped vs data Y. Mirror the tangent Y so the
    // rendered orientation matches the contour in screen space.
    const txPx = tanX;
    const tyPx = -tanY;
    const tLen = Math.hypot(txPx, tyPx) || 1;
    const ux = txPx / tLen;
    const uy = tyPx / tLen;
    // Perpendicular (left of tangent in canvas space).
    const px = -uy;
    const py = ux;
    ctx.beginPath();
    const corners: [number, number][] = [
      [cx - ux * halfLenPx - px * halfThickPx, cy - uy * halfLenPx - py * halfThickPx],
      [cx + ux * halfLenPx - px * halfThickPx, cy + uy * halfLenPx - py * halfThickPx],
      [cx + ux * halfLenPx + px * halfThickPx, cy + uy * halfLenPx + py * halfThickPx],
      [cx - ux * halfLenPx + px * halfThickPx, cy - uy * halfLenPx + py * halfThickPx],
    ];
    ctx.moveTo(corners[0][0], corners[0][1]);
    for (let i = 1; i < corners.length; i++) ctx.lineTo(corners[i][0], corners[i][1]);
    ctx.closePath();
    ctx.fillStyle = fill;
    ctx.fill();
    ctx.lineWidth = 1.25;
    ctx.strokeStyle = stroke;
    ctx.stroke();
  }

  function drawSegment(
    ctx: CanvasRenderingContext2D,
    seg: Segment,
    p: (x: number, y: number) => [number, number],
  ) {
    const [sx, sy] = p(seg.start.x, seg.start.y);
    const [ex, ey] = p(seg.end.x, seg.end.y);

    if (seg.type === 'POINT') {
      ctx.fillStyle = ctx.strokeStyle;
      ctx.beginPath();
      ctx.arc(sx, sy, 2, 0, Math.PI * 2);
      ctx.fill();
      return;
    }

    if (Math.abs(seg.bulge) < 1e-9) {
      ctx.beginPath();
      ctx.moveTo(sx, sy);
      ctx.lineTo(ex, ey);
      ctx.stroke();
      return;
    }

    // Bulge-based arc. Recompute center for robustness — the importer
    // sometimes leaves center=(0,0) on bulged polyline segments.
    const dx = seg.end.x - seg.start.x;
    const dy = seg.end.y - seg.start.y;
    const chord = Math.hypot(dx, dy);
    if (chord < 1e-9) return;
    const bulge = seg.bulge;
    const sagitta = (bulge * chord) / 2;
    // Radius from chord and sagitta.
    const radius = (chord / 2) ** 2 / (2 * Math.abs(sagitta)) + Math.abs(sagitta) / 2;
    // Midpoint of the chord.
    const mx = (seg.start.x + seg.end.x) / 2;
    const my = (seg.start.y + seg.end.y) / 2;
    // Perpendicular unit vector pointing toward the center.
    const ux = -dy / chord;
    const uy = dx / chord;
    // Offset from midpoint to center.
    const h = radius - Math.abs(sagitta);
    const sign = bulge > 0 ? 1 : -1;
    const cx = mx + ux * h * sign;
    const cy = my + uy * h * sign;

    const startAng = Math.atan2(seg.start.y - cy, seg.start.x - cx);
    const endAng = Math.atan2(seg.end.y - cy, seg.end.x - cx);
    const counterClockwise = bulge > 0;

    const [pcx, pcy] = p(cx, cy);
    const r = radius * (sx === ex && sy === ey ? 1 : Math.abs((sx - pcx) / (seg.start.x - cx)));
    // Reverse the y-flip on angles for canvas coords.
    ctx.beginPath();
    ctx.arc(pcx, pcy, r, -startAng, -endAng, counterClockwise);
    ctx.stroke();
  }

  function drawGrid(
    ctx: CanvasRenderingContext2D,
    w: number,
    h: number,
    scale: number,
    offX: number,
    offY: number,
  ) {
    // Major grid every 10 units, minor every 1, when the unit is small enough.
    const majorStep = 10;
    const minorStep = 1;
    const px = Math.abs(scale * minorStep);
    if (px < 6) return; // too tight to be useful
    ctx.lineWidth = 1;
    const minorColor = themeVar('--grid-minor', '#1a1a1a');
    const majorColor = themeVar('--grid-major', '#262626');
    for (const [step, color] of [
      [minorStep, minorColor],
      [majorStep, majorColor],
    ] as const) {
      ctx.strokeStyle = color;
      const start = Math.floor(-offX / scale / step) * step;
      const end = Math.ceil((w - offX) / scale / step) * step;
      ctx.beginPath();
      for (let x = start; x <= end; x += step) {
        const X = x * scale + offX;
        ctx.moveTo(X, 0);
        ctx.lineTo(X, h);
      }
      const ystart = Math.floor((offY - h) / scale / step) * step;
      const yend = Math.ceil(offY / scale / step) * step;
      for (let y = ystart; y <= yend; y += step) {
        const Y = offY - y * scale;
        ctx.moveTo(0, Y);
        ctx.lineTo(w, Y);
      }
      ctx.stroke();
    }
  }

  function drawAxes(
    ctx: CanvasRenderingContext2D,
    w: number,
    h: number,
    offX: number,
    offY: number,
  ) {
    ctx.lineWidth = 1.5;
    ctx.strokeStyle = themeVar('--axis-x', '#882222');
    ctx.beginPath();
    ctx.moveTo(0, offY);
    ctx.lineTo(w, offY);
    ctx.stroke();
    ctx.strokeStyle = themeVar('--axis-y', '#226622');
    ctx.beginPath();
    ctx.moveTo(offX, 0);
    ctx.lineTo(offX, h);
    ctx.stroke();
  }

  /// Dashed rectangle showing the machine work-area envelope in the
  /// XY plane (0,0) → (workArea.x, workArea.y). Sits under the
  /// imported geometry so the user always sees the cuttable area
  /// regardless of what's loaded. Pairs with the dashed wireframe
  /// the 3D scene draws for the full XYZ envelope.
  function drawWorkArea(
    ctx: CanvasRenderingContext2D,
    p: (x: number, y: number) => [number, number],
  ) {
    const wa = project.machine.workArea;
    if (!wa || wa.x <= 0 || wa.y <= 0) return;
    const [x0, y0] = p(0, 0);
    const [x1, y1] = p(wa.x, wa.y);
    const minX = Math.min(x0, x1);
    const maxX = Math.max(x0, x1);
    const minY = Math.min(y0, y1);
    const maxY = Math.max(y0, y1);
    ctx.save();
    ctx.lineWidth = 1.2;
    ctx.strokeStyle = themeVar('--text-muted', '#888');
    ctx.setLineDash([6, 4]);
    ctx.globalAlpha = 0.75;
    ctx.strokeRect(minX, minY, maxX - minX, maxY - minY);
    ctx.restore();
  }
</script>

<svelte:window
  onkeydown={(e) => {
    onCtxKeydown(e);
    if (e.key === 'Alt' || e.altKey) altDown = true;
    if (e.key === 'Shift' || e.shiftKey) shiftDown = true;
  }}
  onkeyup={(e) => {
    if (e.key === 'Alt' || !e.altKey) altDown = false;
    if (e.key === 'Shift' || !e.shiftKey) shiftDown = false;
  }}
  onblur={() => {
    altDown = false;
    shiftDown = false;
  }}
  onclick={onCtxDocClick}
/>
<div class="canvas-host" bind:this={container}>
  <canvas
    bind:this={canvas}
    class="bg"
    onpointermove={onPointerMove}
    onpointerleave={onPointerLeave}
    onpointerdown={onPointerDown}
    onpointerup={onPointerUp}
    onpointercancel={onPointerUp}
    oncontextmenu={onContextMenu}
    onwheel={onWheel}
    ondblclick={onDblClick}
  ></canvas>
  <canvas bind:this={canvasOverlay} class="overlay"></canvas>
  {#if project.selectedEntities.size > 0}
    <div class="selection-hud">{project.selectedEntities.size} selected · esc to clear</div>
  {/if}
  {#if cursorXY}
    <div class="cursor-hud" aria-hidden="true">
      x: {cursorXY.x.toFixed(2)} &nbsp; y: {cursorXY.y.toFixed(2)} mm
    </div>
  {/if}
  {#if tabPopover}
    {@const op = project.operations.find((o) => o.id === tabPopover!.opId)}
    {@const placement = op && isContourOp(op) ? op.tabPlacements?.[tabPopover!.placementIdx] : null}
    {#if op && isContourOp(op) && placement}
      <div
        class="tab-popover"
        style:left={`${tabPopover.x}px`}
        style:top={`${tabPopover.y}px`}
        role="dialog"
      >
        <div class="tab-popover-header">Tab on op #{op.id}</div>
        <label class="tab-popover-row">
          <span>Width</span>
          <input
            type="number"
            step="0.5"
            min="0.1"
            placeholder={String(op.tabWidth ?? 10)}
            value={placement.widthOverrideMm ?? ''}
            oninput={(e) => {
              const raw = (e.target as HTMLInputElement).value;
              const v = raw === '' ? undefined : parseFloat(raw);
              patchTabOverride(tabPopover!.opId, tabPopover!.placementIdx, {
                widthOverrideMm: v === undefined || isNaN(v) ? undefined : v,
              });
            }}
          />
          <span class="unit">mm</span>
        </label>
        <label class="tab-popover-row">
          <span>Height</span>
          <input
            type="number"
            step="0.1"
            min="0.1"
            placeholder={String(op.tabHeight ?? 1)}
            value={placement.heightOverrideMm ?? ''}
            oninput={(e) => {
              const raw = (e.target as HTMLInputElement).value;
              const v = raw === '' ? undefined : parseFloat(raw);
              patchTabOverride(tabPopover!.opId, tabPopover!.placementIdx, {
                heightOverrideMm: v === undefined || isNaN(v) ? undefined : v,
              });
            }}
          />
          <span class="unit">mm</span>
        </label>
        <button
          type="button"
          class="tab-popover-delete"
          onclick={() => deleteTabPlacement(tabPopover!.opId, tabPopover!.placementIdx)}
          >Delete tab</button
        >
        <button type="button" class="tab-popover-close" aria-label="Close" onclick={closeTabPopover}
          >×</button
        >
      </div>
    {/if}
  {/if}
  {#if ctxMenu}
    {@const hasTextSelected = project.selectedTextLayerId != null}
    {@const hasObjsSelected = project.selectedObjects.size > 0}
    {#if !hasTextSelected && !hasObjsSelected}
      <div
        class="ctx-menu empty"
        style:left={`${ctxMenu.x}px`}
        style:top={`${ctxMenu.y}px`}
        role="menu"
      >
        <p class="ctx-hint">
          Select objects to add an operation, or a text layer to reposition it.
        </p>
        <button type="button" onclick={closeCtxMenu}>Dismiss</button>
      </div>
    {:else}
      <div class="ctx-menu" style:left={`${ctxMenu.x}px`} style:top={`${ctxMenu.y}px`} role="menu">
        {#if hasTextSelected}
          <div class="ctx-header">Text layer</div>
          <button
            type="button"
            class="ctx-item"
            onclick={setTextOriginHere}
            title="Move the selected text layer's left-baseline origin to the right-clicked spot."
          >
            Set text origin here
          </button>
          {#if hasObjsSelected}
            <div class="ctx-divider"></div>
          {/if}
        {/if}
        {#if hasObjsSelected}
          <div class="ctx-header">New operation from selection</div>
          <OpKindPicker onPick={pickFromCtx} />
        {/if}
      </div>
    {/if}
  {/if}
  {#if onShowHelp}
    <button
      type="button"
      class="help-btn"
      onclick={onShowHelp}
      title="Keyboard & mouse shortcuts (?)"
      aria-label="Show keyboard and mouse shortcuts">?</button
    >
  {/if}
</div>

<style>
  .canvas-host {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    background: var(--bg-app);
  }
  canvas {
    display: block;
    user-select: none;
    touch-action: none;
  }
  /* Stack bg + overlay canvases so the overlay paints state-bearing
     items (selection / hover / ghost tab / fixtures / tabs / approach /
     box-select / OSnap glyph) without invalidating the heavy imported-
     geometry layer. The overlay must not eat pointer events. */
  canvas.bg,
  canvas.overlay {
    position: absolute;
    top: 0;
    left: 0;
  }
  canvas.overlay {
    pointer-events: none;
  }
  .selection-hud {
    position: absolute;
    top: 0.5rem;
    left: 0.5rem;
    background: color-mix(in srgb, var(--accent) 80%, transparent);
    color: white;
    padding: 0.2rem 0.5rem;
    border-radius: 3px;
    font-size: 0.72rem;
    pointer-events: none;
  }
  /* 7tp5: cursor world-coordinate HUD. Top-right corner so it doesn't
     fight the selection-hud (top-left). Monospace tabular-nums so the
     numbers don't dance as the cursor moves. */
  .cursor-hud {
    position: absolute;
    top: 0.5rem;
    right: 0.5rem;
    background: color-mix(in srgb, var(--bg-elevated) 85%, transparent);
    color: var(--text);
    padding: 0.2rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 3px;
    font-size: 0.72rem;
    font-family: ui-monospace, monospace;
    font-variant-numeric: tabular-nums;
    pointer-events: none;
    white-space: nowrap;
  }
  .ctx-menu {
    position: absolute;
    min-width: 16rem;
    max-width: 22rem;
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.35);
    z-index: var(--z-floating);
    padding: 0.25rem;
  }
  .tab-popover {
    position: absolute;
    min-width: 11rem;
    max-width: 14rem;
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.4);
    z-index: var(--z-floating);
    padding: 0.55rem 0.6rem 0.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    font-size: 0.78rem;
  }
  .tab-popover-header {
    font-size: 0.7rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-bottom: 0.2rem;
  }
  .tab-popover-row {
    display: grid;
    grid-template-columns: 3.5rem 1fr auto;
    gap: 0.35rem;
    align-items: center;
  }
  .tab-popover-row input {
    width: 100%;
    padding: 0.15rem 0.3rem;
  }
  .tab-popover-row .unit {
    color: var(--text-muted);
    font-size: 0.7rem;
  }
  .tab-popover-delete {
    margin-top: 0.3rem;
    background: transparent;
    color: var(--danger, #c44);
    border: 1px solid var(--danger, #c44);
    border-radius: 3px;
    padding: 0.25rem 0.5rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .tab-popover-delete:hover {
    background: color-mix(in srgb, var(--danger, #c44) 15%, transparent);
  }
  .tab-popover-close {
    position: absolute;
    top: 0.25rem;
    right: 0.3rem;
    background: transparent;
    color: var(--text-muted);
    border: 0;
    font-size: 1rem;
    cursor: pointer;
    line-height: 1;
    padding: 0 0.3rem;
  }
  .ctx-header {
    font-size: 0.68rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 0.25rem 0.45rem 0.3rem;
  }
  .ctx-item {
    background: transparent;
    color: var(--text);
    border: 0;
    padding: 0.3rem 0.55rem;
    font-size: 0.78rem;
    text-align: left;
    cursor: pointer;
    border-radius: 3px;
    margin: 0 0.2rem;
  }
  .ctx-item:hover {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .ctx-divider {
    height: 1px;
    background: var(--border);
    margin: 0.2rem 0.1rem;
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
  .help-btn {
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
    font-size: 0.85rem;
    font-weight: bold;
    line-height: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    opacity: 0.7;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }
  .help-btn:hover,
  .help-btn:focus {
    opacity: 1;
    color: var(--text-strong);
  }
</style>
