<script lang="ts">
  /// Non-modal notice shown after a machine-mode switch leaves ops
  /// referencing tools the new mode can't run, or leaves a singleton
  /// mode (laser / plasma / drag) with zero compatible tools. ONE
  /// notice per switch — not a warning per op — with a one-click fix:
  /// "assign <tool> to all" (auto-creating the mode's default when the
  /// library has none) or a plain seed action. Dismissable; never
  /// blocks anything; the library and the ops are untouched until the
  /// user clicks the action.

  import { project } from '../state/project.svelte';
  import { modeNotice } from '../state/mode_notice.svelte';
  import {
    KIND_DISPLAY_LABELS,
    machineModesLabel,
    toolCompatibleWithAnyMode,
  } from '../state/tool_family';
  import { defaultKindForMode } from '../state/tool_mode_defaults';

  const notice = $derived(modeNotice.current);
  /// Re-validate against the LIVE project — the user may have edited
  /// tools / ops since the switch (e.g. retagged a tool's kind), which
  /// can satisfy the notice without ever clicking its action.
  const affected = $derived(
    notice == null
      ? []
      : project.data.operations.filter((op) => {
          if (!notice.affectedOpIds.includes(op.id)) return false;
          const tool = project.data.tools.find((t) => t.id === op.toolId);
          return tool != null && !toolCompatibleWithAnyMode(tool.kind, notice.modes);
        }),
  );
  const compatibleTool = $derived(
    notice == null
      ? null
      : (project.data.tools.find((t) => toolCompatibleWithAnyMode(t.kind, notice.modes)) ?? null),
  );
  const stillRelevant = $derived(
    notice != null && (affected.length > 0 || (notice.seedOffer && compatibleTool == null)),
  );
  /// Button label target: the existing compatible tool by name, or the
  /// default the action would create ("a new Plasma torch").
  const assignTarget = $derived(
    notice == null
      ? ''
      : compatibleTool != null
        ? `"${compatibleTool.name}"`
        : `a new ${KIND_DISPLAY_LABELS[defaultKindForMode(notice.mode)].toLowerCase()}`,
  );

  function assignAll() {
    if (notice == null) return;
    project.assignToolToOps(
      affected.map((op) => op.id),
      compatibleTool?.id ?? null,
    );
    modeNotice.dismiss();
  }

  function seed() {
    project.seedDefaultToolForMode();
    modeNotice.dismiss();
  }
</script>

{#if notice != null && stillRelevant}
  <div class="mode-notice" role="status">
    {#if affected.length > 0}
      <span class="msg">
        {affected.length}
        {affected.length === 1 ? 'operation uses a tool' : 'operations use tools'} that cannot run on
        a {machineModesLabel(notice.modes)} machine.
      </span>
      <button type="button" class="action" onclick={assignAll}>
        Assign {assignTarget} to {affected.length === 1 ? 'it' : 'all'}
      </button>
    {:else}
      <span class="msg">
        No tool in the library can run on a {machineModesLabel(notice.modes)} machine.
      </span>
      <button type="button" class="action" onclick={seed}>
        Add a default {KIND_DISPLAY_LABELS[defaultKindForMode(notice.mode)].toLowerCase()}
      </button>
    {/if}
    <button
      type="button"
      class="dismiss"
      aria-label="Dismiss notice"
      title="Dismiss — nothing is changed. Incompatible assignments are also flagged at Generate time."
      onclick={() => modeNotice.dismiss()}>×</button
    >
  </div>
{/if}

<style>
  /* Bottom-center, above the canvas but below modals — a status strip,
     not an error: the ErrorToast palette stays reserved for failures. */
  .mode-notice {
    position: fixed;
    bottom: 1rem;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 0.6rem;
    max-width: min(640px, 92vw);
    padding: 0.45rem 0.7rem;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    box-shadow: 0 2px 10px rgba(0, 0, 0, 0.35);
    font-size: 0.82rem;
    /* Above on-canvas popovers; below modals — the MachineDialog the
       switch came from should stay on top while still open. */
    z-index: var(--z-floating);
  }
  .msg {
    line-height: 1.3;
  }
  .action {
    background: var(--accent);
    color: #fff;
    border: none;
    border-radius: 3px;
    padding: 0.25rem 0.6rem;
    font-size: 0.78rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .dismiss {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 1rem;
    cursor: pointer;
    padding: 0 0.2rem;
    line-height: 1;
  }
</style>
