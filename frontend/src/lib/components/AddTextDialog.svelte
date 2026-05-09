<script lang="ts">
  /**
   * AddTextDialog — single-step "add text + create CAM op of style X" flow.
   *
   * Replaces the multi-step manual flow (import a TEXT'd DXF → add op →
   * wire its source) with a one-shot dialog that mirrors what Estlcam
   * users expect: pick text, font, position, and a style.
   *
   * Style → op mapping is in `applyStyle()`. The 8 styles plus their
   * default tool kinds and depths are declared in `STYLE_TABLE` below.
   *
   * Bundled fonts ship under `/fonts/`. Users can also load any TTF via
   * the file picker; single-line / engraving fonts are auto-detected by
   * the Rust core (`is_single_line_font`) which drives the warning chip
   * on the Engraving style.
   */
  import { project } from '../state/project.svelte';
  import { defaultClient } from '../api/http';
  import type { Segment, RenderTextRequest } from '../api/types';
  import {
    STYLE_TABLE,
    describeStyleOp,
    engravingMismatch,
    type TextStyle,
  } from './text_style';

  interface Props {
    open: boolean;
    onClose: () => void;
  }
  let { open, onClose }: Props = $props();

  interface BundledFont {
    label: string;
    path: string;
  }

  const BUNDLED_FONTS: BundledFont[] = [
    { label: 'DejaVu Sans (filled-outline, bundled)', path: '/fonts/DejaVuSans.ttf' },
  ];

  let text = $state('Text');
  let style = $state<TextStyle>('engraving');
  let sizeMm = $state(12);
  let posX = $state(0);
  let posY = $state(0);
  let depth = $state(-0.5);
  let toolId = $state<number>(1);
  let userFontFile = $state<File | null>(null);
  let bundledFontPath = $state<string>(BUNDLED_FONTS[0]?.path ?? '');
  let useUserFont = $state(false);
  let busy = $state(false);
  let errorMsg = $state<string | null>(null);
  /// Last successful render's single-line / family classification — drives
  /// the "use a single-line font" chip on the Engraving style.
  let lastFontIsSingleLine = $state<boolean | null>(null);
  let lastFontFamily = $state<string | null>(null);

  /// Cache of loaded font bytes keyed by source URL/filename. Re-renders
  /// (depth tweaks etc.) don't refetch.
  const fontCache = new Map<string, Uint8Array>();
  /// Cached rendered preview, keyed by (font|text|size). The preview
  /// drives the on-canvas placement; we re-render only when the user
  /// changes the text geometry inputs.
  let previewSegments = $state<Segment[] | null>(null);

  const client = defaultClient();

  // Reset the form when reopened. Center position starts from current
  // imported bbox / stock, falling back to (0, 0).
  $effect(() => {
    if (!open) return;
    const def = defaultPosition();
    posX = def.x;
    posY = def.y;
    text = 'Text';
    sizeMm = 12;
    style = 'engraving';
    bundledFontPath = BUNDLED_FONTS[0]?.path ?? '';
    useUserFont = false;
    userFontFile = null;
    depth = STYLE_TABLE[style].defaultDepth ?? -0.5;
    toolId = pickDefaultTool(style);
    errorMsg = null;
    previewSegments = null;
    lastFontIsSingleLine = null;
    lastFontFamily = null;
  });

  // Re-pick a sensible default tool + depth when style changes.
  $effect(() => {
    const spec = STYLE_TABLE[style];
    if (spec.defaultDepth != null) depth = spec.defaultDepth;
    toolId = pickDefaultTool(style);
  });

  // Re-render preview whenever the text geometry inputs change. Keeps
  // the modal responsive — render is local (WASM) or one /text round
  // trip (HTTP), both well under 100 ms for a few characters.
  $effect(() => {
    if (!open) return;
    void renderPreview();
  });

  function defaultPosition(): { x: number; y: number } {
    const stock = project.stock;
    if (project.imported && project.imported.bbox) {
      const b = project.imported.bbox;
      return { x: (b.min_x + b.max_x) / 2, y: (b.min_y + b.max_y) / 2 };
    }
    if (stock.mode === 'manual') {
      return { x: stock.customX / 2, y: stock.customY / 2 };
    }
    return { x: 0, y: 0 };
  }

  function pickDefaultTool(s: TextStyle): number {
    const want = STYLE_TABLE[s].toolKind;
    if (!want) return project.tools[0]?.id ?? 1;
    const match = project.tools.find((t) => t.kind === want);
    if (match) return match.id;
    // Fall back to any tool if the required kind isn't in the library.
    return project.tools[0]?.id ?? 1;
  }

  const filteredTools = $derived.by(() => {
    const want = STYLE_TABLE[style].toolKind;
    if (!want) return project.tools;
    return project.tools.filter((t) => t.kind === want);
  });

  const styleEngravingMismatch = $derived(
    engravingMismatch(style, lastFontIsSingleLine, previewSegments?.length ?? 0),
  );

  async function loadFontBytes(): Promise<Uint8Array | null> {
    if (useUserFont) {
      if (!userFontFile) return null;
      const key = `user:${userFontFile.name}:${userFontFile.size}`;
      const cached = fontCache.get(key);
      if (cached) return cached;
      const buf = new Uint8Array(await userFontFile.arrayBuffer());
      fontCache.set(key, buf);
      return buf;
    }
    const url = bundledFontPath;
    if (!url) return null;
    const cached = fontCache.get(url);
    if (cached) return cached;
    const res = await fetch(url);
    if (!res.ok) {
      throw new Error(`fetch ${url}: ${res.status}`);
    }
    const buf = new Uint8Array(await res.arrayBuffer());
    fontCache.set(url, buf);
    return buf;
  }

  async function renderPreview(): Promise<void> {
    if (!text.trim()) {
      previewSegments = [];
      return;
    }
    try {
      const bytes = await loadFontBytes();
      if (!bytes) {
        previewSegments = null;
        return;
      }
      const req: RenderTextRequest = {
        font_bytes: Array.from(bytes) as unknown as RenderTextRequest['font_bytes'],
        text,
        origin: { x: posX, y: posY },
        height_mm: sizeMm,
        layer: 'TEXT',
        color: 7,
      };
      const resp = await client.renderText(req);
      previewSegments = resp.segments;
      lastFontIsSingleLine = resp.single_line;
      lastFontFamily = resp.family_name ?? null;
      errorMsg = null;
    } catch (e) {
      errorMsg = e instanceof Error ? e.message : String(e);
      previewSegments = null;
    }
  }

  async function apply() {
    busy = true;
    errorMsg = null;
    try {
      // Make sure the latest inputs are reflected before applying.
      await renderPreview();
      const segs = previewSegments;
      if (!segs || segs.length === 0) {
        throw new Error('No geometry produced — check the text and font.');
      }
      const ids = project.appendImportedSegments(
        segs,
        'TEXT',
        lastFontIsSingleLine === true,
      );
      if (style !== 'plain') {
        applyStyle(ids);
      }
      onClose();
    } catch (e) {
      errorMsg = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function applyStyle(objectIds: number[]) {
    const tool = project.tools.find((t) => t.id === toolId);
    const toolDiameter = tool?.diameter ?? 3;
    const desc = describeStyleOp(style, objectIds, toolId, toolDiameter, depth);
    if (!desc) return;
    const op = project.addOperation(desc.kind);
    project.updateOperation(op.id, {
      name: desc.name,
      toolId: desc.toolId,
      depth: desc.depth,
      sourceObjects: desc.sourceObjects,
      sourceCombine: desc.sourceCombine,
      offset: desc.offset,
      frameShape: desc.frameShape,
      framePaddingMm: desc.framePaddingMm,
    });
  }

  function switchToEngravingFont() {
    // Heuristic: there's no bundled engraving font in v1 (license-vetting
    // pending — see issue n4y). Best we can do is point the user at the
    // file picker.
    useUserFont = true;
    errorMsg = 'Pick a single-line / Hershey TTF via "Custom font" — no engraving font is bundled yet.';
  }

  function onUserFontPick(e: Event) {
    const t = e.target as HTMLInputElement;
    const f = t.files?.[0];
    if (f) {
      userFontFile = f;
      useUserFont = true;
    }
  }

  function close() {
    onClose();
  }
</script>

{#if open}
  <div class="overlay" role="dialog" aria-modal="true" aria-labelledby="addtext-title">
    <div class="modal">
      <header>
        <h2 id="addtext-title">Add Text</h2>
        <button class="close" onclick={close} aria-label="Close">×</button>
      </header>

      <div class="body">
        <label class="full">
          <span>Text</span>
          <textarea bind:value={text} rows="2"></textarea>
        </label>

        <fieldset class="full">
          <legend>Font</legend>
          <label class="row">
            <input type="radio" bind:group={useUserFont} value={false} />
            <select bind:value={bundledFontPath} disabled={useUserFont}>
              {#each BUNDLED_FONTS as f (f.path)}
                <option value={f.path}>{f.label}</option>
              {/each}
            </select>
          </label>
          <label class="row">
            <input type="radio" bind:group={useUserFont} value={true} />
            <span class="picker">
              <input type="file" accept=".ttf,.otf" onchange={onUserFontPick} />
              {#if userFontFile}
                <span class="filename">{userFontFile.name}</span>
              {/if}
            </span>
          </label>
          {#if lastFontFamily}
            <p class="font-meta">Loaded: <strong>{lastFontFamily}</strong>{lastFontIsSingleLine ? ' (single-line)' : ''}</p>
          {/if}
        </fieldset>

        <label>
          <span>Size (mm)</span>
          <input type="number" bind:value={sizeMm} step="0.5" min="0.1" />
        </label>
        <label>
          <span>Position X</span>
          <input type="number" bind:value={posX} step="1" />
        </label>
        <label>
          <span>Position Y</span>
          <input type="number" bind:value={posY} step="1" />
        </label>

        <fieldset class="full styles">
          <legend>Style</legend>
          <div class="grid">
            {#each Object.entries(STYLE_TABLE) as [k, spec] (k)}
              <label class="style-opt" title={spec.help}>
                <input type="radio" bind:group={style} value={k as TextStyle} />
                <span>{spec.label}</span>
              </label>
            {/each}
          </div>
        </fieldset>

        {#if styleEngravingMismatch}
          <div class="chip warn">
            <span>This font is filled-outline. Engraving style needs a single-line / Hershey font.</span>
            <button class="chip-btn" onclick={switchToEngravingFont}>Switch font</button>
          </div>
        {/if}

        {#if STYLE_TABLE[style].toolKind != null}
          <label>
            <span>Tool</span>
            <select bind:value={toolId}>
              {#each filteredTools as t (t.id)}
                <option value={t.id}>{t.name} ({t.kind}, {t.diameter} mm)</option>
              {/each}
              {#if filteredTools.length === 0}
                <option value={0}>(no {STYLE_TABLE[style].toolKind} in library)</option>
              {/if}
            </select>
          </label>
        {/if}

        {#if STYLE_TABLE[style].defaultDepth != null}
          <label>
            <span>Depth (mm)</span>
            <input type="number" bind:value={depth} step="0.1" />
          </label>
        {/if}

        {#if errorMsg}
          <p class="error full">{errorMsg}</p>
        {/if}
      </div>

      <footer>
        <button class="secondary" onclick={close}>Cancel</button>
        <button class="primary" onclick={apply} disabled={busy}>Add</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, black 50%, transparent);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 50;
  }
  .modal {
    width: min(540px, 95vw);
    max-height: 90vh;
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    box-shadow: 0 10px 40px rgba(0, 0, 0, 0.4);
    display: grid;
    grid-template-rows: auto 1fr auto;
    overflow: hidden;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.7rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
  }
  h2 {
    font-size: 0.95rem;
    margin: 0;
    color: var(--text-strong);
  }
  .close {
    background: transparent;
    color: var(--text-muted);
    border: 0;
    font-size: 1.2rem;
    cursor: pointer;
    padding: 0 0.3rem;
  }
  .body {
    padding: 0.7rem;
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.5rem;
    overflow: auto;
  }
  .full {
    grid-column: 1 / -1;
  }
  label {
    display: grid;
    grid-template-columns: minmax(0, 7rem) minmax(0, 1fr);
    align-items: center;
    gap: 0.5rem;
    font-size: 0.78rem;
  }
  label.full {
    grid-template-columns: 7rem 1fr;
  }
  textarea,
  input[type='number'],
  select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.2rem 0.4rem;
    font-size: 0.8rem;
    font-family: inherit;
  }
  textarea {
    resize: vertical;
    min-height: 2rem;
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.4rem 0.5rem;
    display: grid;
    gap: 0.3rem;
  }
  legend {
    font-size: 0.7rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 0 0.3rem;
  }
  .row {
    grid-template-columns: auto 1fr;
    gap: 0.4rem;
  }
  .picker {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
  }
  .filename {
    color: var(--text-muted);
    font-size: 0.72rem;
  }
  .font-meta {
    margin: 0.1rem 0 0;
    font-size: 0.7rem;
    color: var(--text-muted);
  }
  .styles .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.2rem;
  }
  .style-opt {
    grid-template-columns: auto 1fr;
    gap: 0.4rem;
    cursor: pointer;
  }
  .chip {
    grid-column: 1 / -1;
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.3rem 0.5rem;
    border-radius: 4px;
    font-size: 0.75rem;
  }
  .chip.warn {
    background: color-mix(in srgb, var(--warn) 16%, var(--bg-elevated));
    border: 1px solid var(--warn);
    color: var(--text-strong);
  }
  .chip-btn {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.1rem 0.4rem;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .error {
    color: var(--error);
    font-size: 0.78rem;
    margin: 0;
  }
  input[type='radio'] {
    accent-color: var(--accent);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
    padding: 0.5rem 0.7rem;
    border-top: 1px solid var(--border);
    background: var(--bg-elevated);
  }
  .primary {
    background: var(--accent);
    color: white;
    border: 0;
    padding: 0.3rem 0.8rem;
    border-radius: 3px;
    cursor: pointer;
  }
  .primary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .secondary {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 0.3rem 0.8rem;
    border-radius: 3px;
    cursor: pointer;
  }
</style>
