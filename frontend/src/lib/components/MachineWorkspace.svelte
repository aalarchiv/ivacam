<script lang="ts">
  /// Machine tab — the first-class home for everything machine.
  /// Toolbar: pick the ACTIVE machine from the defined machines (the
  /// workspace machine profiles) + New / Duplicate / Delete / Load
  /// file… / Save file…. Below, two sub-tabs:
  ///   * Tooling (default): stock the machine from the shop inventory
  ///     — the day-to-day surface (think loading an ATC).
  ///   * Settings: the mostly-static machine config (speeds, dialect,
  ///     work area, post …) — the embedded MachineDialog form.
  /// Unlike the dialog's profile bar, these actions are draft-free:
  /// they operate on the COMMITTED project state.
  import { project } from '../state/project.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { duplicateProfile, profileFromCurrent } from '../state/machine_profiles';
  import * as fileOps from '../services/file_ops';
  import { t } from '../i18n';
  import MachineDialog from './MachineDialog.svelte';
  import MachineTooling from './MachineTooling.svelte';

  let subTab = $state<'tooling' | 'settings'>('tooling');

  const profiles = $derived.by(() => {
    void workspace.version;
    return workspace.get().machine_profiles;
  });
  const activeProfileId = $derived(project.data.machineProfileId);
  const activeProfileMissing = $derived(
    activeProfileId != null && !profiles.some((p) => p.id === activeProfileId),
  );
  let confirmingDelete = $state(false);

  function onSelect(value: string) {
    confirmingDelete = false;
    if (value === (activeProfileId ?? '')) return;
    if (value === '') {
      project.detachMachineProfile();
      return;
    }
    const p = profiles.find((x) => x.id === value);
    if (p) project.applyMachineProfile(p);
  }

  /// New machine = save the current machine settings + stocked tools
  /// as a profile and make it the active machine.
  function newMachine() {
    const profile = profileFromCurrent(
      $state.snapshot(project.data.machine),
      $state.snapshot(project.data.tools),
      profiles,
    );
    workspace.upsertMachineProfile(profile);
    project.applyMachineProfile(profile);
  }

  function duplicateMachine() {
    const src = profiles.find((p) => p.id === activeProfileId);
    if (!src) return;
    const copy = duplicateProfile(src, profiles);
    workspace.upsertMachineProfile(copy);
    project.applyMachineProfile(copy);
  }

  function deleteMachine() {
    if (!confirmingDelete) {
      confirmingDelete = true;
      return;
    }
    confirmingDelete = false;
    if (activeProfileId == null) return;
    workspace.deleteMachineProfile(activeProfileId);
    project.detachMachineProfile();
  }
</script>

<div class="machine-ws">
  <div class="ws-toolbar">
    <!-- File actions leftmost, matching the Project toolbar. -->
    <button
      type="button"
      class="ws-btn"
      onclick={() => void fileOps.loadMachine()}
      title={t('machinews.load_file.title')}>{t('machinews.load_file')}</button
    >
    <button
      type="button"
      class="ws-btn"
      onclick={() => void fileOps.saveMachine()}
      title={t('machinews.save_file.title')}>{t('machinews.save_file')}</button
    >
    <span class="ws-sep"></span>
    <label class="pick" title={t('machinews.pick.title')}>
      <span>{t('machinews.machine')}</span>
      <select
        value={activeProfileId ?? ''}
        onchange={(e) => onSelect((e.currentTarget as HTMLSelectElement).value)}
      >
        <option value="">{t('machinews.project_local')}</option>
        {#each profiles as p (p.id)}
          <option value={p.id}>{p.name}</option>
        {/each}
        {#if activeProfileMissing}
          <option value={activeProfileId}>{t('machinews.referenced_missing')}</option>
        {/if}
      </select>
    </label>
    <button type="button" class="ws-btn" onclick={newMachine} title={t('machinews.new.title')}
      >{t('machinews.new')}</button
    >
    {#if activeProfileId != null && !activeProfileMissing}
      <button
        type="button"
        class="ws-btn"
        onclick={duplicateMachine}
        title={t('machinews.duplicate.title')}>{t('machinews.duplicate')}</button
      >
      <button
        type="button"
        class="ws-btn"
        class:danger={confirmingDelete}
        onclick={deleteMachine}
        title={t('machinews.delete.title')}
        >{confirmingDelete ? t('machinews.delete.confirm') : t('machinews.delete')}</button
      >
    {/if}
    {#if activeProfileMissing}
      <span class="ws-note" title={t('machinews.missing.title')}>{t('machinews.missing')}</span>
    {/if}
  </div>
  <nav class="sub-tabs" aria-label={t('machinews.sections.aria')}>
    <button
      type="button"
      class="sub-tab"
      class:active={subTab === 'tooling'}
      onclick={() => (subTab = 'tooling')}
      title={t('machinews.tooling.title')}>{t('machinews.tooling')}</button
    >
    <button
      type="button"
      class="sub-tab"
      class:active={subTab === 'settings'}
      onclick={() => (subTab = 'settings')}
      title={t('machinews.settings.title')}>{t('machinews.settings')}</button
    >
  </nav>
  <div class="sub-panel" class:tab-hidden={subTab !== 'tooling'}>
    <MachineTooling />
  </div>
  <div class="sub-panel" class:tab-hidden={subTab !== 'settings'}>
    <MachineDialog embedded open={false} onClose={() => {}} />
  </div>
</div>

<style>
  .machine-ws {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }
  .ws-toolbar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.45rem 0.7rem;
    /* Same surface as the activated tab so the toolbar reads as part
       of the panel, not a separate strip. */
    background: var(--bg-panel);
    border-bottom: 1px solid var(--border);
    font-size: 0.8rem;
    flex-wrap: wrap;
  }
  .pick {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .pick select {
    max-width: 240px;
  }
  .ws-btn {
    background: var(--bg-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.2rem 0.55rem;
    font-size: 0.74rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .ws-btn.danger {
    border-color: var(--danger);
    color: var(--danger);
  }
  .ws-note {
    color: var(--text-muted);
    font-size: 0.74rem;
    font-style: italic;
  }
  .ws-sep {
    width: 1px;
    align-self: stretch;
    background: var(--border);
  }
  .sub-tabs {
    display: flex;
    gap: 0.15rem;
    padding: 0.2rem 0.7rem 0;
    background: var(--bg-elevated);
    border-bottom: 1px solid var(--border);
  }
  .sub-tab {
    background: none;
    border: 1px solid transparent;
    border-bottom: none;
    border-radius: 4px 4px 0 0;
    padding: 0.25rem 0.7rem;
    font-size: 0.78rem;
    color: var(--text-muted);
    cursor: pointer;
  }
  .sub-tab:hover {
    color: var(--text);
  }
  .sub-tab.active {
    background: var(--bg-panel);
    border-color: var(--border);
    color: var(--text-strong);
    margin-bottom: -1px;
  }
  .sub-panel {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }
  .tab-hidden {
    display: none !important;
  }
</style>
