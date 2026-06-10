/// Best-fit tool selection when an op is added against a selection.
/// Pure-logic; vitest exercises it without booting the rune runtime.
///
/// Heuristic per op kind:
///   * Drill: square-ish selection (length ≈ width) → tool whose
///     diameter is closest to the inferred hole. Tie-break order:
///       1. exact match within `EXACT_TOL_MM`
///       2. next smaller drill / endmill
///       3. next larger drill / endmill
///     Smaller-preferred because under-drilling is recoverable
///     (you can always enlarge); over-drilling isn't.
///   * Pocket / Profile / V-Carve / Pause / others: no special
///     heuristic yet — keep the existing `tools[0]` default. Drill
///     was the only kind the user singled out and it's the
///     unambiguous geometry signal.

import type { components } from '../api/generated';
import type { MachineMode, OpKind } from './op_types';
import type { ToolEntry } from './project-types';
import { toolCompatibleWithMode } from './tool_family';

type ImportedObject = components['schemas']['ImportedObject'];

/// Tolerance for "length ≈ width" — bboxes within this ratio are
/// treated as round/square. Many DXF circles tessellate into
/// polylines whose bbox sides differ by a few hundredths of a mm.
const SQUARE_RATIO_TOL = 1.1;
/// Diameter delta for "matching" tool. Below this, we don't bother
/// hunting for next-smaller or next-larger.
const EXACT_TOL_MM = 0.05;

/// Split the library into mode-compatible tools and the rest, both in
/// library order. Tool pickers list `compatible` first and group
/// `incompatible` under a labelled section — visible-and-explained,
/// never hidden-and-lost: a machine-mode switch must not strand an op
/// on a tool the picker can no longer even display.
export function partitionToolsForMode(
  tools: readonly ToolEntry[],
  mode: MachineMode,
): { compatible: ToolEntry[]; incompatible: ToolEntry[] } {
  const compatible: ToolEntry[] = [];
  const incompatible: ToolEntry[] = [];
  for (const t of tools) {
    (toolCompatibleWithMode(t.kind, mode) ? compatible : incompatible).push(t);
  }
  return { compatible, incompatible };
}

/// Selects the best tool for a new op against the given selection.
/// Falls back to `tools[0]` (or `null` when the library is empty)
/// when no specialised heuristic applies.
export function pickBestToolForOp(
  kind: OpKind,
  selectionIds: number[],
  meta: readonly ImportedObject[],
  tools: readonly ToolEntry[],
): ToolEntry | null {
  if (tools.length === 0) return null;
  if (kind === 'drill') {
    const dia = inferDrillDiameterMm(selectionIds, meta);
    if (dia != null) {
      const best = pickBestDrillTool(dia, tools);
      if (best) return best;
    }
  }
  return tools[0];
}

/// Infer the hole diameter for a Drill op from the selection bboxes.
/// Returns the minimum diameter across square-ish objects so the
/// picked tool fits the SMALLEST hole — non-matching holes can be
/// recut with manual tool swaps. `null` when no object in the
/// selection looks like a drillable hole.
export function inferDrillDiameterMm(
  selectionIds: number[],
  meta: readonly ImportedObject[],
): number | null {
  let best: number | null = null;
  for (const id of selectionIds) {
    const m = meta[id - 1];
    if (!m) continue;
    const w = m.bbox.max_x - m.bbox.min_x;
    const h = m.bbox.max_y - m.bbox.min_y;
    if (w <= 0 || h <= 0) continue;
    const ratio = Math.max(w, h) / Math.min(w, h);
    if (ratio > SQUARE_RATIO_TOL) continue;
    const d = (w + h) * 0.5;
    if (best == null || d < best) best = d;
  }
  return best;
}

/// Pick the drill / endmill tool whose diameter best matches
/// `holeDiamMm`. Tie-break order: exact (within EXACT_TOL_MM) →
/// next smallest → next largest. Returns `null` when the library
/// has no drillable tools (drill or endmill kinds).
export function pickBestDrillTool(
  holeDiamMm: number,
  tools: readonly ToolEntry[],
): ToolEntry | null {
  const candidates = tools.filter((t) => t.kind === 'drill' || t.kind === 'endmill');
  if (candidates.length === 0) return null;
  const exact = candidates
    .filter((t) => Math.abs(t.diameter - holeDiamMm) <= EXACT_TOL_MM)
    .sort((a, b) => Math.abs(a.diameter - holeDiamMm) - Math.abs(b.diameter - holeDiamMm))[0];
  if (exact) return exact;
  const smaller = candidates
    .filter((t) => t.diameter < holeDiamMm)
    .sort((a, b) => b.diameter - a.diameter)[0];
  if (smaller) return smaller;
  const larger = candidates
    .filter((t) => t.diameter > holeDiamMm)
    .sort((a, b) => a.diameter - b.diameter)[0];
  return larger ?? null;
}
