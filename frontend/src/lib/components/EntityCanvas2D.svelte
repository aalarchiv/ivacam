<script lang="ts">
  import { onMount } from 'svelte';
  import { project } from '../state/project.svelte';
  import type { Segment } from '../api/types';

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
  let container: HTMLDivElement;

  function themeVar(name: string, fallback: string): string {
    if (!container) return fallback;
    const v = getComputedStyle(container).getPropertyValue(name).trim();
    return v || fallback;
  }

  onMount(() => {
    const ro = new ResizeObserver(() => draw());
    ro.observe(container);
    draw();
    // Re-paint when the user toggles their OS theme or picks a manual one.
    const mql = window.matchMedia('(prefers-color-scheme: light)');
    const onChange = () => draw();
    mql.addEventListener('change', onChange);
    // Diff the data-theme value before redrawing — MutationObserver fires
    // on every attribute write, including same-value writes (e.g. an
    // unrelated rerender that re-applies the stored preference). draw()
    // is non-trivial for big imports.
    let lastTheme = document.documentElement.dataset.theme ?? '';
    const themeMo = new MutationObserver(() => {
      const cur = document.documentElement.dataset.theme ?? '';
      if (cur === lastTheme) return;
      lastTheme = cur;
      draw();
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

  $effect(() => {
    void project.imported;
    void project.visibleLayers;
    void project.selectedObjects;
    void project.tabs;
    void project.tabMode;
    void project.generated;
    void project.selectedOpId;
    void project.regionsVisible;
    void hoverIdx;
    draw();
  });

  // Mouse → segment hit testing. We project each segment to canvas space
  // and pick the nearest one within `HIT_PIXEL_TOL`.
  const HIT_PIXEL_TOL = 8;
  let hoverIdx = $state<number | null>(null);
  let lastTransform: { scale: number; offX: number; offY: number } | null = null;

  /// Uniform-grid spatial index for segment hit testing. Without it,
  /// pixelHit() ran an O(N) scan on every pointermove — fine for tiny
  /// DXFs but a million distance computations per second of idle hover
  /// on a 10k-segment file. The grid is rebuilt when project.imported
  /// changes (rare); each query inspects only the cells overlapping the
  /// cursor + tolerance and bails early past that.
  type HitIndex = {
    cellW: number;
    cellH: number;
    minX: number;
    minY: number;
    cols: number;
    rows: number;
    /// One Uint32Array per cell with segment indices that touch it.
    /// Cells with zero hits stay undefined to keep memory bounded.
    cells: (Uint32Array | undefined)[];
  };
  let hitIndex: HitIndex | null = null;

  $effect(() => {
    void project.imported;
    hitIndex = buildHitIndex(project.imported);
  });

  function buildHitIndex(
    data: typeof project.imported,
  ): HitIndex | null {
    if (!data || data.segments.length === 0) return null;
    const { min_x, min_y, max_x, max_y } = data.bbox;
    const dx = Math.max(max_x - min_x, 1e-6);
    const dy = Math.max(max_y - min_y, 1e-6);
    // Aim for ~sqrt(N) cells per side, capped so tiny imports don't get
    // a sparse grid and huge ones don't blow the memory budget.
    const target = Math.min(128, Math.max(8, Math.floor(Math.sqrt(data.segments.length))));
    const cols = target;
    const rows = target;
    const cellW = dx / cols;
    const cellH = dy / rows;
    // Bucket counts then bucket arrays — two-pass build avoids growing
    // arrays per-insert.
    const counts = new Uint32Array(cols * rows);
    const segs = data.segments;
    const visit = (cb: (cellIdx: number, segIdx: number) => void) => {
      for (let i = 0; i < segs.length; i++) {
        const s = segs[i];
        const sxMin = Math.min(s.start.x, s.end.x);
        const syMin = Math.min(s.start.y, s.end.y);
        const sxMax = Math.max(s.start.x, s.end.x);
        const syMax = Math.max(s.start.y, s.end.y);
        const c0 = clamp(Math.floor((sxMin - min_x) / cellW), 0, cols - 1);
        const c1 = clamp(Math.floor((sxMax - min_x) / cellW), 0, cols - 1);
        const r0 = clamp(Math.floor((syMin - min_y) / cellH), 0, rows - 1);
        const r1 = clamp(Math.floor((syMax - min_y) / cellH), 0, rows - 1);
        for (let r = r0; r <= r1; r++) {
          for (let c = c0; c <= c1; c++) {
            cb(r * cols + c, i);
          }
        }
      }
    };
    visit((cellIdx) => {
      counts[cellIdx]++;
    });
    const cells: (Uint32Array | undefined)[] = new Array(cols * rows);
    const writeIdx = new Uint32Array(cols * rows);
    for (let i = 0; i < counts.length; i++) {
      if (counts[i] > 0) cells[i] = new Uint32Array(counts[i]);
    }
    visit((cellIdx, segIdx) => {
      const buf = cells[cellIdx]!;
      buf[writeIdx[cellIdx]++] = segIdx;
    });
    return { cellW, cellH, minX: min_x, minY: min_y, cols, rows, cells };
  }

  function clamp(v: number, lo: number, hi: number): number {
    return v < lo ? lo : v > hi ? hi : v;
  }

  function pixelHit(canvasX: number, canvasY: number): number | null {
    const data = project.imported;
    if (!data || !lastTransform) return null;
    const { scale, offX, offY } = lastTransform;
    const dataX = (canvasX - offX) / scale;
    const dataY = (offY - canvasY) / scale;
    const tolData = HIT_PIXEL_TOL / scale;
    let bestIdx: number | null = null;
    let bestDist = Infinity;
    const segs = data.segments;
    if (hitIndex) {
      const { cellW, cellH, minX, minY, cols, rows, cells } = hitIndex;
      const c0 = clamp(Math.floor((dataX - tolData - minX) / cellW), 0, cols - 1);
      const c1 = clamp(Math.floor((dataX + tolData - minX) / cellW), 0, cols - 1);
      const r0 = clamp(Math.floor((dataY - tolData - minY) / cellH), 0, rows - 1);
      const r1 = clamp(Math.floor((dataY + tolData - minY) / cellH), 0, rows - 1);
      // Dedup across cells — a single segment can land in multiple cells
      // when its bbox crosses cell boundaries.
      const seen = new Set<number>();
      for (let r = r0; r <= r1; r++) {
        for (let c = c0; c <= c1; c++) {
          const buf = cells[r * cols + c];
          if (!buf) continue;
          for (let k = 0; k < buf.length; k++) {
            const i = buf[k];
            if (seen.has(i)) continue;
            seen.add(i);
            const s = segs[i];
            if (!project.visibleLayers.has(s.layer)) continue;
            const d = distanceToSegment(s.start, s.end, dataX, dataY);
            if (d < tolData && d < bestDist) {
              bestIdx = i;
              bestDist = d;
            }
          }
        }
      }
      return bestIdx;
    }
    // Fallback for the rare case the index hasn't been built yet (first
    // mousemove during the initial mount before the $effect fires).
    for (let i = 0; i < segs.length; i++) {
      const s = segs[i];
      if (!project.visibleLayers.has(s.layer)) continue;
      const d = distanceToSegment(s.start, s.end, dataX, dataY);
      if (d < tolData && d < bestDist) {
        bestIdx = i;
        bestDist = d;
      }
    }
    return bestIdx;
  }

  function distanceToSegment(
    a: { x: number; y: number },
    b: { x: number; y: number },
    px: number,
    py: number,
  ): number {
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const lenSq = dx * dx + dy * dy;
    if (lenSq < 1e-12) return Math.hypot(px - a.x, py - a.y);
    let t = ((px - a.x) * dx + (py - a.y) * dy) / lenSq;
    t = Math.max(0, Math.min(1, t));
    const ix = a.x + t * dx;
    const iy = a.y + t * dy;
    return Math.hypot(px - ix, py - iy);
  }

  function onPointerMove(e: PointerEvent) {
    const rect = canvas.getBoundingClientRect();
    const idx = pixelHit(e.clientX - rect.left, e.clientY - rect.top);
    if (idx !== hoverIdx) {
      hoverIdx = idx;
      const baseCursor = project.tabMode ? 'crosshair' : 'default';
      canvas.style.cursor = idx == null ? baseCursor : project.tabMode ? 'cell' : 'pointer';
    }
  }
  function onPointerLeave() {
    hoverIdx = null;
    canvas.style.cursor = project.tabMode ? 'crosshair' : 'default';
  }
  function onPointerDown(e: PointerEvent) {
    const rect = canvas.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;

    // Tab mode takes precedence over selection: a click adds (or removes)
    // a tab at the closest point on the nearest segment.
    if (project.tabMode) {
      const removed = removeTabAtPixel(cx, cy);
      if (removed) return;
      const idx = pixelHit(cx, cy);
      if (idx == null) return;
      const proj = closestPointOnSegment(idx, cx, cy);
      if (proj) project.addTab(idx, proj);
      return;
    }

    const idx = pixelHit(cx, cy);
    const additive = e.ctrlKey || e.metaKey || e.shiftKey;
    if (idx == null) {
      if (!additive) project.clearSelection();
      return;
    }
    // Map segment index → its 1-based object id from the chaining pass.
    const objId = project.imported?.objects?.[idx] ?? 0;
    if (objId === 0) return;
    project.toggleObject(objId, additive);
  }

  function closestPointOnSegment(
    segmentIdx: number,
    canvasX: number,
    canvasY: number,
  ): { x: number; y: number } | null {
    const data = project.imported;
    if (!data || !lastTransform) return null;
    const { scale, offX, offY } = lastTransform;
    const dataX = (canvasX - offX) / scale;
    const dataY = (offY - canvasY) / scale;
    const s = data.segments[segmentIdx];
    if (!s) return null;
    const dx = s.end.x - s.start.x;
    const dy = s.end.y - s.start.y;
    const lenSq = dx * dx + dy * dy;
    if (lenSq < 1e-12) return { x: s.start.x, y: s.start.y };
    let t = ((dataX - s.start.x) * dx + (dataY - s.start.y) * dy) / lenSq;
    t = Math.max(0, Math.min(1, t));
    return { x: s.start.x + t * dx, y: s.start.y + t * dy };
  }

  /// Returns true if a tab marker was clicked and removed.
  function removeTabAtPixel(canvasX: number, canvasY: number): boolean {
    if (!lastTransform) return false;
    const { scale, offX, offY } = lastTransform;
    const tolPx = 10;
    const tolData = tolPx / scale;
    for (const [idxStr, list] of Object.entries(project.tabs)) {
      const segIdx = Number(idxStr);
      for (let i = 0; i < list.length; i++) {
        const t = list[i];
        const cx = t.x * scale + offX;
        const cy = offY - t.y * scale;
        const _ = tolData; // kept to make the threshold doc-explicit
        if (Math.hypot(canvasX - cx, canvasY - cy) <= tolPx) {
          project.removeTab(segIdx, i);
          return true;
        }
      }
    }
    return false;
  }

  function colorFor(c: number): string {
    if (c === 7 || c === 256) return themeVar('--text-strong', '#e6e6e6');
    if (c === 8) return themeVar('--text-muted', '#888');
    return ACI_FIXED[c] ?? themeVar('--text-faint', '#bbbbbb');
  }

  function draw() {
    if (!canvas || !container) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const dpr = window.devicePixelRatio || 1;
    const w = container.clientWidth;
    const h = container.clientHeight;
    canvas.width = w * dpr;
    canvas.height = h * dpr;
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    ctx.fillStyle = themeVar('--bg-app', '#0d0d0d');
    ctx.fillRect(0, 0, w, h);

    const data = project.imported;
    if (!data || data.segments.length === 0) {
      ctx.fillStyle = themeVar('--canvas-empty', '#555');
      ctx.font = '13px system-ui, sans-serif';
      ctx.fillText('Open a file to view geometry', 16, 24);
      return;
    }

    // Fit-to-view transform.
    const { min_x, min_y, max_x, max_y } = data.bbox;
    const dataW = Math.max(max_x - min_x, 1e-6);
    const dataH = Math.max(max_y - min_y, 1e-6);
    const margin = 32;
    const scale = Math.min((w - 2 * margin) / dataW, (h - 2 * margin) / dataH);
    const offX = margin - min_x * scale + (w - 2 * margin - dataW * scale) / 2;
    // Y flipped: DXF y-up, canvas y-down.
    const offY = h - margin - (-min_y) * scale - (h - 2 * margin - dataH * scale) / 2;

    const project2 = (px: number, py: number): [number, number] => [
      px * scale + offX,
      offY - py * scale,
    ];

    drawGrid(ctx, w, h, scale, offX, offY);
    drawAxes(ctx, w, h, offX, offY);
    lastTransform = { scale, offX, offY };

    // Filled-region preview: paint the area each Pocket op will actually
    // machine, before the wireframe so contours stay legible. Regions
    // come from the backend (pipeline.rs build_region_previews) and are
    // present whenever a /generate has run; we draw the current op's
    // region in accent color and others in a faded muted tone so the
    // user can see the full set without the selection getting lost.
    const regions = project.generated?.regions ?? [];
    if (regions.length > 0 && project.regionsVisible) {
      drawRegions(ctx, regions, scale, offX, offY);
    }

    const accent = themeVar('--accent', '#2d6cdf');
    const hoverColor = themeVar('--accent-strong', '#6e9ce6');
    const hoverObj = hoverIdx == null ? 0 : data.objects?.[hoverIdx] ?? 0;
    for (let i = 0; i < data.segments.length; i++) {
      const seg = data.segments[i];
      if (!project.visibleLayers.has(seg.layer)) continue;
      const objId = data.objects?.[i] ?? 0;
      const selected = objId !== 0 && project.selectedObjects.has(objId);
      const hovered = objId !== 0 && objId === hoverObj;
      ctx.lineWidth = selected ? 2.4 : hovered ? 1.8 : 1.25;
      ctx.strokeStyle = selected ? accent : hovered ? hoverColor : colorFor(seg.color);
      drawSegment(ctx, seg, project2);
    }

    drawTabs(ctx, project2);
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

  function regionPaths(
    regions: NonNullable<typeof project.generated>['regions'],
  ): RegionPath[] {
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

  function tracePolygonInto(
    path: Path2D,
    pts: Array<{ x: number; y: number }>,
  ) {
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
  ) {
    const tabFill = themeVar('--tab-marker', '#ffd23a');
    const tabStroke = themeVar('--bg-app', '#0d0d0d');
    for (const list of Object.values(project.tabs)) {
      for (const tab of list) {
        const [cx, cy] = p(tab.x, tab.y);
        ctx.beginPath();
        ctx.arc(cx, cy, 5, 0, Math.PI * 2);
        ctx.fillStyle = tabFill;
        ctx.fill();
        ctx.lineWidth = 1.5;
        ctx.strokeStyle = tabStroke;
        ctx.stroke();
      }
    }
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
</script>

<div class="canvas-host" bind:this={container}>
  <canvas
    bind:this={canvas}
    onpointermove={onPointerMove}
    onpointerleave={onPointerLeave}
    onpointerdown={onPointerDown}
  ></canvas>
  {#if project.selectedEntities.size > 0}
    <div class="selection-hud">{project.selectedEntities.size} selected · esc to clear</div>
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
</style>
