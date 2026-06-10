/// Machine-mode-switch assessment — the data behind the non-modal
/// "operations use tools that cannot run on this machine" notice.
/// Pure-logic so vitest covers it without the rune runtime.
///
/// Principles (the mode-switch UX contract):
///   * a mode switch NEVER mutates the tool library or rewrites ops —
///     the notice offers a one-click fix, the user decides
///   * the toggle is never blocked (users flip modes exploratorily)
///   * the inconsistency is surfaced at the moment it is created, in
///     ONE notice, not one warning per op

import { isProgramOnlyOp, type MachineMode, type OpEntry } from './op_types';
import type { ToolEntry } from './project-types';
import { toolCompatibleWithMode } from './tool_family';

export interface ModeSwitchAssessment {
  /// The mode that was just switched to.
  mode: MachineMode;
  /// Ops referencing a tool the new mode can't run. Empty when the
  /// notice is only a seed offer.
  affectedOpIds: number[];
  /// An existing mode-compatible tool the "assign to all" action can
  /// target (first in library order), or null when none exists — the
  /// action then creates the mode's default tool first.
  compatibleToolId: number | null;
  /// True when the library holds zero tools the new mode can run and
  /// the mode is a singleton (laser / plasma / drag — modes with one
  /// natural tool kind). The notice then offers one-click seeding even
  /// with no affected ops.
  seedOffer: boolean;
}

/// Assess a completed mode switch. Returns null when nothing needs the
/// user's attention (no ops reference now-incompatible tools and a
/// compatible tool exists or the mode is mill).
export function assessModeSwitch(
  mode: MachineMode,
  operations: readonly OpEntry[],
  tools: readonly ToolEntry[],
): ModeSwitchAssessment | null {
  const toolById = new Map(tools.map((t) => [t.id, t]));
  const affectedOpIds = operations
    .filter((op) => {
      if (isProgramOnlyOp(op.kind)) return false;
      const tool = toolById.get(op.toolId);
      // A dangling tool reference is the tool-existence validator's
      // problem, not a mode-compatibility one.
      if (!tool) return false;
      return !toolCompatibleWithMode(tool.kind, mode);
    })
    .map((op) => op.id);
  const compatibleToolId = tools.find((t) => toolCompatibleWithMode(t.kind, mode))?.id ?? null;
  const seedOffer = compatibleToolId == null && mode !== 'mill';
  if (affectedOpIds.length === 0 && !seedOffer) return null;
  return { mode, affectedOpIds, compatibleToolId, seedOffer };
}
