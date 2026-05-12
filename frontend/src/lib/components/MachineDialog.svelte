<script lang="ts">
  /// Machine settings dialog. Project-scoped CNC config — units, mode,
  /// fast move height, comments / arcs / toolchange flags. The op-driven
  /// pipeline reads these from the Project; until that wires up the
  /// values are also mirrored into setup.machine via SetupPanel so the
  /// legacy Generate path keeps working.
  import {
    project,
    defaultAxesConfig,
    type AxesConfig,
    type AxisFormat,
    type AxisLimits,
    type MachineSettings,
    type PostProfile,
  } from '../state/project.svelte';
  import Modal from './Modal.svelte';

  interface Props {
    open: boolean;
    onClose: () => void;
  }
  let { open, onClose }: Props = $props();

  // Local working copy so the user can cancel without committing.
  let draft = $state<MachineSettings>(cloneSettings(project.machine));

  // Jerk fields — toggled by a single checkbox, default off (trapezoidal
  // profile only; S-curve refinement is Phase 2).
  let jerkEnabled = $state(!!project.machine.jerk);
  let jerkDraft = $state<AxisLimits>(project.machine.jerk ?? { x: 100, y: 100, z: 50 });

  $effect(() => {
    if (open) {
      draft = cloneSettings(project.machine);
      jerkEnabled = !!project.machine.jerk;
      jerkDraft = project.machine.jerk
        ? { ...project.machine.jerk }
        : { x: 100, y: 100, z: 50 };
    }
  });

  function cloneSettings(m: MachineSettings): MachineSettings {
    return {
      ...m,
      accel: m.accel ? { ...m.accel } : { x: 250, y: 250, z: 250 },
      jerk: m.jerk ? { ...m.jerk } : undefined,
      postProfile: m.postProfile ? { ...m.postProfile } : undefined,
    };
  }

  /// Map the current `postProfile` to one of our preset keys so the
  /// preset dropdown reflects what the user picked. Matched by NAME,
  /// not exact equality — the user might tweak templates on top of
  /// 'Mach3 metric' and we still want the dropdown to show Mach3 (so
  /// the next tweak doesn't snap it back to the canonical preset).
  function profilePreset(p: PostProfile | undefined): string {
    if (!p) return 'none';
    if (p.name === 'LinuxCNC default') return 'linuxcnc';
    if (p.name === 'GRBL default') return 'grbl';
    if (p.name === 'Mach3 metric') return 'mach3';
    return 'custom';
  }

  /// One-line summary of a per-axis config — shown next to its row
  /// when collapsed, so users can scan the seven axes without
  /// expanding each. Empty when the axis is at its baseline (enabled,
  /// natural name, %.3f / %d, scale 1.0).
  function axisSummary(af: AxisFormat, defaultName: string, defaultFormat: string): string {
    const tweaks: string[] = [];
    if (!af.enabled) return 'disabled';
    if (af.name !== defaultName) tweaks.push(`→${af.name}`);
    if (af.format !== defaultFormat) tweaks.push(af.format);
    if (af.scale !== 1.0) tweaks.push(`×${af.scale}`);
    return tweaks.join(' ');
  }

  function patchAxis(key: keyof AxesConfig, patch: Partial<AxisFormat>) {
    if (!draft.postProfile || !draft.postProfile.axes) return;
    const cur = draft.postProfile.axes[key];
    draft.postProfile = {
      ...draft.postProfile,
      axes: {
        ...draft.postProfile.axes,
        [key]: { ...cur, ...patch },
      },
    };
  }

  function commit() {
    const out: MachineSettings = { ...draft };
    out.jerk = jerkEnabled ? { ...jerkDraft } : undefined;
    project.setMachine(out);
    onClose();
  }

  function close() {
    onClose();
  }
</script>

{#if open}
  <Modal onClose={close} modalClass="machine-modal">
    <header>
      <h2 id="machine-title">Machine</h2>
      <button class="close" onclick={close} aria-label="Close">×</button>
    </header>

      <div class="grid">
        <label>Unit
          <select bind:value={draft.unit}>
            <option value="mm">mm</option>
            <option value="inch">inch</option>
          </select>
        </label>
        <label>Mode
          <select bind:value={draft.mode}>
            <option value="mill">Mill (CNC)</option>
            <option value="laser">Laser</option>
            <option value="drag">Drag-knife / vinyl</option>
          </select>
        </label>
        <label>Fast-move Z
          <span class="field"><input type="number" bind:value={draft.fastMoveZ} step="0.1" /><span class="unit">mm</span></span>
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={draft.comments} />
          Emit comments in G-code
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={draft.arcs} />
          Emit G2 / G3 arc moves
        </label>
        <label
          class:disabled={!draft.arcs}
          title="How far the fitted arc may deviate from the original polyline. Smaller = tighter, more arcs split. Typical values 0.005-0.05 mm."
        >
          Arc fitting tolerance
          <span class="field"><input
            type="number"
            min="0"
            step="0.001"
            disabled={!draft.arcs}
            value={draft.arcFitToleranceMm ?? 0.01}
            oninput={(e) => {
              const v = (e.target as HTMLInputElement).valueAsNumber;
              draft.arcFitToleranceMm = isFinite(v) && v >= 0 ? v : undefined;
            }}
          /><span class="unit">mm</span></span>
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={draft.supportsToolchange} />
          Machine supports tool changes (M6)
        </label>
        <label class="check" title="Plot-mode Z (rt1.35): collapse every cut to a single pass at the op's cut depth and skip the multi-step descent / ramp / helix machinery. Z values in gcode are restricted to fast_move_z (pen up) and cut depth (pen down). Right setting for laser / plasma / pen plotters / 3D-printer extrusion and drag-knife controllers.">
          <input type="checkbox" bind:checked={draft.plotModeZ} />
          Plot-mode Z (single-pass, binary up/down)
        </label>

        <div class="section-title">G-code formatting</div>
        <label title="Some EU-locale Siemens / Heidenhain controllers require X1,5 instead of X1.5. Default is the period.">
          Decimal separator
          <span class="field">
            <select
              value={draft.decimalSeparator ?? '.'}
              onchange={(e) => {
                const v = (e.currentTarget as HTMLSelectElement).value;
                draft.decimalSeparator = v === ',' ? ',' : '.';
              }}
            >
              <option value=".">period (.)</option>
              <option value=",">comma (,)</option>
            </select>
          </span>
        </label>
        <label title="Prefix every emitted line with N10, N20, N30, … Required by some FANUC / vintage controllers; useful operator reference even on modern ones. Empty / 0 disables numbering.">
          Line numbering start
          <span class="field">
            <input
              type="number"
              min="0"
              step="10"
              placeholder="off"
              value={draft.lineNumberStart ?? ''}
              oninput={(e) => {
                const raw = (e.target as HTMLInputElement).value;
                if (raw === '') {
                  draft.lineNumberStart = undefined;
                  return;
                }
                const v = parseInt(raw, 10);
                draft.lineNumberStart = isFinite(v) && v > 0 ? v : undefined;
              }}
            />
            <span class="unit">N</span>
          </span>
        </label>

        <div class="section-title">Post-processor profile (rt1.15)</div>
        <label title="Pick a built-in profile or write your own templates below. Built-in profiles fill the templates with sensible defaults for that controller; you can still edit them. 'None' uses wiac's hard-coded defaults.">
          Profile preset
          <span class="field">
            <select
              value={profilePreset(draft.postProfile)}
              onchange={(e) => {
                const v = (e.currentTarget as HTMLSelectElement).value;
                if (v === 'none') {
                  draft.postProfile = undefined;
                } else if (v === 'linuxcnc') {
                  draft.postProfile = { name: 'LinuxCNC default', file_extension: 'nc', line_ending: '\n' };
                } else if (v === 'mach3') {
                  draft.postProfile = {
                    name: 'Mach3 metric',
                    file_extension: 'tap',
                    line_ending: '\r\n',
                    program_start: '%\nN10 G21 G90 (wiac <version>)',
                    program_end: 'M30\n%',
                  };
                } else if (v === 'grbl') {
                  draft.postProfile = {
                    name: 'GRBL default',
                    file_extension: 'nc',
                    line_ending: '\n',
                    program_start: '; wiac <version> — GRBL',
                    program_end: 'M2',
                    tool_change: '; toolchange to T<t> (manual on GRBL)',
                  };
                } else if (v === 'custom') {
                  draft.postProfile = draft.postProfile ?? { name: 'Custom' };
                }
              }}
            >
              <option value="none">None (built-in defaults)</option>
              <option value="linuxcnc">LinuxCNC default</option>
              <option value="grbl">GRBL default</option>
              <option value="mach3">Mach3 metric</option>
              <option value="custom">Custom</option>
            </select>
          </span>
        </label>
        {#if draft.postProfile}
          <label title="File extension on save (no leading dot). Mach3 typically uses 'tap'.">
            File extension
            <span class="field">
              <input
                type="text"
                placeholder="nc"
                value={draft.postProfile.file_extension ?? ''}
                oninput={(e) => {
                  const v = (e.target as HTMLInputElement).value;
                  if (!draft.postProfile) return;
                  draft.postProfile = { ...draft.postProfile, file_extension: v || undefined };
                }}
              />
            </span>
          </label>
          <label title="Multi-line prologue prepended to every program. Tokens: <version>, <unit>, <t>, <n>, <d>, <f>, <s>, <op>, <nl>. Leave blank for the controller's hard-coded default.">
            Program start
            <span class="field full">
              <textarea
                rows="3"
                placeholder="(generated by wiaConstructor)"
                value={draft.postProfile.program_start ?? ''}
                oninput={(e) => {
                  const v = (e.target as HTMLTextAreaElement).value;
                  if (!draft.postProfile) return;
                  draft.postProfile = { ...draft.postProfile, program_start: v || undefined };
                }}
              ></textarea>
            </span>
          </label>
          <label title="Footer appended to every program. Same token set as program_start.">
            Program end
            <span class="field full">
              <textarea
                rows="2"
                placeholder="M30"
                value={draft.postProfile.program_end ?? ''}
                oninput={(e) => {
                  const v = (e.target as HTMLTextAreaElement).value;
                  if (!draft.postProfile) return;
                  draft.postProfile = { ...draft.postProfile, program_end: v || undefined };
                }}
              ></textarea>
            </span>
          </label>
          <label title="Tool change template. Tokens see the NEW tool's number / name / diameter via <t> / <n> / <d>. Empty = default 'T<n> M6' for LinuxCNC; no toolchange on GRBL.">
            Tool change
            <span class="field full">
              <textarea
                rows="2"
                placeholder="T<t> M6"
                value={draft.postProfile.tool_change ?? ''}
                oninput={(e) => {
                  const v = (e.target as HTMLTextAreaElement).value;
                  if (!draft.postProfile) return;
                  draft.postProfile = { ...draft.postProfile, tool_change: v || undefined };
                }}
              ></textarea>
            </span>
          </label>

          <details class="axes-block" open={!!draft.postProfile?.axes}>
            <summary>Per-axis output (advanced)</summary>
            <p class="axes-note">
              Rename, reformat, scale or disable individual axis words.
              Common uses: <code>scale = -1</code> on Z to flip a Z-up
              controller, <code>enabled = off</code> on Z for a laser
              that doesn't have one, <code>name = A</code> on Z for a
              rotary-as-Z setup.
            </p>
            <label class="axes-toggle">
              <input
                type="checkbox"
                checked={!!draft.postProfile.axes}
                onchange={(e) => {
                  if (!draft.postProfile) return;
                  const on = (e.target as HTMLInputElement).checked;
                  draft.postProfile = {
                    ...draft.postProfile,
                    axes: on ? defaultAxesConfig() : undefined,
                  };
                }}
              />
              Override per-axis output
            </label>
            {#if draft.postProfile.axes}
              {@const axes = draft.postProfile.axes}
              <div class="axes-table" role="table">
                <div class="axes-row axes-head" role="row">
                  <span role="columnheader">Axis</span>
                  <span role="columnheader">On</span>
                  <span role="columnheader">Name</span>
                  <span role="columnheader">Format</span>
                  <span role="columnheader">Scale</span>
                </div>
                {#each [
                  { key: 'x' as const, label: 'X', defaultName: 'X', defaultFormat: '%.3f' },
                  { key: 'y' as const, label: 'Y', defaultName: 'Y', defaultFormat: '%.3f' },
                  { key: 'z' as const, label: 'Z', defaultName: 'Z', defaultFormat: '%.3f' },
                  { key: 'i' as const, label: 'I (arc)', defaultName: 'I', defaultFormat: '%.3f' },
                  { key: 'j' as const, label: 'J (arc)', defaultName: 'J', defaultFormat: '%.3f' },
                  { key: 'feed' as const, label: 'Feed', defaultName: 'F', defaultFormat: '%d' },
                  { key: 'speed' as const, label: 'Spindle', defaultName: 'S', defaultFormat: '%d' },
                ] as row}
                  {@const af = axes[row.key]}
                  <div class="axes-row" role="row" class:dimmed={!af.enabled}>
                    <span role="cell" class="axes-label">
                      {row.label}
                      {#if axisSummary(af, row.defaultName, row.defaultFormat)}
                        <em>{axisSummary(af, row.defaultName, row.defaultFormat)}</em>
                      {/if}
                    </span>
                    <span role="cell">
                      <input
                        type="checkbox"
                        checked={af.enabled}
                        onchange={(e) => patchAxis(row.key, { enabled: (e.target as HTMLInputElement).checked })}
                        aria-label="Enable {row.label}"
                      />
                    </span>
                    <span role="cell">
                      <input
                        type="text"
                        value={af.name}
                        placeholder={row.defaultName}
                        size="3"
                        oninput={(e) => patchAxis(row.key, { name: (e.target as HTMLInputElement).value })}
                        aria-label="{row.label} name"
                      />
                    </span>
                    <span role="cell">
                      <input
                        type="text"
                        value={af.format}
                        placeholder={row.defaultFormat}
                        size="6"
                        oninput={(e) => patchAxis(row.key, { format: (e.target as HTMLInputElement).value })}
                        aria-label="{row.label} format"
                      />
                    </span>
                    <span role="cell">
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
        {/if}

        <div class="section-title">Kinematics</div>
        <label>Rapid speed
          <span class="field"><input type="number" min="0" step="100" bind:value={draft.rapidSpeed} /><span class="unit">mm/min</span></span>
        </label>
        <label>Tool-change time
          <span class="field"><input type="number" min="0" step="0.5" bind:value={draft.toolchangeS} /><span class="unit">s</span></span>
        </label>
        <div class="triplet-label">Acceleration X / Y / Z <span class="unit">mm/s²</span></div>
        <div class="triplet">
          <input type="number" min="0" step="10"
            aria-label="Acceleration X (mm/s²)"
            value={draft.accel?.x ?? 250}
            oninput={(e) => {
              const v = (e.target as HTMLInputElement).valueAsNumber;
              draft.accel = { ...(draft.accel ?? { x: 250, y: 250, z: 250 }), x: isFinite(v) ? v : 250 };
            }} />
          <input type="number" min="0" step="10"
            aria-label="Acceleration Y (mm/s²)"
            value={draft.accel?.y ?? 250}
            oninput={(e) => {
              const v = (e.target as HTMLInputElement).valueAsNumber;
              draft.accel = { ...(draft.accel ?? { x: 250, y: 250, z: 250 }), y: isFinite(v) ? v : 250 };
            }} />
          <input type="number" min="0" step="10"
            aria-label="Acceleration Z (mm/s²)"
            value={draft.accel?.z ?? 250}
            oninput={(e) => {
              const v = (e.target as HTMLInputElement).valueAsNumber;
              draft.accel = { ...(draft.accel ?? { x: 250, y: 250, z: 250 }), z: isFinite(v) ? v : 250 };
            }} />
        </div>
        <label class="check">
          <input type="checkbox" bind:checked={jerkEnabled} />
          Enable jerk limits (S-curve, Phase 2)
        </label>
        {#if jerkEnabled}
          <div class="triplet-label">Jerk X / Y / Z <span class="unit">mm/s³</span></div>
          <div class="triplet">
            <input type="number" min="0" step="10" aria-label="Jerk X (mm/s³)" bind:value={jerkDraft.x} />
            <input type="number" min="0" step="10" aria-label="Jerk Y (mm/s³)" bind:value={jerkDraft.y} />
            <input type="number" min="0" step="10" aria-label="Jerk Z (mm/s³)" bind:value={jerkDraft.z} />
          </div>
        {/if}
      </div>

      <footer>
        <button class="secondary" onclick={close}>Cancel</button>
        <button class="primary" onclick={commit}>OK</button>
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
  .grid {
    padding: 0.7rem;
    display: grid;
    gap: 0.5rem;
  }
  label {
    display: grid;
    grid-template-columns: minmax(0, 9rem) minmax(0, 1fr);
    align-items: center;
    gap: 0.6rem;
    font-size: 0.8rem;
  }
  label.check {
    grid-template-columns: auto 1fr;
    gap: 0.4rem;
  }
  label.disabled {
    color: var(--text-muted);
    opacity: 0.6;
  }
  input[type='number'],
  select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.2rem 0.4rem;
    font-size: 0.8rem;
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
  }
  input[type='checkbox'] {
    accent-color: var(--accent);
  }
  .section-title {
    grid-column: 1 / -1;
    margin-top: 0.4rem;
    padding-top: 0.4rem;
    border-top: 1px solid var(--border);
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
  }
  .triplet-label {
    font-size: 0.8rem;
    color: var(--text);
  }
  .triplet {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 0.3rem;
  }
  .triplet input[type='number'] {
    width: 100%;
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
  .secondary {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 0.3rem 0.8rem;
    border-radius: 3px;
    cursor: pointer;
  }
  .axes-block {
    grid-column: 1 / -1;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.4rem 0.6rem;
    background: var(--bg-elevated);
    font-size: 0.85rem;
  }
  .axes-block summary {
    cursor: pointer;
    font-weight: 600;
  }
  .axes-note {
    color: var(--text-muted);
    margin: 0.4rem 0;
    font-size: 0.78rem;
    line-height: 1.4;
  }
  .axes-note code {
    background: var(--bg);
    padding: 0 0.2rem;
    border-radius: 2px;
  }
  .axes-toggle {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin: 0.3rem 0 0.5rem;
    font-weight: 500;
  }
  .axes-table {
    display: grid;
    gap: 0.15rem;
  }
  .axes-row {
    display: grid;
    grid-template-columns: 5.5rem 2rem 4.5rem 5rem 5rem;
    align-items: center;
    gap: 0.3rem;
    padding: 0.15rem 0.2rem;
    border-radius: 2px;
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
</style>
