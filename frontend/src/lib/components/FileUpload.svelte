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
  /// the unsaved-changes guard). This module intentionally adds no
  /// window 'drop' listener — a duplicate one would double-import every
  /// dropped file.

  import { onMount } from 'svelte';
  import { wireFileAssociationOpen } from '../state/desktop';
  import {
    addDrawingFile,
    loadFromPath,
    loadProjectFile,
    loadSample,
    loadSampleWithGenerate,
    handleOpenPick,
  } from '../services/file_ops';
  import ErrorToast from './ErrorToast.svelte';

  let inputEl: HTMLInputElement;
  let projectInput: HTMLInputElement;
  let openInput: HTMLInputElement;

  /// Drawing-only input — fired exclusively by the layer panel's "+ Add ▸
  /// Open drawing file", so it ADDS (appends a layer) rather than replaces.
  function onPick(e: Event) {
    const target = e.target as HTMLInputElement;
    const f = target.files?.[0];
    if (f) void addDrawingFile(f);
    target.value = '';
  }
  function onProjectPick(e: Event) {
    const target = e.target as HTMLInputElement;
    const f = target.files?.[0];
    if (f) void loadProjectFile(f);
    target.value = '';
  }
  /// Unified "Open" (7jug.14): hand the picked file to file_ops, which
  /// routes projects (replace) vs drawings (New/Add prompt) by extension.
  function onOpenPick(e: Event) {
    const target = e.target as HTMLInputElement;
    const f = target.files?.[0];
    if (f) void handleOpenPick(f);
    target.value = '';
  }

  /// Register the hidden inputs globally so file_ops.openFile /
  /// openProject can fire them when running in a browser (no native
  /// picker available there). Cleared on unmount.
  function exposeInputs() {
    (window as unknown as Record<string, unknown>).__ivacFileInput = inputEl;
    (window as unknown as Record<string, unknown>).__ivacProjectInput = projectInput;
    (window as unknown as Record<string, unknown>).__ivacOpenInput = openInput;
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
      delete (window as unknown as Record<string, unknown>).__ivacOpenInput;
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
<input
  bind:this={openInput}
  type="file"
  accept=".dxf,.svg,.ivac-project.json,.vc-project.json,.json"
  onchange={onOpenPick}
  hidden
/>

<ErrorToast />
