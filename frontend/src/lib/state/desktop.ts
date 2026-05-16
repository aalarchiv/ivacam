// Desktop-only capability surface. Components import from here instead
// of branching on `isTauri()` directly — that keeps transport awareness
// pinned to the state layer and lets `grep -rn 'isTauri' src/lib` stay
// short (audit dqnd).
//
// Functions in this module **self-guard**: callers can invoke them
// unconditionally on either transport. The `isDesktop` flag is only
// needed when the UI itself has to render conditionally (e.g. omit a
// menu entry).

import { isTauri } from '../api/env';
import { project } from './project.svelte';

/// True when running inside the Tauri shell. Exposed for the few UI
/// bits that *must* render conditionally (e.g. a menu entry that has
/// no web equivalent). All other transport branches live inside the
/// functions below.
export function isDesktop(): boolean {
  return isTauri();
}

/// Install the desktop source-file watcher: when an imported file
/// changes on disk, either auto-reimport or surface a stale-source
/// toast (depending on the user's setting). Returns the cleanup
/// callback (no-op on web). Safe to call unconditionally.
export async function wireSourceWatch(): Promise<() => void> {
  if (!isTauri()) return () => {};
  try {
    const { onSourceFileChanged } = await import('../api/tauri');
    return await onSourceFileChanged(async ({ path }) => {
      if (path !== project.lastImportPath) return;
      if (project.settings.autoReloadSources) {
        await project.reimportFromPath(path);
      } else {
        project.sourceFileStaleNotice = { path, auto_reload: false };
      }
    });
  } catch (e) {
    console.warn('source watch wiring failed:', e);
    return () => {};
  }
}

/// Install the desktop file-association listener: when the OS invokes
/// the app via "Open with…", the Tauri main process forwards the path
/// here. Returns a cleanup callback (no-op on web).
export async function wireFileAssociationOpen(
  onPath: (path: string) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  try {
    const { listen } = await import('@tauri-apps/api/event');
    const unlisten = await listen<string>('app:open_path', (event) => {
      if (typeof event.payload === 'string') onPath(event.payload);
    });
    return unlisten;
  } catch (e) {
    console.warn('file-association listener wiring failed:', e);
    return () => {};
  }
}

/// Trigger the auto-updater flow: check for a new release, download,
/// install, relaunch. No-op on web (no installer to update). Errors
/// surface via `project.setError`.
export async function runUpdateCheck(): Promise<void> {
  if (!isTauri()) return;
  try {
    const { check } = await import('@tauri-apps/plugin-updater');
    const update = await check();
    if (!update) return;
    await update.downloadAndInstall();
    const { relaunch } = await import('@tauri-apps/plugin-process');
    await relaunch();
  } catch (e) {
    project.setError(`update: ${e instanceof Error ? e.message : String(e)}`);
  }
}
