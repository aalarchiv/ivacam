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
  import {
    defaultAxesConfig,
    type AxesConfig,
    type AxisFormat,
    type PostProfile,
  } from '../state/project.svelte';
  import { previewGcode, AXIS_DEFAULTS } from '../cam/post-profile-preview';
  import { project } from '../state/project.svelte';
  import { DialogDraft } from './dialog-draft.svelte';
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

  // The draft/dirty/discard lifecycle lives in DialogDraft (kdfh): it
  // mirrors the parent's `initial` prop at open() but lets us edit
  // without committing until Save. We avoid reading `initial` at
  // module init (svelte's state_referenced_locally rule) — the effect
  // below handles both the first open and subsequent re-opens.
  const dd = new DialogDraft<PostProfile>();
  const draft = $derived(dd.draft ?? {});
  let importErr = $state<string | null>(null);

  $effect(() => {
    if (open) {
      dd.open(initial);
      importErr = null;
    }
  });

  /// Replace the draft with `cur ⊕ patch`, dropping keys patched to
  /// `undefined`. The dirty check is structural (deepEqual counts keys),
  /// so leaving an explicit `name: undefined` behind would flag a
  /// type-then-erase edit as dirty even though nothing changed.
  function patchProfile(patch: Partial<PostProfile>) {
    const next: PostProfile = { ...draft, ...patch };
    for (const k of Object.keys(next) as (keyof PostProfile)[]) {
      if (next[k] === undefined) delete next[k];
    }
    dd.draft = next;
  }

  // Feed the real project tool library into the preview so the
  // `<tools>` token renders the actual multi-line listing the
  // generated gcode will carry (was: a single placeholder line).
  let toolsListing = $derived(
    project.tools.map((t) => `T${t.id} (${t.name}) ⌀${t.diameter.toFixed(3)}`).join('\n'),
  );
  let preview = $derived(previewGcode(draft, toolsListing ? { toolsListing } : {}));

  function patchAxis(key: keyof AxesConfig, patch: Partial<AxisFormat>) {
    if (!draft.axes) return;
    const cur = draft.axes[key];
    patchProfile({
      axes: {
        ...draft.axes,
        [key]: { ...cur, ...patch },
      },
    });
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
    // No confirm-before-overwrite: the recovery path already exists.
    // If the user imports the wrong file, the editor's Cancel/X
    // discards the draft without calling `onSave`, so the parent
    // (MachineDialog) still has the unedited `postProfile`. The prior
    // `window.confirm` was both over-cautious and unreliable —
    // WebKitGTK silently returns false in our Tauri build (audit-C10).
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
          // Full validation: every axis must be a complete AxisFormat
          // object with correctly-typed fields. Looser checks were
          // accepting malformed JSON that crashed formatAxisValue at
          // render time.
          const required: (keyof AxesConfig)[] = ['x', 'y', 'z', 'i', 'j', 'feed', 'speed'];
          const isAxisFormat = (a: unknown): boolean => {
            if (!a || typeof a !== 'object') return false;
            const rec = a as Record<string, unknown>;
            return (
              typeof rec.enabled === 'boolean' &&
              typeof rec.name === 'string' &&
              typeof rec.format === 'string' &&
              typeof rec.scale === 'number' &&
              Number.isFinite(rec.scale)
            );
          };
          if (required.every((k) => isAxisFormat(parsed.axes[k]))) {
            next.axes = parsed.axes as AxesConfig;
          } else {
            importErr =
              'JSON has an axes section but one or more axes are missing required fields (enabled / name / format / scale). Skipped axes.';
          }
        }
        dd.draft = next;
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

  /// Discard guard: a draft that diverged from `initial` prompts before
  /// closing (audit hzlr flagged the earlier unguarded close as
  /// inconsistent with MachineDialog / ToolLibraryDialog). DialogDraft
  /// owns the dirty check + two-step confirm.
  function close() {
    if (dd.requestClose()) onClose();
  }
</script>

{#if open}
  <Modal onClose={close} width="min(1000px, 94vw)" ariaLabelledBy="post-editor-title">
    <header>
      <h2 id="post-editor-title">Post-processor editor</h2>
      <button class="dlg-close" onclick={close} aria-label="Close">×</button>
    </header>

    <div class="pp-grid">
      <div class="pp-form">
        <label class="full">
          Profile name
          <input
            type="text"
            placeholder="My controller"
            value={draft.name ?? ''}
            oninput={(e) =>
              patchProfile({ name: (e.target as HTMLInputElement).value || undefined })}
          />
        </label>

        <details class="full token-legend">
          <summary>Template tokens (click to expand)</summary>
          <p class="hint">
            Tokens are case-insensitive <code>&lt;name&gt;</code> placeholders. Type them verbatim into
            any template field (header / footer / tool-change). Unknown tokens pass through unchanged.
          </p>
          <table class="legend-grid">
            <tbody>
              <tr><td><code>&lt;version&gt;</code></td><td>ivac version string</td></tr>
              <tr><td><code>&lt;unit&gt;</code></td><td><code>mm</code> or <code>in</code></td></tr>
              <tr><td><code>&lt;t&gt;</code></td><td>tool number</td></tr>
              <tr><td><code>&lt;n&gt;</code></td><td>tool name</td></tr>
              <tr><td><code>&lt;d&gt;</code></td><td>tool diameter</td></tr>
              <tr><td><code>&lt;f&gt;</code></td><td>feed (mm/min)</td></tr>
              <tr><td><code>&lt;s&gt;</code></td><td>spindle (RPM)</td></tr>
              <tr><td><code>&lt;op&gt;</code></td><td>current operation name</td></tr>
              <tr
                ><td><code>&lt;tools&gt;</code></td><td>full tool-library listing (one per line)</td
                ></tr
              >
              <tr
                ><td><code>&lt;project&gt;</code></td><td>project name (save-file basename)</td></tr
              >
              <tr><td><code>&lt;nl&gt;</code></td><td>explicit newline</td></tr>
            </tbody>
          </table>
        </details>

        <details open>
          <summary>File output</summary>
          <label>
            Extension
            <input
              type="text"
              placeholder="nc"
              size="6"
              value={draft.file_extension ?? ''}
              oninput={(e) =>
                patchProfile({
                  file_extension: (e.target as HTMLInputElement).value || undefined,
                })}
            />
          </label>
          <label>
            Line ending
            <select
              value={draft.line_ending ?? '\n'}
              onchange={(e) =>
                patchProfile({
                  line_ending: (e.target as HTMLSelectElement).value || undefined,
                })}
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
              placeholder="(generated by ivaCAM)"
              value={draft.program_start ?? ''}
              oninput={(e) =>
                patchProfile({
                  program_start: (e.target as HTMLTextAreaElement).value || undefined,
                })}
            ></textarea>
          </label>
          <label class="full">
            Program end
            <textarea
              rows="2"
              placeholder="M30"
              value={draft.program_end ?? ''}
              oninput={(e) =>
                patchProfile({
                  program_end: (e.target as HTMLTextAreaElement).value || undefined,
                })}
            ></textarea>
          </label>
          <label class="full">
            Tool change
            <textarea
              rows="2"
              placeholder="T<t> M6"
              value={draft.tool_change ?? ''}
              oninput={(e) =>
                patchProfile({
                  tool_change: (e.target as HTMLTextAreaElement).value || undefined,
                })}
            ></textarea>
          </label>
          <label class="full">
            Coolant flood on
            <input
              type="text"
              placeholder="M8"
              value={draft.coolant_flood_on ?? ''}
              oninput={(e) =>
                patchProfile({
                  coolant_flood_on: (e.target as HTMLInputElement).value || undefined,
                })}
            />
          </label>
          <label class="full">
            Coolant flood off / mist off
            <input
              type="text"
              placeholder="M9"
              value={draft.coolant_flood_off ?? ''}
              oninput={(e) =>
                patchProfile({
                  coolant_flood_off: (e.target as HTMLInputElement).value || undefined,
                })}
            />
          </label>
          <label class="full">
            Coolant mist on
            <input
              type="text"
              placeholder="M7"
              value={draft.coolant_mist_on ?? ''}
              oninput={(e) =>
                patchProfile({
                  coolant_mist_on: (e.target as HTMLInputElement).value || undefined,
                })}
            />
          </label>
        </details>

        <details open={!!draft.axes}>
          <summary>Per-axis output</summary>
          <p class="hint">
            Rename, reformat, scale, or disable individual axis words. Common uses: <code
              >scale = -1</code
            >
            on Z to flip Z-up, or
            <code>enabled = off</code> on Z for a laser.
          </p>
          <label class="axes-toggle">
            <input
              type="checkbox"
              checked={!!draft.axes}
              onchange={(e) => {
                const on = (e.target as HTMLInputElement).checked;
                patchProfile({ axes: on ? defaultAxesConfig() : undefined });
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
              {#each [{ key: 'x' as const, label: 'X' }, { key: 'y' as const, label: 'Y' }, { key: 'z' as const, label: 'Z' }, { key: 'i' as const, label: 'I (arc)' }, { key: 'j' as const, label: 'J (arc)' }, { key: 'feed' as const, label: 'Feed' }, { key: 'speed' as const, label: 'Spindle' }] as row (row.key)}
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
                      onchange={(e) =>
                        patchAxis(row.key, { enabled: (e.target as HTMLInputElement).checked })}
                      aria-label="Enable {row.label}"
                    />
                  </span>
                  <span>
                    <input
                      type="text"
                      value={af.name}
                      size="3"
                      oninput={(e) =>
                        patchAxis(row.key, { name: (e.target as HTMLInputElement).value })}
                      aria-label="{row.label} name"
                    />
                  </span>
                  <span>
                    <input
                      type="text"
                      value={af.format}
                      size="6"
                      oninput={(e) =>
                        patchAxis(row.key, { format: (e.target as HTMLInputElement).value })}
                      aria-label="{row.label} format"
                    />
                  </span>
                  <span>
                    <input
                      type="number"
                      step="0.001"
                      value={af.scale}
                      title="Linear multiplier applied to the axis value before formatting. -1 flips Z-up vs Z-down; 0.0393701 converts mm to inch. 0 is rejected — it would emit a constant axis word."
                      oninput={(e) => {
                        const v = (e.target as HTMLInputElement).valueAsNumber;
                        // Reject zero — `scale: 0` produces a constant
                        // axis word (e.g. always "X0") and silently
                        // breaks every emitted line. Negative is allowed
                        // (flip-Z) so we explicitly check `!== 0`.
                        if (Number.isFinite(v) && v !== 0) patchAxis(row.key, { scale: v });
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
          <span class="preview-sub"
            >synthetic 2-pass toolpath: header → tool change → move + plunge + arc + retract →
            footer · re-renders on every edit</span
          >
        </div>
        <pre class="preview-pane">{preview}</pre>
      </div>
    </div>

    <footer>
      {#if dd.confirmingDiscard}
        <span class="discard-prompt">Discard unsaved profile edits?</span>
        <button class="btn-secondary" onclick={() => dd.cancelDiscard()}>Keep editing</button>
        <button class="btn-danger" onclick={close}>Discard</button>
      {:else}
        <label class="btn-secondary import-btn">
          Import JSON…
          <input type="file" accept="application/json" onchange={onImportChange} hidden />
        </label>
        <button class="btn-secondary" onclick={exportJson}>Export JSON</button>
        <span class="spacer"></span>
        <button class="btn-secondary" onclick={close}>Cancel</button>
        <button class="btn-primary" onclick={save}>Save</button>
      {/if}
    </footer>
  </Modal>
{/if}

<style>
  /* Footer spacer (was `style="flex:1"` inline). Promoted to a class so
     CSP-strict deployments don't break. */
  .spacer {
    flex: 1;
  }
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
  .pp-form .token-legend .hint {
    font-size: 0.78rem;
    color: var(--text-muted);
    margin: 0.4rem 0;
  }
  .pp-form .token-legend code {
    font-family: ui-monospace, monospace;
    font-size: 0.8em;
    background: var(--bg-input);
    padding: 0 0.2rem;
    border-radius: 2px;
  }
  .pp-form .legend-grid {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.8rem;
  }
  .pp-form .legend-grid td {
    padding: 0.15rem 0.4rem;
    vertical-align: top;
  }
  .pp-form .legend-grid td:first-child {
    width: 7rem;
    white-space: nowrap;
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
    color: var(--error);
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
</style>
