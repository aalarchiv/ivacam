<script lang="ts">
  /// Drag-and-drop overlay + hidden file inputs + URL-param boot loader.
  ///
  /// Now headless: the user-visible Open / Save / Sample buttons live in
  /// the App.svelte toolbar and call `state/file_ops.ts` directly. This
  /// component only owns the things that have to stay alive in the DOM:
  ///
  ///   * The two `<input type=file hidden>` elements that `file_ops` clicks
  ///     when running in a browser (no native picker available).
  ///   * The full-window drag-and-drop catcher.
  ///   * The URL-param boot loader (?sample=…&gen=…) and the
  ///     `app:open_path` Tauri event listener for OS file-association
  ///     launches.

  import { onMount } from 'svelte';
  import { isTauri } from '../api/env';
  import { project } from '../state/project.svelte';
  import {
    importDroppedFile,
    loadFile,
    loadFromPath,
    loadProjectFile,
    loadSample,
    loadSampleWithGenerate,
  } from '../state/file_ops';
  import ErrorToast from './ErrorToast.svelte';

  let dragOver = $state(false);
  let inputEl: HTMLInputElement;
  let projectInput: HTMLInputElement;

  function onPick(e: Event) {
    const target = e.target as HTMLInputElement;
    const f = target.files?.[0];
    if (f) void loadFile(f);
    target.value = '';
  }
  function onProjectPick(e: Event) {
    const target = e.target as HTMLInputElement;
    const f = target.files?.[0];
    if (f) void loadProjectFile(f);
    target.value = '';
  }

  function onWindowDragOver(e: DragEvent) {
    if (!e.dataTransfer?.types.includes('Files')) return;
    e.preventDefault();
    dragOver = true;
  }
  function onWindowDragLeave(e: DragEvent) {
    // Only count it as "left" when the cursor leaves the window itself,
    // not when the drag crosses between child elements (the browser
    // fires dragleave on every nested element transition).
    if (e.relatedTarget == null) dragOver = false;
  }
  function onWindowDrop(e: DragEvent) {
    if (!e.dataTransfer?.types.includes('Files')) return;
    e.preventDefault();
    dragOver = false;
    const file = e.dataTransfer.files[0];
    if (file) void importDroppedFile(file);
  }

  /// Register the hidden inputs globally so file_ops.openFile /
  /// openProject can fire them when running in a browser (no native
  /// picker available there). Cleared on unmount.
  function exposeInputs() {
    (window as unknown as Record<string, unknown>).__wiacFileInput = inputEl;
    (window as unknown as Record<string, unknown>).__wiacProjectInput = projectInput;
  }

  onMount(() => {
    exposeInputs();
    window.addEventListener('dragover', onWindowDragOver);
    window.addEventListener('dragleave', onWindowDragLeave);
    window.addEventListener('drop', onWindowDrop);
    if (isTauri()) {
      void (async () => {
        const { listen } = await import('@tauri-apps/api/event');
        // OS file-association launches: main.rs forwards the path here.
        void listen<string>('app:open_path', (event) => {
          if (typeof event.payload === 'string') void loadFromPath(event.payload);
        });
      })();
    }
    const params = new URLSearchParams(window.location.search);
    const sample = params.get('sample');
    const gen = params.get('gen');
    if (sample && gen) {
      void loadSampleWithGenerate(`/samples/${sample}.json`, `/samples/${gen}.json`);
    } else if (sample) {
      void loadSample(`/samples/${sample}.json`);
    }
    return () => {
      window.removeEventListener('dragover', onWindowDragOver);
      window.removeEventListener('dragleave', onWindowDragLeave);
      window.removeEventListener('drop', onWindowDrop);
      delete (window as unknown as Record<string, unknown>).__wiacFileInput;
      delete (window as unknown as Record<string, unknown>).__wiacProjectInput;
    };
  });
</script>

<input bind:this={inputEl} type="file" accept=".dxf,.svg" onchange={onPick} hidden />
<input
  bind:this={projectInput}
  type="file"
  accept=".wiac-project.json,.vc-project.json,.json"
  onchange={onProjectPick}
  hidden
/>

<div class="drop-catcher" class:drag-over={dragOver} aria-hidden="true"></div>

{#if project.error}
  <ErrorToast error={project.error} />
{/if}

<style>
  /* The drag indicator overlay is purely visual — window-level event
     listeners do the real work, so pointer-events stays off and the
     overlay never intercepts clicks. */
  .drop-catcher {
    position: fixed;
    inset: 0;
    pointer-events: none;
    z-index: var(--z-drop-catcher);
    background: transparent;
    transition: background 80ms;
  }
  .drop-catcher.drag-over {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    outline: 2px dashed var(--accent);
    outline-offset: -8px;
  }
</style>
