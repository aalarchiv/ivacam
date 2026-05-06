<script lang="ts">
  // Simplified bar: post-processor + Generate + Download. The full setup
  // tree lives in SetupPanel and feeds project.setup.

  import { defaultClient } from '../api/http';
  import { isTauri } from '../api/env';
  import { project } from '../state/project.svelte';
  import { buildProject, type GenerateRequestWithProject } from '../api/build-project';
  import { _ } from 'svelte-i18n';

  const client = defaultClient();
  let post: 'linuxcnc' | 'grbl' | 'hpgl' = $state('linuxcnc');
  let progressMsg = $state<string>('');
  let progressFrac = $state<number>(0);

  async function run() {
    if (!project.imported) return;
    project.generating = true;
    project.error = null;
    progressMsg = '';
    progressFrac = 0;
    try {
      const opProject = buildProject(project);
      if (!opProject) {
        project.setError('Add at least one operation to generate gcode.');
        return;
      }
      const req: GenerateRequestWithProject = {
        post_processor: post,
        project: opProject,
      };
      const r = client.generateStream
        ? await client.generateStream(req, (ev) => {
            progressMsg = ev.message;
            progressFrac = ev.fraction;
          })
        : await client.generate(req);
      project.setGenerated(r);
    } catch (e) {
      project.setError(e instanceof Error ? e.message : String(e));
    } finally {
      project.generating = false;
      progressMsg = '';
      progressFrac = 0;
    }
  }

  async function downloadGcode() {
    if (!project.generated) return;
    const base = project.imported?.filename?.replace(/\.[^.]+$/, '') ?? 'output';
    const ext = post === 'hpgl' ? 'plt' : 'ngc';
    const filename = `${base}.${ext}`;
    if (isTauri()) {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const { writeTextFile } = await import('@tauri-apps/plugin-fs');
      const path = await save({
        defaultPath: filename,
        filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
      });
      if (typeof path === 'string') {
        try {
          await writeTextFile(path, project.generated.gcode);
        } catch (e) {
          project.setError(`save: ${e instanceof Error ? e.message : String(e)}`);
        }
      }
      return;
    }
    const blob = new Blob([project.generated.gcode], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
  }
</script>

<div class="bar">
  <span class="title">{$_('generate.title')}</span>
  <label
    >{$_('generate.post')}
    <select bind:value={post}>
      <option value="linuxcnc">LinuxCNC</option>
      <option value="grbl">GRBL</option>
      <option value="hpgl">HPGL</option>
    </select>
  </label>
  <button onclick={run} disabled={!project.imported || project.generating}>
    {project.generating ? $_('generate.running') : $_('generate.run')}
  </button>
  {#if project.generating}
    <div
      class="progress"
      role="progressbar"
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={Math.round(progressFrac * 100)}
      title={progressMsg}
    >
      <div class="bar-fill" style="width: {Math.round(progressFrac * 100)}%"></div>
      <span class="progress-text">{progressMsg || $_('generate.starting')}</span>
    </div>
  {/if}
  {#if project.generated}
    <button onclick={downloadGcode} class="download">
      {post === 'hpgl' ? $_('generate.download_plt') : $_('generate.download_ngc')}
    </button>
    <span class="stats">
      {$_('generate.stats', {
        values: {
          objects: project.generated.stats.object_count,
          offsets: project.generated.stats.offset_count,
          moves: project.generated.toolpath.length,
        },
      })}
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
  .progress {
    position: relative;
    flex: 1;
    height: 1.2rem;
    min-width: 8rem;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: 3px;
    overflow: hidden;
  }
  .bar-fill {
    height: 100%;
    background: var(--accent);
    transition: width 120ms ease-out;
  }
  .progress-text {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.7rem;
    color: var(--text-strong);
    pointer-events: none;
    text-shadow: 0 0 4px var(--bg-app);
  }
</style>
