<script lang="ts">
  /// rt1.12: RasterEngrave op-properties fieldset. Shown when op.kind ===
  /// 'raster_engrave'. Reuses the relief image-import path (image →
  /// ReliefSource brightness grid, referenced by sourceId) — only the
  /// processing differs: brightness maps to laser power (S), not Z. Owns
  /// the source picker + image loader, resolution (mm / DPI), the
  /// power-curve radio + per-curve params with a LIVE preview + brightness
  /// histogram, scan direction, link mode, overscan, and a burn-time
  /// estimate. Styles inherited from OpPropertiesPanel's :global(.props…).
  import {
    project,
    type OpField,
    type OpFieldValue,
    type PowerCurve,
    type PowerCurveKind,
    type RasterEngraveOp,
  } from '../../state/project.svelte';
  import { decodeImageFile } from '../../state/relief_image';
  import {
    powerGrid,
    maxPower,
    powerGridToRgba,
    brightnessHistogram,
    estimateBurnSeconds,
  } from '../../cam/raster_preview';

  interface Props {
    op: RasterEngraveOp;
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();

  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let fileInput: HTMLInputElement | null = $state(null);

  const source = $derived(project.data.reliefSources.find((s) => s.id === op.sourceId) ?? null);
  const widthMm = $derived(source ? source.cols * source.cell : 0);
  const heightMm = $derived(source ? source.rows * source.cell : 0);

  /// Effective row pitch: the op's resolution, or the source's native
  /// cell size when 0 (matches the backend's `resolution_mm == 0` rule).
  const effectiveResMm = $derived(op.resolutionMm > 0 ? op.resolutionMm : source ? source.cell : 0);
  const dpi = $derived(effectiveResMm > 0 ? Math.round(25.4 / effectiveResMm) : 0);

  const tool = $derived(project.data.tools.find((t) => t.id === op.toolId) ?? null);
  const burnSeconds = $derived(
    source && tool
      ? estimateBurnSeconds({
          widthMm,
          heightMm,
          resolutionMm: effectiveResMm,
          feedMmMin: op.feedRateOverride ?? tool.feedRate,
          link: op.link,
          overscanFactor: op.overscanFactor,
          scanDirection: op.scanDirection,
        })
      : 0,
  );

  /// "1 h 23 m" / "4 m 12 s" / "38 s" — coarse, matches the estimate's
  /// precision (no false sub-second exactness).
  function formatDuration(sec: number): string {
    if (!(sec > 0)) return '—';
    const total = Math.round(sec);
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) return `${h} h ${m} m`;
    if (m > 0) return `${m} m ${s} s`;
    return `${s} s`;
  }

  // --- live preview + histogram ---------------------------------------
  let previewCanvas: HTMLCanvasElement | null = $state(null);
  let histCanvas: HTMLCanvasElement | null = $state(null);

  /// The brightness threshold a binary curve cuts at — drives the
  /// histogram cursor. null for curves with no single cutoff (linear).
  const thresholdLevel = $derived(
    op.powerCurve.kind === 'threshold' || op.powerCurve.kind === 'floyd_steinberg'
      ? op.powerCurve.level
      : null,
  );

  $effect(() => {
    const cv = previewCanvas;
    if (!cv || !source) return;
    const { cols, rows, brightness } = source;
    const grid = powerGrid(op.powerCurve, brightness, cols, rows);
    if (grid.length === 0) return;
    cv.width = cols;
    cv.height = rows;
    const ctx = cv.getContext('2d');
    if (!ctx) return;
    const rgba = powerGridToRgba(grid, cols, rows, maxPower(op.powerCurve));
    const img = ctx.createImageData(cols, rows);
    img.data.set(rgba);
    ctx.putImageData(img, 0, 0);
  });

  $effect(() => {
    const cv = histCanvas;
    if (!cv || !source) return;
    const bins = 48;
    const hist = brightnessHistogram(source.brightness, bins);
    const peak = Math.max(1, ...hist);
    const W = 200;
    const H = 56;
    cv.width = W;
    cv.height = H;
    const ctx = cv.getContext('2d');
    if (!ctx) return;
    ctx.clearRect(0, 0, W, H);
    ctx.fillStyle = 'rgba(120,120,120,0.85)';
    const bw = W / bins;
    for (let i = 0; i < bins; i++) {
      const bh = (hist[i] / peak) * (H - 2);
      ctx.fillRect(i * bw, H - bh, Math.max(1, bw - 0.5), bh);
    }
    // Threshold cursor (brightness 0→1 maps left→right).
    if (thresholdLevel != null) {
      const x = Math.min(W, Math.max(0, thresholdLevel * W));
      ctx.strokeStyle = 'var(--accent-strong, #e0533d)';
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, H);
      ctx.stroke();
    }
  });

  async function onImagePicked(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = ''; // allow re-picking the same file
    if (!file) return;
    loading = true;
    loadError = null;
    try {
      const grid = await decodeImageFile(file, 256);
      if (grid.cols === 0 || grid.rows === 0) throw new Error('empty image');
      // Default to a 100 mm-wide engrave at (0,0); rescale via Width.
      const targetWidthMm = widthMm > 0 ? widthMm : 100;
      const cell = targetWidthMm / grid.cols;
      const added = project.addReliefSource({
        name: file.name,
        origin: { x: 0, y: 0 },
        cell,
        cols: grid.cols,
        rows: grid.rows,
        brightness: grid.brightness,
      });
      patch('sourceId', added.id);
    } catch (err) {
      loadError = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  function setWidthMm(v: number) {
    if (!source || !(v > 0)) return;
    project.updateReliefSource(source.id, { cell: v / source.cols });
  }

  function numFromEvent(e: Event): number {
    return parseFloat((e.currentTarget as HTMLInputElement).value);
  }

  /// Switch the power-curve variant, seeding sensible defaults for the
  /// new kind (the whole tagged object is patched so undo is atomic).
  function setCurveKind(kind: PowerCurveKind) {
    if (kind === op.powerCurve.kind) return;
    // Reuse the current power magnitude where it carries over.
    const cur = op.powerCurve;
    const power = cur.kind === 'linear' ? cur.max : cur.power;
    let next: PowerCurve;
    switch (kind) {
      case 'linear':
        next = { kind: 'linear', min: 0, max: power || 1000 };
        break;
      case 'threshold':
        next = { kind: 'threshold', level: 0.5, power: power || 1000 };
        break;
      case 'floyd_steinberg':
        next = { kind: 'floyd_steinberg', level: 0.5, power: power || 1000 };
        break;
      case 'bayer':
        next = { kind: 'bayer', matrixSize: 4, power: power || 1000 };
        break;
    }
    patch('powerCurve', next);
  }

  /// Patch a single field of the current curve, preserving its kind.
  function patchCurve(partial: Partial<PowerCurve>) {
    patch('powerCurve', { ...op.powerCurve, ...partial } as PowerCurve);
  }

  const DPI_PRESETS = [
    { dpi: 127, mm: 0.2 },
    { dpi: 254, mm: 0.1 },
    { dpi: 508, mm: 0.05 },
  ];
</script>

<fieldset>
  <legend>Engrave source</legend>
  <label class="row">
    <span>Image</span>
    <div class="num-cell">
      <select
        value={op.sourceId}
        onchange={(e) =>
          patch('sourceId', parseInt((e.currentTarget as HTMLSelectElement).value, 10))}
      >
        {#if project.data.reliefSources.length === 0}
          <option value={0}>— none loaded —</option>
        {/if}
        {#each project.data.reliefSources as s (s.id)}
          <option value={s.id}>{s.name} ({s.cols}×{s.rows})</option>
        {/each}
      </select>
    </div>
  </label>
  <input
    type="file"
    accept="image/*"
    style="display:none"
    bind:this={fileInput}
    onchange={onImagePicked}
  />
  <button type="button" onclick={() => fileInput?.click()} disabled={loading}>
    {loading ? 'Decoding…' : 'Load image…'}
  </button>
  {#if loadError}
    <p class="err" role="alert">Couldn’t load image: {loadError}</p>
  {/if}
  {#if source}
    <label
      class="row"
      title="Physical width of the engrave on the workpiece. Height follows the image aspect ratio."
    >
      <span>Width</span>
      <div class="num-cell">
        <input
          type="number"
          step="1"
          min="1"
          value={widthMm.toFixed(2)}
          onchange={(e) => setWidthMm(numFromEvent(e))}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <p class="hint">
      {source.cols}×{source.rows} px → {widthMm.toFixed(0)} × {heightMm.toFixed(0)} mm
    </p>
  {/if}
</fieldset>

{#if source}
  <fieldset>
    <legend>Power curve</legend>
    <div class="curve-radios" role="radiogroup" aria-label="Power curve">
      {#each [['linear', 'Linear'], ['threshold', 'Threshold'], ['floyd_steinberg', 'Floyd–Steinberg'], ['bayer', 'Bayer']] as [k, label] (k)}
        <label class="radio">
          <input
            type="radio"
            name="power-curve-{op.id}"
            checked={op.powerCurve.kind === k}
            onchange={() => setCurveKind(k as PowerCurveKind)}
          />
          <span>{label}</span>
        </label>
      {/each}
    </div>

    {#if op.powerCurve.kind === 'linear'}
      <label class="row" title="Laser S at white (brightest pixels). Usually 0 = off.">
        <span>Min S</span>
        <div class="num-cell">
          <input
            type="number"
            step="10"
            min="0"
            value={op.powerCurve.min}
            onchange={(e) => {
              const v = numFromEvent(e);
              if (!isNaN(v) && v >= 0) patchCurve({ min: v });
            }}
          />
        </div>
      </label>
      <label class="row" title="Laser S at black (darkest pixels) — the hottest burn.">
        <span>Max S</span>
        <div class="num-cell">
          <input
            type="number"
            step="10"
            min="0"
            value={op.powerCurve.max}
            onchange={(e) => {
              const v = numFromEvent(e);
              if (!isNaN(v) && v >= 0) patchCurve({ max: v });
            }}
          />
        </div>
      </label>
    {:else if op.powerCurve.kind === 'threshold' || op.powerCurve.kind === 'floyd_steinberg'}
      <label
        class="row"
        title="Binarization point on brightness [0,1]. Pixels darker than this burn; lighter pixels stay off."
      >
        <span>Level</span>
        <div class="num-cell">
          <input
            type="number"
            step="0.05"
            min="0"
            max="1"
            value={op.powerCurve.level}
            onchange={(e) => {
              const v = numFromEvent(e);
              if (!isNaN(v) && v >= 0 && v <= 1) patchCurve({ level: v });
            }}
          />
        </div>
      </label>
      <label class="row" title="Laser S emitted for an 'on' (burning) pixel.">
        <span>Power S</span>
        <div class="num-cell">
          <input
            type="number"
            step="10"
            min="0"
            value={op.powerCurve.power}
            onchange={(e) => {
              const v = numFromEvent(e);
              if (!isNaN(v) && v >= 0) patchCurve({ power: v });
            }}
          />
        </div>
      </label>
    {:else if op.powerCurve.kind === 'bayer'}
      <label
        class="row"
        title="Ordered-dither tile size. Larger = finer tonal steps, coarser tile."
      >
        <span>Matrix</span>
        <div class="num-cell">
          <select
            value={op.powerCurve.matrixSize}
            onchange={(e) =>
              patchCurve({
                matrixSize: parseInt((e.currentTarget as HTMLSelectElement).value, 10),
              })}
          >
            <option value={2}>2 × 2</option>
            <option value={4}>4 × 4</option>
            <option value={8}>8 × 8</option>
          </select>
        </div>
      </label>
      <label class="row" title="Laser S emitted for an 'on' (burning) pixel.">
        <span>Power S</span>
        <div class="num-cell">
          <input
            type="number"
            step="10"
            min="0"
            value={op.powerCurve.power}
            onchange={(e) => {
              const v = numFromEvent(e);
              if (!isNaN(v) && v >= 0) patchCurve({ power: v });
            }}
          />
        </div>
      </label>
    {/if}

    <div class="preview-wrap" title="Live preview of the engrave: darker = hotter burn.">
      <canvas bind:this={previewCanvas} class="preview"></canvas>
    </div>
    <div class="hist-wrap">
      <canvas bind:this={histCanvas} class="hist"></canvas>
      <p class="hint">
        Brightness histogram (dark ← → light){thresholdLevel != null ? ' — line = level' : ''}
      </p>
    </div>
  </fieldset>

  <fieldset>
    <legend>Scan</legend>
    <label class="row" title="Per-pixel scan resolution (row pitch). Finer = sharper, slower.">
      <span>Resolution</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.01"
          min="0"
          placeholder="native"
          value={op.resolutionMm}
          onchange={(e) => {
            const v = numFromEvent(e);
            patch('resolutionMm', isNaN(v) || v < 0 ? 0 : v);
          }}
        />
        <span class="unit">mm</span>
      </div>
    </label>
    <div class="dpi-presets">
      {#each DPI_PRESETS as p (p.dpi)}
        <button type="button" class="chip" onclick={() => patch('resolutionMm', p.mm)}>
          {p.dpi} DPI
        </button>
      {/each}
    </div>
    {#if dpi > 0}
      <p class="hint">≈ {dpi} DPI ({effectiveResMm.toFixed(3)} mm/px)</p>
    {/if}
    <label class="row">
      <span>Direction</span>
      <div class="num-cell">
        <select
          value={op.scanDirection}
          onchange={(e) =>
            patch(
              'scanDirection',
              (e.currentTarget as HTMLSelectElement).value as 'along_x' | 'along_y',
            )}
        >
          <option value="along_x">Along X</option>
          <option value="along_y">Along Y</option>
        </select>
      </div>
    </label>
    <label
      class="row"
      title="Lift-between: every row scans the same way (artifact-free, slower). Bidirectional: alternate rows reverse (faster; prefer Bayer dither to avoid alignment artifacts)."
    >
      <span>Link</span>
      <div class="num-cell">
        <select
          value={op.link}
          onchange={(e) =>
            patch(
              'link',
              (e.currentTarget as HTMLSelectElement).value as 'lift_between' | 'bidirectional',
            )}
        >
          <option value="lift_between">Lift between</option>
          <option value="bidirectional">Bidirectional</option>
        </select>
      </div>
    </label>
    <label
      class="row"
      title="Overscan past each row edge as a fraction of the row length, so the head reaches commanded power before crossing pixels. 0 = none."
    >
      <span>Overscan</span>
      <div class="num-cell">
        <input
          type="number"
          step="0.05"
          min="0"
          value={op.overscanFactor}
          onchange={(e) => {
            const v = numFromEvent(e);
            if (!isNaN(v) && v >= 0) patch('overscanFactor', v);
          }}
        />
        <span class="unit">×</span>
      </div>
    </label>
    <p class="hint estimate">
      Estimated burn time: <strong>{formatDuration(burnSeconds)}</strong>
      {#if burnSeconds > 0}<span class="approx"> (rough)</span>{/if}
    </p>
    <p class="hint">
      Raster engraving burns a grayscale image row-by-row — dark pixels burn hotter. Tune the power
      curve so the preview matches the contrast you want before kicking off a long run.
    </p>
  </fieldset>
{/if}

<style>
  .err {
    color: var(--danger, #c0392b);
    font-size: 0.8em;
    margin: 0.25em 0 0;
  }
  .hint {
    font-size: 0.78em;
    opacity: 0.7;
    margin: 0.35em 0 0;
  }
  .estimate {
    opacity: 0.95;
  }
  .approx {
    opacity: 0.6;
  }
  .curve-radios {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.15rem 0.6rem;
    margin-bottom: 0.4rem;
  }
  .radio {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.82em;
    cursor: pointer;
  }
  .preview-wrap {
    margin-top: 0.5rem;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: repeating-conic-gradient(#808080 0% 25%, #a0a0a0 0% 50%) 50% / 12px 12px;
    overflow: hidden;
  }
  .preview {
    display: block;
    width: 100%;
    height: auto;
    image-rendering: pixelated;
  }
  .hist-wrap {
    margin-top: 0.4rem;
  }
  .hist {
    display: block;
    width: 100%;
    height: auto;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: var(--bg-elevated, #1c1c1c);
  }
  .dpi-presets {
    display: flex;
    gap: 0.3rem;
    margin-top: 0.3rem;
  }
  .chip {
    font-size: 0.74em;
    padding: 0.1rem 0.4rem;
  }
</style>
