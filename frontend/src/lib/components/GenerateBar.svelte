<script lang="ts">
  // Simplified bar: post-processor + Generate + Download. The full setup
  // tree lives in SetupPanel and feeds project.setup.

  import { defaultClient } from '../api/http';
  import { project } from '../state/project.svelte';
  import type { GenerateRequest } from '../api/types';

  const client = defaultClient();
  let post: 'linuxcnc' | 'grbl' | 'hpgl' = $state('linuxcnc');

  async function run() {
    if (!project.imported) return;
    project.generating = true;
    project.error = null;
    try {
      // Auto-enable tabs in the setup when the user has placed any — the
      // backend gates emission on setup.tabs.active.
      const tabsCount = Object.values(project.tabs).reduce((n, l) => n + l.length, 0);
      const setup = (project.setup as Record<string, unknown>) ?? {};
      const setupWithTabs = tabsCount > 0
        ? { ...setup, tabs: { ...(setup.tabs ?? {}), active: true } }
        : setup;
      const req: GenerateRequest & { tabs?: Record<number, { x: number; y: number }[]> } = {
        segments: project.imported.segments,
        post_processor: post,
        setup: setupWithTabs as GenerateRequest['setup'],
        // Tab placements keyed by imported-segment index.
        tabs: project.tabs,
      };
      const r = await client.generate(req);
      project.setGenerated(r);
    } catch (e) {
      project.setError(e instanceof Error ? e.message : String(e));
    } finally {
      project.generating = false;
    }
  }

  function downloadGcode() {
    if (!project.generated) return;
    const blob = new Blob([project.generated.gcode], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    const base = project.imported?.filename?.replace(/\.[^.]+$/, '') ?? 'output';
    a.download = `${base}.${post === 'hpgl' ? 'plt' : 'ngc'}`;
    a.click();
    URL.revokeObjectURL(url);
  }
</script>

<div class="bar">
  <span class="title">Generate:</span>
  <label
    >post
    <select bind:value={post}>
      <option value="linuxcnc">LinuxCNC</option>
      <option value="grbl">GRBL</option>
      <option value="hpgl">HPGL</option>
    </select>
  </label>
  <button onclick={run} disabled={!project.imported || project.generating}>
    {project.generating ? 'Generating…' : 'Generate'}
  </button>
  {#if project.generated}
    <button onclick={downloadGcode} class="download">
      Download {post === 'hpgl' ? '.plt' : '.ngc'}
    </button>
    <span class="stats">
      {project.generated.stats.object_count} obj · {project.generated.stats.offset_count} offsets ·
      {project.generated.toolpath.length} moves
    </span>
  {/if}
</div>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 0.7rem;
    padding: 0.4rem 0.9rem;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--border);
    color: var(--text);
    flex-wrap: wrap;
    font-size: 0.78rem;
  }
  .title {
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-size: 0.7rem;
  }
  label {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
  }
  select {
    background: var(--bg-input);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 0.18rem 0.3rem;
    font-size: 0.78rem;
  }
  button {
    background: var(--accent);
    color: white;
    border: none;
    padding: 0.3rem 0.7rem;
    border-radius: 4px;
    font-size: 0.78rem;
    cursor: pointer;
  }
  button.download {
    background: var(--success-bg);
  }
  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .stats {
    color: var(--success);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
</style>
