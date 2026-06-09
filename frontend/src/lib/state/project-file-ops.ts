// Project-file / workspace lifecycle extracted from the ProjectState
// god root (361x part 2). Save/load of .ivac-project files, the
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
  p.operations = [];
  p.fixtures = [];
  p.textLayers = [];
  p.reliefSources = [];
  p.groupOpsByTool = false;
  p.stock = { ...p.stock };
  // j4tv: workOffset is per-project (the user pre-zeros their machine
  // at a different point per drawing), so reset to default like ops.
  p.workOffset = defaultWorkOffset();
  p.generated = null;
  p.toolpathCumLen = null;
  p.toolpathTotalLen = 0;
  p.selectedEntities = new Set();
  p.selectedObjects = new Set();
  p.selectedOpId = null;
  p.selectedFixtureId = null;
  p.selectedTextLayerId = null;
  p.hoverSegment = null;
  p.visibleLayers = new Set();
  p.activeProjectPath = null;
  p.sourceFileStaleNotice = null;
  p.error = null;
  p.dirty = false;
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
    if (restored.length > 0) p.visibleLayers = new Set(restored);
  }
  if (saved.selected_op_id != null && p.operations.some((o) => o.id === saved.selected_op_id)) {
    p.selectedOpId = saved.selected_op_id;
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
/// dispatch chain landed on top of the eb8.6 commit, this caused the
/// entire reactivity scheduler to abort silently after the first DXF
/// import (toolbar buttons stopped responding, file picker opened but
/// imports didn't propagate, etc.). The try/catch guards against the
/// throw still leaking past the microtask boundary.
export function persistPerProjectState(p: ProjectState) {
  const path = p.activeProjectPath;
  if (!path) return;
  const snapshot = {
    visible_layers: [...p.visibleLayers],
    selected_op_id: p.selectedOpId,
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
/// users who expected their saved file to dictate visibility (audit
/// vep). Old projects that still carry them load fine via the
/// `?? []` fallback in restore().
export function snapshotProject(p: ProjectState): ProjectFile {
  return {
    kind: 'ivac-project',
    version: 1,
    imports: p.data.imports,
    visibleLayers: [],
    selectedEntities: [],
    stock: p.stock,
    tools: p.tools,
    machine: p.machine,
    operations: p.operations,
    fixtures: p.fixtures,
    textLayers: p.textLayers,
    ...(p.reliefSources.length > 0 ? { reliefSources: p.reliefSources } : {}),
    // i5g4 / j4tv: only persist work_offset when non-default so legacy
    // / unset projects keep their compact .ivac-project payloads. The
    // restore() side defaults to defaultWorkOffset() when absent.
    ...(isDefaultWorkOffset(p.workOffset) ? {} : { workOffset: p.workOffset }),
    // l8lk: persist the tool-grouping toggle only when on.
    ...(p.groupOpsByTool ? { groupOpsByTool: true } : {}),
  };
}

export function restoreProject(p: ProjectState, file: ProjectFile) {
  if (file.kind !== 'ivac-project') {
    throw new Error('not a ivaCAM project file');
  }
  // wrsu Phase 1: imports[] is the canonical shape. Pre-wrsu project
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
  // (audit vep) so workspace can be the single source of truth.
  if (Array.isArray(file.visibleLayers) && file.visibleLayers.length > 0) {
    p.visibleLayers = new Set(file.visibleLayers);
  }
  if (Array.isArray(file.selectedEntities) && file.selectedEntities.length > 0) {
    p.selectedEntities = new Set(file.selectedEntities);
  }
  if (file.stock) p.stock = { ...p.stock, ...file.stock };
  if (Array.isArray(file.tools) && file.tools.length > 0)
    p.tools = file.tools.map(migrateLegacyToolTerms);
  if (file.machine) p.machine = { ...p.machine, ...migrateMachineSettings(file.machine) };
  if (Array.isArray(file.operations)) p.operations = file.operations;
  p.fixtures = Array.isArray(file.fixtures) ? file.fixtures : [];
  p.textLayers = Array.isArray(file.textLayers) ? file.textLayers : [];
  p.reliefSources = Array.isArray(file.reliefSources) ? file.reliefSources : [];
  // j4tv: restore the program-level WCS offset. Legacy files lack
  // this field — fall back to all-zero @ G54, which matches the
  // pre-i5g4 behavior (geometry origin = WCS origin).
  p.workOffset = file.workOffset
    ? { ...defaultWorkOffset(), ...file.workOffset }
    : defaultWorkOffset();
  // l8lk: restore the tool-grouping toggle (legacy files lack it → false).
  p.groupOpsByTool = file.groupOpsByTool === true;
  p.selectedFixtureId = null;
  p.selectedOpId = null;
  // This content came from a saved .ivac-project file — mark it saved
  // so re-opening it (unedited) doesn't trigger the unsaved-work guard.
  // Must run AFTER the internal setImported above, which clears it.
  p.savedToProject = true;
  // Loading a project resets to a clean undo baseline.
  p.history.clear();
}
