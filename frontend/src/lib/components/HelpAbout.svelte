<script lang="ts">
  /// Help/About tab — help (keyboard & mouse shortcuts) on the left,
  /// About (version, license, acknowledgements) on the right. Replaces
  /// the two modals; both render their existing content in embedded
  /// mode.
  import ShortcutHelp from './ShortcutHelp.svelte';
  import AboutDialog from './AboutDialog.svelte';
</script>

<div class="help-about">
  <div class="col">
    <ShortcutHelp embedded onClose={() => {}} />
  </div>
  <div class="col">
    <AboutDialog embedded onClose={() => {}} />
  </div>
</div>

<style>
  .help-about {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1rem;
    padding: 0.7rem;
    flex: 1;
    min-height: 0;
    overflow: auto;
    align-items: start;
  }
  .col {
    min-width: 0;
  }
  .col :global(.embedded-col) {
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-elevated);
    padding: 0.6rem;
    overflow: auto;
  }
  @media (max-width: 900px) {
    .help-about {
      grid-template-columns: 1fr;
    }
  }
</style>
