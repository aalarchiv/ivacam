// Import/geometry domain operations extracted from the ProjectState god
// root (361x). Each function takes the live `ProjectState` and mutates
// through its slices + the command bus; ProjectState keeps one-line
// delegators so the component-facing `project.*` API is unchanged.
// Splitting by domain (not by mechanism) gives the import lifecycle —
// set/add/remove imports, layer edits, text-segment appends, per-import
// file transforms, and the desktop source-file watcher — a single home.

import type { ImportResponse, ImportedObject, Segment } from '../api/types';
import { isTauri as isTauriEnv } from '../api/env';
import { bboxOfSegments } from '../canvas/selection-geometry';
import { resetPreviewCache } from './text_preview.svelte';
import { setImportsCommand, updateOperationCommand } from './commands';
import {
  identityFileTransform,
  inferDefaultWorkOffset,
  isIdentityFileTransform,
  placementFileTransform,
  type FileTransform,
  type ImportEntry,
} from './project-types';
import { applyFileTransformToPoint, invertFileTransformPoint } from './file-transform';
import { isContourOp, type OpPatch } from './op_types';
import type { ProjectState } from './project.svelte';

function isAbsolutePath(path: string): boolean {
  return path.startsWith('/') || /^[a-zA-Z]:[\\/]/.test(path);
}

/// Append another drawing to the project as its own ImportEntry
/// (wrsu Phase 2). Each entry keeps its own fileTransform so the user
/// can position drawings independently on stock. Layer visibility
/// opens for newly-arrived names so the user sees the new drawing.
///
/// Object id namespacing is handled at view time by `combineImports`
/// — each entry occupies a contiguous id range starting after the
/// previous entries. Existing op references stay valid because
/// imports[0]'s id range is unchanged.
///
/// Undo: not history-tracked in Phase 2A — adding a drawing crosses
/// a project boundary similar to setImported. Phase 2B is filed to
/// thread the add through a proper command if users complain.
export function addImported(p: ProjectState, r: ImportResponse, sourcePath?: string | null) {
  if (p.data.imports.length === 0) {
    // First import: behave identically to setImported, since the
    // open-file flows always call addImported.
    setImported(p, r, sourcePath);
    return;
  }
  const nextId = p.data.imports.reduce((m, e) => (e.id > m ? e.id : m), 0) + 1;
  const before = p.data.imports;
  const after: ImportEntry[] = [
    ...before,
    {
      id: nextId,
      source: r,
      fileTransform: identityFileTransform(),
      lastImportPath: sourcePath ?? null,
    },
  ];
  const label = `Add ${sourcePath?.split(/[\\/]/).pop() ?? r.filename ?? 'drawing'}`;
  p.history.exec(setImportsCommand(before, after, label), p.target());
  // Visibility lives outside history (UI-only); reveal the new layers
  // now even though undo won't reverse the toggle.
  const nextVis = new Set(p.data.visibleLayers);
  for (const l of r.layers) nextVis.add(l.name);
  p.data.visibleLayers = nextVis;
  p.gen.generated = null;
  p.gen.toolpathCumLen = null;
  p.gen.toolpathTotalLen = 0;
  p.error = null;
  void refreshSourceWatch(p);
}

/// Remove an import by its ImportEntry.id (wrsu Phase 2). Layer
/// visibility entries that no longer have any backing import are
/// pruned (visibility lives outside history). Undoable via the
/// `setImportsCommand` shape.
export function removeImport(p: ProjectState, id: number) {
  const before = p.data.imports;
  const after = before.filter((e) => e.id !== id);
  if (after.length === before.length) return;
  const removed = before.find((e) => e.id === id);
  const label = `Remove ${removed?.source.filename ?? 'drawing'}`;
  p.history.exec(setImportsCommand(before, after, label), p.target());
  const stillThere = new Set<string>();
  for (const e of after) for (const l of e.source.layers) stillThere.add(l.name);
  const filtered = new Set<string>();
  for (const l of p.data.visibleLayers) if (stillThere.has(l)) filtered.add(l);
  p.data.visibleLayers = filtered;
  p.gen.generated = null;
  p.gen.toolpathCumLen = null;
  p.gen.toolpathTotalLen = 0;
  void refreshSourceWatch(p);
}

export function setImported(p: ProjectState, r: ImportResponse, sourcePath?: string | null) {
  // Replace imports[0] in place: inherit the previous entry's id when
  // there was one (so undo entries built against that id stay valid),
  // reset the per-import fileTransform to identity (a new source means
  // the old layout was for different geometry), and seed lastImportPath
  // from `sourcePath` when the caller provided one.
  const prev = p.data.imports[0];
  const nextPath = sourcePath !== undefined ? sourcePath : (prev?.lastImportPath ?? null);
  // xeio: auto-place the drawing's bottom-left at the work-area origin
  // (unless it already sits fully inside the bed) so the emitted g-code
  // is reachable. Translate-only; flows through the normal FileTransform
  // path so the pipeline / g-code see the placed coordinates.
  const placement = placementFileTransform(r.bbox, p.data.machine.workArea);
  p.data.imports = [
    {
      id: prev?.id ?? 1,
      source: r,
      fileTransform: placement,
      lastImportPath: nextPath,
    },
  ];
  p.gen.generated = null;
  p.gen.toolpathCumLen = null;
  p.gen.toolpathTotalLen = 0;
  p.data.dirty = true;
  // A raw drawing import is not a saved project — even before any edit,
  // discarding it loses the user's imported work, so `hasUnsavedWork`
  // must flag it. `restore()` calls setImported while loading a saved
  // project and flips this back to true at its end.
  p.savedToProject = false;
  p.error = null;
  p.data.visibleLayers = new Set(r.layers.map((l) => l.name));
  p.sel.selectedEntities = new Set();
  p.sel.selectedObjects = new Set();
  p.sel.hoverSegment = null;
  p.sourceFileStaleNotice = null;
  // gldc: auto-default work_offset to the geometry bbox's bottom-left
  // when the drawing was authored off-origin in CAD and the user
  // hasn't explicitly set an offset. Suppresses the
  // `stock_origin_outside_geometry_bbox` pipeline warning at its
  // most common firing site (drawings centered around a non-zero
  // point in the source CAD), matching the canonical CNC workflow
  // (operator zeros at the bottom-left corner of the drawing).
  // No-op when the user has already moved away from default.
  // Snap the WCS to the PLACED bottom-left, not the raw (pre-placement)
  // coords — xeio's translate may have moved the geometry to origin.
  const placedBbox = {
    min_x: r.bbox.min_x + placement.translate.x,
    min_y: r.bbox.min_y + placement.translate.y,
    max_x: r.bbox.max_x + placement.translate.x,
    max_y: r.bbox.max_y + placement.translate.y,
  };
  p.data.workOffset = inferDefaultWorkOffset(placedBbox, p.data.workOffset);
  // Replacing the imported geometry implies a new project boundary —
  // drop any text-preview segments cached from the previous project
  // so we don't paint stale TextLayer glyphs over the new file.
  resetPreviewCache();
  // Imports cross a project boundary; undoing back across that boundary
  // would mix incompatible geometry/op state, so drop history here.
  p.history.clear();
  void refreshSourceWatch(p);
}

/// Refresh the desktop file-system watcher to track every absolute
/// source path the project depends on. No-op outside Tauri; failure
/// surfaces as a console warning so the rest of the app isn't blocked
/// when the watcher backend is unavailable (e.g. inotify quota hit).
export async function refreshSourceWatch(p: ProjectState): Promise<void> {
  if (typeof window === 'undefined') return;
  if (!isTauriEnv()) return;
  const paths = new Set<string>();
  // wrsu Phase 2: watch every import's source path, not just imports[0].
  for (const entry of p.data.imports) {
    if (entry.lastImportPath && isAbsolutePath(entry.lastImportPath)) {
      paths.add(entry.lastImportPath);
    }
  }
  if (p.activeProjectPath && isAbsolutePath(p.activeProjectPath)) {
    paths.add(p.activeProjectPath);
  }
  try {
    const mod = await import('../api/tauri');
    await mod.watchSourcePaths(Array.from(paths));
  } catch (e) {
    console.warn('source watch:', e);
  }
}

/// Drop every active watch slot. Called when the project closes.
export async function stopSourceWatch(): Promise<void> {
  if (typeof window === 'undefined') return;
  if (!isTauriEnv()) return;
  try {
    const mod = await import('../api/tauri');
    await mod.unwatchAll();
  } catch (e) {
    console.warn('source watch stop:', e);
  }
}

/// Re-import the named source path and swap it in. Wraps the swap as a
/// single-step undoable transaction so Ctrl+Z reverts to the prior
/// geometry. Used by both the auto-reload effect and the manual
/// "Reload" button on SourceStaleToast.
///
/// After the swap, ops whose `sourceObjects` reference object ids no
/// longer present in the new geometry are flagged via console.warn —
/// richer recovery is a follow-up. Returns true on success.
/// Source-file watcher callback (eb8.4 + wrsu Phase 2). The watcher
/// fires per-path; we look up the matching ImportEntry and replace
/// its source in place, preserving its fileTransform + id. If no
/// entry matches the path (stale watch), bail rather than overwrite
/// an unrelated import.
export async function reimportFromPath(p: ProjectState, path: string): Promise<boolean> {
  if (typeof window === 'undefined') return false;
  if (!isTauriEnv()) return false;
  const idx = p.data.imports.findIndex((e) => e.lastImportPath === path);
  if (idx < 0) {
    p.setError(`reload: no import is watching ${path}`);
    return false;
  }
  let after: ImportResponse;
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    after = await invoke<ImportResponse>('import_path', { path });
  } catch (e) {
    p.setError(`reload: ${e instanceof Error ? e.message : String(e)}`);
    return false;
  }
  const next = [...p.data.imports];
  next[idx] = { ...next[idx], source: after };
  p.data.imports = next;
  p.data.dirty = true;
  p.sourceFileStaleNotice = null;
  // Orphan-source detection runs against the merged view (post-reload)
  // so ops keyed by ids from OTHER imports still see their objects.
  // eb8.7's inline Re-pick chip on OperationsList rows surfaces the
  // affected ops; this warn keeps the dev console signal too.
  // Use the augmented view so an op targeting the synthetic stock
  // outline (STOCK_OUTLINE_ID) isn't mistaken for an orphan (8jce).
  const presentIds = new Set(p.geometryView?.objects ?? []);
  for (const op of p.data.operations) {
    if (!Array.isArray(op.sourceObjects) || op.sourceObjects.length === 0) continue;
    const orphans = op.sourceObjects.filter((id) => !presentIds.has(id));
    if (orphans.length > 0) {
      console.warn(
        `op "${op.name}" (#${op.id}): source geometry missing for ids ${orphans.join(', ')}`,
      );
    }
  }
  return true;
}

export function toggleLayer(p: ProjectState, name: string) {
  const next = new Set(p.data.visibleLayers);
  if (next.has(name)) next.delete(name);
  else next.add(name);
  p.data.visibleLayers = next;
}

/// Delete every imported segment that belongs to `layerName`. Drops
/// the layer entry, the visibleLayers entry, and (parallel-index)
/// the `objects[]` per-segment mapping. `object_meta` is left intact
/// — entries for deleted objects become orphaned but no remaining
/// segment references them, so they're harmless until the next
/// re-import. Bbox is recomputed from the surviving segments.
/// Undoable via the imports-snapshot command pattern.
///
/// Multi-file: removes the layer from EVERY import that carries it,
/// matching what the user sees in the unioned LayerList count.
export function removeImportedLayer(p: ProjectState, layerName: string) {
  const before = p.data.imports;
  if (before.length === 0) return;
  let touched = false;
  const after = before.map((entry) => {
    const src = entry.source;
    const keep = src.segments.map((s) => s.layer !== layerName);
    if (keep.every((k) => k)) return entry;
    touched = true;
    const newSegments = src.segments.filter((_, i) => keep[i]);
    const newObjects = (src.objects ?? []).filter((_, i) => keep[i]);
    const newLayers = src.layers.filter((l) => l.name !== layerName);
    const newBbox = bboxOfSegments(newSegments);
    return {
      ...entry,
      source: {
        ...src,
        segments: newSegments,
        layers: newLayers,
        bbox: newBbox,
        objects: newObjects,
      },
    };
  });
  if (!touched) return;
  p.history.beginTransaction(`Delete layer "${layerName}"`);
  p.history.exec(setImportsCommand(before, after, `Delete layer "${layerName}"`), p.target());
  // Drop visibility tracking for the gone layer too — visibleLayers
  // lives outside the command target, so this is a plain mutation.
  if (p.data.visibleLayers.has(layerName)) {
    const next = new Set(p.data.visibleLayers);
    next.delete(layerName);
    p.data.visibleLayers = next;
  }
  p.history.commitTransaction();
}

/// Append the rendered segments from AddTextDialog to the imported
/// geometry layer and return the 1-based object ids the chaining pass
/// produced for them. The chaining pass owns object id assignment, so
/// after appending we re-run the lightweight client-side approximation:
/// each closed contour gets a fresh contiguous id higher than any
/// existing one. This keeps the dialog's "use these objects as the op's
/// source" wiring correct without round-tripping through /import.
///
/// `singleLine` — when true, segments are open polylines (engraving
/// strokes) and should NOT be treated as closed objects; they go in as
/// id 0 (unchained), but we still return an array of ids so callers
/// can use the same flow.
export function appendImportedSegments(
  p: ProjectState,
  segments: Segment[],
  layerName: string,
  singleLine: boolean,
): number[] {
  const before = p.data.imports;
  // imports[0] is the canonical Add-Text target. Synthesize an empty
  // entry when none exists so the user can author text in a fresh
  // project before importing geometry; this synthesis is captured in
  // the command's `before` snapshot so a single undo wipes the whole
  // Add-Text run including the synthetic seed.
  const seedEntry: ImportEntry = before[0] ?? {
    id: 1,
    source: {
      filename: 'text',
      format: 'text',
      bbox: { min_x: 0, min_y: 0, max_x: 0, max_y: 0 },
      layers: [],
      segments: [],
      unit_scale: 1,
      warnings: [],
      objects: [],
      object_meta: [],
    },
    fileTransform: identityFileTransform(),
    lastImportPath: null,
  };
  const cur = seedEntry.source;
  const baseObjId = (cur.objects ?? []).reduce((m, o) => Math.max(m, o), 0);

  // Group consecutive segments by closed contour heuristic: each chain
  // of head→tail-touching segments becomes one object. Open polylines
  // (single_line) get id 0 (unchained).
  const newObjects: number[] = [];
  const newMeta: ImportedObject[] = [];
  if (singleLine) {
    newObjects.push(...new Array(segments.length).fill(0));
  } else {
    let nextId = baseObjId;
    let curId: number | null = null;
    let prevEnd: { x: number; y: number } | null = null;
    const close = 1e-6;
    const eq = (a: { x: number; y: number }, b: { x: number; y: number }) =>
      Math.abs(a.x - b.x) < close && Math.abs(a.y - b.y) < close;
    for (const s of segments) {
      if (curId == null || prevEnd == null || !eq(prevEnd, s.start)) {
        nextId += 1;
        curId = nextId;
      }
      newObjects.push(curId);
      prevEnd = s.end;
    }
    // Build minimal object metadata. closed=true as a hint; the
    // backend will reclassify on next /generate. This is enough for
    // the OperationsList / canvas selection wiring to recognize the
    // ids without a round trip.
    const ids = Array.from(new Set(newObjects.filter((i) => i > baseObjId)));
    for (const id of ids) {
      const owned = segments.filter((_, i) => newObjects[i] === id);
      let minX = Infinity,
        minY = Infinity,
        maxX = -Infinity,
        maxY = -Infinity;
      for (const s of owned) {
        minX = Math.min(minX, s.start.x, s.end.x);
        minY = Math.min(minY, s.start.y, s.end.y);
        maxX = Math.max(maxX, s.start.x, s.end.x);
        maxY = Math.max(maxY, s.start.y, s.end.y);
      }
      newMeta.push({
        id,
        closed: true,
        layer: layerName,
        color: owned[0]?.color ?? 7,
        bbox: { min_x: minX, min_y: minY, max_x: maxX, max_y: maxY },
      });
    }
  }

  // Recompute layer summary.
  const layers = [...cur.layers];
  let layerEntry = layers.find((l) => l.name === layerName);
  if (!layerEntry) {
    layerEntry = { name: layerName, color: segments[0]?.color ?? 7, segment_count: 0 };
    layers.push(layerEntry);
  }
  layerEntry.segment_count += segments.length;

  // Expand bbox to enclose appended geometry.
  let bbox = { ...cur.bbox };
  for (const s of segments) {
    bbox.min_x = Math.min(bbox.min_x, s.start.x, s.end.x);
    bbox.min_y = Math.min(bbox.min_y, s.start.y, s.end.y);
    bbox.max_x = Math.max(bbox.max_x, s.start.x, s.end.x);
    bbox.max_y = Math.max(bbox.max_y, s.start.y, s.end.y);
  }
  if (cur.segments.length === 0) {
    // First import — bbox starts from the appended geometry only.
    bbox = {
      min_x: Math.min(...segments.flatMap((s) => [s.start.x, s.end.x])),
      min_y: Math.min(...segments.flatMap((s) => [s.start.y, s.end.y])),
      max_x: Math.max(...segments.flatMap((s) => [s.start.x, s.end.x])),
      max_y: Math.max(...segments.flatMap((s) => [s.start.y, s.end.y])),
    };
  }

  const afterSource: ImportResponse = {
    ...cur,
    segments: [...cur.segments, ...segments],
    objects: [...(cur.objects ?? []), ...newObjects],
    object_meta: [...(cur.object_meta ?? []), ...newMeta],
    layers,
    bbox,
  };
  const after: ImportEntry[] = [{ ...seedEntry, source: afterSource }, ...before.slice(1)];
  p.history.exec(setImportsCommand(before, after, 'Add geometry'), p.target());
  p.data.visibleLayers = new Set([...p.data.visibleLayers, layerName]);

  // Return the de-duplicated set of new object ids (in insertion order).
  const distinct: number[] = [];
  const seen = new Set<number>();
  for (const id of newObjects) {
    if (id > 0 && !seen.has(id)) {
      seen.add(id);
      distinct.push(id);
    }
  }
  return distinct;
}

/// Per-import variant of patchFileTransform (wrsu Phase 2). Undoable;
/// the optional coalesceKey is per-import-per-field so two consecutive
/// nudges of the X spinner on entry #3 collapse to one history step.
///
/// 43l2: after swapping the transform, project every op's
/// approachPoint and (mirror-sensitive) tabPlacements through the
/// delta so they stay attached to the same geometry the user sees.
/// Approach points round-trip via raw-import space; tab `t` values
/// flip 1-t when the mirror parity changed since contour traversal
/// reverses. Bundled into the same transaction as the imports swap
/// so Ctrl+Z reverts the whole intent in one step.
export function patchFileTransformForImport(
  p: ProjectState,
  importId: number,
  patch: Partial<Omit<FileTransform, 'translate'>> & {
    translate?: Partial<FileTransform['translate']>;
  },
  coalesceKey?: string,
) {
  const idx = p.data.imports.findIndex((e) => e.id === importId);
  if (idx < 0) return;
  const entry = p.data.imports[idx];
  const beforeXf = entry.fileTransform;
  const afterXf: FileTransform = {
    ...beforeXf,
    ...patch,
    translate: { ...beforeXf.translate, ...(patch.translate ?? {}) },
  };
  const before = p.data.imports;
  const after = [...before];
  after[idx] = { ...entry, fileTransform: afterXf };
  const label = 'Edit file transform';
  const opPatches = computeOpPatchesForXfDelta(p, before, idx, beforeXf, afterXf);
  if (opPatches.length === 0) {
    // Hot path: spinner drags with no affected ops stay as a single
    // command so the coalesce key still collapses streaks of nudges
    // into one undo entry.
    p.history.exec(
      setImportsCommand(
        before,
        after,
        label,
        coalesceKey ? `xform:${importId}:${coalesceKey}` : undefined,
      ),
      p.target(),
    );
    return;
  }
  p.history.beginTransaction(label);
  try {
    p.history.exec(setImportsCommand(before, after, label), p.target());
    for (const { opId, patch: opPatch } of opPatches) {
      p.history.exec(updateOperationCommand(opId, opPatch), p.target());
    }
    p.history.commitTransaction();
  } catch (e) {
    p.history.cancelTransaction(p.target());
    throw e;
  }
}

/// 43l2 helper: compute the per-op `approachPoint` + `tabPlacements`
/// patches needed to keep the user's authored markers stuck to the
/// geometry when the import at `idx`'s fileTransform changes. Returns
/// only ops that actually need an update; ops whose source touches
/// OTHER imports aren't moved. Pure — doesn't mutate.
function computeOpPatchesForXfDelta(
  p: ProjectState,
  imports: readonly ImportEntry[],
  idx: number,
  beforeXf: FileTransform,
  afterXf: FileTransform,
): { opId: number; patch: OpPatch }[] {
  if (isIdentityFileTransform(beforeXf) && isIdentityFileTransform(afterXf)) {
    return [];
  }
  // Compute this entry's namespaced object-id range (matches
  // combineImports' offset arithmetic — entries[0] keeps 1..N0,
  // entries[1] gets N0+1..N0+N1, etc.).
  let idOffset = 0;
  for (let i = 0; i < idx; i++) {
    const m = (imports[i].source.objects ?? []).reduce((max, id) => (id > max ? id : max), 0);
    idOffset += m;
  }
  const localMax = (imports[idx].source.objects ?? []).reduce((m, id) => (id > m ? id : m), 0);
  const lo = idOffset + 1;
  const hi = idOffset + localMax;
  const ownsId = (id: number) => id >= lo && id <= hi;
  // The pivot for both forward + inverse is the RAW import bbox
  // centre, which doesn't change with the transform itself.
  const rawBbox = imports[idx].source.bbox;
  const mirrorParityChanged =
    Number(beforeXf.mirrorX) !== Number(afterXf.mirrorX) ||
    Number(beforeXf.mirrorY) !== Number(afterXf.mirrorY);
  const out: { opId: number; patch: OpPatch }[] = [];
  for (const op of p.data.operations) {
    // approachPoint + tabPlacements live on contour ops only.
    // Non-contour ops (Drill / VCarve / …) have no markers to keep
    // attached, so skip them — also avoids narrowing pain on the
    // OpEntry discriminated union.
    if (!isContourOp(op)) continue;
    // Empty sourceObjects = "all geometry" — ambiguous which import
    // it belongs to. Skip; the user re-positions if needed (no worse
    // than today for that case).
    if (!Array.isArray(op.sourceObjects) || op.sourceObjects.length === 0) continue;
    const ownedIds = op.sourceObjects.filter(ownsId);
    if (ownedIds.length === 0) continue;
    // The op is narrowed to ProfileOp | PocketOp by isContourOp
    // above, so its patch can carry the contour-only fields directly.
    const patch: Partial<typeof op> = {};
    // Approach point: world(before) → raw → world(after).
    if (op.approachPoint) {
      const [ax, ay] = op.approachPoint;
      const raw = invertFileTransformPoint({ x: ax, y: ay }, beforeXf, rawBbox);
      const next = applyFileTransformToPoint(raw, afterXf, rawBbox);
      // Skip the write when the result is identical (no-op transforms
      // that still survived the identity guard above).
      if (Math.abs(next.x - ax) > 1e-9 || Math.abs(next.y - ay) > 1e-9) {
        patch.approachPoint = [next.x, next.y];
      }
    }
    // Tab placements: mirror parity flip reverses contour traversal,
    // so t → 1-t per placement on this import's objects.
    const tabs = op.tabPlacements;
    if (mirrorParityChanged && Array.isArray(tabs) && tabs.length > 0) {
      const flipped = tabs.map((tp) => (ownsId(tp.objectId) ? { ...tp, t: 1 - tp.t } : tp));
      // Only emit when at least one placement actually flipped.
      if (flipped.some((tp, i) => tp.t !== tabs[i].t)) {
        patch.tabPlacements = flipped;
      }
    }
    if (Object.keys(patch).length > 0) out.push({ opId: op.id, patch });
  }
  return out;
}

export function resetFileTransformForImport(p: ProjectState, importId: number) {
  const idx = p.data.imports.findIndex((e) => e.id === importId);
  if (idx < 0) return;
  if (isIdentityFileTransform(p.data.imports[idx].fileTransform)) return;
  const before = p.data.imports;
  const after = [...before];
  after[idx] = { ...after[idx], fileTransform: identityFileTransform() };
  p.history.exec(setImportsCommand(before, after, 'Reset file transform'), p.target());
}
