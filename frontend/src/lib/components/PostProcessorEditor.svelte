<script lang="ts">
  /// Dedicated post-processor editor (uzz). Splits the inline editor
  /// out of MachineDialog into its own modal with a live preview
  /// pane and JSON import/export. Matches Estlcam's editor in spirit:
  /// flat form on the left, ~12-line preview on the right that
  /// re-renders on every keystroke.
  ///
  /// The dialog edits a local `draft` clone; only `Save` commits back
  /// to the parent via the `onSave` callback. The parent (MachineDialog)
  /// receives a fully formed `PostProfile | undefined`.
  import type { PostProfile, AxesConfig, AxisFormat } from '../state/project.svelte';
  import { previewGcode, AXIS_DEFAULTS } from '../cam/post-profile-preview';
  import Modal from './Modal.svelte';

  interface Props {
    open: boolean;
    /// Initial profile (already cloned by the caller). The editor
    /// never mutates this — it operates on a local copy.
    initial: PostProfile;
    /// Called with the edited profile when the user clicks Save.
    /// `undefined` means "remove the override entirely" (None).
    onSave: (next: PostProfile | undefined) => void;
    onClose: () => void;
  }
  let { open, initial, onSave, onClose }: Props = $props();

  // `draft` mirrors the parent's `initial` prop but lets us edit
  // without committing until Save. We avoid reading `initial` at
  // module init (svelte's state_referenced_locally rule) — the effect
  // below handles both the first open and subsequent re-opens.
  let draft = $state<PostProfile>({});
  let importErr = $state<string | null>(null);

  $effect(() => {
    if (open) {
      draft = structuredClone(initial);
      importErr = null;
    }
  });

  let preview = $derived(previewGcode(draft));

  function defaultAxesConfig(): AxesConfig {
    const coord = (name: string): AxisFormat => ({
      enabled: true,
      name,
      format: '%.3f',
      scale: 1.0,
    });
    const int = (name: string): AxisFormat => ({
      enabled: true,
      name,
      format: '%d',
      scale: 1.0,
    });
    return {
      x: coord('X'),
      y: coord('Y'),
      z: coord('Z'),
      i: coord('I'),
      j: coord('J'),
      feed: int('F'),
      speed: int('S'),
    };
  }

  function patchAxis(key: keyof AxesConfig, patch: Partial<AxisFormat>) {
    if (!draft.axes) return;
    const cur = draft.axes[key];
    draft = {
      ...draft,
      axes: {
        ...draft.axes,
        [key]: { ...cur, ...patch },
      },
    };
  }

  function axisSummary(af: AxisFormat, defaultName: string, defaultFormat: string): string {
    if (!af.enabled) return 'disabled';
    const tweaks: string[] = [];
    if (af.name !== defaultName) tweaks.push(`→${af.name}`);
    if (af.format !== defaultFormat) tweaks.push(af.format);
    if (af.scale !== 1.0) tweaks.push(`×${af.scale}`);
    return tweaks.join(' ');
  }

  function exportJson() {
    const json = JSON.stringify(draft, null, 2);
    const blob = new Blob([json], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    const safeName = (draft.name || 'post-profile').replace(/[^a-zA-Z0-9._-]+/g, '-');
    a.download = `${safeName}.json`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
  }

  function importJson(file: File) {
    file.text().then((text) => {
      try {
        const parsed = JSON.parse(text);
        if (typeof parsed !== 'object' || parsed === null) {
          throw new Error('not an object');
        }
        // Trust but verify: only keep known fields, drop anything else.
        const next: PostProfile = {};
        if (typeof parsed.name === 'string') next.name = parsed.name;
        if (typeof parsed.file_extension === 'string') next.file_extension = parsed.file_extension;
        if (typeof parsed.line_ending === 'string') next.line_ending = parsed.line_ending;
        for (const k of [
          'program_start',
          'program_end',
          'tool_change',
          'coolant_flood_on',
          'coolant_flood_off',
          'coolant_mist_on',
          'coolant_mist_off',
        ] as const) {
          if (typeof parsed[k] === 'string') next[k] = parsed[k];
        }
        if (parsed.axes && typeof parsed.axes === 'object') {
          // Light validation: only accept if every required axis is present.
          const required: (keyof AxesConfig)[] = ['x', 'y', 'z', 'i', 'j', 'feed', 'speed'];
          if (required.every((k) => parsed.axes[k] && typeof parsed.axes[k] === 'object')) {
            next.axes = parsed.axes as AxesConfig;
          }
        }
        draft = next;
        importErr = null;
      } catch (e) {
        importErr = `Could not parse JSON: ${(e as Error).message}`;
      }
    });
  }

  function onImportChange(e: Event) {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    if (file) importJson(file);
    input.value = '';
  }

  function save() {
    // Empty draft → undefined (remove the profile entirely).
    const hasContent = !!(
      draft.name ||
      draft.file_extension ||
      draft.line_ending ||
      draft.program_start ||
      draft.program_end ||
      draft.tool_change ||
      draft.coolant_flood_on ||
      draft.coolant_flood_off ||
      draft.coolant_mist_on ||
      draft.coolant_mist_off ||
      draft.axes
    );
    onSave(hasContent ? draft : undefined);
  }
</script>

{#if open}
  <Modal onClose={onClose} modalClass="pp-editor-modal">
    <header>
      <h2>Post-processor editor</h2>
      <button class="close" onclick={onClose} aria-label="Close">×</button>
    </header>

    <div class="pp-grid">
      <div class="pp-form">
        <label class="full">
          Profile name
          <input
            type="text"
            placeholder="My controller"
            value={draft.name ?? ''}
            oninput={(e) => (draft = { ...draft, name: (e.target as HTMLInputElement).value || undefined })}
          />
        </label>

        <details open>
          <summary>File output</summary>
          <label>
            Extension
            <input
              type="text"
              placeholder="nc"
              size="6"
              value={draft.file_extension ?? ''}
              oninput={(e) => (draft = { ...draft, file_extension: (e.target as HTMLInputElement).value || undefined })}
            />
          </label>
          <label>
            Line ending
            <select
              value={draft.line_ending ?? '\n'}
              onchange={(e) => (draft = { ...draft, line_ending: (e.target as HTMLSelectElement).value || undefined })}
            >
              <option value="\n">LF (\n, Linux / Mac / GRBL)</option>
              <option value="\r\n">CRLF (\r\n, Windows / FANUC)</option>
            </select>
          </label>
        </details>

        <details open>
          <summary>Templates</summary>
          <p class="hint">
            Token markers: <code>&lt;version&gt;</code>, <code>&lt;unit&gt;</code>,
            <code>&lt;t&gt;</code> (tool#), <code>&lt;n&gt;</code> (tool name),
            <code>&lt;d&gt;</code> (diameter), <code>&lt;f&gt;</code> (feed),
            <code>&lt;s&gt;</code> (spindle), <code>&lt;op&gt;</code>,
            <code>&lt;tools&gt;</code>, <code>&lt;project&gt;</code>,
            <code>&lt;nl&gt;</code> (newline). Case-insensitive.
          </p>
          <label class="full">
            Program start
            <textarea
              rows="3"
              placeholder="(generated by wiaConstructor)"
              value={draft.program_start ?? ''}
              oninput={(e) => (draft = { ...draft, program_start: (e.target as HTMLTextAreaElement).value || undefined })}
            ></textarea>
          </label>
          <label class="full">
            Program end
            <textarea
              rows="2"
              placeholder="M30"
              value={draft.program_end ?? ''}
              oninput={(e) => (draft = { ...draft, program_end: (e.target as HTMLTextAreaElement).value || undefined })}
            ></textarea>
          </label>
          <label class="full">
            Tool change
            <textarea
              rows="2"
              placeholder="T<t> M6"
              value={draft.tool_change ?? ''}
              oninput={(e) => (draft = { ...draft, tool_change: (e.target as HTMLTextAreaElement).value || undefined })}
            ></textarea>
          </label>
          <label class="full">
            Coolant flood on
            <input
              type="text"
              placeholder="M8"
              value={draft.coolant_flood_on ?? ''}
              oninput={(e) => (draft = { ...draft, coolant_flood_on: (e.target as HTMLInputElement).value || undefined })}
            />
          </label>
          <label class="full">
            Coolant flood off / mist off
            <input
              type="text"
              placeholder="M9"
              value={draft.coolant_flood_off ?? ''}
              oninput={(e) => (draft = { ...draft, coolant_flood_off: (e.target as HTMLInputElement).value || undefined })}
            />
          </label>
          <label class="full">
            Coolant mist on
            <input
              type="text"
              placeholder="M7"
              value={draft.coolant_mist_on ?? ''}
              oninput={(e) => (draft = { ...draft, coolant_mist_on: (e.target as HTMLInputElement).value || undefined })}
            />
          </label>
        </details>

        <details open={!!draft.axes}>
          <summary>Per-axis output</summary>
          <p class="hint">
            Rename, reformat, scale, or disable individual axis words.
            Common uses: <code>scale = -1</code> on Z to flip Z-up, or
            <code>enabled = off</code> on Z for a laser.
          </p>
          <label class="axes-toggle">
            <input
              type="checkbox"
              checked={!!draft.axes}
              onchange={(e) => {
                const on = (e.target as HTMLInputElement).checked;
                draft = { ...draft, axes: on ? defaultAxesConfig() : undefined };
              }}
            />
            Override per-axis output
          </label>
          {#if draft.axes}
            {@const axes = draft.axes}
            <div class="axes-table">
              <div class="axes-row axes-head">
                <span>Axis</span>
                <span>On</span>
                <span>Name</span>
                <span>Format</span>
                <span>Scale</span>
              </div>
              {#each [
                { key: 'x' as const, label: 'X' },
                { key: 'y' as const, label: 'Y' },
                { key: 'z' as const, label: 'Z' },
                { key: 'i' as const, label: 'I (arc)' },
                { key: 'j' as const, label: 'J (arc)' },
                { key: 'feed' as const, label: 'Feed' },
                { key: 'speed' as const, label: 'Spindle' },
              ] as row}
                {@const af = axes[row.key]}
                {@const def = AXIS_DEFAULTS[row.key]}
                <div class="axes-row" class:dimmed={!af.enabled}>
                  <span class="axes-label">
                    {row.label}
                    {#if axisSummary(af, def.letter, def.format)}
                      <em>{axisSummary(af, def.letter, def.format)}</em>
                    {/if}
                  </span>
                  <span>
                    <input
                      type="checkbox"
                      checked={af.enabled}
                      onchange={(e) => patchAxis(row.key, { enabled: (e.target as HTMLInputElement).checked })}
                      aria-label="Enable {row.label}"
                    />
                  </span>
                  <span>
                    <input
                      type="text"
                      value={af.name}
                      size="3"
                      oninput={(e) => patchAxis(row.key, { name: (e.target as HTMLInputElement).value })}
                      aria-label="{row.label} name"
                    />
                  </span>
                  <span>
                    <input
                      type="text"
                      value={af.format}
                      size="6"
                      oninput={(e) => patchAxis(row.key, { format: (e.target as HTMLInputElement).value })}
                      aria-label="{row.label} format"
                    />
                  </span>
                  <span>
                    <input
                      type="number"
                      step="0.001"
                      value={af.scale}
                      oninput={(e) => {
                        const v = (e.target as HTMLInputElement).valueAsNumber;
                        if (Number.isFinite(v)) patchAxis(row.key, { scale: v });
                      }}
                      aria-label="{row.label} scale"
                    />
                  </span>
                </div>
              {/each}
            </div>
          {/if}
        </details>

        {#if importErr}
          <p class="import-err">{importErr}</p>
        {/if}
      </div>

      <div class="pp-preview">
        <div class="preview-head">
          Live preview
          <span class="preview-sub">representative program · re-renders on edit</span>
        </div>
        <pre class="preview-pane">{preview}</pre>
      </div>
    </div>

    <footer>
      <label class="secondary import-btn">
        Import JSON…
        <input type="file" accept="application/json" onchange={onImportChange} hidden />
      </label>
      <button class="secondary" onclick={exportJson}>Export JSON</button>
      <span style="flex:1"></span>
      <button class="secondary" onclick={onClose}>Cancel</button>
      <button class="primary" onclick={save}>Save</button>
    </footer>
  </Modal>
{/if}

<style>
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.7rem;
    border-bottom: 1px solid var(--border);
  }
  header h2 {
    margin: 0;
    font-size: 1rem;
  }
  .close {
    border: 0;
    background: transparent;
    color: var(--text);
    font-size: 1.4rem;
    cursor: pointer;
    line-height: 1;
  }
  .pp-grid {
    display: grid;
    grid-template-columns: minmax(420px, 1fr) 420px;
    gap: 1rem;
    padding: 0.8rem;
    max-height: 70vh;
    overflow: hidden;
  }
  .pp-form {
    overflow-y: auto;
    padding-right: 0.4rem;
    display: grid;
    gap: 0.5rem;
  }
  .pp-form details {
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.4rem 0.6rem;
    background: var(--bg-elevated);
  }
  .pp-form details summary {
    font-weight: 600;
    cursor: pointer;
  }
  .pp-form label {
    display: grid;
    grid-template-columns: 9rem 1fr;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.85rem;
    margin: 0.3rem 0;
  }
  .pp-form label.full {
    grid-template-columns: 1fr;
  }
  .pp-form label.full input,
  .pp-form label.full textarea {
    width: 100%;
  }
  .pp-form textarea {
    font-family: ui-monospace, monospace;
    font-size: 0.82rem;
    padding: 0.3rem;
    resize: vertical;
  }
  .pp-form input[type='text'],
  .pp-form input[type='number'],
  .pp-form select {
    padding: 0.25rem 0.4rem;
    font-size: 0.85rem;
  }
  .hint {
    margin: 0.3rem 0 0.5rem;
    font-size: 0.78rem;
    color: var(--text-muted);
    line-height: 1.4;
  }
  .hint code {
    background: var(--bg);
    padding: 0 0.2rem;
    border-radius: 2px;
    font-size: 0.78rem;
  }
  .axes-toggle {
    display: flex !important;
    grid-template-columns: none !important;
    align-items: center;
    gap: 0.4rem;
    font-weight: 500;
  }
  .axes-table {
    display: grid;
    gap: 0.15rem;
    margin-top: 0.4rem;
  }
  .axes-row {
    display: grid;
    grid-template-columns: 6rem 2rem 4.5rem 5rem 5rem;
    align-items: center;
    gap: 0.3rem;
    padding: 0.15rem 0.2rem;
    border-radius: 2px;
    font-size: 0.82rem;
  }
  .axes-row:hover:not(.axes-head) {
    background: var(--bg);
  }
  .axes-row.dimmed {
    opacity: 0.55;
  }
  .axes-head {
    font-size: 0.72rem;
    text-transform: uppercase;
    color: var(--text-muted);
    letter-spacing: 0.04em;
  }
  .axes-label {
    display: flex;
    flex-direction: column;
    line-height: 1.1;
  }
  .axes-label em {
    font-size: 0.7rem;
    color: var(--accent);
    font-style: normal;
  }
  .axes-row input[type='text'],
  .axes-row input[type='number'] {
    width: 100%;
    padding: 0.2rem 0.3rem;
    font-size: 0.82rem;
    font-family: ui-monospace, monospace;
  }
  .pp-preview {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
  }
  .preview-head {
    padding: 0.4rem 0.6rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
    font-weight: 600;
    font-size: 0.85rem;
    display: flex;
    flex-direction: column;
  }
  .preview-sub {
    font-weight: 400;
    color: var(--text-muted);
    font-size: 0.72rem;
  }
  .preview-pane {
    flex: 1;
    margin: 0;
    padding: 0.6rem;
    font-family: ui-monospace, monospace;
    font-size: 0.85rem;
    line-height: 1.45;
    overflow: auto;
    white-space: pre;
  }
  .import-err {
    color: #b04646;
    font-size: 0.78rem;
    margin: 0.3rem 0;
  }
  footer {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.5rem 0.7rem;
    border-top: 1px solid var(--border);
    background: var(--bg-elevated);
  }
  .import-btn {
    cursor: pointer;
    display: inline-flex;
    align-items: center;
  }
  .primary {
    background: var(--accent);
    color: white;
    border: 0;
    padding: 0.3rem 0.8rem;
    border-radius: 3px;
    cursor: pointer;
  }
  .secondary {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 0.3rem 0.8rem;
    border-radius: 3px;
    cursor: pointer;
  }
  :global(.pp-editor-modal) {
    width: min(1000px, 94vw);
  }
</style>
