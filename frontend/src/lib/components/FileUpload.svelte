<script lang="ts">
  /// Hidden file inputs + URL-param boot loader + file-association wiring.
  ///
  /// Now headless: the user-visible Open / Save / Sample buttons live in
  /// the App.svelte toolbar and call `services/file_ops.ts` directly. This
  /// component only owns the things that have to stay alive in the DOM:
  ///
  ///   * The two `<input type=file hidden>` elements that `file_ops` clicks
  ///     when running in a browser (no native picker available).
  ///   * The URL-param boot loader (?sample=…&gen=…) and the
  ///     `app:open_path` Tauri event listener for OS file-association
  ///     launches.
  ///
  /// Drag-and-drop lives entirely in App.svelte (richer drag visual +
  /// the unsaved-changes guard). A duplicate window 'drop' listener here
  /// used to double-import every dropped file — removed (944t).

  import { onMount } from 'svelte';
  import { wireFileAssociationOpen } from '../state/desktop';
  import {
    loadFile,
    loadFromPath,
    loadProjectFile,
    loadSample,
    loadSampleWithGenerate,
  } from '../services/file_ops';
  import ErrorToast from './ErrorToast.svelte';

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

  /// Register the hidden inputs globally so file_ops.openFile /
  /// openProject can fire them when running in a browser (no native
  /// picker available there). Cleared on unmount.
  function exposeInputs() {
    (window as unknown as Record<string, unknown>).__ivacFileInput = inputEl;
    (window as unknown as Record<string, unknown>).__ivacProjectInput = projectInput;
  }

  onMount(() => {
    exposeInputs();
    // OS file-association launches forward the path here. Self-guards on
    // web; the returned unlisten is a no-op there.
    let unlistenFileAssoc: (() => void) | null = null;
    void wireFileAssociationOpen((path) => void loadFromPath(path)).then((u) => {
      unlistenFileAssoc = u;
    });
    const params = new URLSearchParams(window.location.search);
    const sample = params.get('sample');
    const gen = params.get('gen');
    if (sample && gen) {
      void loadSampleWithGenerate(`/samples/${sample}.json`, `/samples/${gen}.json`);
    } else if (sample) {
      void loadSample(`/samples/${sample}.json`);
    }
    return () => {
      unlistenFileAssoc?.();
      delete (window as unknown as Record<string, unknown>).__ivacFileInput;
      delete (window as unknown as Record<string, unknown>).__ivacProjectInput;
    };
  });
</script>

<input bind:this={inputEl} type="file" accept=".dxf,.svg" onchange={onPick} hidden />
<input
  bind:this={projectInput}
  type="file"
  accept=".ivac-project.json,.vc-project.json,.json"
  onchange={onProjectPick}
  hidden
/>

<ErrorToast />
