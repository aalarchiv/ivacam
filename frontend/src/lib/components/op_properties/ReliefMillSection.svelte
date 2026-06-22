<script lang="ts">
  /// ReliefMill op-properties fieldset. Shown when op.kind ===
  /// 'relief_mill'. Owns the relief-source picker + image loader, the depth
  /// range, scallop/stepover, scan direction, and the physical width (which
  /// sets the source cell size). Styles inherited from OpPropertiesPanel's
  /// :global(.props ...) rules.
  import {
    project,
    type OpField,
    type OpFieldValue,
    type ReliefMillOp,
  } from '../../state/project.svelte';
  import { t } from '../../i18n';
  import { decodeImageFile } from '../../state/relief_image';

  interface Props {
    op: ReliefMillOp;
    patch: <K extends OpField>(field: K, value: OpFieldValue<K>) => void;
  }
  let { op, patch }: Props = $props();

  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let fileInput: HTMLInputElement | null = $state(null);

  const source = $derived(project.data.reliefSources.find((s) => s.id === op.sourceId) ?? null);
  /// Physical width (mm) of the loaded relief = cols * cell. Editing it
  /// rescales the source's cell so the relief covers that width.
  const widthMm = $derived(source ? source.cols * source.cell : 0);
  const heightMm = $derived(source ? source.rows * source.cell : 0);

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
      // Default to a 100 mm-wide relief at (0,0); the user can rescale via
      // the Width field.
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
</script>

<fieldset>
  <legend>{t('ops.relief.source.legend')}</legend>
  <label class="row">
    <span>Image</span>
    <div class="num-cell">
      <select
        value={op.sourceId}
        onchange={(e) =>
          patch('sourceId', parseInt((e.currentTarget as HTMLSelectElement).value, 10))}
      >
        {#if project.data.reliefSources.length === 0}
          <option value={0}>{t('ops.image.none_loaded')}</option>
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
      title="Physical width of the relief on the workpiece. Height follows the image aspect ratio."
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

<fieldset>
  <legend>{t('ops.relief.depth.legend')}</legend>
  <label class="row" title="Deepest cut — where the darkest pixels map (negative).">
    <span>Min Z</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.5"
        max="0"
        value={op.zMinMm}
        onchange={(e) => {
          const v = numFromEvent(e);
          if (!isNaN(v)) patch('zMinMm', v);
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label
    class="row"
    title="Shallowest cut — where the brightest pixels map (usually 0 = stock top)."
  >
    <span>Max Z</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.5"
        max="0"
        value={op.zMaxMm}
        onchange={(e) => {
          const v = numFromEvent(e);
          if (!isNaN(v)) patch('zMaxMm', v);
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label
    class="row"
    title="Swap the mapping so dark areas become high and light areas become deep."
  >
    <span>Invert</span>
    <input
      type="checkbox"
      checked={op.invert}
      onchange={(e) => patch('invert', (e.currentTarget as HTMLInputElement).checked)}
    />
  </label>
</fieldset>

<fieldset>
  <legend>{t('ops.relief.finish.legend')}</legend>
  <label
    class="row"
    title="Allowed ridge height left between adjacent passes. Smaller = finer finish, longer cut."
  >
    <span>Scallop</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.01"
        min="0.005"
        value={op.scallopHeightMm}
        onchange={(e) => {
          const v = numFromEvent(e);
          if (!isNaN(v) && v > 0) patch('scallopHeightMm', v);
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label
    class="row"
    title="Override the stepover directly instead of deriving it from the scallop height. Empty = auto (from scallop)."
  >
    <span>Stepover</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.1"
        min="0"
        placeholder="auto"
        value={op.stepoverMm ?? ''}
        onchange={(e) => {
          const v = numFromEvent(e);
          patch('stepoverMm', isNaN(v) || v <= 0 ? null : v);
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <label class="row">
    <span>Scan</span>
    <div class="num-cell">
      <select
        value={op.scanDirection}
        onchange={(e) =>
          patch(
            'scanDirection',
            (e.currentTarget as HTMLSelectElement).value as 'along_x' | 'along_y',
          )}
      >
        <option value="along_x">{t('ops.relief.scan.along_x')}</option>
        <option value="along_y">{t('ops.relief.scan.along_y')}</option>
      </select>
    </div>
  </label>
  <label class="row" title="Point spacing along each scanline. Finer = smoother path, more G-code.">
    <span>Step</span>
    <div class="num-cell">
      <input
        type="number"
        step="0.1"
        min="0.05"
        value={op.alongStepMm}
        onchange={(e) => {
          const v = numFromEvent(e);
          if (!isNaN(v) && v > 0) patch('alongStepMm', v);
        }}
      />
      <span class="unit">mm</span>
    </div>
  </label>
  <p class="hint">
    Relief surfacing is a finish pass — rough the bulk first with a flat endmill (Pocket / Profile),
    then this ball-nose pass cleans the surface.
  </p>
</fieldset>

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
</style>
