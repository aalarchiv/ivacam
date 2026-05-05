<script lang="ts">
  import { defaultClient } from '../api/http';
  import { project } from '../state/project.svelte';
  import type { GenerateRequest } from '../api/types';

  const client = defaultClient();
  let post: 'linuxcnc' | 'grbl' | 'hpgl' = $state('linuxcnc');
  let diameter = $state(3);
  let depth = $state(-2);
  let step = $state(-1);
  let mode: 'outside' | 'inside' | 'on' | 'none' = $state('outside');

  async function run() {
    if (!project.imported) return;
    project.generating = true;
    project.error = null;
    try {
      const req: GenerateRequest = {
        segments: project.imported.segments,
        post_processor: post,
        setup: {
          machine: {
            unit: 'mm',
            mode: 'mill',
            comments: true,
            arcs: true,
            supports_toolchange: false,
          },
          tool: {
            number: 1,
            diameter,
            speed: 18000,
            pause: 1,
            mist: false,
            flood: false,
            dragoff: null,
            rate_v: 100,
            rate_h: 800,
          },
          mill: {
            active: true,
            depth,
            start_depth: 0,
            step,
            fast_move_z: 5,
            helix_mode: false,
            reverse: false,
            objectorder: 'nearest',
            offset: mode,
          },
          pockets: {
            active: false,
            islands: false,
            zigzag: false,
            insideout: false,
            nocontour: false,
          },
          tabs: { active: false, width: 10, height: 1, tab_type: 'rectangle' },
          leads: { in: 'off', out: 'off', in_lenght: 5, out_lenght: 5 },
        },
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
    const base = project.imported?.filename?.replace(/\.[^.]+$/, '') || 'output';
    a.download = `${base}.${post === 'hpgl' ? 'plt' : 'ngc'}`;
    a.click();
    URL.revokeObjectURL(url);
  }
</script>

<div class="bar">
  <span class="title">Generate:</span>
  <label
    >diameter
    <input type="number" bind:value={diameter} step="0.1" min="0.1" />
    mm</label
  >
  <label
    >depth
    <input type="number" bind:value={depth} step="0.1" />
    mm</label
  >
  <label
    >step
    <input type="number" bind:value={step} step="0.1" />
    mm</label
  >
  <label
    >mode
    <select bind:value={mode}>
      <option value="outside">outside</option>
      <option value="inside">inside</option>
      <option value="on">on</option>
      <option value="none">none</option>
    </select>
  </label>
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
    background: #161616;
    border-bottom: 1px solid #2b2b2b;
    color: #d6d6d6;
    flex-wrap: wrap;
    font-size: 0.78rem;
  }
  .title {
    color: #888;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-size: 0.7rem;
  }
  label {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
  }
  input[type='number'] {
    background: #0d0d0d;
    color: #e6e6e6;
    border: 1px solid #2b2b2b;
    border-radius: 3px;
    padding: 0.18rem 0.3rem;
    width: 4.5rem;
    font-size: 0.78rem;
  }
  select {
    background: #0d0d0d;
    color: #e6e6e6;
    border: 1px solid #2b2b2b;
    border-radius: 3px;
    padding: 0.18rem 0.3rem;
    font-size: 0.78rem;
  }
  button {
    background: #2d6cdf;
    color: white;
    border: none;
    padding: 0.3rem 0.7rem;
    border-radius: 4px;
    font-size: 0.78rem;
    cursor: pointer;
  }
  button.download {
    background: #2d8c4d;
  }
  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .stats {
    color: #6ec068;
  }
</style>
