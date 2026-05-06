<script lang="ts">
  /// Machine settings dialog. Project-scoped CNC config — units, mode,
  /// fast move height, comments / arcs / toolchange flags. The op-driven
  /// pipeline reads these from the Project; until that wires up the
  /// values are also mirrored into setup.machine via SetupPanel so the
  /// legacy Generate path keeps working.
  import { project, type MachineSettings } from '../state/project.svelte';

  interface Props {
    open: boolean;
    onClose: () => void;
  }
  let { open, onClose }: Props = $props();

  // Local working copy so the user can cancel without committing.
  let draft = $state<MachineSettings>({ ...project.machine });

  $effect(() => {
    if (open) draft = { ...project.machine };
  });

  function commit() {
    project.machine = { ...draft };
    onClose();
  }

  function close() {
    onClose();
  }
</script>

{#if open}
  <div class="overlay" role="dialog" aria-modal="true" aria-labelledby="machine-title">
    <div class="modal">
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
          <input type="number" bind:value={draft.fastMoveZ} step="0.1" />
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={draft.comments} />
          Emit comments in g-code
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={draft.arcs} />
          Emit G2 / G3 arc moves
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={draft.supportsToolchange} />
          Machine supports tool changes (M6)
        </label>
      </div>

      <footer>
        <button class="secondary" onclick={close}>Cancel</button>
        <button class="primary" onclick={commit}>OK</button>
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
    width: min(440px, 95vw);
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
  input[type='number'],
  select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.2rem 0.4rem;
    font-size: 0.8rem;
  }
  input[type='checkbox'] {
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
  .secondary {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 0.3rem 0.8rem;
    border-radius: 3px;
    cursor: pointer;
  }
</style>
