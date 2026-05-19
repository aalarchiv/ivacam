<script lang="ts">
  /**
   * AddTextDialog — single-step "add editable text + create engrave op" flow.
   *
   * Phase 3 of the text-engraving rework: instead of baking the rendered
   * glyphs into `project.imported.segments` and creating an op that
   * targets the resulting object ids, we now create a persistent
   * `TextLayer` plus an Engrave op pointing at the layer's synthetic
   * geometry name (`__text_<id>`). Editing the text afterwards happens
   * via the sidebar TextList — the dialog is purely "create new".
   *
   * Bundled fonts ship under `/fonts/`. Users can also load any TTF via
   * the file picker; single-line / engraving fonts are auto-detected by
   * the Rust core (`is_single_line_font`) which drives the warning chip
   * on the Engraving style.
   */
  import { project } from '../state/project.svelte';
  import { defaultClient } from '../api/http';
  import type { Segment, RenderTextRequest } from '../api/types';
  import type { TextFontSource, TextLayer } from '../state/project.svelte';
  import { STYLE_TABLE, engravingMismatch, type TextStyle } from './text_style';
  import { computeFootprint } from '../sim/driver';
  import Modal from './Modal.svelte';
  import { onMount } from 'svelte';

  interface Props {
    open: boolean;
    onClose: () => void;
  }
  let { open, onClose }: Props = $props();

  interface BundledFont {
    label: string;
    path: string;
    /// CSS @font-face family name registered at mount time; the
    /// dropdown row + sample chip render in this family so the user
    /// previews the actual font glyphs before choosing (6y3m).
    family: string;
  }

  const BUNDLED_FONTS: BundledFont[] = [
    {
      label: 'DejaVu Sans (filled-outline, bundled)',
      path: '/fonts/DejaVuSans.ttf',
      family: 'wiac-preview-dejavu',
    },
  ];
  /// Glyph sample drawn in each bundled font's family on the dropdown
  /// rows. Mixed digits / ASCII / accented / currency so the user can
  /// pick visually based on the shapes that actually matter.
  const FONT_SAMPLE = 'AaBb 0123 äöß€';

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
  /// 6y3m: dropdown popover state. Each bundled font is registered as a
  /// FontFace at mount so the rows + selected chip can render in the
  /// actual font's glyphs (vs the platform default that <select>'s
  /// option text would have used).
  let fontDropdownOpen = $state(false);
  let fontsLoaded = $state(false);
  onMount(() => {
    if (typeof document === 'undefined' || !('fonts' in document)) return;
    let cancelled = false;
    void Promise.all(
      BUNDLED_FONTS.map(async (f) => {
        try {
          const face = new FontFace(f.family, `url(${f.path})`);
          await face.load();
          if (!cancelled) document.fonts.add(face);
        } catch (e) {
          console.warn('bundled font load failed', f, e);
        }
      }),
    ).then(() => {
      if (!cancelled) fontsLoaded = true;
    });
    return () => {
      cancelled = true;
    };
  });
  const selectedBundledFont = $derived(BUNDLED_FONTS.find((f) => f.path === bundledFontPath));
  function pickBundledFont(path: string) {
    bundledFontPath = path;
    useUserFont = false;
    fontDropdownOpen = false;
  }
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
    // Center the new text on the effective stock footprint — same
    // computation Scene3D / sim use, so the preview lands inside the
    // visible workpiece regardless of whether a drawing is loaded.
    const fp = computeFootprint(project.transformedImport, project.stock, project.machine.workArea);
    return {
      x: (fp.minX + fp.maxX) / 2,
      y: (fp.minY + fp.maxY) / 2,
    };
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

  /// Base64-encode a Uint8Array without blowing the JS stack on bigger
  /// TTFs. String.fromCharCode(...bytes) breaks at ~100 KB; we chunk.
  function bytesToBase64(bytes: Uint8Array): string {
    let binary = '';
    const chunk = 0x8000;
    for (let i = 0; i < bytes.length; i += chunk) {
      binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
    }
    return btoa(binary);
  }

  async function apply() {
    busy = true;
    errorMsg = null;
    let txOpen = false;
    try {
      const bytes = await loadFontBytes();
      if (!bytes) {
        throw new Error('No font selected.');
      }
      const trimmed = text.trim();
      if (trimmed.length === 0) {
        throw new Error('Text must not be empty.');
      }
      // Refresh the single-line classification one last time so the
      // TextLayer.singleLine cached flag reflects the current font.
      await renderPreview();

      const bytes_b64 = bytesToBase64(bytes);
      const fontSource: TextFontSource = useUserFont
        ? {
            kind: 'user',
            filename: userFontFile?.name ?? 'font.ttf',
            bytes_b64,
          }
        : {
            kind: 'bundled',
            path: bundledFontPath,
            bytes_b64,
          };
      const isMultiline = trimmed.includes('\n');
      const layerSeed: Omit<TextLayer, 'id' | 'name'> = {
        kind: isMultiline ? 'MTEXT' : 'TEXT',
        text: trimmed,
        fontSource,
        sizeMm,
        origin: { x: posX, y: posY },
        rotationDeg: 0,
        letterSpacingMm: 0,
        lineSpacingMm: 0,
        alignment: 'left',
        singleLine: lastFontIsSingleLine === true,
      };

      project.history.beginTransaction('Add text');
      txOpen = true;
      const layer = project.addTextLayer(layerSeed);
      if (style !== 'plain') {
        const op = project.addOperation('engrave');
        const opName =
          style === 'engraving' ? `Engrave ${layer.name}` : `${STYLE_TABLE[style].label} ${layer.name}`;
        project.updateOperation(op.id, {
          name: opName,
          toolId,
          depth,
          sourceObjects: undefined,
          sourceLayers: [`__text_${layer.id}`],
          offset: 'on',
        });
      }
      project.history.commitTransaction();
      txOpen = false;
      project.selectedTextLayerId = layer.id;
      onClose();
    } catch (e) {
      if (txOpen) project.cancelTransaction();
      errorMsg = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function switchToEngravingFont() {
    // Heuristic: there's no bundled engraving font in v1 (license-vetting
    // pending — see issue n4y). Best we can do is point the user at the
    // file picker.
    useUserFont = true;
    errorMsg =
      'Pick a single-line / Hershey TTF via "Custom font" — no engraving font is bundled yet.';
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
  <Modal onClose={close} modalClass="addtext-modal">
    <header>
      <h2 id="addtext-title">Add Text</h2>
      <button class="close" onclick={close} aria-label="Close">×</button>
    </header>

    <div class="body">
      <label class="full" title="Text to render. Newlines split into multiple lines (MTEXT).">
        <span>Text</span>
        <textarea bind:value={text} rows="2"></textarea>
      </label>

      <fieldset class="full">
        <legend>Font</legend>
        <label
          class="row"
          title="Use a font bundled with wiaconstructor. Bundled fonts are filled-outline (good for V-carve / pocket / drag-knife — not single-line engraving)."
        >
          <input type="radio" bind:group={useUserFont} value={false} />
          <div class="font-dd" class:open={fontDropdownOpen}>
            <button
              type="button"
              class="font-dd-button"
              disabled={useUserFont}
              aria-haspopup="listbox"
              aria-expanded={fontDropdownOpen}
              onclick={() => (fontDropdownOpen = !fontDropdownOpen)}
            >
              <span
                class="font-dd-sample"
                style:font-family={selectedBundledFont && fontsLoaded
                  ? `'${selectedBundledFont.family}', system-ui, sans-serif`
                  : 'system-ui, sans-serif'}
              >
                {FONT_SAMPLE}
              </span>
              <span class="font-dd-label">{selectedBundledFont?.label ?? '—'}</span>
              <span class="font-dd-caret">▾</span>
            </button>
            {#if fontDropdownOpen && !useUserFont}
              <ul class="font-dd-list" role="listbox">
                {#each BUNDLED_FONTS as f (f.path)}
                  <!-- svelte-ignore a11y_click_events_have_key_events -->
                  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
                  <li
                    role="option"
                    aria-selected={f.path === bundledFontPath}
                    class:active={f.path === bundledFontPath}
                    onclick={() => pickBundledFont(f.path)}
                  >
                    <span
                      class="font-dd-sample"
                      style:font-family={fontsLoaded
                        ? `'${f.family}', system-ui, sans-serif`
                        : 'system-ui, sans-serif'}
                    >
                      {FONT_SAMPLE}
                    </span>
                    <span class="font-dd-label">{f.label}</span>
                  </li>
                {/each}
              </ul>
            {/if}
          </div>
        </label>
        <label
          class="row"
          title="Load any TTF/OTF from disk. Single-line / Hershey fonts are auto-detected and required for the Engraving style."
        >
          <input type="radio" bind:group={useUserFont} value={true} />
          <span class="picker">
            <input type="file" accept=".ttf,.otf" onchange={onUserFontPick} />
            {#if userFontFile}
              <span class="filename">{userFontFile.name}</span>
            {/if}
          </span>
        </label>
        {#if lastFontFamily}
          <p class="font-meta">
            Loaded: <strong>{lastFontFamily}</strong>{lastFontIsSingleLine ? ' (single-line)' : ''}
          </p>
        {/if}
      </fieldset>

      <label title="Cap height of the text in millimeters.">
        <span>Size</span>
        <span class="field"
          ><input type="number" bind:value={sizeMm} step="0.5" min="0.1" /><span class="unit"
            >mm</span
          ></span
        >
      </label>
      <label title="X-position of the text origin (left baseline) in stock coordinates.">
        <span>Position X</span>
        <span class="field"
          ><input type="number" bind:value={posX} step="1" /><span class="unit">mm</span></span
        >
      </label>
      <label title="Y-position of the text origin (left baseline) in stock coordinates.">
        <span>Position Y</span>
        <span class="field"
          ><input type="number" bind:value={posY} step="1" /><span class="unit">mm</span></span
        >
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
          <span
            >This font is filled-outline. Engraving style needs a single-line / Hershey font.</span
          >
          <button class="chip-btn" onclick={switchToEngravingFont}>Switch font</button>
        </div>
      {/if}

      {#if STYLE_TABLE[style].toolKind != null}
        <label
          title="Tool to use for this text op. The list is filtered to tools matching the style's required kind."
        >
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
        <label
          title="Final Z depth for the text op. Negative values cut into the stock — e.g. -0.5 mm for a typical engraving."
        >
          <span>Depth</span>
          <span class="field"
            ><input type="number" bind:value={depth} step="0.1" /><span class="unit">mm</span></span
          >
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
  </Modal>
{/if}

<style>
  :global(.addtext-modal) {
    width: min(540px, 95vw);
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
  .field {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    min-width: 0;
  }
  .field input[type='number'] {
    flex: 1;
    min-width: 0;
    width: 100%;
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
  /* 6y3m: custom font dropdown with preview glyphs. <select> can't
     render different fonts per option, so we paint our own popover.
     Each row shows a sample of the font's actual glyphs next to its
     label so picking is visual rather than guess-from-name. */
  .font-dd {
    position: relative;
    min-width: 0;
  }
  .font-dd-button {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    align-items: center;
    gap: 0.4rem;
    width: 100%;
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.4rem;
    font-size: 0.78rem;
    text-align: left;
    cursor: pointer;
  }
  .font-dd-button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .font-dd-sample {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.95rem;
    color: var(--text-strong);
  }
  .font-dd-label {
    color: var(--text-muted);
    font-size: 0.7rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .font-dd-caret {
    color: var(--text-muted);
    font-size: 0.7rem;
  }
  .font-dd-list {
    position: absolute;
    top: 100%;
    left: 0;
    right: 0;
    margin: 4px 0 0;
    padding: 0.15rem;
    list-style: none;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.35);
    z-index: var(--z-dropdown);
    max-height: 14rem;
    overflow-y: auto;
  }
  .font-dd-list li {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: 0.4rem;
    padding: 0.35rem 0.5rem;
    border-radius: 3px;
    cursor: pointer;
    color: var(--text);
  }
  .font-dd-list li:hover {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  .font-dd-list li.active {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
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
