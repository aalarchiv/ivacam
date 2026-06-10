// Project-file / workspace lifecycle extracted from the ProjectState
// god root. Save/load of .ivac-project files, the
// project-boundary reset, and the per-project workspace view state all
// live here; ProjectState keeps one-line delegators so the
// component-facing `project.*` API is unchanged.

import {
  defaultWorkOffset,
  isDefaultWorkOffset,
  migrateMachineSettings,
  type ProjectFile,
} from './project-types';
import { migrateLegacyToolTerms } from './tool-migration';
import { resetPreviewCache } from './text_preview.svelte';
import { workspace } from './workspace.svelte';
import { refreshSourceWatch, setImported } from './import-ops';
import type { ProjectState } from './project.svelte';

/// Reset every project-scoped field to its empty / default state.
/// Preserves `tools` (per-user library) and `machine` (per-shop
/// config) — those persist across project boundaries by design.
/// Drops imports, ops, fixtures, textLayers, stock, generated
/// state, selections, dirty flag, history.
///
/// Called by the open-file / open-recent flows before loading a
/// new drawing so leftover ops from the previous project don't
/// silently re-target unrelated objects in the new geometry.
export function clearProject(p: ProjectState) {
  p.data.imports = [];
  p.data.operations = [];
  p.data.fixtures = [];
  p.data.textLayers = [];
  p.data.reliefSources = [];
  p.data.groupOpsByTool = false;
  p.data.stock = { ...p.data.stock };
  // workOffset is per-project (the user pre-zeros their machine
  // at a different point per drawing), so reset to default like ops.
  p.data.workOffset = defaultWorkOffset();
  p.gen.generated = null;
  p.gen.toolpathCumLen = null;
  p.gen.toolpathTotalLen = 0;
  p.sel.selectedEntities = new Set();
  p.sel.selectedObjects = new Set();
  p.sel.selectedOpId = null;
  p.sel.selectedFixtureId = null;
  p.sel.selectedTextLayerId = null;
  p.sel.hoverSegment = null;
  p.data.visibleLayers = new Set();
  p.activeProjectPath = null;
  p.sourceFileStaleNotice = null;
  p.error = null;
  p.data.dirty = false;
  p.savedToProject = false;
  resetPreviewCache();
  p.history.clear();
}

/// Switch the active project path and apply the persisted per-project
/// workspace state (visible_layers / selected_op_id / playhead). Call
/// AFTER `setImported` / `restore` so the layer set is already populated
/// — we filter the saved layer names against what the import actually
/// contains.
export function setActiveProjectPath(p: ProjectState, path: string | null) {
  p.activeProjectPath = path;
  void refreshSourceWatch(p);
  if (path == null) return;
  const saved = workspace.get().per_project[path];
  if (!saved) return;
  const view = p.transformedImport;
  if (view && saved.visible_layers.length > 0) {
    const valid = new Set(view.layers.map((l) => l.name));
    const restored = saved.visible_layers.filter((n) => valid.has(n));
    if (restored.length > 0) p.data.visibleLayers = new Set(restored);
  }
  if (
    saved.selected_op_id != null &&
    p.data.operations.some((o) => o.id === saved.selected_op_id)
  ) {
    p.sel.selectedOpId = saved.selected_op_id;
  }
  if (typeof saved.playhead === 'number') {
    p.playhead = Math.max(0, Math.min(1, saved.playhead));
  }
}

/// Persist the current per-project view state. Called from $effects in
/// App.svelte when `visibleLayers` / `selectedOpId` / `playhead` change.
/// No-op when no path is active (browser uploads, samples, etc.).
///
/// Defers the workspace write off the synchronous Svelte 5 effect flush
/// via queueMicrotask. The write would otherwise mutate
/// `workspace.version` ($state) inside the effect body — when the
/// dispatch chain caused the
/// entire reactivity scheduler to abort silently after the first DXF
/// import (toolbar buttons stopped responding, file picker opened but
/// imports didn't propagate, etc.). The try/catch guards against the
/// throw still leaking past the microtask boundary.
export function persistPerProjectState(p: ProjectState) {
  const path = p.activeProjectPath;
  if (!path) return;
  const snapshot = {
    visible_layers: [...p.data.visibleLayers],
    selected_op_id: p.sel.selectedOpId,
    playhead: p.playhead,
  };
  queueMicrotask(() => {
    try {
      workspace.setPerProject(path, snapshot);
    } catch (e) {
      console.warn('persist per-project state:', e);
    }
  });
}

/// Snapshot for project save.
///
/// View-state fields (`visibleLayers`, `selectedEntities`) are
/// intentionally OMITTED — they're per-installation UI preferences
/// owned by `workspace.per_project[path].visible_layers`. Including
/// them in the .ivac-project save caused a two-source-of-truth
/// conflict where workspace silently won on reopen, surprising
/// users who expected their saved file to dictate visibility.
/// Old projects that still carry them load fine via the
/// `?? []` fallback in restore().
export function snapshotProject(p: ProjectState): ProjectFile {
  return {
    kind: 'ivac-project',
    version: 1,
    imports: p.data.imports,
    visibleLayers: [],
    selectedEntities: [],
    stock: p.data.stock,
    tools: p.data.tools,
    machine: p.data.machine,
    operations: p.data.operations,
    fixtures: p.data.fixtures,
    textLayers: p.data.textLayers,
    ...(p.data.reliefSources.length > 0 ? { reliefSources: p.data.reliefSources } : {}),
    // Only persist work_offset when non-default so legacy
    // / unset projects keep their compact .ivac-project payloads. The
    // restore() side defaults to defaultWorkOffset() when absent.
    ...(isDefaultWorkOffset(p.data.workOffset) ? {} : { workOffset: p.data.workOffset }),
    // Persist the tool-grouping toggle only when on.
    ...(p.data.groupOpsByTool ? { groupOpsByTool: true } : {}),
  };
}

export function restoreProject(p: ProjectState, file: ProjectFile) {
  if (file.kind !== 'ivac-project') {
    throw new Error('not a ivaCAM project file');
  }
  // imports[] is the canonical shape. Pre-migration project
  // files (with bare `imported` / `fileTransform` / `lastImportPath`
  // fields) are no longer loadable — the user explicitly waived
  // backward compatibility for this migration.
  p.data.imports = Array.isArray(file.imports) ? file.imports : [];
  if (p.data.imports[0]) {
    setImported(p, p.data.imports[0].source, p.data.imports[0].lastImportPath ?? null);
  }
  // Layer visibility precedence (best wins):
  //   1. workspace.per_project[path].visible_layers (applied in
  //      setActiveProjectPath after restore returns).
  //   2. file.visibleLayers, when the saved project carries any —
  //      e.g. a shared .ivac-project file from another machine
  //      whose workspace we don't have.
  //   3. setImported defaults (all layers visible).
  // Empty `file.visibleLayers` is treated as "no opinion" and falls
  // through to setImported defaults — new saves OMIT these fields
  // so workspace can be the single source of truth.
  if (Array.isArray(file.visibleLayers) && file.visibleLayers.length > 0) {
    p.data.visibleLayers = new Set(file.visibleLayers);
  }
  if (Array.isArray(file.selectedEntities) && file.selectedEntities.length > 0) {
    p.sel.selectedEntities = new Set(file.selectedEntities);
  }
  if (file.stock) p.data.stock = { ...p.data.stock, ...file.stock };
  if (Array.isArray(file.tools) && file.tools.length > 0)
    p.data.tools = file.tools.map(migrateLegacyToolTerms);
  if (file.machine) p.data.machine = { ...p.data.machine, ...migrateMachineSettings(file.machine) };
  if (Array.isArray(file.operations)) p.data.operations = file.operations;
  p.data.fixtures = Array.isArray(file.fixtures) ? file.fixtures : [];
  p.data.textLayers = Array.isArray(file.textLayers) ? file.textLayers : [];
  p.data.reliefSources = Array.isArray(file.reliefSources) ? file.reliefSources : [];
  // Restore the program-level WCS offset. Legacy files lack
  // this field — fall back to all-zero @ G54, which matches the
  // original behavior (geometry origin = WCS origin).
  p.data.workOffset = file.workOffset
    ? { ...defaultWorkOffset(), ...file.workOffset }
    : defaultWorkOffset();
  // Restore the tool-grouping toggle (legacy files lack it → false).
  p.data.groupOpsByTool = file.groupOpsByTool === true;
  p.sel.selectedFixtureId = null;
  p.sel.selectedOpId = null;
  // This content came from a saved .ivac-project file — mark it saved
  // so re-opening it (unedited) doesn't trigger the unsaved-work guard.
  // Must run AFTER the internal setImported above, which clears it.
  p.savedToProject = true;
  // Loading a project resets to a clean undo baseline.
  p.history.clear();
}
