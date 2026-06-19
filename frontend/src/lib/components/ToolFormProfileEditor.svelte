<script lang="ts">
  import type { FormProfileSample } from '../state/project.svelte';
  import { dovetailProfile, tslotProfile } from '../state/tool_form_profiles';

  // Form / profile cutter cross-section editor: the (z, r) sample table
  // plus the dovetail and T-slot generators that overwrite it. Owns the
  // transient generator inputs locally; the committed sample table lives
  // on the tool and is round-tripped through `rows` / `onChange`.
  let {
    rows,
    diameterMm,
    onChange,
  }: {
    rows: FormProfileSample[];
    diameterMm: number | undefined;
    onChange: (rows: FormProfileSample[]) => void;
  } = $props();

  const round3 = (v: number) => Math.round(v * 1000) / 1000;

  // Transient generator inputs. A dovetail bit is widest at the bottom
  // (z=0) and narrows upward; the T-slot is a wide disk then a narrow neck.
  let dovetail = $state({ diaMm: 12.7, angleDeg: 14, heightMm: 9.5 });
  let tslot = $state({ headDiaMm: 12.7, headThickMm: 3, neckDiaMm: 6, neckLenMm: 6 });

  function addRow() {
    const last = rows[rows.length - 1];
    const next: FormProfileSample = last
      ? { zMm: round3(last.zMm + 1), rMm: last.rMm }
      : { zMm: 0, rMm: round3((diameterMm ?? 0) / 2) };
    onChange([...rows, next]);
  }
  function updateRow(row: number, key: 'zMm' | 'rMm', v: number) {
    onChange(rows.map((s, r) => (r === row ? { ...s, [key]: v } : s)));
  }
  function removeRow(row: number) {
    onChange(rows.filter((_, r) => r !== row));
  }
</script>

<div class="holder-row pass-overrides">
  <span
    class="holder-label"
    title="Form / profile cutter cross-section (cove / ogee / dovetail / T-slot / custom). The (z, r) table — height above the tip vs radius — drives the simulator's cut shape. Needs ≥2 rows; otherwise the sim falls back to a tip→diameter taper. Use a preset below or edit rows directly."
    >Form profile</span
  >
</div>
<div class="holder-row dovetail-gen">
  <label>
    <span>Dovetail ⌀ (mm)</span>
    <input
      type="number"
      step="0.1"
      min="0"
      value={dovetail.diaMm}
      title="Widest cutting diameter (at the bottom face) of a dovetail bit."
      onchange={(e) =>
        (dovetail.diaMm = parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
    />
  </label>
  <label>
    <span>Angle (°)</span>
    <input
      type="number"
      step="1"
      min="0"
      max="89"
      value={dovetail.angleDeg}
      title="Flank angle from the tool axis. The radius narrows by tan(angle) per mm of rise. 7°–14° typical."
      onchange={(e) =>
        (dovetail.angleDeg = parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
    />
  </label>
  <label>
    <span>Cut height (mm)</span>
    <input
      type="number"
      step="0.5"
      min="0"
      value={dovetail.heightMm}
      title="Flute / cutting height — how tall the angled profile is from the bottom face up to the neck."
      onchange={(e) =>
        (dovetail.heightMm = parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
    />
  </label>
  <button
    type="button"
    class="profile-btn"
    title="Overwrite the sample table below with a 2-row dovetail profile generated from these inputs."
    onclick={() => onChange(dovetailProfile(dovetail))}>Generate dovetail</button
  >
</div>
<div class="holder-row dovetail-gen">
  <label>
    <span>T-slot head ⌀ (mm)</span>
    <input
      type="number"
      step="0.1"
      min="0"
      value={tslot.headDiaMm}
      title="Widest cutting-disk diameter at the tip of a T-slot / keyway cutter."
      onchange={(e) =>
        (tslot.headDiaMm = parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
    />
  </label>
  <label>
    <span>Head thick (mm)</span>
    <input
      type="number"
      step="0.5"
      min="0"
      value={tslot.headThickMm}
      title="Height of the cutting disk (how tall the wide undercut head is)."
      onchange={(e) =>
        (tslot.headThickMm = parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
    />
  </label>
  <label>
    <span>Neck ⌀ (mm)</span>
    <input
      type="number"
      step="0.1"
      min="0"
      value={tslot.neckDiaMm}
      title="Diameter of the narrow neck above the head — must be smaller than the head ⌀."
      onchange={(e) =>
        (tslot.neckDiaMm = parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
    />
  </label>
  <label>
    <span>Neck length (mm)</span>
    <input
      type="number"
      step="0.5"
      min="0"
      value={tslot.neckLenMm}
      title="Length of the narrow neck above the head, up to where the shank begins."
      onchange={(e) =>
        (tslot.neckLenMm = parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
    />
  </label>
  <button
    type="button"
    class="profile-btn"
    title="Overwrite the sample table below with a 4-row T-slot profile (wide disk → narrow neck) generated from these inputs."
    onclick={() => onChange(tslotProfile(tslot))}>Generate T-slot</button
  >
</div>
<div class="profile-table">
  <div class="profile-table-head">
    <span>z above tip (mm)</span>
    <span>radius (mm)</span>
    <span></span>
  </div>
  {#each rows as row, r (r)}
    <div class="profile-row">
      <input
        type="number"
        step="0.1"
        min="0"
        value={row.zMm}
        aria-label="z above tip (mm)"
        onchange={(e) =>
          updateRow(r, 'zMm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
      />
      <input
        type="number"
        step="0.1"
        min="0"
        value={row.rMm}
        aria-label="radius (mm)"
        onchange={(e) =>
          updateRow(r, 'rMm', parseFloat((e.currentTarget as HTMLInputElement).value) || 0)}
      />
      <button
        type="button"
        class="profile-btn del"
        title="Delete this sample row"
        onclick={() => removeRow(r)}>✕</button
      >
    </div>
  {/each}
  <div class="profile-actions">
    <button type="button" class="profile-btn" onclick={addRow}>+ Add row</button>
    {#if rows.length < 2}
      <span class="profile-hint"
        >Add at least 2 rows (tip → top) for the sim to carve the real profile.</span
      >
    {/if}
  </div>
</div>

<style>
  .holder-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.6rem;
  }
  .holder-row label {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    font-size: 0.7rem;
    color: var(--text-muted);
    min-width: 7rem;
  }
  .holder-row label span {
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .holder-row .holder-label {
    color: var(--text-muted);
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  input {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.32rem;
    font-size: 0.78rem;
    min-width: 0;
    width: 100%;
    box-sizing: border-box;
  }
  .profile-btn {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.2rem 0.5rem;
    font-size: 0.72rem;
    cursor: pointer;
    align-self: flex-end;
  }
  .profile-btn.del {
    padding: 0.2rem 0.4rem;
    color: var(--text-muted);
  }
  .profile-table {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin-top: 0.3rem;
  }
  .profile-table-head,
  .profile-row {
    display: grid;
    grid-template-columns: 8rem 8rem 2rem;
    gap: 0.4rem;
    align-items: center;
  }
  .profile-table-head span {
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
  }
  .profile-row input {
    width: 100%;
  }
  .profile-actions {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 0.2rem;
  }
  .profile-hint {
    font-size: 0.7rem;
    color: var(--text-muted);
  }
</style>
