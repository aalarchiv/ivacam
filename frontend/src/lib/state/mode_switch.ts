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
import { effectiveModes, toolCompatibleWithAnyMode } from './tool_family';

export interface ModeSwitchAssessment {
  /// The primary mode that was just switched to — drives which default
  /// tool the seed / auto-create action would add.
  mode: MachineMode;
  /// The machine's full effective mode set (primary + capabilities) —
  /// what compatibility is judged against, so a combo machine's second
  /// head doesn't trip the notice.
  modes: MachineMode[];
  /// Ops referencing a tool the machine can't run. Empty when the
  /// notice is only a seed offer.
  affectedOpIds: number[];
  /// An existing compatible tool the "assign to all" action can
  /// target (first in library order), or null when none exists — the
  /// action then creates the mode's default tool first.
  compatibleToolId: number | null;
  /// True when the library holds zero tools the machine can run and
  /// the primary mode is a singleton (laser / plasma / drag — modes
  /// with one natural tool kind). The notice then offers one-click
  /// seeding even with no affected ops.
  seedOffer: boolean;
}

/// Assess a completed machine change (mode switch or capability edit).
/// Compatibility is judged against the machine's EFFECTIVE mode set —
/// mirroring the Rust tool_incompatible_with_machine_mode backstop.
/// Returns null when nothing needs the user's attention (no ops
/// reference now-incompatible tools and a compatible tool exists or
/// the primary mode is mill).
export function assessModeSwitch(
  machine: { mode: MachineMode; capabilities?: readonly MachineMode[] },
  operations: readonly OpEntry[],
  tools: readonly ToolEntry[],
): ModeSwitchAssessment | null {
  const modes = effectiveModes(machine);
  const toolById = new Map(tools.map((t) => [t.id, t]));
  const affectedOpIds = operations
    .filter((op) => {
      if (isProgramOnlyOp(op.kind)) return false;
      const tool = toolById.get(op.toolId);
      // A dangling tool reference is the tool-existence validator's
      // problem, not a mode-compatibility one.
      if (!tool) return false;
      return !toolCompatibleWithAnyMode(tool.kind, modes);
    })
    .map((op) => op.id);
  const compatibleToolId = tools.find((t) => toolCompatibleWithAnyMode(t.kind, modes))?.id ?? null;
  const seedOffer = compatibleToolId == null && machine.mode !== 'mill';
  if (affectedOpIds.length === 0 && !seedOffer) return null;
  return { mode: machine.mode, modes, affectedOpIds, compatibleToolId, seedOffer };
}
