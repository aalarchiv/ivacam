/// Default tool seeding per machine mode. Pure-logic so vitest covers
/// it without the rune runtime.
///
/// Two consumers:
///   * ToolLibraryDialog's "+ Add tool" — a new tool on a plasma
///     machine should start as a torch, not an endmill that the
///     mode filter immediately hides.
///   * The mode-switch flow — switching to a singleton mode
///     (laser / plasma / drag) with zero compatible tools offers a
///     one-click default so the user isn't stranded with an empty
///     picker.

import type { MachineMode, ToolKind } from './op_types';
import type { ToolEntry } from './project-types';

/// The signature tool kind for each machine mode — what a fresh tool
/// on that machine should default to. Mill machines default to the
/// classic endmill; the singleton modes each have exactly one
/// natural kind (the engraver's mill+drag dual use doesn't make it
/// the drag DEFAULT — a drag machine is first a knife cutter).
export function defaultKindForMode(mode: MachineMode): ToolKind {
  switch (mode) {
    case 'mill':
      return 'endmill';
    case 'laser':
      return 'laser_beam';
    case 'drag':
      return 'drag_knife';
    case 'plasma':
      return 'plasma_torch';
  }
}

/// A ready-to-cut default tool for `mode` with the given library id.
/// Values are conservative starting points, not recommendations — the
/// user tunes them in the tool library:
///   * mill — the 3 mm endmill every new project already seeds
///   * laser — beam with the 0.15 mm default kerf made explicit
///   * drag — 45° blade with a typical 0.25 mm trailing offset
///   * plasma — torch with the stock 3.8 / 1.5 / 0.5 pierce entry
///     and a 1.5 mm kerf (typical hobby-plasma cut width)
export function defaultToolForMode(mode: MachineMode, id: number): ToolEntry {
  switch (mode) {
    case 'mill':
      return {
        id,
        name: `Tool #${id}`,
        kind: 'endmill',
        diameter: 3,
        flutes: 2,
        speed: 18000,
        plungeRate: 100,
        feedRate: 800,
        coolant: 'off',
      };
    case 'laser':
      return {
        id,
        name: 'Laser beam',
        kind: 'laser_beam',
        diameter: 0.15,
        flutes: 0,
        speed: 0,
        plungeRate: 0,
        feedRate: 1000,
        coolant: 'off',
        kerfMm: 0.15,
      };
    case 'drag':
      return {
        id,
        name: 'Drag knife',
        kind: 'drag_knife',
        diameter: 0.9,
        flutes: 0,
        speed: 0,
        plungeRate: 0,
        feedRate: 800,
        coolant: 'off',
        dragoff: 0.25,
      };
    case 'plasma':
      return {
        id,
        name: 'Plasma torch',
        kind: 'plasma_torch',
        diameter: 1.5,
        flutes: 0,
        speed: 0,
        plungeRate: 0,
        feedRate: 2000,
        coolant: 'off',
        kerfMm: 1.5,
        pierceHeightMm: 3.8,
        cutHeightMm: 1.5,
        pierceDelaySec: 0.5,
      };
  }
}
