/// Shop-inventory ↔ stocked-tools glue. Pure logic so vitest covers it
/// without the rune runtime.
///
/// Model: `workspace.tool_inventory` is the single EDITOR surface for
/// tool definitions (the Tool library tab). A machine stocks a subset —
/// id-preserving copies in `project.data.tools` — which is what op
/// dropdowns and the pipeline consume, and what the machine-profile
/// mirror persists per machine. Editing an inventory tool propagates
/// into same-id stocked copies so "the 6 mm endmill" stays one tool
/// everywhere it's loaded.

import type { ToolEntry } from './project-types';
import { migrateLegacyToolTerms } from './tool-migration';

/// Apply inventory edits to the stocked set: every stocked tool whose
/// id exists in the inventory takes the inventory's definition; stocked
/// tools without an inventory counterpart (legacy / project-local) stay
/// untouched. Returns the new stocked list, or null when nothing
/// changed (callers skip the undoable replace).
export function syncStockedFromInventory(
  inventory: readonly ToolEntry[],
  stocked: readonly ToolEntry[],
): ToolEntry[] | null {
  const byId = new Map(inventory.map((t) => [t.id, t]));
  let changed = false;
  const next = stocked.map((t) => {
    const inv = byId.get(t.id);
    if (!inv) return t;
    if (JSON.stringify(inv) === JSON.stringify(t)) return t;
    changed = true;
    return JSON.parse(JSON.stringify(inv)) as ToolEntry;
  });
  return changed ? next : null;
}

/// Stock an inventory tool onto the machine (= into the project's
/// working tool set). Id-preserving when free; a DIFFERENT tool already
/// holding that id (legacy project data) bumps the copy to the next
/// free id — the link to the inventory is lost for that copy, which is
/// the honest outcome of a collision. Returns null when the exact tool
/// is already stocked.
export function stockTool(
  inventoryTool: ToolEntry,
  stocked: readonly ToolEntry[],
): ToolEntry | null {
  const clone = JSON.parse(JSON.stringify(inventoryTool)) as ToolEntry;
  const existing = stocked.find((t) => t.id === inventoryTool.id);
  if (existing) {
    if (JSON.stringify(existing) === JSON.stringify(inventoryTool)) return null;
    clone.id = stocked.reduce((m, t) => Math.max(m, t.id), 0) + 1;
  }
  return clone;
}

/// Seed an empty inventory from a project's tools (first activation of
/// the Tool library tab on an installation that predates the shop
/// inventory). Runs the legacy-term migrations like a project load.
export function seedInventoryFromProject(projectTools: readonly ToolEntry[]): ToolEntry[] {
  return (JSON.parse(JSON.stringify(projectTools)) as ToolEntry[]).map(migrateLegacyToolTerms);
}
