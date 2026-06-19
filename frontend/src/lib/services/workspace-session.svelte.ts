/// Workspace/session wiring extracted from App.svelte (part 2):
/// startup reopen prompt, recent-project loads, desktop source-watch +
/// close-confirm wiring, and the window-level drag-and-drop import.
/// Same house pattern as `file_ops.ts` — plain functions over the
/// imported singletons — except for the two bits of reactive UI state
/// (`reopenPrompt`, `dragOver`), which live in the exported `sessionUi`
/// `$state` object so App's markup can render them. The `$effect`s that
/// subscribe to project changes stay in App.svelte (runes-in-component);
/// their bodies call into here.

import { project } from '../state/project.svelte';
import { workspace } from '../state/workspace.svelte';
import { confirmStore } from '../state/confirm.svelte';
import {
  isDesktop,
  wireSourceWatch as wireDesktopSourceWatch,
  wireCloseRequested,
  confirmClose,
} from '../state/desktop';
import {
  loadFromPath,
  loadProjectPath,
  loadFile,
  loadProjectFile,
  saveProject,
  confirmDiscardIfDirty,
} from './file_ops';
import { dragHasFiles } from '../state/app-menu';

/// Startup banner payload: when set, the user was previously editing a
/// project and we offer to reopen it. Styled in-app instead of a
/// native window.confirm so the first impression of the app isn't
/// an unstyled OS dialog (audit C10).
export interface ReopenPrompt {
  path: string;
  filename: string;
}

/// Reactive session-UI state App renders from. A `$state` object (not
/// bare reassignable exports — those can't cross module boundaries
/// reactively).
export const sessionUi = $state({
  reopenPrompt: null as ReopenPrompt | null,
  dragOver: false,
});

/// Path-based loads route on extension: a saved project restores ops +
/// settings; anything else re-imports as a drawing.
function isProjectFilePath(path: string): boolean {
  return /\.(ivac|vc)-project\.json$|\.json$/i.test(path);
}

/// Pull persisted workspace state at startup. After load completes,
/// prune any per-project / recent entries pointing at files that have
/// disappeared (desktop only — both `pruneMissingProjects` and the
/// reopen prompt self-guard via the workspace API, which returns null
/// for `last_project` on web because there's no filesystem path).
export async function loadWorkspaceAndMaybeReopen(): Promise<void> {
  try {
    await workspace.load();
  } catch {
    // ignore — defaults are fine.
  }
  // Await prune so a deleted-last-project entry has already been
  // dropped by the time we read `last_project` below. Without the
  // await, the reopen banner can appear for a path that prune is
  // about to remove — clicking Reopen then falls into an import-path
  // error toast for a file the user no longer has.
  try {
    await workspace.pruneMissingProjects();
  } catch {
    // ignore — best-effort cleanup.
  }
  if (isDesktop()) {
    const last = workspace.get().last_project;
    if (last) {
      const filename = last.split(/[\\/]/).pop() ?? last;
      sessionUi.reopenPrompt = { path: last, filename };
    }
  }
}

export async function acceptReopen(): Promise<void> {
  if (!sessionUi.reopenPrompt) return;
  const path = sessionUi.reopenPrompt.path;
  sessionUi.reopenPrompt = null;
  if (isProjectFilePath(path)) await loadProjectPath(path);
  else await loadFromPath(path);
  // If the project file already restored layer-visibility from
  // per-project workspace state, leave it alone — overwriting was
  // the previous behavior. If the user had every layer
  // hidden when they closed (rare but possible), expand to
  // all-visible so the user isn't staring at an empty canvas.
  if (project.transformedImport && project.data.visibleLayers.size === 0) {
    project.data.visibleLayers = new Set(project.transformedImport.layers.map((l) => l.name));
  }
}

export function dismissReopen(): void {
  sessionUi.reopenPrompt = null;
}

/// Body of App's auto-dismiss `$effect`: drop the reopen banner once a
/// project / drawing is loaded by any path (the user clicked Open,
/// dragged a file, or accepted the banner). The banner only makes sense
/// as a startup affordance. The synchronous `project` reads register the
/// effect's subscriptions.
export function dismissReopenOnceLoaded(): void {
  const hasImport = project.transformedImport;
  const hasPath = project.activeProjectPath;
  if (!hasImport && !hasPath) return;
  // Deferred so the prompt clear runs outside the effect scheduler.
  // Inline mutation would self-trigger the calling effect (it reads
  // `reopenPrompt` itself), which works but is fragile to refactor.
  // queueMicrotask matches the locale-sync effect in App.
  queueMicrotask(() => {
    if (sessionUi.reopenPrompt) sessionUi.reopenPrompt = null;
  });
}

/// Body of App's persist `$effect`: write per-project workspace state
/// when the user adjusts visible layers / selected op / playhead. The
/// `void` reads register the effect's subscriptions.
export function persistPerProjectStateOnChange(): void {
  void project.data.visibleLayers;
  void project.sel.selectedOpId;
  void project.playhead;
  if (project.activeProjectPath) {
    project.persistPerProjectState();
  }
}

/// Mirror the project's machine + tools into the workspace machine
/// profile it references — the "tools belong to machines" write-back.
/// Called from an App `$effect`; the deep `$state.snapshot` reads
/// register subscriptions on every nested machine / tool field, so any
/// edit (including undo/redo, which bypasses the project methods)
/// re-runs it. The store no-ops when nothing actually changed or the
/// profile doesn't exist here. Deferred off the effect flush like
/// `persistPerProjectState` — `workspace.version` is `$state` and must
/// not be bumped synchronously inside an effect body.
export function mirrorMachineProfileOnChange(): void {
  const id = project.data.machineProfileId;
  const machine = $state.snapshot(project.data.machine);
  const tools = $state.snapshot(project.data.tools);
  if (id == null) return;
  queueMicrotask(() => {
    try {
      workspace.mirrorMachineProfile(id, machine, tools);
    } catch (e) {
      console.warn('mirror machine profile:', e);
    }
  });
}

/// Load a Recent-projects entry. Dirty-check once here so we don't
/// double-prompt when loadFromPath / loadProjectPath also vet it.
/// `openFile` / `openProject` do their own check; the path variants
/// don't, because the OS file-association launch + reopen banner cases
/// intentionally skip the prompt. Routes through the shared styled
/// ConfirmPrompt (same dialog as Open / Quit) rather than a native
/// window.confirm.
export async function openRecentProject(path: string): Promise<void> {
  if (!(await confirmDiscardIfDirty('load the recent project'))) return;
  if (isProjectFilePath(path)) await loadProjectPath(path);
  else await loadFromPath(path);
}

/// Subscribe to backend `source-file-changed` events emitted by the
/// project watcher. Stored so App's onMount cleanup can disable the
/// watch on HMR / component-tree teardown — without it the listener
/// leaks every time App.svelte is reloaded during dev. Implementation
/// lives in `lib/state/desktop.ts`; this trampoline preserves the
/// HMR-safe cleanup binding (`unwireSession` reads the current value).
let unlistenSourceWatch: (() => void) | null = null;
export async function wireSourceWatch(): Promise<void> {
  unlistenSourceWatch = await wireDesktopSourceWatch();
}

/// Desktop close interception. Always confirm — accidental
/// closes lose work even on a "clean" project (camera, panel sizes,
/// in-progress text not yet committed via Add). The double-click
/// escape hatch in the Tauri backend covers the case where the user
/// really wants out fast.
let unlistenCloseRequested: (() => void) | null = null;
export async function wireCloseConfirm(): Promise<void> {
  unlistenCloseRequested = await wireCloseRequested(async () => {
    if (project.hasUnsavedWork) {
      // Three-way so a misclick on the close button doesn't force a
      // choice between losing work and re-opening the app.
      const choice = await confirmStore.askChoice({
        title: 'Quit ivaCAM?',
        body: 'You have unsaved changes. Save before you quit?',
        primaryLabel: 'Save & quit',
        extraLabel: 'Discard & quit',
        cancelLabel: 'Keep editing',
        danger: false,
        extraDanger: true,
      });
      if (choice === 'cancel') return;
      if (choice === 'primary') {
        await saveProject();
        // Save cancelled (native dialog dismissed) or failed →
        // hasUnsavedWork stays set; don't quit and lose the kept work.
        if (project.hasUnsavedWork) return;
      }
      void confirmClose();
      return;
    }
    // Clean project: still confirm — an accidental close loses camera,
    // panel sizes, and in-progress text not yet committed via Add.
    const ok = await confirmStore.ask({
      title: 'Quit ivaCAM?',
      body: 'Are you sure you want to quit?',
      primaryLabel: 'Quit',
      cancelLabel: 'Cancel',
      danger: false,
    });
    if (ok) void confirmClose();
  });
}

/// Android system-back exit (ivac-h0ai). Unlike the deliberate Exit menu
/// item — which only prompts when there's unsaved work (confirmDiscardIfDirty)
/// — an accidental edge-swipe on the first screen should always confirm
/// before quitting. Mirrors the desktop close-confirm three-way above, but
/// quits the process directly (no Tauri window to close on mobile). Resolves
/// without exiting if the user cancels or a save fails.
export async function confirmExitApp(): Promise<void> {
  if (project.hasUnsavedWork) {
    const choice = await confirmStore.askChoice({
      title: 'Quit ivaCAM?',
      body: 'You have unsaved changes. Save before you quit?',
      primaryLabel: 'Save & quit',
      extraLabel: 'Discard & quit',
      cancelLabel: 'Keep editing',
      danger: false,
      extraDanger: true,
    });
    if (choice === 'cancel') return;
    if (choice === 'primary') {
      await saveProject();
      // Save cancelled or failed → keep the app open rather than lose work.
      if (project.hasUnsavedWork) return;
    }
  } else {
    const ok = await confirmStore.ask({
      title: 'Quit ivaCAM?',
      body: 'Are you sure you want to quit?',
      primaryLabel: 'Quit',
      cancelLabel: 'Cancel',
      danger: false,
    });
    if (!ok) return;
  }
  try {
    const { exit } = await import('@tauri-apps/plugin-process');
    await exit(0);
  } catch (e) {
    console.warn('exit failed:', e);
  }
}

/// onMount-cleanup counterpart to the two wire* calls above.
export function unwireSession(): void {
  unlistenSourceWatch?.();
  unlistenSourceWatch = null;
  unlistenCloseRequested?.();
  unlistenCloseRequested = null;
}

// Window-level drag-and-drop import. Accept .dxf / .svg
// (loadFile) and .ivac-project.json / .json (loadProjectFile). The
// overlay only paints while a drag with a `Files` payload is over
// the window; we count enter / leave to avoid flicker when the
// cursor crosses child elements.
let dragDepth = 0;
export function onDragEnter(e: DragEvent): void {
  if (!dragHasFiles(e)) return;
  dragDepth += 1;
  sessionUi.dragOver = true;
}
export function onDragOver(e: DragEvent): void {
  if (!dragHasFiles(e)) return;
  e.preventDefault();
  if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
}
export function onDragLeave(e: DragEvent): void {
  if (!dragHasFiles(e)) return;
  dragDepth = Math.max(0, dragDepth - 1);
  if (dragDepth === 0) sessionUi.dragOver = false;
}
export async function onDrop(e: DragEvent): Promise<void> {
  if (!dragHasFiles(e)) return;
  e.preventDefault();
  sessionUi.dragOver = false;
  dragDepth = 0;
  const file = e.dataTransfer?.files?.[0];
  if (!file) return;
  const name = file.name.toLowerCase();
  // .ivac-project.json / *-project.json / bare .json are treated as a
  // project; anything else (.dxf / .svg / …) as a drawing. Both are
  // REPLACE loads, so gate on unsaved changes like the menu paths do —
  // a drop is just as destructive as File ▸ Open.
  const isProject =
    name.endsWith('.ivac-project.json') || name.endsWith('-project.json') || name.endsWith('.json');
  if (
    !(await confirmDiscardIfDirty(
      isProject ? 'open the dropped project' : 'open the dropped drawing',
    ))
  )
    return;
  if (isProject) {
    // Bare .json is also routed here; loadProjectFile validates the
    // kind: 'ivac-project' field and rejects otherwise.
    await loadProjectFile(file);
  } else {
    await loadFile(file);
  }
}
