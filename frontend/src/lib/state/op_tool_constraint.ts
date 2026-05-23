/// Per-op-kind acceptable-tool-kind constraints (k94n).
///
/// Each OpKind expects a SET of ToolKinds it can sensibly run with.
/// `expectedToolKinds('drill')` returns `['drill', 'endmill']` — endmills
/// CAN drill (poor chip evacuation but it works for shallow holes), so
/// they're acceptable; v-bits / drag-knives / lasers cannot.
///
/// `isToolKindAcceptable(opKind, toolKind)` is the boolean check used
/// by OpPropertiesPanel to surface a 'Tool kind mismatch' chip when
/// the assigned tool doesn't fit. `Pause` returns true for everything
/// because Pause carries no tool reference.

import type { OpKind, ToolKind } from './op_types';

export function expectedToolKinds(op: OpKind): readonly ToolKind[] {
  switch (op) {
    case 'profile':
      // Contouring along an outline — any rotating cutter with a
      // defined diameter works. Laser ablates along the outline at
      // constant Z and uses the kerf width as its effective cut
      // diameter, so it fits the same op kind. Drag-knife / drill
      // don't.
      return [
        'endmill',
        'ball_nose',
        'bull_nose',
        'compression',
        't_slot',
        'form_profile',
        'laser_beam',
      ];
    case 'pocket':
      // Pocketing needs a flat or near-flat bottom. V-bits / engravers
      // taper, so they leave wedge-shaped residue across the floor.
      return ['endmill', 'ball_nose', 'bull_nose', 'compression'];
    case 'drill':
      // Twist drill is the natural fit; endmills work for shallow
      // holes with poor chip evacuation. Anything else is wrong.
      return ['drill', 'endmill'];
    case 'thread':
      // Single-point thread mill (modeled as endmill or form_profile
      // until we add a dedicated kind).
      return ['endmill', 'form_profile'];
    case 'chamfer':
      // 45° (or other apex) bevel along an edge — V-bit or engraver.
      return ['v_bit', 'engraver'];
    case 'engrave':
      // Engraving uses V-bit / engraver. A small-diameter endmill works
      // too — many users engrave with a 0.5 mm flat tool. Laser
      // engraving (raster or vector along curves) is the natural fit
      // when the machine has a laser head — same op kind, kerf width
      // drives the line weight.
      return ['v_bit', 'engraver', 'endmill', 'laser_beam'];
    case 'drag_knife':
      // Dedicated kind — the post's pivot-arc compensation expects the
      // dragoff geometry.
      return ['drag_knife'];
    case 'vcarve':
      // Depth-vs-radius math assumes a conical cutter with a known
      // tip angle.
      return ['v_bit'];
    case 'pause':
      // No tool reference — accept anything (Op.tool_id may be 0).
      return [];
    default:
      return [];
  }
}

export function isToolKindAcceptable(op: OpKind, tool: ToolKind | undefined): boolean {
  if (op === 'pause') return true;
  if (tool == null) return true;
  const allowed = expectedToolKinds(op);
  if (allowed.length === 0) return true;
  return allowed.includes(tool);
}

const KIND_LABELS: Record<ToolKind, string> = {
  endmill: 'endmill',
  ball_nose: 'ball-nose',
  v_bit: 'V-bit',
  engraver: 'engraver',
  drag_knife: 'drag-knife',
  drill: 'drill',
  laser_beam: 'laser',
  bull_nose: 'bull-nose',
  compression: 'compression',
  t_slot: 'T-slot',
  form_profile: 'form profile',
};

/// Human-readable list for the "needs X / Y / Z" warning chip.
/// Renders as "drill or endmill" (2 items) or
/// "endmill, ball-nose, bull-nose, or compression" (≥3 items).
export function formatExpectedToolKinds(op: OpKind): string {
  const list = expectedToolKinds(op).map((k) => KIND_LABELS[k]);
  if (list.length === 0) return '';
  if (list.length === 1) return list[0];
  if (list.length === 2) return `${list[0]} or ${list[1]}`;
  return `${list.slice(0, -1).join(', ')}, or ${list[list.length - 1]}`;
}
