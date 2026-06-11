/// Auto-proposed names for tools and machines, derived from their
/// settings — the editor-autocompletion behavior of the name fields.
/// Pure logic so vitest covers it without the rune runtime.
///
/// Conventions: units follow the number with NO space ("3mm endmill",
/// not "3 mm endmill") so text search normalizes well, and words stay
/// lowercase for tools (matching the historical default names).
/// A name counts as "auto" while it's empty or still equals its own
/// suggestion — only then do setting edits rewrite it; a name the user
/// typed is never touched.

import type { ToolEntry, MachineSettings } from './project-types';

function fmt(v: number): string {
  return String(Math.round(v * 100) / 100);
}

type NameInputs = Pick<ToolEntry, 'kind' | 'diameter'> &
  Partial<Pick<ToolEntry, 'tipAngleDeg' | 'tipDiameter' | 'kerfMm'>>;

export function suggestToolName(t: NameInputs): string {
  const d = fmt(t.diameter);
  switch (t.kind) {
    case 'endmill':
      return `${d}mm endmill`;
    case 'ball_nose':
      return `${d}mm ball-nose`;
    case 'bull_nose':
      return `${d}mm bull-nose`;
    case 'compression':
      return `${d}mm compression`;
    case 'drill':
      return `${d}mm drill`;
    case 'thread_mill':
      return `${d}mm thread mill`;
    case 'form_profile':
      return `${d}mm form cutter`;
    case 'v_bit':
      return `${fmt(t.tipAngleDeg ?? 60)}° v-bit`;
    case 'cone':
      return `${fmt(t.tipAngleDeg ?? 30)}° cone`;
    case 'engraver':
      return t.tipDiameter != null && t.tipDiameter > 0
        ? `${fmt(t.tipDiameter)}mm engraver`
        : `${fmt(t.tipAngleDeg ?? 60)}° engraver`;
    case 'drag_knife':
      return 'drag knife';
    case 'laser_beam':
      return t.kerfMm != null && t.kerfMm > 0 ? `${fmt(t.kerfMm)}mm laser` : 'laser beam';
    case 'plasma_torch':
      return t.kerfMm != null && t.kerfMm > 0 ? `${fmt(t.kerfMm)}mm plasma torch` : 'plasma torch';
  }
}

/// Whether setting edits may rewrite this tool's name: it's empty, or
/// it still equals what the suggestion for the CURRENT settings would
/// be (i.e. the user never customized it).
export function isAutoToolName(t: NameInputs & Pick<ToolEntry, 'name'>): boolean {
  const name = t.name.trim();
  return name === '' || name === suggestToolName(t);
}

/// Machine-name proposal from its settings: primary mode + work area,
/// e.g. "Mill 200×300" or "Plasma 1500×3000".
export function suggestMachineName(
  machine: Pick<MachineSettings, 'mode'> & { workArea?: { x: number; y: number } },
): string {
  const mode =
    machine.mode === 'mill'
      ? 'Mill'
      : machine.mode === 'laser'
        ? 'Laser'
        : machine.mode === 'drag'
          ? 'Drag-knife'
          : 'Plasma';
  const wa = machine.workArea;
  return wa ? `${mode} ${fmt(wa.x)}×${fmt(wa.y)}` : mode;
}
