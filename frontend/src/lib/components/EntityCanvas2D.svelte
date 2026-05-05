<script lang="ts">
  import { onMount } from 'svelte';
  import { project } from '../state/project.svelte';
  import type { Segment } from '../api/types';

  // Minimal AutoCAD ACI palette. Replace with full table later (mtm.8 follow-up).
  const ACI_COLORS: Record<number, string> = {
    1: '#ff0000',
    2: '#ffff00',
    3: '#00ff00',
    4: '#00ffff',
    5: '#0000ff',
    6: '#ff00ff',
    7: '#e6e6e6',
    8: '#414141',
    9: '#808080',
    256: '#e6e6e6',
  };

  let canvas: HTMLCanvasElement;
  let container: HTMLDivElement;

  onMount(() => {
    const ro = new ResizeObserver(() => draw());
    ro.observe(container);
    draw();
    return () => ro.disconnect();
  });

  $effect(() => {
    void project.imported;
    void project.visibleLayers;
    draw();
  });

  function colorFor(c: number): string {
    return ACI_COLORS[c] ?? '#bbbbbb';
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

    ctx.fillStyle = '#0d0d0d';
    ctx.fillRect(0, 0, w, h);

    const data = project.imported;
    if (!data || data.segments.length === 0) {
      ctx.fillStyle = '#555';
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

    ctx.lineWidth = 1.25;
    for (const seg of data.segments) {
      if (!project.visibleLayers.has(seg.layer)) continue;
      ctx.strokeStyle = colorFor(seg.color);
      drawSegment(ctx, seg, project2);
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
    for (const [step, color] of [
      [minorStep, '#1a1a1a'],
      [majorStep, '#262626'],
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
    ctx.strokeStyle = '#882222';
    ctx.beginPath();
    ctx.moveTo(0, offY);
    ctx.lineTo(w, offY);
    ctx.stroke();
    ctx.strokeStyle = '#226622';
    ctx.beginPath();
    ctx.moveTo(offX, 0);
    ctx.lineTo(offX, h);
    ctx.stroke();
  }
</script>

<div class="canvas-host" bind:this={container}>
  <canvas bind:this={canvas}></canvas>
</div>

<style>
  .canvas-host {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    background: #0d0d0d;
  }
  canvas {
    display: block;
  }
</style>
