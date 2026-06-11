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
  import { DialogDraft } from './dialog-draft.svelte';
  import * as fileOps from '../services/file_ops';
  import { workspace } from '../state/workspace.svelte';
  import { duplicateProfile, profileFromCurrent } from '../state/machine_profiles';

  interface Props {
    open: boolean;
    onClose: () => void;
    /// Render as a first-class tab panel instead of a modal: no Modal
    /// wrapper, no × / Cancel (the component stays mounted across tab
    /// switches, so an in-progress draft survives), footer becomes
    /// Apply / Revert.
    embedded?: boolean;
  }
  let { open, onClose, embedded = false }: Props = $props();
  const active = $derived(open || embedded);

  // PostProcessor editor — the heavy editing surface lives in a
  // dedicated dialog with a live preview pane and JSON I/O. We just
  // own the "is it open" flag.
  let editorOpen = $state(false);

  /// The dialog edits two top-level pieces — the machine settings plus
  /// the jerk fields (a single enable checkbox + AxisLimits, default
  /// off: trapezoidal profile only; S-curve refinement is Phase 2) —
  /// so the DialogDraft wraps them as one composite. One draft, one
  /// pristine snapshot, one dirty check covering both.
  interface MachineDraft {
    machine: MachineSettings;
    jerkEnabled: boolean;
    jerk: AxisLimits;
  }

  function compositeOf(m: MachineSettings): MachineDraft {
    return {
      machine: cloneSettings(m),
      jerkEnabled: !!m.jerk,
      jerk: m.jerk ? { ...m.jerk } : { x: 100, y: 100, z: 50 },
    };
  }

  const dd = new DialogDraft<MachineDraft>();
  dd.open(compositeOf(project.data.machine));
  /// Narrow aliases so the template / commit path keep reading
  /// `draft.*` / `jerkDraft.*` — two-way bindings still mutate the
  /// deeply-reactive dd.draft underneath. The `??` arms are unreachable
  /// (dd is opened at init and never close()d) but keep the types
  /// null-free.
  const composite = $derived(dd.draft ?? compositeOf(project.data.machine));
  const draft = $derived(composite.machine);
  const jerkDraft = $derived(composite.jerk);

  $effect(() => {
    if (active) {
      // Embedded panels stay mounted, so external machine changes
      // (undo, profile switch) re-run this — refresh a CLEAN draft to
      // stay in sync, but never clobber in-progress edits.
      if (embedded && dd.isDirty) return;
      // try/catch so a throw from cloning can't propagate into the
      // Svelte 5 reactivity scheduler and abort it — a dead scheduler
      // manifests as "every button still fires its onclick, but
      // visible state stops updating" (see App.svelte:117).
      try {
        dd.open(compositeOf(project.data.machine));
      } catch (e) {
        console.error('MachineDialog.open: init failed', e);
      }
      confirmingProfileDelete = false;
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
          program_start: '%\nN10 G21 G90 (ivac <version>)',
          program_end: 'M30\n%',
        },
      },
      {
        key: 'grbl',
        profile: {
          name: 'GRBL default',
          file_extension: 'nc',
          line_ending: '\n',
          program_start: '; ivac <version> — GRBL',
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

  /// Plain MachineSettings from the composite draft — the exact value
  /// commit() hands to setMachine, also reused by "Save as profile".
  /// $state.snapshot strips the proxy (Svelte 5 proxies can trip
  /// `structuredClone` on some WebKit builds, silently aborting the
  /// commit); jerk folds back in from its enable toggle; a non-empty
  /// capability set is normalized to contain the primary mode (the
  /// emitter targets it; Rust's effective-capability logic reads
  /// capabilities INSTEAD of mode when non-empty — and the disabled
  /// primary checkbox never fires onchange, so a mode change after
  /// customizing capabilities can leave the draft set lacking it).
  function draftSnapshot(): MachineSettings {
    const snap = $state.snapshot(draft) as MachineSettings;
    snap.jerk = composite.jerkEnabled ? { ...jerkDraft } : undefined;
    if (snap.capabilities && snap.capabilities.length > 0) {
      snap.capabilities = [...new Set([...snap.capabilities, snap.mode])];
    }
    return snap;
  }

  function commit() {
    // Whole body in try/catch so any throw from the snapshot or
    // setMachine still reaches `onClose()` — otherwise OK appears
    // broken. The error is logged and surfaces via the global error
    // banner so the underlying bug stays visible.
    try {
      project.setMachine(draftSnapshot());
    } catch (e) {
      console.error('MachineDialog.commit: setMachine failed', e);
    }
    // Embedded (tab) mode: Apply commits and stays — re-baseline the
    // draft instead of closing.
    if (embedded) dd.markClean();
    else onClose();
  }

  /// Embedded-mode Revert: drop the draft back to the committed machine.
  function revert() {
    dd.open(compositeOf(project.data.machine));
    confirmingProfileDelete = false;
  }

  // ── machine profiles ──────────────────────────────────────────────
  // Workspace-level named (machine + tool library) bundles — one per
  // physical machine. The picker applies a profile to the PROJECT
  // (machine + tools + reference, one undoable step); while a profile
  // is referenced, project edits mirror back into it (App-level
  // effect). The project file keeps its own embedded snapshot, so a
  // referenced-but-missing profile still loads exactly as saved.
  const profiles = $derived.by(() => {
    void workspace.version;
    return workspace.get().machine_profiles;
  });
  const activeProfileId = $derived(project.data.machineProfileId);
  const activeProfileMissing = $derived(
    activeProfileId != null && !profiles.some((p) => p.id === activeProfileId),
  );
  /// Two-step delete guard, same inline pattern as the discard bar.
  let confirmingProfileDelete = $state(false);

  function reseedDraft() {
    try {
      dd.open(compositeOf(project.data.machine));
    } catch (e) {
      console.error('MachineDialog: draft reseed failed', e);
    }
    confirmingProfileDelete = false;
  }

  function onProfileSelect(value: string) {
    confirmingProfileDelete = false;
    if (value === (activeProfileId ?? '')) return;
    if (value === '') {
      project.detachMachineProfile();
      reseedDraft();
      return;
    }
    const p = profiles.find((x) => x.id === value);
    if (!p) return;
    project.applyMachineProfile(p);
    reseedDraft();
  }

  /// Save the CURRENT draft machine + the project's tool library as a
  /// new profile and link the project to it. Applying the profile also
  /// commits the draft machine (same undoable step), so the dialog
  /// reseeds clean afterwards.
  function saveAsProfile() {
    try {
      const profile = profileFromCurrent(draftSnapshot(), project.data.tools, profiles);
      workspace.upsertMachineProfile(profile);
      project.applyMachineProfile(profile);
    } catch (e) {
      console.error('MachineDialog: save-as-profile failed', e);
    }
    reseedDraft();
  }

  function duplicateActiveProfile() {
    const src = profiles.find((p) => p.id === activeProfileId);
    if (!src) return;
    try {
      const copy = duplicateProfile(src, profiles);
      workspace.upsertMachineProfile(copy);
      project.applyMachineProfile(copy);
    } catch (e) {
      console.error('MachineDialog: duplicate-profile failed', e);
    }
    reseedDraft();
  }

  /// First click arms; second deletes the profile from the workspace
  /// and detaches the project (machine + tools stay as they are —
  /// deleting a profile never touches the working copy).
  function deleteActiveProfile() {
    if (!confirmingProfileDelete) {
      confirmingProfileDelete = true;
      return;
    }
    confirmingProfileDelete = false;
    if (activeProfileId == null) return;
    workspace.deleteMachineProfile(activeProfileId);
    project.detachMachineProfile();
  }

  /// Close protocol (dd.requestClose): the first attempt on a dirty
  /// draft arms the inline "Discard / Keep editing" footer pair; the
  /// second confirms. The inline bar replaces the prior `window.confirm`
  /// prompt, which silently returns false in some Tauri / WebKitGTK
  /// builds — the project's audit-C10 note already flagged
  /// `window.confirm` as unreliable for this reason.
  function close() {
    if (dd.requestClose()) onClose();
  }
</script>

{#snippet shell()}
  <header>
    <h2 id="machine-title">Machine</h2>
    {#if !embedded}
      <button class="dlg-close" onclick={close} aria-label="Close">×</button>
    {/if}
  </header>

  <!-- Machine profiles: named (machine + tool library) bundles
         stored per-user, one per physical machine. Picking one applies
         its config AND its tool library to the project as a single
         undoable step. -->
  {#if !embedded}
    <div class="profile-bar">
      <label
        class="profile-pick"
        title="Machine profile — a named machine setup plus its own tool library, saved on this computer (not in the project). Switching profiles swaps both into the project in one undoable step. While a profile is selected, edits to the machine or the tool library are saved back into it."
      >
        <span>Profile</span>
        <select
          value={activeProfileId ?? ''}
          disabled={dd.isDirty}
          title={dd.isDirty
            ? 'You have unsaved machine edits — OK or Cancel them first, then switch profiles.'
            : ''}
          onchange={(e) => onProfileSelect((e.currentTarget as HTMLSelectElement).value)}
        >
          <option value="">— project-local (no profile) —</option>
          {#each profiles as p (p.id)}
            <option value={p.id}>{p.name}</option>
          {/each}
          {#if activeProfileMissing}
            <option value={activeProfileId}>Referenced profile (not on this computer)</option>
          {/if}
        </select>
      </label>
      <button
        type="button"
        class="profile-btn"
        onclick={saveAsProfile}
        title="Save the machine settings shown below together with the project's current tool library as a new profile, and link this project to it."
        >Save as profile</button
      >
      {#if activeProfileId != null && !activeProfileMissing}
        <button
          type="button"
          class="profile-btn"
          onclick={duplicateActiveProfile}
          title="Copy this profile (machine + tools) under a new name — for variants of the same machine."
          >Duplicate</button
        >
        <button
          type="button"
          class="profile-btn"
          class:danger={confirmingProfileDelete}
          onclick={deleteActiveProfile}
          title="Remove this profile from this computer. The project keeps its current machine settings and tools — nothing in the project is deleted."
          >{confirmingProfileDelete ? 'Delete profile?' : 'Delete'}</button
        >
      {/if}
      {#if activeProfileMissing}
        <span
          class="profile-note"
          title="This project references a machine profile that doesn't exist on this computer (it was saved elsewhere). The project loaded with its own embedded machine + tools — use 'Save as profile' to recreate the profile here."
          >not on this computer</span
        >
      {/if}
    </div>
  {/if}

  <!--
      ninc: storage is always mm regardless of `draft.unit`. The unit
      selector below only switches the G20/G21 word in emitted gcode.
      We hint that once here instead of stamping "mm" onto every
      individual numeric input — a literal "mm" suffix while the user
      had unit=inch selected was misleading.
    -->
  <p class="storage-note">
    Lengths are stored in <strong>mm</strong> regardless of the Unit selector below — the Unit only switches
    the G20/G21 word in emitted gcode.
  </p>

  <div class="grid">
    <label
      title="Free-text identifier for this machine setup. Shown in the dialog header + persisted into .ivac-machine.json save files."
    >
      Name
      <input
        type="text"
        placeholder="e.g. Shop CNC"
        value={draft.name ?? ''}
        oninput={(e) => (draft.name = (e.currentTarget as HTMLInputElement).value)}
      />
    </label>
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
      title="Primary mode — drives the G-code emitter. Mill: subtractive CNC, full Z control. Laser: M3/M5 power, ignores Z. Drag: vinyl cutter / drag knife, emits HPGL. Plasma: torch with a pierce/cut-height entry sequence. Adjust 'Capabilities' below if the machine can do more than this primary mode."
    >
      Mode
      <select bind:value={draft.mode}>
        <option value="mill">Mill (CNC)</option>
        <option value="laser">Laser</option>
        <option value="drag">Drag-knife / vinyl</option>
        <option value="plasma">Plasma</option>
      </select>
    </label>
    <fieldset class="capabilities">
      <legend
        title="Which op kinds the machine can run. The op-picker hides kinds the machine doesn't support — e.g. a laser-only machine never shows Drill. Defaults to just the primary Mode above if left empty."
      >
        Capabilities
      </legend>
      {#each ['mill', 'laser', 'drag', 'plasma'] as cap (cap)}
        {@const isPrimary = cap === draft.mode}
        <!-- The primary mode is always a capability (the gcode
               emitter targets it), so its checkbox is DISABLED rather
               than silently re-added on commit — unchecking it and
               watching it come back on reopen reads as a bug. -->
        <label class="cap-toggle">
          <input
            type="checkbox"
            disabled={isPrimary}
            title={isPrimary
              ? 'The primary mode (selected under Mode above) is always available — pick a different Mode to remove it.'
              : ''}
            checked={isPrimary ||
              (draft.capabilities ?? [draft.mode]).includes(
                cap as 'mill' | 'laser' | 'drag' | 'plasma',
              )}
            onchange={(e) => {
              const on = (e.currentTarget as HTMLInputElement).checked;
              const cur = new Set(draft.capabilities ?? [draft.mode]);
              if (on) cur.add(cap as 'mill' | 'laser' | 'drag' | 'plasma');
              else cur.delete(cap as 'mill' | 'laser' | 'drag' | 'plasma');
              // Belt-and-braces: keep the primary mode in the set so
              // the gcode emitter never targets a removed capability
              // (the checkbox above is disabled, but a stale draft
              // could still lack it).
              cur.add(draft.mode);
              draft.capabilities = [...cur];
            }}
          />
          <span
            >{cap === 'mill'
              ? 'Mill'
              : cap === 'laser'
                ? 'Laser'
                : cap === 'drag'
                  ? 'Drag-knife'
                  : 'Plasma'}</span
          >
        </label>
      {/each}
    </fieldset>
    <label
      title="Safe Z height for rapids between cuts. Spindle rapids to this height before XY moves so the tool clears clamps and stock."
    >
      Fast-move Z
      <span class="field"
        ><input type="number" bind:value={draft.fastMoveZ} step="0.1" /><span class="unit">mm</span
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
                draft.workArea = {
                  x: v,
                  y: draft.workArea?.y ?? 300,
                  z: draft.workArea?.z ?? 50,
                };
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
                draft.workArea = {
                  x: draft.workArea?.x ?? 200,
                  y: v,
                  z: draft.workArea?.z ?? 50,
                };
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
                draft.workArea = {
                  x: draft.workArea?.x ?? 200,
                  y: draft.workArea?.y ?? 300,
                  z: v,
                };
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
      title="How the post handles a tool change between ops with different tools. ATC: emit T<n> M6, the changer swaps automatically. Manual (M6 prompt): grblHAL / FluidNC — emit M6, the controller parks and prompts the operator. Manual (M0 pause): portable M0 program pause for stock GRBL / Marlin, which reject M6. Ignore: emit no tool-change handling (you manage swaps yourself)."
    >
      Tool changes
      <select bind:value={draft.toolchangeStrategy}>
        <option value="atc">Automatic changer (T&lt;n&gt; M6)</option>
        <option value="manual_m6_prompt">Manual — M6 prompt (grblHAL / FluidNC)</option>
        <option value="manual_m0_pause">Manual — M0 pause (stock GRBL / Marlin)</option>
        <option value="ignore">Ignore (no tool-change output)</option>
      </select>
    </label>
    <!-- Optional-stop M1 alternative to M0 at program pauses. -->
    <label
      class="check"
      title="Emit M1 (optional stop) instead of M0 (mandatory stop) at every program pause — the Pause op and the manual tool-change halt. M1 is honored only when the controller's optional-stop switch is ON, so a vetted program can run unattended (switch off skips the pauses) yet still stop on demand. Off = mandatory M0."
    >
      <input type="checkbox" bind:checked={draft.optionalStop} />
      Optional stop (M1 instead of M0)
    </label>
    <!-- z9zh: GRBL dynamic-power laser mode (M4). -->
    <label
      class="check"
      title="GRBL only: emit M4 (dynamic power) instead of M3 for laser cuts and raster engraving. The controller ramps the S power with the actual feed rate, so corners and edges — where the head slows — don't over-burn, and rapids force S0 automatically. Strongly preferred for laser engraving. Requires GRBL laser mode ($32=1). Off = portable M3."
    >
      <input type="checkbox" bind:checked={draft.laserDynamicPower} />
      Laser dynamic power (GRBL M4)
    </label>
    <label
      class="check"
      title="Plot-mode Z: collapse every cut to a single pass at the op's cut depth and skip the multi-step descent / ramp / helix machinery. Z values in G-code are restricted to fast_move_z (pen up) and cut depth (pen down). Right setting for laser / plasma / pen plotters / 3D-printer extrusion and drag-knife controllers."
    >
      <input type="checkbox" bind:checked={draft.plotModeZ} />
      Plot-mode Z (single-pass, binary up/down)
    </label>

    <div class="section-title">G-code formatting</div>
    <label
      title="Output dialect for this machine's controller. LinuxCNC: standard RS-274 G-code. GRBL: hobby-CNC subset with manual tool-change prompts. HPGL: vinyl-cutter / plotter language (drag-knife mode)."
    >
      G-code dialect
      <span class="field">
        <select
          value={draft.gcodeDialect ?? 'linuxcnc'}
          onchange={(e) => {
            const v = (e.currentTarget as HTMLSelectElement).value;
            draft.gcodeDialect = v === 'grbl' || v === 'hpgl' ? v : 'linuxcnc';
          }}
        >
          <option value="linuxcnc">LinuxCNC</option>
          <option value="grbl">GRBL</option>
          <option value="hpgl">HPGL</option>
        </select>
      </span>
    </label>
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

    <div class="section-title">Post-processor profile</div>
    {#if draft.mode === 'drag'}
      <p class="hpgl-note">
        Drag mode emits HPGL plotter commands, not G-code. The post-processor profile (templates,
        axes, etc.) is ignored — HPGL has no analogue for these tokens.
      </p>
    {/if}
    <label
      title="Pick a built-in profile or write your own templates below. Built-in profiles fill the templates with sensible defaults for that controller; you can still edit them. 'None' uses ivac's hard-coded defaults."
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
                program_start: '%\nN10 G21 G90 (ivac <version>)',
                program_end: 'M30\n%',
              };
            } else if (v === 'grbl') {
              draft.postProfile = {
                name: 'GRBL default',
                file_extension: 'nc',
                line_ending: '\n',
                program_start: '; ivac <version> — GRBL',
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
        ><input type="number" min="0" step="100" bind:value={draft.rapidSpeed} /><span class="unit"
          >mm/min</span
        ></span
      >
    </label>
    <label
      title="Wall-clock seconds spent on an M6 tool change. Used by the simulator for total runtime estimates."
    >
      Tool-change time
      <span class="field"
        ><input type="number" min="0" step="0.5" bind:value={draft.toolchangeS} /><span class="unit"
          >s</span
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
      <input type="checkbox" bind:checked={composite.jerkEnabled} />
      Enable jerk limits (S-curve, Phase 2)
    </label>
    {#if composite.jerkEnabled}
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

    <div class="section-title">Spindle clamps &amp; warmup</div>
    <label
      title="Lower spindle-RPM clamp (M3 S<rpm>). Tool / op RPMs below this clamp UP to the min and emit a 'spindle_speed_clamped_below_min' warning. Empty = no floor (default)."
    >
      Spindle RPM min
      <span class="field"
        ><input
          type="number"
          min="0"
          step="100"
          placeholder="—"
          value={draft.spindleRpmMin ?? ''}
          oninput={(e) => {
            const raw = (e.target as HTMLInputElement).value;
            if (raw === '') {
              draft.spindleRpmMin = undefined;
              return;
            }
            const v = parseInt(raw, 10);
            draft.spindleRpmMin = isFinite(v) && v >= 0 ? v : undefined;
          }}
        /><span class="unit">RPM</span></span
      >
    </label>
    <label
      title="Upper spindle-RPM clamp. Tool / op RPMs above this clamp DOWN to the max and emit a 'spindle_speed_clamped_above_max' warning. Empty = no ceiling (default)."
    >
      Spindle RPM max
      <span class="field"
        ><input
          type="number"
          min="0"
          step="500"
          placeholder="—"
          value={draft.spindleRpmMax ?? ''}
          oninput={(e) => {
            const raw = (e.target as HTMLInputElement).value;
            if (raw === '') {
              draft.spindleRpmMax = undefined;
              return;
            }
            const v = parseInt(raw, 10);
            draft.spindleRpmMax = isFinite(v) && v >= 0 ? v : undefined;
          }}
        /><span class="unit">RPM</span></span
      >
    </label>
    <label
      title="Upper feed clamp. Cutting / plunge feeds above this clamp DOWN to the max and emit a 'feed_clamped_above_max' warning, so an out-of-range feed can't reach the controller. Empty = no ceiling (default)."
    >
      Max feed
      <span class="field"
        ><input
          type="number"
          min="0"
          step="100"
          placeholder="—"
          value={draft.maxFeedMmMin ?? ''}
          oninput={(e) => {
            const raw = (e.target as HTMLInputElement).value;
            if (raw === '') {
              draft.maxFeedMmMin = undefined;
              return;
            }
            const v = parseInt(raw, 10);
            draft.maxFeedMmMin = isFinite(v) && v >= 0 ? v : undefined;
          }}
        /><span class="unit">mm/min</span></span
      >
    </label>
    <label
      title="Spindle-start dwell inserted into the M6 tool-change envelope after M3 S<rpm>. Lets the spindle reach commanded RPM before the next cut. Stacks with the per-tool ToolEntry.pause. Empty = 0.5 s default."
    >
      Spindle start dwell
      <span class="field"
        ><input
          type="number"
          min="0"
          step="0.1"
          placeholder="0.5"
          value={draft.spindleStartDwellSec ?? ''}
          oninput={(e) => {
            const raw = (e.target as HTMLInputElement).value;
            if (raw === '') {
              draft.spindleStartDwellSec = undefined;
              return;
            }
            const v = parseFloat(raw);
            draft.spindleStartDwellSec = isFinite(v) && v >= 0 ? v : undefined;
          }}
        /><span class="unit">s</span></span
      >
    </label>
    <label
      title="Spindle-stop dwell inserted between M5 and the actual T<n> M6. Gives the spindle time to spin down before the chuck is touched. Most VFD spindles want 0.5–1 s; high-inertia big-iron may want 1–2 s. Set to 0 to skip. Empty = 0.5 s default."
    >
      Spindle stop dwell
      <span class="field"
        ><input
          type="number"
          min="0"
          step="0.1"
          placeholder="0.5"
          value={draft.spindleStopDwellSec ?? ''}
          oninput={(e) => {
            const raw = (e.target as HTMLInputElement).value;
            if (raw === '') {
              draft.spindleStopDwellSec = undefined;
              return;
            }
            const v = parseFloat(raw);
            draft.spindleStopDwellSec = isFinite(v) && v >= 0 ? v : undefined;
          }}
        /><span class="unit">s</span></span
      >
    </label>
    <label
      class="check"
      title="When on, the program_end footer emits G53 G0 X0 Y0 — retract to machine home after the safe-Z lift, before spindle-off and M30. When off, falls back to G0 X0 Y0 in the current WCS (work zero) and the optional Park XY below applies."
    >
      <input type="checkbox" bind:checked={draft.parkAtHome} />
      Park at machine home (G53)
    </label>
    {#if !draft.parkAtHome}
      <div class="triplet-label">
        Park XY <span class="unit">mm, WCS</span>
        <small class="park-help"
          >Optional — empty = retract to work zero (0, 0). Set both fields to route the head to a
          specific load / tool-station point after the safe-Z lift.</small
        >
      </div>
      <div class="park-pair">
        <input
          type="number"
          step="1"
          aria-label="Park X (mm, WCS)"
          placeholder="X"
          value={draft.parkXy?.[0] ?? ''}
          oninput={(e) => {
            const raw = (e.target as HTMLInputElement).value;
            const cur = draft.parkXy ?? [0, 0];
            if (raw === '') {
              // Clearing X drops the whole pair — both must be set
              // for park_xy to mean anything on the wire.
              draft.parkXy = undefined;
              return;
            }
            const v = parseFloat(raw);
            if (!isFinite(v)) return;
            draft.parkXy = [v, cur[1]];
          }}
        />
        <input
          type="number"
          step="1"
          aria-label="Park Y (mm, WCS)"
          placeholder="Y"
          value={draft.parkXy?.[1] ?? ''}
          oninput={(e) => {
            const raw = (e.target as HTMLInputElement).value;
            const cur = draft.parkXy ?? [0, 0];
            if (raw === '') {
              draft.parkXy = undefined;
              return;
            }
            const v = parseFloat(raw);
            if (!isFinite(v)) return;
            draft.parkXy = [cur[0], v];
          }}
        />
      </div>
    {/if}
  </div>

  <footer>
    {#if dd.confirmingDiscard}
      <span class="discard-prompt">Discard unsaved changes?</span>
      <button class="btn-secondary" onclick={() => dd.cancelDiscard()}>Keep editing</button>
      <button class="btn-danger" onclick={close}>Discard</button>
    {:else}
      <button
        class="btn-secondary"
        onclick={async () => {
          // Commit any pending edits first so the saved snapshot
          // reflects what the user is looking at, not the
          // previously-committed values.
          commit();
          await fileOps.saveMachine();
        }}
        title="Save this machine config to a .ivac-machine.json file."
      >
        Save…
      </button>
      <button
        class="btn-secondary"
        onclick={async () => {
          await fileOps.loadMachine();
          // Refresh draft so the dialog shows the freshly-loaded
          // values; otherwise it still mirrors the old pre-load draft.
          // Assigned directly (not dd.open) so the pristine snapshot
          // from open is kept — loading a different config counts as
          // an unsaved change, same as before.
          const m = $state.snapshot(project.data.machine) as MachineSettings;
          dd.draft = {
            machine: m,
            jerkEnabled: !!m.jerk,
            jerk: m.jerk ? { ...m.jerk } : { x: 0, y: 0, z: 0 },
          };
        }}
        title="Replace the active machine config with the contents of a .ivac-machine.json file."
      >
        Load…
      </button>
      <span class="sep"></span>
      {#if embedded}
        <button class="btn-secondary" onclick={revert} disabled={!dd.isDirty}>Revert</button>
        <button class="btn-primary" onclick={commit} disabled={!dd.isDirty}>Apply</button>
      {:else}
        <button class="btn-secondary" onclick={close}>Cancel</button>
        <button class="btn-primary" onclick={commit}>OK</button>
      {/if}
    {/if}
  </footer>
{/snippet}

{#if embedded}
  <section class="embedded-shell">{@render shell()}</section>
{:else if open}
  <Modal
    onClose={close}
    modalClass="machine-modal"
    persistKey="machine"
    width="min(880px, 96vw)"
    draggable
    resizable
    ariaLabelledBy="machine-title"
  >
    {@render shell()}
  </Modal>
{/if}
{#if active}
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
  /* `.dlg-close` lives in Modal.svelte; `.btn-primary|secondary|danger`
     + `.discard-prompt` in app.css (audit hbi7). */
  /* Tab-panel (embedded) shell — fills the main area; the grid scrolls
     between the sticky header and footer. */
  .embedded-shell {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    background: var(--bg-panel);
    overflow: auto;
  }
  /* Machine-profile picker strip under the header. */
  .profile-bar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.45rem 0.7rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
    font-size: 0.8rem;
  }
  .profile-pick {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    min-width: 0;
  }
  .profile-pick select {
    max-width: 220px;
  }
  .profile-btn {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.2rem 0.55rem;
    font-size: 0.74rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .profile-btn.danger {
    border-color: var(--danger);
    color: var(--danger);
  }
  .profile-note {
    color: var(--text-muted);
    font-size: 0.74rem;
    font-style: italic;
  }
  .storage-note {
    margin: 0;
    padding: 0.45rem 0.7rem;
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--accent) 4%, var(--bg-panel));
    color: var(--text-muted);
    font-size: 0.78rem;
    line-height: 1.4;
  }
  .storage-note strong {
    color: var(--text);
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
  .park-pair {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 0.3rem;
  }
  .park-pair input[type='number'] {
    width: 100%;
  }
  .park-help {
    display: block;
    color: var(--text-muted);
    font-size: 0.7rem;
    font-weight: normal;
    margin-top: 0.15rem;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
    padding: 0.5rem 0.7rem;
    border-top: 1px solid var(--border);
    background: var(--bg-elevated);
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
