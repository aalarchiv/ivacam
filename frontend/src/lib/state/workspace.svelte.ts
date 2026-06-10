// Per-installation workspace persistence: 3D camera, panel sizes, last
// project path, recent project list, per-project view state. Distinct
// from ProjectState (per-project) and AppSettings (per-installation
// prefs in `ivac.settings`). Workspace state is explicitly NOT part of
// project undo — opening a project doesn't yank you back to yesterday's
// camera angle once you've nudged it.
//
// Browser builds keep the JSON under `localStorage['ivac-workspace']`.
// Tauri builds round-trip via `read_workspace_file` /
// `write_workspace_file` commands so the file is human-editable at
// `$APP_DATA_DIR/ivacam/workspace.json`.
//
// The actual store class is plain TS (in `./workspace.ts`) so vitest can
// import it without the Svelte rune compiler. This file just exposes a
// reactive wrapper for components: `workspace.version` ticks on every
// mutation so $effect subscribers re-run.

import { WorkspaceStore } from './workspace';
export {
  DEFAULT_WORKSPACE,
  WorkspaceStore,
  type CameraState,
  type MachineProfile,
  type PanelLayout,
  type PerProjectState,
  type RecentProject,
  type WorkspaceState,
  type WorkspaceTransport,
} from './workspace';

/// localStorage key the underlying `WorkspaceStore` uses. Keep in
/// sync with `STORAGE_KEY` in `./workspace.ts`. We listen to the
/// `storage` event so a second tab's writes don't get silently
/// clobbered by the first tab's debounced flush.
const STORAGE_KEY = 'ivac-workspace';

class ReactiveWorkspaceStore extends WorkspaceStore {
  /// Bumped whenever the underlying state changes. Components that need
  /// to react to workspace changes use `void workspace.version` inside
  /// a `$effect` body to subscribe.
  version = $state(0);

  constructor() {
    super();
    this.subscribe(() => {
      this.version += 1;
    });
    // Cross-tab sync: when another tab writes the workspace key,
    // re-read so our in-memory state reflects theirs. Without this,
    // two tabs open on the same project race on every panel resize /
    // recent-files add and the last debounced flush wins blindly.
    if (typeof window !== 'undefined') {
      window.addEventListener('storage', (e) => {
        if (e.key !== STORAGE_KEY || e.newValue == null) return;
        void this.load();
      });
    }
  }
}

export const workspace = new ReactiveWorkspaceStore();
