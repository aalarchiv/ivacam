<script lang="ts">
  /// Machine settings dialog. Project-scoped CNC config — units, mode,
  /// fast move height, comments / arcs / toolchange flags. The op-driven
  /// pipeline reads these from the Project; until that wires up the
  /// values are also mirrored into setup.machine via SetupPanel so the
  /// legacy Generate path keeps working.
  import {
    project,
    type AxisLimits,
    type MachineSettings,
    type PostProfile,
  } from '../state/project.svelte';
  import Modal from './Modal.svelte';
  import PostProcessorEditor from './PostProcessorEditor.svelte';

  interface Props {
    open: boolean;
    onClose: () => void;
  }
  let { open, onClose }: Props = $props();

  // Local working copy so the user can cancel without committing.
  let draft = $state<MachineSettings>(cloneSettings(project.machine));

  // PostProcessor editor (uzz) — the heavy editing surface lives in
  // a dedicated dialog with a live preview pane and JSON I/O. We just
  // own the "is it open" flag.
  let editorOpen = $state(false);

  // Jerk fields — toggled by a single checkbox, default off (trapezoidal
  // profile only; S-curve refinement is Phase 2).
  let jerkEnabled = $state(!!project.machine.jerk);
  let jerkDraft = $state<AxisLimits>(project.machine.jerk ?? { x: 100, y: 100, z: 50 });

  // Snapshot captured at open — the dirty check stringifies draft+jerk
  // and compares to this string so X / Esc / click-outside can prompt
  // before silently discarding edits (audit-dh1n).
  let pristine = $state<string>('');

  /// Build the dirty-check fingerprint. Wrapped in try/catch because a
  /// throw inside `$derived.by` (which calls this) would propagate into
  /// the Svelte 5 reactivity scheduler and abort it — and a dead
  /// scheduler manifests exactly as "every button still fires its
  /// onclick, but visible state stops updating" (see App.svelte:117).
  /// On error, return an empty string — the comparison against
  /// `pristine` then degrades to "assume clean", which is the safer
  /// default than "assume dirty" (the dialog would refuse to close on
  /// every attempt).
  function snapshotKey(): string {
    try {
      return JSON.stringify({
        draft,
        jerk: jerkEnabled ? jerkDraft : null,
      });
    } catch (e) {
      console.error('MachineDialog.snapshotKey: serialize failed', e);
      return '';
    }
  }

  $effect(() => {
    if (open) {
      try {
        // Build snapshots from local vars BEFORE assigning the
        // $state. Calling snapshotKey() here would re-read the
        // proxies we just wrote — Svelte tracks those reads as
        // dependencies of this very effect, and the writes then
        // re-schedule it. After ~1000 self-runs Svelte throws
        // `effect_update_depth_exceeded`, which kills the reactivity
        // scheduler for the whole app — every button still fires
        // onclick but the UI never repaints. That's the bug the
        // Machine dialog manifested as "X / Cancel / OK don't close".
        const m = project.machine;
        const newDraft = cloneSettings(m);
        const newJerkEnabled = !!m.jerk;
        const newJerkDraft = m.jerk ? { ...m.jerk } : { x: 100, y: 100, z: 50 };
        const newPristine = JSON.stringify({
          draft: newDraft,
          jerk: newJerkEnabled ? newJerkDraft : null,
        });
        draft = newDraft;
        jerkEnabled = newJerkEnabled;
        jerkDraft = newJerkDraft;
        pristine = newPristine;
      } catch (e) {
        console.error('MachineDialog.open: init failed', e);
      }
    }
  });

  let isDirty = $derived.by(() => {
    try {
      return open && snapshotKey() !== pristine;
    } catch (e) {
      console.error('MachineDialog.isDirty: derive failed', e);
      return false;
    }
  });

  function cloneSettings(m: MachineSettings): MachineSettings {
    return {
      ...m,
      accel: m.accel ? { ...m.accel } : { x: 250, y: 250, z: 250 },
      jerk: m.jerk ? { ...m.jerk } : undefined,
      workArea: m.workArea ? { ...m.workArea } : { x: 200, y: 300, z: 50 },
      postProfile: m.postProfile ? { ...m.postProfile } : undefined,
    };
  }

  /// Map the current `postProfile` to one of our preset keys so the
  /// preset dropdown reflects what the user picked. Matched by NAME,
  /// not exact equality — the user might tweak templates on top of
  /// 'Mach3 metric' and we still want the dropdown to show Mach3 (so
  /// the next tweak doesn't snap it back to the canonical preset).
  /// Map the current `postProfile` to one of our preset keys. Match
  /// the NAME *and* check that the user hasn't tweaked anything on
  /// top of the preset — if they edited a template, the dropdown
  /// should snap to "custom" so users can tell they've diverged from
  /// the canonical preset (was: name-only match, which silently
  /// claimed a heavily-edited profile was still "LinuxCNC default").
  function profilePreset(p: PostProfile | undefined): string {
    if (!p) return 'none';
    const matches = (a: PostProfile, b: PostProfile): boolean =>
      a.file_extension === b.file_extension &&
      (a.line_ending ?? null) === (b.line_ending ?? null) &&
      (a.program_start ?? null) === (b.program_start ?? null) &&
      (a.program_end ?? null) === (b.program_end ?? null) &&
      (a.tool_change ?? null) === (b.tool_change ?? null) &&
      (a.coolant_flood_on ?? null) === (b.coolant_flood_on ?? null) &&
      (a.coolant_flood_off ?? null) === (b.coolant_flood_off ?? null) &&
      (a.coolant_mist_on ?? null) === (b.coolant_mist_on ?? null) &&
      (a.coolant_mist_off ?? null) === (b.coolant_mist_off ?? null) &&
      !a.axes === !b.axes;
    const presets: { key: string; profile: PostProfile }[] = [
      {
        key: 'linuxcnc',
        profile: { name: 'LinuxCNC default', file_extension: 'nc', line_ending: '\n' },
      },
      {
        key: 'mach3',
        profile: {
          name: 'Mach3 metric',
          file_extension: 'tap',
          line_ending: '\r\n',
          program_start: '%\nN10 G21 G90 (wiac <version>)',
          program_end: 'M30\n%',
        },
      },
      {
        key: 'grbl',
        profile: {
          name: 'GRBL default',
          file_extension: 'nc',
          line_ending: '\n',
          program_start: '; wiac <version> — GRBL',
          program_end: 'M2',
          tool_change: '; toolchange to T<t> (manual on GRBL)',
        },
      },
    ];
    for (const { key, profile } of presets) {
      if (p.name === profile.name && matches(p, profile)) return key;
    }
    return 'custom';
  }

  /// One-line summary of what's been customized in the current
  /// profile, shown as a "X tweaks: header, footer, axes" chip next
  /// to the Edit button so users don't have to open the editor to
  /// see whether they have any overrides.
  function profileTweakSummary(p: PostProfile | undefined): string {
    if (!p) return '';
    const parts: string[] = [];
    if (p.program_start) parts.push('header');
    if (p.program_end) parts.push('footer');
    if (p.tool_change) parts.push('toolchange');
    if (p.coolant_flood_on || p.coolant_flood_off) parts.push('coolant');
    if (p.coolant_mist_on || p.coolant_mist_off) parts.push('mist');
    if (p.axes) parts.push('axes');
    if (p.file_extension || p.line_ending) parts.push('file');
    return parts.join(', ');
  }

  function commit() {
    // JSON round-trip the $state proxy so `setMachineCommand` receives
    // a plain object — Svelte 5 proxies can trip `structuredClone` on
    // some WebKit builds, silently aborting the commit.
    //
    // Whole body in try/catch so any throw from JSON serialise or
    // setMachine still reaches `onClose()` — otherwise OK appears
    // broken. The error is logged and surfaces via the global error
    // banner so the underlying bug stays visible.
    try {
      const snap = structuredClone(draft) as MachineSettings;
      snap.jerk = jerkEnabled ? { ...jerkDraft } : undefined;
      project.setMachine(snap);
    } catch (e) {
      console.error('MachineDialog.commit: setMachine failed', e);
    }
    onClose();
  }

  /// Two-step close-on-dirty: first attempt arms `confirmingDiscard`
  /// so the footer swaps to a "Discard / Keep editing" pair; second
  /// click on Discard actually fires `onClose`. Replaces the prior
  /// `window.confirm` prompt, which silently returns false in some
  /// Tauri / WebKitGTK builds — the project's audit-C10 note already
  /// flagged `window.confirm` as unreliable for this reason.
  let confirmingDiscard = $state(false);

  function close() {
    if (isDirty) {
      confirmingDiscard = true;
      return;
    }
    onClose();
  }

  function discardAndClose() {
    confirmingDiscard = false;
    onClose();
  }

  function cancelDiscard() {
    confirmingDiscard = false;
  }
</script>

{#if open}
  <Modal onClose={close} modalClass="machine-modal">
    <header>
      <h2 id="machine-title">Machine</h2>
      <button class="close" onclick={close} aria-label="Close">×</button>
    </header>

    <div class="grid">
      <label
        title="Output units in the emitted G-code (G20 inch / G21 mm). Internal storage is always mm — this only affects the post."
      >
        Unit
        <select bind:value={draft.unit}>
          <option value="mm">mm</option>
          <option value="inch">inch</option>
        </select>
      </label>
      <label
        title="Machine kind. Mill: subtractive CNC, full Z control. Laser: M3/M5 power, ignores Z. Drag: vinyl cutter / drag knife, emits HPGL instead of G-code."
      >
        Mode
        <select bind:value={draft.mode}>
          <option value="mill">Mill (CNC)</option>
          <option value="laser">Laser</option>
          <option value="drag">Drag-knife / vinyl</option>
        </select>
      </label>
      <label
        title="Safe Z height for rapids between cuts. Spindle rapids to this height before XY moves so the tool clears clamps and stock."
      >
        Fast-move Z
        <span class="field"
          ><input type="number" bind:value={draft.fastMoveZ} step="0.1" /><span class="unit"
            >mm</span
          ></span
        >
      </label>
      <fieldset class="work-area">
        <legend
          title="Machine work envelope in mm — the stock auto-defaults to this size when no drawing is loaded, and (future) sim checks use it as the soft-limit reference."
          >Work area</legend
        >
        <label
          >X
          <span class="field"
            ><input
              type="number"
              min="1"
              step="10"
              value={draft.workArea?.x ?? 200}
              oninput={(e) => {
                const v = (e.target as HTMLInputElement).valueAsNumber;
                if (Number.isFinite(v) && v > 0) {
                  draft.workArea = { x: v, y: draft.workArea?.y ?? 300, z: draft.workArea?.z ?? 50 };
                }
              }}
            /><span class="unit">mm</span></span
          >
        </label>
        <label
          >Y
          <span class="field"
            ><input
              type="number"
              min="1"
              step="10"
              value={draft.workArea?.y ?? 300}
              oninput={(e) => {
                const v = (e.target as HTMLInputElement).valueAsNumber;
                if (Number.isFinite(v) && v > 0) {
                  draft.workArea = { x: draft.workArea?.x ?? 200, y: v, z: draft.workArea?.z ?? 50 };
                }
              }}
            /><span class="unit">mm</span></span
          >
        </label>
        <label
          >Z
          <span class="field"
            ><input
              type="number"
              min="1"
              step="5"
              value={draft.workArea?.z ?? 50}
              oninput={(e) => {
                const v = (e.target as HTMLInputElement).valueAsNumber;
                if (Number.isFinite(v) && v > 0) {
                  draft.workArea = { x: draft.workArea?.x ?? 200, y: draft.workArea?.y ?? 300, z: v };
                }
              }}
            /><span class="unit">mm</span></span
          >
        </label>
      </fieldset>
      <label
        class="check"
        title="Include (parenthesized) comments in the G-code — section markers, op names, tool numbers. Disable for controllers that reject comments."
      >
        <input type="checkbox" bind:checked={draft.comments} />
        Emit comments in G-code
      </label>
      <label
        class="check"
        title="Fit curved polylines into native G2/G3 arc moves where possible. Yields smaller, smoother G-code. Disable if your controller has buggy arc handling."
      >
        <input type="checkbox" bind:checked={draft.arcs} />
        Emit G2 / G3 arc moves
      </label>
      <label
        class:disabled={!draft.arcs}
        title="How far the fitted arc may deviate from the original polyline. Smaller = tighter, more arcs split. Typical values 0.005-0.05 mm."
      >
        Arc fitting tolerance
        <span class="field"
          ><input
            type="number"
            min="0"
            step="0.001"
            disabled={!draft.arcs}
            value={draft.arcFitToleranceMm ?? 0.01}
            oninput={(e) => {
              const v = (e.target as HTMLInputElement).valueAsNumber;
              draft.arcFitToleranceMm = isFinite(v) && v >= 0 ? v : undefined;
            }}
          /><span class="unit">mm</span></span
        >
      </label>
      <label
        class="check"
        title="Emit M6 tool-change commands between ops with different tools. Disable for hobby controllers (GRBL etc.) that need manual tool-change prompts instead."
      >
        <input type="checkbox" bind:checked={draft.supportsToolchange} />
        Machine supports tool changes (M6)
      </label>
      <label
        class="check"
        title="Plot-mode Z (rt1.35): collapse every cut to a single pass at the op's cut depth and skip the multi-step descent / ramp / helix machinery. Z values in gcode are restricted to fast_move_z (pen up) and cut depth (pen down). Right setting for laser / plasma / pen plotters / 3D-printer extrusion and drag-knife controllers."
      >
        <input type="checkbox" bind:checked={draft.plotModeZ} />
        Plot-mode Z (single-pass, binary up/down)
      </label>

      <div class="section-title">G-code formatting</div>
      <label
        title="Some EU-locale Siemens / Heidenhain controllers require X1,5 instead of X1.5. Default is the period."
      >
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
      <label
        title="Prefix every emitted line with N10, N20, N30, … Required by some FANUC / vintage controllers; useful operator reference even on modern ones. Empty / 0 disables numbering."
      >
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
      {#if draft.mode === 'drag'}
        <p class="hpgl-note">
          Drag mode emits HPGL plotter commands, not G-code. The post-processor profile (templates,
          axes, etc.) is ignored — HPGL has no analogue for these tokens.
        </p>
      {/if}
      <label
        title="Pick a built-in profile or write your own templates below. Built-in profiles fill the templates with sensible defaults for that controller; you can still edit them. 'None' uses wiac's hard-coded defaults."
      >
        Profile preset
        <span class="field">
          <select
            value={profilePreset(draft.postProfile)}
            onchange={(e) => {
              const v = (e.currentTarget as HTMLSelectElement).value;
              if (v === 'none') {
                draft.postProfile = undefined;
              } else if (v === 'linuxcnc') {
                draft.postProfile = {
                  name: 'LinuxCNC default',
                  file_extension: 'nc',
                  line_ending: '\n',
                };
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
        <div class="pp-summary">
          <span class="pp-summary-tweaks">
            {#if profileTweakSummary(draft.postProfile)}
              Overrides: <em>{profileTweakSummary(draft.postProfile)}</em>
            {:else}
              No overrides yet — preset defaults.
            {/if}
          </span>
          <button type="button" class="pp-edit-btn" onclick={() => (editorOpen = true)}
            >Edit templates / axes…</button
          >
        </div>
      {/if}

      <div class="section-title">Kinematics</div>
      <label
        title="G0 rapid feed rate. Used by the simulator for time estimates; most controllers ignore the F-word on G0 and use their own internal max."
      >
        Rapid speed
        <span class="field"
          ><input type="number" min="0" step="100" bind:value={draft.rapidSpeed} /><span
            class="unit">mm/min</span
          ></span
        >
      </label>
      <label
        title="Wall-clock seconds spent on an M6 tool change. Used by the simulator for total runtime estimates."
      >
        Tool-change time
        <span class="field"
          ><input type="number" min="0" step="0.5" bind:value={draft.toolchangeS} /><span
            class="unit">s</span
          ></span
        >
      </label>
      <div class="triplet-label">Acceleration X / Y / Z <span class="unit">mm/s²</span></div>
      <div class="triplet">
        <input
          type="number"
          min="0"
          step="10"
          aria-label="Acceleration X (mm/s²)"
          value={draft.accel?.x ?? 250}
          oninput={(e) => {
            const v = (e.target as HTMLInputElement).valueAsNumber;
            draft.accel = {
              ...(draft.accel ?? { x: 250, y: 250, z: 250 }),
              x: isFinite(v) ? v : 250,
            };
          }}
        />
        <input
          type="number"
          min="0"
          step="10"
          aria-label="Acceleration Y (mm/s²)"
          value={draft.accel?.y ?? 250}
          oninput={(e) => {
            const v = (e.target as HTMLInputElement).valueAsNumber;
            draft.accel = {
              ...(draft.accel ?? { x: 250, y: 250, z: 250 }),
              y: isFinite(v) ? v : 250,
            };
          }}
        />
        <input
          type="number"
          min="0"
          step="10"
          aria-label="Acceleration Z (mm/s²)"
          value={draft.accel?.z ?? 250}
          oninput={(e) => {
            const v = (e.target as HTMLInputElement).valueAsNumber;
            draft.accel = {
              ...(draft.accel ?? { x: 250, y: 250, z: 250 }),
              z: isFinite(v) ? v : 250,
            };
          }}
        />
      </div>
      <label
        class="check"
        title="Use S-curve (jerk-limited) acceleration in the simulator. Better matches modern controllers (LinuxCNC trajectory planner, MachineKit). Off = simple trapezoidal velocity profile."
      >
        <input type="checkbox" bind:checked={jerkEnabled} />
        Enable jerk limits (S-curve, Phase 2)
      </label>
      {#if jerkEnabled}
        <div class="triplet-label">Jerk X / Y / Z <span class="unit">mm/s³</span></div>
        <div class="triplet">
          <input
            type="number"
            min="0"
            step="10"
            aria-label="Jerk X (mm/s³)"
            bind:value={jerkDraft.x}
          />
          <input
            type="number"
            min="0"
            step="10"
            aria-label="Jerk Y (mm/s³)"
            bind:value={jerkDraft.y}
          />
          <input
            type="number"
            min="0"
            step="10"
            aria-label="Jerk Z (mm/s³)"
            bind:value={jerkDraft.z}
          />
        </div>
      {/if}
    </div>

    <footer>
      {#if confirmingDiscard}
        <span class="discard-prompt">Discard unsaved changes?</span>
        <button class="secondary" onclick={cancelDiscard}>Keep editing</button>
        <button class="danger" onclick={discardAndClose}>Discard</button>
      {:else}
        <button class="secondary" onclick={close}>Cancel</button>
        <button class="primary" onclick={commit}>OK</button>
      {/if}
    </footer>
  </Modal>
  <PostProcessorEditor
    open={editorOpen}
    initial={draft.postProfile ?? { name: 'Custom' }}
    onSave={(next) => {
      draft.postProfile = next;
      editorOpen = false;
    }}
    onClose={() => (editorOpen = false)}
  />
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
  .hpgl-note {
    grid-column: 1 / -1;
    margin: 0.3rem 0;
    padding: 0.4rem 0.6rem;
    border: 1px dashed var(--border);
    border-radius: 4px;
    background: color-mix(in srgb, var(--accent) 4%, var(--bg-panel));
    color: var(--text-muted);
    font-size: 0.78rem;
    line-height: 1.4;
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
  .danger {
    background: var(--danger, #c0392b);
    color: white;
    border: 0;
    padding: 0.3rem 0.8rem;
    border-radius: 3px;
    cursor: pointer;
  }
  .discard-prompt {
    margin-right: auto;
    color: var(--danger, #c0392b);
    font-size: 0.85rem;
    align-self: center;
  }
  .pp-summary {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    padding: 0.3rem 0.6rem;
    border: 1px dashed var(--border);
    border-radius: 4px;
    background: var(--bg-elevated);
    font-size: 0.82rem;
  }
  .pp-summary-tweaks em {
    color: var(--accent);
    font-style: normal;
  }
  .pp-edit-btn {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 0.25rem 0.6rem;
    border-radius: 3px;
    cursor: pointer;
    font-size: 0.82rem;
  }
  .pp-edit-btn:hover {
    background: var(--bg);
  }
</style>
