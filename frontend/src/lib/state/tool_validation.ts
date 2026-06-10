// Pure tool-row validation, field-applicability, and disabled-field
// reasoning extracted from ToolLibraryDialog.svelte so the
// dialog stays a thin reactive shell and the predicates can be unit-
// tested without the rune runtime.
//
// The dialog wired all three concerns inline: validity flags (red-border
// classes + OK-button gate), per-kind field-applicability gates, and the
// tooltip strings explaining why a disabled field is disabled. They're
// pure functions of (kind, field, ToolEntry) — exactly the shape that
// pays off when you extract them.

import type { ToolEntry } from './project-types';
import type { ToolKind } from './op_types';
import { attrApplies, KIND_DISPLAY_LABELS } from './tool_family';

/// Diameter must be at or above the HTML min="0.01" mm floor. The
/// prior `> 0` check accepted values that downstream clamping silently
/// ignored, denying the user red-border feedback.
export function diameterInvalid(t: ToolEntry): boolean {
  return !(t.diameter >= 0.01);
}

/// Spindle speed (RPM) must be ≥ 1 when the kind spins at all. Drag-knife
/// and laser kinds don't apply.
export function speedInvalid(t: ToolEntry): boolean {
  if (!fieldApplies('speed', t.kind)) return false;
  return !(t.speed >= 1);
}

/// Feed rate (mm/min) is required for every cutting kind.
export function feedInvalid(t: ToolEntry): boolean {
  return !(t.feedRate >= 1);
}

/// Plunge rate (mm/min) must be ≥ 1 when the kind plunges. Drill, drag-
/// knife and laser kinds don't apply (drill folds plunge into feed).
export function plungeInvalid(t: ToolEntry): boolean {
  if (!fieldApplies('plunge', t.kind)) return false;
  return !(t.plungeRate >= 1);
}

/// Aggregate "this row blocks OK" check. Mirrors the dialog's
/// `hasInvalidRow = draft.some(rowInvalid)` derived gate.
export function rowInvalid(t: ToolEntry): boolean {
  return (
    diameterInvalid(t) ||
    speedInvalid(t) ||
    feedInvalid(t) ||
    plungeInvalid(t) ||
    // defaultStep is a depth: must be negative when set.
    (t.defaultStep !== undefined && t.defaultStep >= 0)
  );
}

/// Whether a given main-row field is meaningful for a tool kind.
/// Inapplicable fields are kept in the grid (so the row layout stays
/// stable) but disabled with a tooltip explaining why.
///
/// Most cases delegate to `attrApplies` (the shared capability table in
/// tool_family.ts); the special `coolant` case stays here because it's
/// a UX choice (generic "assist" toggle), not a geometry attribute.
export function fieldApplies(field: string, kind: ToolKind): boolean {
  switch (field) {
    case 'flutes':
      return attrApplies('flutes', kind);
    case 'tipDiameter':
      return attrApplies('tipDiameter', kind);
    case 'speed':
      return attrApplies('speed', kind);
    case 'plunge':
      return attrApplies('plunge', kind);
    case 'defaultStep':
      return attrApplies('defaultStep', kind);
    case 'tipAngleDeg':
      return attrApplies('tipAngleDeg', kind);
    case 'coolant':
      // Laser uses gas-assist (not implemented yet) — coolant dropdown
      // still applies as a generic "assist" toggle. Not a geometry
      // attribute, so it stays outside the family table.
      return true;
    default:
      return true;
  }
}

/// Whether the kind row's expanded section is load-bearing enough that
/// the dialog should auto-expand it. A kind needs auto-expansion
/// when it has a kind-specific attribute without which the emitter falls
/// back to wrong defaults — drag-knife dragoff, bull-nose corner radius,
/// or form-profile samples.
export function kindNeedsExpansion(kind: ToolKind): boolean {
  return (
    attrApplies('dragoff', kind) ||
    attrApplies('cornerRadius', kind) ||
    attrApplies('formProfile', kind)
  );
}

/// User-facing tooltip explaining why a disabled field is disabled for
/// the current tool kind. Returns the empty string when the field has no
/// per-kind reason — callers can fall through to a default tooltip.
export function fieldDisabledReason(field: string, kind: ToolKind): string {
  const k = KIND_DISPLAY_LABELS[kind];
  if (field === 'flutes' && kind === 'drag_knife') return `Drag-knife doesn't cut by rotation.`;
  if (field === 'flutes' && kind === 'laser_beam') return `Laser has no cutting edges.`;
  if (field === 'flutes') return `Flutes not used for ${k.toLowerCase()}.`;
  if (field === 'tipDiameter') return `Tip ⌀ only applies to V-bits / engravers.`;
  if (field === 'speed' && kind === 'drag_knife') return `Drag-knife doesn't spin.`;
  if (field === 'speed' && kind === 'laser_beam')
    return `Laser uses power (set in machine config), not RPM.`;
  if (field === 'plunge' && kind === 'drag_knife') return `Drag-knife stays at cut depth.`;
  if (field === 'plunge' && kind === 'laser_beam') return `Laser cuts at constant Z.`;
  if (field === 'plunge' && kind === 'drill') return `Drill uses the cut feed as its plunge rate.`;
  if (field === 'defaultStep' && kind === 'drill')
    return `Drill uses the peck step in the expanded section, not the generic Z step.`;
  if (field === 'defaultStep' && kind === 'drag_knife') return `Drag-knife runs at fixed depth.`;
  if (field === 'defaultStep' && kind === 'laser_beam') return `Laser cuts at constant Z.`;
  if (field === 'tipAngleDeg')
    return `Tip angle drives V-Carve depth math (V-bits / engravers) and the drill-tip 3D preview.`;
  return '';
}
