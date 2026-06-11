/// Machine-profile helpers — pure logic between the workspace store
/// (which persists profiles per-user) and the project (which holds the
/// working copy + a profile reference). Rune-free so vitest covers it
/// directly.
///
/// Model: a profile is a named (machine config + tool library) bundle
/// for one physical machine. The PROJECT remains the working copy —
/// applying a profile copies its machine + tools into the project (one
/// undoable command), and while a profile is referenced, project edits
/// mirror back into it. The project file keeps its own embedded
/// machine + tools snapshot, so a project referencing a profile that
/// doesn't exist on this installation still loads exactly as saved.

import type { MachineProfile } from './workspace';
import type { MachineSettings, ToolEntry } from './project-types';
import { migrateMachineSettings } from './project-types';
import { migrateLegacyToolTerms } from './tool-migration';
import { suggestMachineName } from './tool_naming';

/// Stable profile identity. Time + randomness is plenty — profiles are
/// created by a click, not in bulk.
export function newProfileId(): string {
  return `mp-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

/// Display name for a profile created from `machine`: the machine's
/// own name when set, else a numbered fallback that doesn't collide
/// with existing profile names.
export function profileNameFor(
  machine: MachineSettings,
  existing: readonly MachineProfile[],
): string {
  const base = machine.name?.trim();
  if (base) return base;
  // No user name: propose from the settings ("Mill 200×300"), with a
  // numeric suffix on collision so two unnamed machines stay distinct.
  const suggestion = suggestMachineName(machine);
  const names = new Set(existing.map((p) => p.name));
  if (!names.has(suggestion)) return suggestion;
  let n = 2;
  while (names.has(`${suggestion} (${n})`)) n += 1;
  return `${suggestion} (${n})`;
}

/// Build a profile from the project's current machine + tools. Deep
/// clones so the profile can't alias live $state proxies.
export function profileFromCurrent(
  machine: MachineSettings,
  tools: readonly ToolEntry[],
  existing: readonly MachineProfile[],
  id: string = newProfileId(),
): MachineProfile {
  return {
    id,
    name: profileNameFor(machine, existing),
    machine: JSON.parse(JSON.stringify(machine)) as MachineSettings,
    tools: JSON.parse(JSON.stringify(tools)) as ToolEntry[],
  };
}

/// Copy a profile under a new id and a "(copy)" name, for dialing in
/// variants (same machine, different post tweaks) without losing the
/// original.
export function duplicateProfile(
  src: MachineProfile,
  existing: readonly MachineProfile[],
  id: string = newProfileId(),
): MachineProfile {
  const names = new Set(existing.map((p) => p.name));
  let name = `${src.name} (copy)`;
  let n = 2;
  while (names.has(name)) {
    name = `${src.name} (copy ${n})`;
    n += 1;
  }
  const clone = JSON.parse(JSON.stringify(src)) as MachineProfile;
  // The duplicate's machine.name follows the new display name so the
  // mirror write-back doesn't immediately rename it back.
  clone.machine.name = name;
  return { ...clone, id, name };
}

/// The machine + tools a profile would put into the project, run
/// through the same migrations a loaded project file gets — so a
/// profile saved by an older build ages exactly like an old project.
export function profilePayload(profile: MachineProfile): {
  machine: MachineSettings;
  tools: ToolEntry[];
} {
  const machine = migrateMachineSettings(
    JSON.parse(JSON.stringify(profile.machine)) as MachineSettings,
  );
  const tools = (JSON.parse(JSON.stringify(profile.tools)) as ToolEntry[]).map(
    migrateLegacyToolTerms,
  );
  return { machine, tools };
}
