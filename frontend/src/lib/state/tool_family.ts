// Single-source tool-capability table (Phase 1 of the tool-library
// family-model refactor, epic wiaconstructor-l9zn).
//
// Before this module, "which settings apply to which tool kind" was
// hand-maintained in two places that drifted: `fieldApplies()` in
// ToolLibraryDialog (main-row inputs) AND the inline `{#if tool.kind
// === ...}` gates around the kind-specific sections. `expectedToolKinds`
// (op_tool_constraint.ts) repeated the kind groupings a third time, and
// the Rust sim (`heightmap::from_tool`) a fourth.
//
// The fix: classify every ToolKind into a geometry FAMILY whose shared
// parent geometry implies a base attribute set, then layer per-kind
// extras on top. `attrApplies(attr, kind)` is now the one predicate the
// dialog consults; `kindsInFamily(family)` is the one the op-constraint
// table composes from. The Rust side mirrors `ToolKind::family()` (see
// crates/ivac-core/src/project/tool.rs) — keep the two in sync.

import type { ToolKind } from './op_types';

/// Geometry families — the "shared parent implementation" tools group
/// under. Kinds in the same family carve with the same primitive shape
/// and differ only by constraints / extra attributes:
///   - cylindrical: flat bottom, straight wall (endmill, compression)
///   - radiused:    rounded bottom edge (ball-nose, bull-nose)
///   - conical:     cone from a tip radius to full radius (v-bit,
///                  engraver, and — Phase 2 — cone)
///   - profile:     arbitrary (z, r) cross-section (form-profile, and
///                  — Phase 4 — t-slot + dovetail as presets)
///   - drill:       conical point on a cylindrical body
///   - drag_knife:  non-rotating trailing blade
///   - laser:       non-contact beam (no physical radius)
export type ToolFamily =
  | 'cylindrical'
  | 'radiused'
  | 'conical'
  | 'profile'
  | 'drill'
  | 'drag_knife'
  | 'laser'
  | 'thread';

/// The gateable attributes a tool row can expose. Each maps to one or
/// more inputs / sub-sections in ToolLibraryDialog.
export type ToolAttr =
  | 'flutes'
  | 'tipDiameter'
  | 'tipAngleDeg'
  | 'speed'
  | 'plunge'
  | 'defaultStep'
  | 'cornerRadius' // bull-nose corner radius
  | 'dragoff' // drag-knife trailing offset
  | 'formProfile' // (z, r) sample table (incl. folded-in T-slot, z5yw)
  | 'compressionTransition' // compression up/down flute-split height
  | 'threadPitch' // thread-mill pitch
  | 'laser'; // pierce / lead-in / kerf

/// Kind → family. The authoritative classification; everything else
/// derives from it. Mirror of `ToolKind::family()` in Rust.
// Declaration order matters: `kindsInFamily()` preserves it, and
// op_tool_constraint composes its (order-sensitive) acceptable-tool
// arrays from family unions. Ordered flat → rounded → conical → drill →
// drag → profile → laser so those unions read naturally.
export const TOOL_FAMILY: Record<ToolKind, ToolFamily> = {
  endmill: 'cylindrical',
  ball_nose: 'radiused',
  bull_nose: 'radiused',
  compression: 'cylindrical',
  v_bit: 'conical',
  engraver: 'conical',
  cone: 'conical',
  drill: 'drill',
  drag_knife: 'drag_knife',
  form_profile: 'profile',
  laser_beam: 'laser',
  thread_mill: 'thread',
};

/// Base attribute set implied by each family. Per-kind extras are
/// layered on in `KIND_EXTRA_ATTRS`.
const FAMILY_BASE_ATTRS: Record<ToolFamily, readonly ToolAttr[]> = {
  // Rotating cutters with a flat or rounded bottom: full feed/speed/step
  // controls, no tip geometry.
  cylindrical: ['flutes', 'speed', 'plunge', 'defaultStep'],
  radiused: ['flutes', 'speed', 'plunge', 'defaultStep'],
  // Conical cutters add tip ⌀ + apex angle (the V-Carve depth math).
  conical: ['flutes', 'speed', 'plunge', 'defaultStep', 'tipDiameter', 'tipAngleDeg'],
  profile: ['flutes', 'speed', 'plunge', 'defaultStep'],
  // Drill: feed IS the plunge (no separate plunge), peck-step replaces
  // the generic Z step, and the conical point carries an apex angle.
  drill: ['flutes', 'speed', 'tipAngleDeg'],
  // Drag-knife doesn't spin or plunge — only the trailing offset.
  drag_knife: ['dragoff'],
  // Laser has no flutes / RPM / plunge — power + pierce/lead/kerf.
  laser: ['laser'],
  // Thread mill: flutes + RPM + a ramp-in plunge, the thread flank
  // angle (tipAngleDeg), and the pitch. No generic Z step — depth is
  // the thread, advanced helically by the op.
  thread: ['flutes', 'speed', 'plunge', 'tipAngleDeg', 'threadPitch'],
};

/// Per-kind attributes beyond the family base.
const KIND_EXTRA_ATTRS: Partial<Record<ToolKind, readonly ToolAttr[]>> = {
  bull_nose: ['cornerRadius'],
  compression: ['compressionTransition'],
  form_profile: ['formProfile'],
};

export function toolFamily(kind: ToolKind): ToolFamily {
  return TOOL_FAMILY[kind];
}

/// All tool kinds belonging to a family, in TOOL_FAMILY declaration
/// order. Used by op_tool_constraint to compose acceptable-tool sets
/// from family membership instead of repeating kind lists.
export function kindsInFamily(...families: ToolFamily[]): ToolKind[] {
  const set = new Set(families);
  return (Object.keys(TOOL_FAMILY) as ToolKind[]).filter((k) => set.has(TOOL_FAMILY[k]));
}

/// Whether a given attribute is meaningful for a tool kind — the single
/// predicate the dialog uses to enable inputs and show kind-specific
/// sections.
export function attrApplies(attr: ToolAttr, kind: ToolKind): boolean {
  const base = FAMILY_BASE_ATTRS[TOOL_FAMILY[kind]];
  if (base.includes(attr)) return true;
  const extra = KIND_EXTRA_ATTRS[kind];
  return extra ? extra.includes(attr) : false;
}

/// User-facing display label for each tool kind. Single source of truth
/// shared between the kind-selector dropdown, the disabled-field tooltip
/// reasons, and any other UI surface that names a tool kind.
export const KIND_DISPLAY_LABELS: Record<ToolKind, string> = {
  endmill: 'Endmill',
  ball_nose: 'Ball-nose',
  v_bit: 'V-bit',
  engraver: 'Engraver',
  drag_knife: 'Drag-knife',
  drill: 'Drill',
  laser_beam: 'Laser',
  bull_nose: 'Bull-nose (radius)',
  compression: 'Compression',
  form_profile: 'Form / profile',
  cone: 'Cone',
  thread_mill: 'Thread mill',
};
