<script lang="ts">
  import { onMount } from 'svelte';
  import { project, isContourOp, type OpEntry } from '../state/project.svelte';
  import { opSourceCss } from '../state/op-color';
  import { STOCK_OUTLINE_LAYER } from '../state/stock-outline';
  import { buildObjectPolylines, polylineAtT, type ObjectPolyline } from '../cam/tabs';
  import type { Segment, BBox } from '../api/types';
  import {
    buildHitIndex as buildHitIndexPure,
    queryHit,
    type HitIndex,
  } from '../canvas/spatial-index';
  import { fixtureAt } from '../canvas/fixture-hit';
  import { projectGhostTab, type GhostTab } from '../canvas/ghost-tab';
  import { reduceCanvasClick } from '../canvas/entity-selection';
  import { computeViewportTransform, placementsBBox, type Rect } from '../canvas/viewport';
  import {
    applyPinch,
    withinTapTolerance,
    LONG_PRESS_MS,
    type PointerPos,
  } from '../canvas/touch-gestures';
  import { objectsContainedInBox } from '../canvas/box_select';
  import { resolveAci, hexToCss } from '../canvas/aci-color';
  import { unpackFixtureColor } from '../canvas/fixture-color';
  import {
    DEFAULT_OSNAP_SETTINGS,
    findOSnap,
    precomputeOSnapTargets,
    type OSnapCandidate,
    type OSnapTargets,
  } from '../canvas/osnap';
  import OpKindPicker, { PICKER_LABEL, type PickerKind } from './OpKindPicker.svelte';
  import { computeFootprint } from '../sim/driver';
  import { previewSegmentsFor, previewVersion, requestPreview } from '../state/text_preview.svelte';
  import { brightnessToRgba } from '../state/raster_preview';
  import type { ReliefSource, TextLayer } from '../state/project-types';

  interface Props {
    onShowHelp?: () => void;
    /// Hint the sidebar to switch its accordion to a given pane.
    /// Used by right-click "Add op from selection" so the new op
    /// row is visible in the Operations panel without an extra
    /// click on the sidebar.
    onActivateSidebarPane?: (pane: 'stock' | 'layers' | 'text' | 'operations') => void;
  }
  let { onShowHelp, onActivateSidebarPane }: Props = $props();

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
  /// Theme-observed CSS-var cache. `resetThemeCache()` clears it when
  /// the MutationObserver in onMount sees a `data-theme` change.
  let themeCache = new Map<string, string>();
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
    // Defer the resize-driven redraw to the next animation frame.
    // ResizeObserver fires synchronously during layout; if the
    // callback mutates the observed element's children (which
    // drawBoth does — it resizes the canvases inside `container`),
    // the browser logs "ResizeObserver loop completed with
    // undelivered notifications" and skips dispatching the next
    // batch. Coalescing into one rAF eliminates the warning and
    // makes the redraw cost predictable across multi-event resizes.
    let resizeFrame = 0;
    const ro = new ResizeObserver(() => {
      if (resizeFrame !== 0) return;
      resizeFrame = requestAnimationFrame(() => {
        resizeFrame = 0;
        drawBoth();
      });
    });
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
    void project.geometryView;
    void project.visibleLayers;
    void project.regionsVisible;
    void project.generated;
    void project.textLayers;
    void project.selectedTextLayerId;
    void previewVersion.v;
    void project.machine.workArea;
    void project.stock;
    void project.settings.previewLineWidth;
    // rt1.12 (fvb0): a raster-engrave-only project (no imported geometry)
    // draws its grid / axes fit to the placement bbox, so the bg must
    // react to sources / ops appearing / moving. Same per-frame cost as
    // a pan (which already repaints this layer), so no new regression.
    void project.reliefSources;
    void project.operations;
    void userZoom;
    void userPanX;
    void userPanY;
    drawBackground();
  });

  $effect(() => {
    void project.geometryView;
    void project.visibleLayers;
    void project.selectedObjects;
    void project.operations;
    void project.selectedOpId;
    void project.fixtures;
    void project.selectedFixtureId;
    void project.selectedTextLayerId;
    // rt1.12 (j7b4): raster-engrave placement images live on the overlay
    // (faint, under the interaction chrome) so the heavy bg layer stays
    // pure. Repaint when a source moves / resizes.
    void project.reliefSources;
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
    project.pickMode?.kind === 'approach-point' && project.pickMode.opId === selectedOp?.id,
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

  /// rt1.12 (j7b4): drag state for repositioning a raster-engrave
  /// placement image. `grabDX/DY` is the data-space offset between the
  /// pointer and the source origin at grab time, so the origin tracks
  /// the cursor without jumping. Committed live (coalesced into one undo
  /// entry) on every move, mirroring the approach-marker drag.
  let rasterDrag = $state<{
    sourceId: number;
    pointerId: number;
    grabDX: number;
    grabDY: number;
  } | null>(null);

  /// rt1.12 (ywf9): drag state for repositioning a text layer's origin.
  /// `grabDX/DY` is the pointer→origin offset at grab time so the origin
  /// tracks the cursor. Committed live (coalesced — see
  /// coalesceKeyForTextPatch) on every move, mirroring the raster drag.
  let textDrag = $state<{
    id: number;
    pointerId: number;
    grabDX: number;
    grabDY: number;
  } | null>(null);

  /// Cache of the decoded brightness image per relief source, keyed by
  /// source id. Invalidated when the source's `brightness` array
  /// reference changes (origin / cell edits keep the same array, so a
  /// drag never rebuilds the 256² ImageData).
  const rasterImageCache = new Map<
    number,
    { brightness: readonly number[]; canvas: HTMLCanvasElement }
  >();

  /// Precomputed OSnap target collection. Rebuilt only when the
  /// imported geometry changes — never per pointermove. (64p.)
  const osnapTargets = $derived<OSnapTargets>(
    approachPickActive || approachDrag != null
      ? precomputeOSnapTargets(project.geometryView)
      : { endpoints: [], midpoints: [], intersections: [], centers: [] },
  );

  /// OSnap settings come from `project.settings.osnap` (li0m). Falls
  /// back to the hardcoded defaults for the brief window between
  /// component mount and the settings hydration completing.
  const osnapSettings = $derived(project.settings.osnap ?? DEFAULT_OSNAP_SETTINGS);

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

  /// bwt7 — touch gesture bookkeeping. These are plain (non-reactive)
  /// fields: they drive the reactive `userZoom/Pan*` fields, but nothing
  /// renders them directly, so they don't need `$state`.
  ///
  /// `activePointers` maps every live touch pointerId → its last
  /// canvas-relative position, so a second finger landing turns the pair
  /// into a pinch/pan. `pinch` holds the prior two-finger frame the next
  /// move diffs against. `longPress*` arm the hold→context-menu timer.
  const activePointers = new Map<number, PointerPos>();
  let pinch: { idA: number; idB: number; prevA: PointerPos; prevB: PointerPos } | null = null;
  let longPressTimer: ReturnType<typeof setTimeout> | null = null;
  let longPressStart: PointerPos | null = null;

  /// bwt7: keyboardless multi-select. On a touchscreen with no hardware
  /// keyboard, shift/ctrl-tap are unreachable, so this toggle makes a
  /// plain tap behave like ctrl-click (toggle into the selection).
  /// Surfaced as an overlay button on touch-capable devices only.
  let addToSelection = $state(false);
  const isTouchDevice = typeof navigator !== 'undefined' && (navigator.maxTouchPoints ?? 0) > 0;

  /// Cancel a pending long-press hold (finger moved, lifted, or a second
  /// finger arrived). Idempotent.
  function cancelLongPress() {
    if (longPressTimer != null) {
      clearTimeout(longPressTimer);
      longPressTimer = null;
    }
    longPressStart = null;
  }

  /// Reset pan + zoom when the imported file changes (different filename
  /// or going from no-import to imported). Keeps mid-session zooms
  /// intact across normal redraws.
  let _lastImportedKey: string | null = null;
  $effect(() => {
    const key = project.transformedImport?.filename ?? null;
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
    void project.geometryView;
    hitIndex = buildHitIndexPure(project.geometryView);
  });

  function pixelHit(canvasX: number, canvasY: number): number | null {
    const data = project.geometryView;
    if (!data || !lastTransform) return null;
    const { scale, offX, offY } = lastTransform;
    const dataX = (canvasX - offX) / scale;
    const dataY = (offY - canvasY) / scale;
    const tolData = HIT_PIXEL_TOL / scale;
    return queryHit(
      data,
      hitIndex,
      dataX,
      dataY,
      tolData,
      (l) =>
        // 8jce/vm3c: the synthetic stock-outline layer isn't in the
        // user's visibleLayers set, but it must always be hittable.
        l === STOCK_OUTLINE_LAYER || project.visibleLayers.has(l),
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
    // bwt7: feed the live touch position into the gesture tracker and,
    // while a pinch is active, recompute zoom + pan from the two
    // fingers' movement (consuming the event before any hover / select).
    if (activePointers.has(e.pointerId)) {
      activePointers.set(e.pointerId, { x: cx, y: cy });
    }
    if (pinch) {
      const a = activePointers.get(pinch.idA);
      const b = activePointers.get(pinch.idB);
      if (a && b && lastBaseTransform) {
        const next = applyPinch(
          { zoom: userZoom, panX: userPanX, panY: userPanY },
          lastBaseTransform,
          { a: pinch.prevA, b: pinch.prevB },
          { a, b },
        );
        userZoom = next.zoom;
        userPanX = next.panX;
        userPanY = next.panY;
        pinch.prevA = { ...a };
        pinch.prevB = { ...b };
      }
      return;
    }
    // A held finger that wanders past tap tolerance is a drag, not a
    // hold — cancel the pending long-press context menu.
    if (longPressStart && !withinTapTolerance(longPressStart, { x: cx, y: cy })) {
      cancelLongPress();
    }
    // n79: in approach-pick mode, the cursor IS the picker — update
    // the preview marker on every move and short-circuit the
    // hover-hit / box-select paths below.
    if (approachPickActive) {
      const data = pxToData(cx, cy);
      if (data) {
        const tol = approachSnapToleranceData();
        const snap = shiftDown ? null : findOSnap(osnapTargets, data.x, data.y, tol, osnapSettings);
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
        const snap = shiftDown ? null : findOSnap(osnapTargets, data.x, data.y, tol, osnapSettings);
        const x = snap ? snap.x : data.x;
        const y = snap ? snap.y : data.y;
        project.updateOperation(approachDrag.opId, { approachPoint: [x, y] });
        approachPreview = { x, y, snap: snap?.kind ?? null };
      }
      canvas.style.cursor = 'grabbing';
      return;
    }

    // rt1.12 (j7b4): live drag of a raster-engrave placement image.
    // Commits straight to the source origin (coalesced ⇒ one undo
    // entry); the overlay repaint tracks the new origin reactively.
    if (rasterDrag && e.pointerId === rasterDrag.pointerId) {
      const data = pxToData(cx, cy);
      if (data) {
        project.updateReliefSource(rasterDrag.sourceId, {
          origin: { x: data.x - rasterDrag.grabDX, y: data.y - rasterDrag.grabDY },
        });
      }
      canvas.style.cursor = 'grabbing';
      return;
    }

    // rt1.12 (ywf9): live drag of a text layer's origin (coalesced ⇒ one
    // undo entry); the bg repaint tracks the new origin reactively.
    if (textDrag && e.pointerId === textDrag.pointerId) {
      const data = pxToData(cx, cy);
      if (data) {
        project.updateTextLayer(textDrag.id, {
          origin: { x: data.x - textDrag.grabDX, y: data.y - textDrag.grabDY },
        });
      }
      canvas.style.cursor = 'grabbing';
      return;
    }

    // Audit kj8i: hover-near-marker preview. Mirror the hit-test that
    // onPointerDown does for click-to-drag so the cursor flips to
    // `grab` BEFORE the user mousedowns — without this the marker is
    // draggable but invisibly so.
    {
      const selOp =
        project.selectedOpId == null
          ? null
          : project.operations.find((o) => o.id === project.selectedOpId);
      if (
        selOp &&
        (selOp.kind === 'profile' || selOp.kind === 'pocket') &&
        selOp.approachPoint &&
        !panDrag &&
        !boxSelect
      ) {
        const data = pxToData(cx, cy);
        if (data) {
          const hitR = approachMarkerHitRadiusData();
          const [ax, ay] = selOp.approachPoint;
          const dx = data.x - ax;
          const dy = data.y - ay;
          if (dx * dx + dy * dy <= hitR * hitR) {
            canvas.style.cursor = 'grab';
            return;
          }
        }
      }
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
      const ghost = ghostTabAt(cx, cy);
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
    // bwt7: release touch tracking + end any active gesture. A quick
    // down→up cancels the long-press (it was a tap, not a hold); a
    // finger leaving a pinch ends the gesture and drops any armed
    // box-select so the remaining finger's lift is a no-op.
    if (e.pointerType === 'touch') {
      activePointers.delete(e.pointerId);
    }
    cancelLongPress();
    if (pinch && (e.pointerId === pinch.idA || e.pointerId === pinch.idB)) {
      pinch = null;
      boxSelect = null;
      try {
        canvas.releasePointerCapture(e.pointerId);
      } catch {}
      return;
    }
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
    // rt1.12 (j7b4): end an active raster placement drag.
    if (rasterDrag && e.pointerId === rasterDrag.pointerId) {
      rasterDrag = null;
      canvas.style.cursor = 'default';
      try {
        canvas.releasePointerCapture(e.pointerId);
      } catch {}
      return;
    }
    // rt1.12 (ywf9): end an active text-layer drag.
    if (textDrag && e.pointerId === textDrag.pointerId) {
      textDrag = null;
      canvas.style.cursor = 'default';
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
    fitView();
  }
  /// Shared reset — invoked from the fit-view button, double-click
  /// empty space, and the keyboard `F` / `Home` shortcuts. Pulls the
  /// canvas back to its auto-fit baseline (no user pan, no user zoom).
  function fitView() {
    userZoom = 1;
    userPanX = 0;
    userPanY = 0;
    drawBackground();
  }

  /// Return the set of object ids whose bbox lies fully INSIDE the
  /// screen rectangle drawn between (x0,y0) and (x1,y1) — Illustrator /
  /// Inkscape style containment select, so dragging the rubber-band
  /// across part of an object does NOT pick it. Works in DATA
  /// coordinates: we transform the rectangle once into data space and
  /// containment-test each object's bbox (audit-1dqh).
  function objectsInBox(x0: number, y0: number, x1: number, y1: number): number[] {
    const data = project.geometryView;
    if (!data || !lastTransform) return [];
    return objectsContainedInBox(
      data.object_meta ?? [],
      project.visibleLayers,
      lastTransform,
      x0,
      y0,
      x1,
      y1,
      STOCK_OUTLINE_LAYER,
    );
  }
  function onPointerLeave() {
    hoverIdx = null;
    ghostTab = null;
    cursorXY = null;
    // bwt7: a finger dragged off the canvas can't complete a hold.
    cancelLongPress();
    canvas.style.cursor = tabPlacementActive ? 'crosshair' : 'default';
  }

  /// Cache of the per-object polylines for the current import. Cleared
  /// when the import changes; the projection helpers reuse it.
  let objectPolylinesCache: ObjectPolyline[] | null = null;
  let objectPolylinesCacheKey: unknown = null;
  function getObjectPolylines(): ObjectPolyline[] {
    const imp = project.geometryView;
    if (!imp) return [];
    if (objectPolylinesCacheKey !== imp) {
      objectPolylinesCache = buildObjectPolylines(imp);
      objectPolylinesCacheKey = imp;
    }
    return objectPolylinesCache ?? [];
  }

  /// Thin reactive wrapper over the pure `projectGhostTab` geometry
  /// (lib/canvas/ghost-tab.ts): pull the selected contour op, transform,
  /// and osnap state out of component scope and assemble the context.
  /// Returns null when there's no contour op selected or no transform yet.
  function ghostTabAt(cx: number, cy: number): GhostTab | null {
    const op = selectedOp;
    if (!op || !isContourOp(op) || !lastTransform) return null;
    return projectGhostTab(cx, cy, {
      transform: lastTransform,
      polylines: getObjectPolylines(),
      sourceObjects: op.sourceObjects,
      tabPlacements: op.tabPlacements ?? undefined,
      altDown,
      osnapTargets,
      osnapSettings,
    });
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
    // A touch long-press may have already opened the menu; a native
    // contextmenu event firing on the same hold would re-anchor it. The
    // long-press path cancels its timer, so by here any native event is
    // a real mouse right-click — just (re)open at the cursor.
    cancelLongPress();
    const rect = canvas.getBoundingClientRect();
    openContextMenuAt(e.clientX - rect.left, e.clientY - rect.top);
  }

  /// Open the canvas context menu at a canvas-relative pixel position.
  /// Shared by mouse right-click (`onContextMenu`) and the touch
  /// long-press (bwt7) so both reach the same tab-popover / op-picker /
  /// "set text origin here" actions.
  function openContextMenuAt(cx: number, cy: number) {
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
    // Audit kj8i: F / Home reset the 2D view to its auto-fit baseline.
    // Mirrors the new `.fit-btn` overlay button and the 3D pane's
    // equivalent. Bail when the user is typing — we don't want F to
    // wipe an unrelated input.
    if (
      (e.key === 'f' || e.key === 'F' || e.key === 'Home') &&
      !e.ctrlKey &&
      !e.metaKey &&
      !e.altKey
    ) {
      const t = e.target as HTMLElement | null;
      const tag = t?.tagName ?? '';
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || t?.isContentEditable) {
        return;
      }
      fitView();
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
    // Bounce the sidebar to Operations so the freshly-added op
    // row is visible without a second click on the sidebar.
    onActivateSidebarPane?.('operations');
    ctxMenu = null;
  }

  function onPointerDown(e: PointerEvent) {
    const rect = canvas.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;

    // bwt7: touch gesture tracking. A second finger promotes the
    // gesture to a pinch-zoom / two-finger pan; the first finger held
    // still becomes a long-press → context menu. Mouse / pen fall
    // straight through to the button-based paths below.
    if (e.pointerType === 'touch') {
      activePointers.set(e.pointerId, { x: cx, y: cy });
      if (activePointers.size >= 2) {
        // Abandon whatever the first finger armed (selection already
        // happened on its down; a box-select / marker drag would fight
        // the pinch), then diff finger movement from here.
        cancelLongPress();
        boxSelect = null;
        approachDrag = null;
        rasterDrag = null;
        textDrag = null;
        const entries = [...activePointers.entries()];
        const first = entries[0];
        const second = entries[1];
        if (first && second) {
          pinch = {
            idA: first[0],
            idB: second[0],
            prevA: { ...first[1] },
            prevB: { ...second[1] },
          };
          for (const id of [first[0], second[0]]) {
            try {
              canvas.setPointerCapture(id);
            } catch {}
          }
        }
        canvas.style.cursor = 'default';
        e.preventDefault();
        return;
      }
      // First finger down — arm the long-press hold. The selection
      // logic below still runs (tap-to-select on down); the timer just
      // overlays a context-menu open if the finger stays put.
      cancelLongPress();
      longPressStart = { x: cx, y: cy };
      longPressTimer = setTimeout(() => {
        longPressTimer = null;
        longPressStart = null;
        // Fire only while exactly one finger is still down — a pinch
        // would have cleared this. Open at the press position.
        if (activePointers.size === 1) openContextMenuAt(cx, cy);
      }, LONG_PRESS_MS);
    }

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
        const snap = shiftDown ? null : findOSnap(osnapTargets, data.x, data.y, tol, osnapSettings);
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

    // Past this point we only handle LEFT-click. Right-click (button 2)
    // is exclusively a context-menu trigger — onContextMenu runs next
    // and reads the current selection. Letting right-click fall through
    // into the hit-test + selection reducer collapsed multi-selections
    // (user report) and silently fired tab placements / approach-marker
    // drags. Forward / back navigation buttons (3, 4) also bail here.
    if (e.button !== 0) return;

    // n79: dragging an already-placed approach marker. Only allowed
    // when the selected op has one and we're NOT in pick mode.
    if (
      !approachPickActive &&
      selectedOp &&
      (selectedOp.kind === 'profile' || selectedOp.kind === 'pocket') &&
      selectedOp.approachPoint &&
      e.button === 0
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

    // rt1.12 (j7b4): grab a raster-engrave placement image to drag it.
    // Clicking the image also selects its op (raster ops have no source
    // geometry, so the canvas is their only spatial handle). Gated out
    // of the pick / tab modes above.
    if (!approachPickActive && !tabPlacementActive) {
      const data = pxToData(cx, cy);
      const hit = data ? rasterPlacementAtData(data.x, data.y) : null;
      if (data && hit) {
        project.selectedOpId = hit.op.id;
        rasterDrag = {
          sourceId: hit.src.id,
          pointerId: e.pointerId,
          grabDX: data.x - hit.src.origin.x,
          grabDY: data.y - hit.src.origin.y,
        };
        try {
          canvas.setPointerCapture(e.pointerId);
        } catch {}
        canvas.style.cursor = 'grabbing';
        e.preventDefault();
        return;
      }
    }

    // rt1.12 (ywf9): grab the SELECTED text layer to drag its origin.
    // Restricted to the already-selected layer (selected via the sidebar
    // text list, like the approach-marker drag needs its op selected) so
    // a text bbox overlapping imported geometry doesn't hijack object
    // selection. Same mode gating as the raster grab.
    if (!approachPickActive && !tabPlacementActive && project.selectedTextLayerId != null) {
      const data = pxToData(cx, cy);
      const hit = data ? selectedTextAtData(data.x, data.y) : null;
      if (data && hit) {
        textDrag = {
          id: hit.id,
          pointerId: e.pointerId,
          grabDX: data.x - hit.origin.x,
          grabDY: data.y - hit.origin.y,
        };
        try {
          canvas.setPointerCapture(e.pointerId);
        } catch {}
        canvas.style.cursor = 'grabbing';
        e.preventDefault();
        return;
      }
    }

    // rt1.10: tab-placement mode (selected op has Manual / Mixed
    // tab_mode). Click toggles a placement at the contour projection
    // — Estlcam-style. ToleranceT picks the "is this near an existing
    // tab" threshold: ~3 px of contour length.
    if (tabPlacementActive && selectedOp) {
      const ghost = ghostTabAt(cx, cy);
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
    // Map segment index → its 1-based object id (or null for empty
    // space). The pure reducer in lib/canvas/entity-selection.ts (774f)
    // resolves modifiers and emits the action list; we dispatch and
    // arm the box-select store.
    const hitObjectId = idx == null ? null : (project.geometryView?.objects?.[idx] ?? 0);
    const actions = reduceCanvasClick(
      {
        hitObjectId,
        shiftKey: e.shiftKey,
        // bwt7: the "add to selection" toggle stands in for ctrl on a
        // keyboardless touch device — a plain tap then toggles.
        ctrlKey: e.ctrlKey || addToSelection,
        metaKey: e.metaKey,
      },
      objectToOps,
    );
    for (const action of actions) {
      switch (action.kind) {
        case 'clear-selection':
          project.clearSelection();
          break;
        case 'clear-fixture-selection':
          project.selectFixture(null);
          break;
        case 'select-objects':
          project.selectObjects(action.ids, action.mode);
          break;
        case 'series-select-to':
          project.seriesSelectTo(action.id);
          break;
        case 'set-active-op':
          if (project.selectedOpId !== action.opId) project.selectedOpId = action.opId;
          break;
        case 'arm-box-select':
          boxSelect = {
            startX: cx,
            startY: cy,
            curX: cx,
            curY: cy,
            mode: action.mode,
            armed: true,
          };
          // Capture so pointermove keeps firing if the user drags past
          // the canvas edge — otherwise box-select would freeze at the
          // last point inside the canvas.
          try {
            canvas.setPointerCapture(e.pointerId);
          } catch {
            /* not all browsers / older versions; harmless */
          }
          break;
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

  function colorFor(c: number): string {
    const r = resolveAci(c);
    return r.kind === 'fixed' ? hexToCss(r.hex) : themeVar(r.token, hexToCss(r.fallback));
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
  /// Thin reactive wrapper over `computeViewportTransform` (lib/canvas/
  /// viewport.ts): pulls the live user pan/zoom out of $state, computes
  /// the active transform, and caches both the base and active values
  /// so hit-tests can read them without recomputing.
  function computeTransform(
    bbox: BBox,
    w: number,
    h: number,
  ): {
    scale: number;
    offX: number;
    offY: number;
    project2: (x: number, y: number) => [number, number];
  } {
    const t = computeViewportTransform(
      bbox,
      { w, h },
      { zoom: userZoom, panX: userPanX, panY: userPanY },
    );
    lastBaseTransform = { scale: t.baseScale, offX: t.baseOffX, offY: t.baseOffY };
    lastTransform = { scale: t.scale, offX: t.offX, offY: t.offY };
    return { scale: t.scale, offX: t.offX, offY: t.offY, project2: t.project2 };
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

    const data = project.geometryView;
    const hasGeom = !!data && data.segments.length > 0;
    // rt1.12 (fvb0 / ywf9): a placement-only project (raster images
    // and/or text, no imported DXF) has no geometry — fall back to a
    // bbox over the placements / bed so the grid + axes + draggable
    // entities still render.
    const fallbackBBox = hasGeom ? null : placementFallbackBBox();
    if (!hasGeom && !fallbackBBox) {
      ctx.fillStyle = themeVar('--canvas-empty', '#555');
      ctx.font = '13px system-ui, sans-serif';
      ctx.fillText('Open a file to view geometry', 16, 24);
      return;
    }

    const { scale, offX, offY, project2 } = computeTransform(
      hasGeom ? data!.bbox : fallbackBBox!,
      w,
      h,
    );

    drawGrid(ctx, w, h, scale, offX, offY);
    drawAxes(ctx, w, h, offX, offY);
    drawWorkArea(ctx, project2);
    drawStock(ctx, project2);

    // Imported-geometry chrome (regions + base wireframe) — only when a
    // DXF is loaded. A placement-only project skips straight to the text
    // previews below (raster images live on the overlay).
    if (hasGeom && data) {
      // Filled-region preview painted under the wireframe so contours
      // stay legible. Regions come from the backend (pipeline.rs
      // build_region_previews).
      const regions = project.generated?.regions ?? [];
      if (regions.length > 0 && project.regionsVisible) {
        drawRegions(ctx, regions, scale, offX, offY);
      }

      // Imported segments — paint in BASE layer color only. State-bearing
      // overlays (selection / hover / op-assignment halos) go on the
      // overlay canvas, so editing those does NOT invalidate this layer.
      const visibleLayersSnap = new Set(project.visibleLayers);
      visibleLayersSnap.add(STOCK_OUTLINE_LAYER); // vm3c: synthetic layer always drawn
      ctx.lineWidth = project.settings.previewLineWidth;
      for (let i = 0; i < data.segments.length; i++) {
        const seg = data.segments[i];
        if (!visibleLayersSnap.has(seg.layer)) continue;
        ctx.strokeStyle = colorFor(seg.color);
        drawSegment(ctx, seg, project2);
      }
    }

    // Text-layer previews. Rendered with OR without imported geometry so
    // a text-only engrave project is visible (and draggable) on a bare
    // canvas. The cache is filled by requestPreview() in the top-of-file
    // effect; drawTextPreview also reads selectedTextLayerId for the
    // highlight.
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

    const data = project.geometryView;
    const hasGeom = !!data && data.segments.length > 0;
    // rt1.12 (fvb0 / ywf9): mirror drawBackground — fall back to the
    // placement / bed bbox so a geometry-less project is draggable.
    const fallbackBBox = hasGeom ? null : placementFallbackBBox();
    if (!hasGeom && !fallbackBBox) return;
    const { scale, project2 } = computeTransform(hasGeom ? data!.bbox : fallbackBBox!, w, h);

    const accent = themeVar('--accent', '#2d6cdf');

    // rt1.12 (j7b4): faint raster-engrave placement images, painted
    // first so selection halos / chrome layer over them.
    drawRasterPlacements(ctx, project2, scale);

    if (hasGeom && data) {
      const hoverColor = themeVar('--accent-strong', '#6e9ce6');
      const selOpId = project.selectedOpId;
      // Halo color = a high-contrast outline drawn UNDER selected /
      // hovered / op-assigned objects so the state stays visible even
      // when the underlying layer's ACI color happens to match the state
      // color. Uses --text-strong so it inverts automatically in light
      // theme.
      const haloColor = themeVar('--text-strong', '#ffffff');
      const hoverObj = hoverIdx == null ? 0 : (data.objects?.[hoverIdx] ?? 0);
      const visibleLayersSnap = new Set(project.visibleLayers);
      visibleLayersSnap.add(STOCK_OUTLINE_LAYER); // vm3c: synthetic layer always drawn
      const selectedObjectsSnap = new Set(project.selectedObjects);
      for (let i = 0; i < data.segments.length; i++) {
        const seg = data.segments[i];
        if (!visibleLayersSnap.has(seg.layer)) continue;
        const objId = data.objects?.[i] ?? 0;
        if (objId === 0) continue;
        const selected = selectedObjectsSnap.has(objId);
        const hovered = objId === hoverObj;
        const assignedOps = objectToOps.get(objId);
        if (!selected && !hovered && !assignedOps) continue;

        // Per-op assignment outlines (concentric rings, one band per op).
        // Each assigned op gets the SAME hue here as its toolpath in 3D.
        // When an object belongs to several ops we draw nested rings —
        // widest (outermost) first so narrower bands paint on top:
        // "outline, outline of outline, …". The selected op is ordered
        // innermost and rendered brighter so it reads as the primary
        // assignment without hiding the others.
        if (assignedOps && assignedOps.length > 0) {
          // Selected op last → drawn innermost / on top.
          const ids = [...assignedOps].sort(
            (a, b) => (a === selOpId ? 1 : 0) - (b === selOpId ? 1 : 0) || a - b,
          );
          const n = ids.length;
          const step = 2.4;
          const innerWidth = 2.0;
          // Faint contrast halo behind the widest band.
          const prevAlpha = ctx.globalAlpha;
          ctx.globalAlpha = 0.35;
          ctx.lineWidth = innerWidth + (n - 1) * step + 3;
          ctx.strokeStyle = haloColor;
          drawSegment(ctx, seg, project2);
          ctx.globalAlpha = prevAlpha;
          for (let k = 0; k < n; k++) {
            const opId = ids[k];
            // k=0 is the outermost (widest) band; the last is innermost.
            ctx.lineWidth = innerWidth + (n - 1 - k) * step;
            ctx.strokeStyle = opSourceCss(opId, opId === selOpId);
            drawSegment(ctx, seg, project2);
          }
        }

        // Hover / selection strokes paint on top so they stay legible even
        // over the assignment rings.
        if (hovered && !selected) {
          ctx.lineWidth = 1.8;
          ctx.strokeStyle = hoverColor;
          drawSegment(ctx, seg, project2);
        }
        if (selected) {
          const prevAlpha = ctx.globalAlpha;
          ctx.globalAlpha = 0.6;
          ctx.lineWidth = 2.4 + 3;
          ctx.strokeStyle = haloColor;
          drawSegment(ctx, seg, project2);
          ctx.globalAlpha = prevAlpha;
          ctx.lineWidth = 2.4;
          ctx.strokeStyle = accent;
          drawSegment(ctx, seg, project2);
        }
      }
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
    // green = locked-to-vertex (matches EstlCam). Pulls from `--success`
    // so light theme gets the deeper #166534 forest instead of #3c3 which
    // gets lost against pale canvas backgrounds.
    const snapColor = themeVar('--success', '#3c3');
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
  /// rt1.12 (j7b4): build / fetch the cached grayscale canvas for a
  /// relief source's brightness grid (top-down, ready for drawImage).
  /// Cached by source id; rebuilt only when the `brightness` array
  /// reference changes, so an origin / cell drag never re-decodes.
  function rasterImageCanvas(src: ReliefSource): HTMLCanvasElement | null {
    if (src.cols <= 0 || src.rows <= 0) return null;
    const cached = rasterImageCache.get(src.id);
    if (cached && cached.brightness === src.brightness) return cached.canvas;
    const cv = document.createElement('canvas');
    cv.width = src.cols;
    cv.height = src.rows;
    const ictx = cv.getContext('2d');
    if (!ictx) return null;
    const rgba = brightnessToRgba(src.brightness, src.cols, src.rows);
    const img = ictx.createImageData(src.cols, src.rows);
    img.data.set(rgba);
    ictx.putImageData(img, 0, 0);
    rasterImageCache.set(src.id, { brightness: src.brightness, canvas: cv });
    return cv;
  }

  /// The distinct relief sources referenced by raster-engrave ops, each
  /// paired with the (first) op referencing it — so the selection
  /// highlight + drag target know which op an image belongs to.
  function rasterPlacements(): { op: OpEntry; src: ReliefSource }[] {
    const out: { op: OpEntry; src: ReliefSource }[] = [];
    const seen = new Set<number>();
    for (const op of project.operations) {
      if (op.kind !== 'raster_engrave' || !op.enabled) continue;
      const src = project.reliefSources.find((s) => s.id === op.sourceId);
      if (!src || src.cols <= 0 || src.rows <= 0 || seen.has(src.id)) continue;
      seen.add(src.id);
      out.push({ op, src });
    }
    return out;
  }

  /// Hit-test a data-space point against the placed raster images,
  /// preferring the selected op's image (so overlapping placements stay
  /// grabbable) then the topmost. Returns the placement or null.
  function rasterPlacementAtData(x: number, y: number): { op: OpEntry; src: ReliefSource } | null {
    const ordered = rasterPlacements().sort(
      (a, b) =>
        (a.op.id === project.selectedOpId ? 1 : 0) - (b.op.id === project.selectedOpId ? 1 : 0),
    );
    for (let i = ordered.length - 1; i >= 0; i--) {
      const { src } = ordered[i];
      const wmm = src.cols * src.cell;
      const hmm = src.rows * src.cell;
      if (
        x >= src.origin.x &&
        x <= src.origin.x + wmm &&
        y >= src.origin.y &&
        y <= src.origin.y + hmm
      ) {
        return ordered[i];
      }
    }
    return null;
  }

  /// Axis-aligned bbox over a segment list's endpoints (good enough for
  /// view-fit + a drag hit-test; arc bulges are ignored). Null for an
  /// empty list.
  function segsBBox(segs: readonly Segment[]): Rect | null {
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const s of segs) {
      minX = Math.min(minX, s.start.x, s.end.x);
      minY = Math.min(minY, s.start.y, s.end.y);
      maxX = Math.max(maxX, s.start.x, s.end.x);
      maxY = Math.max(maxY, s.start.y, s.end.y);
    }
    if (!Number.isFinite(minX)) return null;
    return { minX, minY, maxX, maxY };
  }

  /// rt1.12 (fvb0 / ywf9): a viewport bbox for a project with no imported
  /// geometry + no visible stock, so placement-only entities (raster
  /// images, text layers) still render + drag. Prefers the machine work
  /// area — a STABLE reference, so the view doesn't jiggle while an
  /// origin is dragged (the entity moves within the bed). Falls back to
  /// the union of all placement extents (+10% margin) when no bed is
  /// defined. Null when there's nothing placeable to frame.
  function placementFallbackBBox(): BBox | null {
    const rects: Rect[] = [];
    for (const { src } of rasterPlacements()) {
      rects.push({
        minX: src.origin.x,
        minY: src.origin.y,
        maxX: src.origin.x + src.cols * src.cell,
        maxY: src.origin.y + src.rows * src.cell,
      });
    }
    for (const layer of project.textLayers) {
      const bb = segsBBox(previewSegmentsFor(layer.id) ?? []);
      if (bb) rects.push(bb);
    }
    if (rects.length === 0) return null;
    const wa = project.machine.workArea;
    if (wa && wa.x > 0 && wa.y > 0) {
      return { min_x: 0, min_y: 0, max_x: wa.x, max_y: wa.y };
    }
    return placementsBBox(rects);
  }

  /// Whether a data-space point lands within the selected text layer's
  /// rendered bbox (+ a small screen-constant tolerance) — the hit-test
  /// for click-drag repositioning. Returns the layer when hit, else null.
  function selectedTextAtData(x: number, y: number): TextLayer | null {
    const id = project.selectedTextLayerId;
    if (id == null) return null;
    const layer = project.textLayers.find((l) => l.id === id);
    if (!layer) return null;
    const bb = segsBBox(previewSegmentsFor(id) ?? []);
    if (!bb) return null;
    const tol = lastTransform ? 4 / Math.max(Math.abs(lastTransform.scale), 1e-6) : 0;
    const inside =
      x >= bb.minX - tol && x <= bb.maxX + tol && y >= bb.minY - tol && y <= bb.maxY + tol;
    return inside ? layer : null;
  }

  /// Paint the faint placed raster images (+ selection / placement
  /// border) on the overlay, under the interaction chrome. Drawn from
  /// `drawOverlay` so a source move repaints without touching the heavy
  /// bg layer. Editable whenever a transform exists (imported geometry
  /// or visible stock); a geometry-less project shows the empty canvas.
  function drawRasterPlacements(
    ctx: CanvasRenderingContext2D,
    project2: (x: number, y: number) => [number, number],
    scale: number,
  ) {
    const placements = rasterPlacements();
    if (placements.length === 0) return;
    const accent = themeVar('--accent', '#2d6cdf');
    const border = themeVar('--border', '#555');
    for (const { op, src } of placements) {
      const cv = rasterImageCanvas(src);
      if (!cv) continue;
      const wmm = src.cols * src.cell;
      const hmm = src.rows * src.cell;
      const [x0, y0] = project2(src.origin.x, src.origin.y + hmm); // world top-left
      const wpx = wmm * scale;
      const hpx = hmm * scale;
      const prevAlpha = ctx.globalAlpha;
      const prevSmooth = ctx.imageSmoothingEnabled;
      ctx.globalAlpha = 0.5;
      ctx.imageSmoothingEnabled = false;
      ctx.drawImage(cv, x0, y0, wpx, hpx);
      ctx.globalAlpha = prevAlpha;
      ctx.imageSmoothingEnabled = prevSmooth;
      const selected = project.selectedOpId === op.id;
      ctx.lineWidth = selected ? 2 : 1;
      ctx.strokeStyle = selected ? accent : border;
      if (!selected) ctx.setLineDash([4, 3]);
      ctx.strokeRect(x0, y0, wpx, hpx);
      ctx.setLineDash([]);
    }
  }

  function drawTextPreview(
    ctx: CanvasRenderingContext2D,
    p: (x: number, y: number) => [number, number],
    accent: string,
    _hoverColor: string,
    haloColor: string,
  ) {
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
      const { r, g, b, a } = unpackFixtureColor(f.color);
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
    const paths = regionPaths(regions);
    // Compose data → canvas transform on top of the existing dpr scale.
    // Y is flipped (canvas y-down vs DXF y-up) so we use -scale on Y +
    // offY as the canvas-space origin of data-y=0.
    ctx.save();
    ctx.transform(scale, 0, 0, -scale, offX, offY);
    for (const rp of paths) {
      const isSelected = project.selectedOpId === rp.op_id;
      // Accent tint, clearly visible so toggling Regions is obvious (the
      // old ~10% muted-grey fill was near-invisible). Selected op's
      // region is brighter. Still translucent so contours read through.
      ctx.fillStyle = isSelected
        ? `${accent}66` // ~40% alpha
        : `${accent}33`; // ~20% alpha
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
    // 7iej.19: screen-space radius by projecting a point `radius` away from
    // the center and measuring. The viewport transform is a uniform scale,
    // so direction is irrelevant — and this avoids the div-by-near-zero the
    // old `(sx - pcx) / (seg.start.x - cx)` ratio hit on a vertical chord
    // (start directly above/below the center).
    const [prx, pry] = p(cx + radius, cy);
    const r = Math.hypot(prx - pcx, pry - pcy);
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

  /// Solid outline of the workpiece bounds in XY. Mirrors the
  /// translucent stock box the 3D scene already paints — the 2D pane
  /// previously omitted it entirely, so users couldn't see whether
  /// their drawing sat inside the stock without flipping to 3D.
  function drawStock(ctx: CanvasRenderingContext2D, p: (x: number, y: number) => [number, number]) {
    const fp = computeFootprint(project.transformedImport, project.stock, project.machine.workArea);
    const sizeX = fp.maxX - fp.minX;
    const sizeY = fp.maxY - fp.minY;
    if (sizeX <= 0 || sizeY <= 0) return;
    const [x0, y0] = p(fp.minX, fp.minY);
    const [x1, y1] = p(fp.maxX, fp.maxY);
    const minX = Math.min(x0, x1);
    const maxX = Math.max(x0, x1);
    const minY = Math.min(y0, y1);
    const maxY = Math.max(y0, y1);
    ctx.save();
    ctx.lineWidth = 1;
    ctx.strokeStyle = themeVar('--stock-edge', '#888');
    ctx.globalAlpha = 0.85;
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
      {#if shiftDown && (approachPickActive || tabPlacementActive)}
        <!-- Audit kj8i: visible cue that Shift is suppressing snap.
             Without this the snap glyph just silently disappears and
             the user can't tell why their click no longer locks. -->
        <span class="snap-off">snap off</span>
      {/if}
    </div>
  {/if}
  {#if project.transformedImport && project.operations.length === 0}
    <div class="firstrun-hint" role="status">
      <span class="firstrun-step">1</span>
      <span>Click an object to select it</span>
      <span class="firstrun-arrow">→</span>
      <span class="firstrun-step">2</span>
      <span>Right-click for new operation</span>
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
  <!-- Fit-to-view affordance mirroring Scene3D's .fit-btn (audit kj8i).
       Doubleclick on empty space already resets, but that's undocumented
       — adding the button gives an obvious affordance and matches the 3D
       pane. F / Home shortcuts also call fitView when canvas has focus. -->
  <button
    type="button"
    class="fit-btn"
    onclick={fitView}
    title="Fit view to scene (F)"
    aria-label="Fit view to scene"
  >
    ⌖
  </button>
  {#if isTouchDevice}
    <!-- bwt7: keyboardless multi-select toggle. On touch there's no
         shift/ctrl-tap, so this latches "tap = add/remove from
         selection". Touch devices only — mouse users have modifiers. -->
    <button
      type="button"
      class="multiselect-btn"
      class:active={addToSelection}
      aria-pressed={addToSelection}
      onclick={() => (addToSelection = !addToSelection)}
      title="Add to selection (tap multiple objects)"
      aria-label="Toggle add-to-selection mode"
    >
      ⧉
    </button>
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
  /* a4ab: first-run hint when imported && no ops. Center bottom so it
     hangs below the geometry without covering it. Auto-dismisses the
     instant the user adds an op. */
  .firstrun-hint {
    position: absolute;
    bottom: 1.2rem;
    left: 50%;
    transform: translateX(-50%);
    display: inline-flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0.5rem 0.9rem;
    background: color-mix(in srgb, var(--bg-elevated) 92%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 35%, var(--border));
    border-radius: 999px;
    color: var(--text-strong);
    font-size: 0.78rem;
    box-shadow: 0 6px 18px var(--shadow-modal);
    pointer-events: none;
    max-width: calc(100% - 2rem);
    white-space: nowrap;
  }
  .firstrun-step {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.4rem;
    height: 1.4rem;
    border-radius: 50%;
    background: var(--accent);
    color: #fff;
    font-weight: 700;
    font-size: 0.72rem;
  }
  .firstrun-arrow {
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  /* 7tp5: cursor world-coordinate HUD. Top-right corner so it doesn't
     fight the selection-hud (top-left). Monospace tabular-nums so the
     numbers don't dance as the cursor moves. */
  .cursor-hud {
    position: absolute;
    /* Sits below the fit-btn / help-btn cluster so it doesn't collide
       with the round overlay buttons in the top-right corner. */
    top: 2.4rem;
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
  .cursor-hud .snap-off {
    margin-left: 0.5rem;
    padding: 0 0.3rem;
    border-radius: 2px;
    background: color-mix(in srgb, var(--warn) 28%, transparent);
    color: var(--text-strong);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.02em;
  }
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
  .tab-popover {
    position: absolute;
    min-width: 11rem;
    max-width: 14rem;
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px var(--shadow-modal);
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
    color: var(--danger);
    border: 1px solid var(--danger);
    border-radius: 3px;
    padding: 0.25rem 0.5rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .tab-popover-delete:hover {
    background: color-mix(in srgb, var(--danger) 15%, transparent);
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
  /* Fit-to-view button — visual twin of Scene3D's .fit-btn, sits to
     the LEFT of the help-btn so both float in the same top-right
     cluster regardless of which pane the user is in. */
  .fit-btn {
    position: absolute;
    top: 0.5rem;
    right: 2.5rem;
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
  }
  .fit-btn:hover,
  .fit-btn:focus-visible {
    opacity: 1;
    color: var(--text-strong);
  }
  /* bwt7: keyboardless multi-select toggle — visual twin of .fit-btn,
     sits one slot further left. Latches an accent fill while active. */
  .multiselect-btn {
    position: absolute;
    top: 0.5rem;
    right: 4.5rem;
    width: 1.6rem;
    height: 1.6rem;
    border-radius: 50%;
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text-muted);
    cursor: pointer;
    font-size: 0.95rem;
    line-height: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    opacity: 0.7;
    transition:
      opacity 0.12s ease,
      color 0.12s ease,
      background 0.12s ease;
  }
  .multiselect-btn:hover,
  .multiselect-btn:focus-visible {
    opacity: 1;
    color: var(--text-strong);
  }
  .multiselect-btn.active {
    opacity: 1;
    color: var(--accent-contrast, #fff);
    background: var(--accent);
    border-color: var(--accent);
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
  .help-btn:focus-visible {
    opacity: 1;
    color: var(--text-strong);
  }
</style>
