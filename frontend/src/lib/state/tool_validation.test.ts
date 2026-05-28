import { describe, it, expect } from 'vitest';
import {
  diameterInvalid,
  speedInvalid,
  feedInvalid,
  plungeInvalid,
  rowInvalid,
  fieldApplies,
  fieldDisabledReason,
  kindNeedsExpansion,
} from './tool_validation';
import type { ToolEntry } from './project-types';
import type { ToolKind } from './op_types';

// Build a baseline-valid endmill row. Tests override fields as needed.
function tool(over: Partial<ToolEntry> = {}): ToolEntry {
  const raw: Record<string, unknown> = {
    id: 1,
    name: 'T1',
    kind: 'endmill',
    diameter: 6,
    flutes: 2,
    speed: 18000,
    plungeRate: 200,
    feedRate: 1200,
    coolant: 'off',
    ...over,
  };
  return raw as unknown as ToolEntry;
}

describe('tool_validation predicates', () => {
  it('a baseline endmill row is fully valid', () => {
    const t = tool();
    expect(diameterInvalid(t)).toBe(false);
    expect(speedInvalid(t)).toBe(false);
    expect(feedInvalid(t)).toBe(false);
    expect(plungeInvalid(t)).toBe(false);
    expect(rowInvalid(t)).toBe(false);
  });

  it('rejects diameters below the HTML floor (0.01 mm)', () => {
    expect(diameterInvalid(tool({ diameter: 0.005 }))).toBe(true);
    expect(diameterInvalid(tool({ diameter: 0 }))).toBe(true);
    expect(diameterInvalid(tool({ diameter: 0.01 }))).toBe(false);
  });

  it('speed: required ≥ 1 when the kind spins, ignored when it does not', () => {
    expect(speedInvalid(tool({ speed: 0 }))).toBe(true);
    expect(speedInvalid(tool({ speed: 1 }))).toBe(false);
    // Drag-knife and laser don't spin — speed value is irrelevant.
    expect(speedInvalid(tool({ kind: 'drag_knife', speed: 0 }))).toBe(false);
    expect(speedInvalid(tool({ kind: 'laser_beam', speed: 0 }))).toBe(false);
  });

  it('plunge: required ≥ 1 when the kind plunges, ignored for drill/drag-knife/laser', () => {
    expect(plungeInvalid(tool({ plungeRate: 0 }))).toBe(true);
    expect(plungeInvalid(tool({ kind: 'drill', plungeRate: 0 }))).toBe(false);
    expect(plungeInvalid(tool({ kind: 'drag_knife', plungeRate: 0 }))).toBe(false);
    expect(plungeInvalid(tool({ kind: 'laser_beam', plungeRate: 0 }))).toBe(false);
  });

  it('rowInvalid rejects a non-negative defaultStep (it is a depth, must be negative)', () => {
    expect(rowInvalid(tool({ defaultStep: -1 }))).toBe(false);
    expect(rowInvalid(tool({ defaultStep: 0 }))).toBe(true);
    expect(rowInvalid(tool({ defaultStep: 1 }))).toBe(true);
  });
});

describe('fieldApplies', () => {
  it('coolant is the only field always-on regardless of kind', () => {
    for (const k of ['endmill', 'drag_knife', 'laser_beam', 'drill'] as ToolKind[]) {
      expect(fieldApplies('coolant', k)).toBe(true);
    }
  });

  it('flutes / speed gated off for drag-knife and laser', () => {
    expect(fieldApplies('flutes', 'drag_knife')).toBe(false);
    expect(fieldApplies('flutes', 'laser_beam')).toBe(false);
    expect(fieldApplies('speed', 'drag_knife')).toBe(false);
    expect(fieldApplies('speed', 'laser_beam')).toBe(false);
  });

  it('plunge gated off for drill (drill uses feed)', () => {
    expect(fieldApplies('plunge', 'drill')).toBe(false);
    expect(fieldApplies('plunge', 'endmill')).toBe(true);
  });

  it('unknown field names default to applies=true (forward-compat for new inputs)', () => {
    expect(fieldApplies('unknown_field', 'endmill')).toBe(true);
  });
});

describe('kindNeedsExpansion', () => {
  it('auto-expands kinds with load-bearing kind-specific attributes', () => {
    expect(kindNeedsExpansion('drag_knife')).toBe(true); // dragoff
    expect(kindNeedsExpansion('bull_nose')).toBe(true); // cornerRadius
    expect(kindNeedsExpansion('form_profile')).toBe(true); // formProfile
  });

  it('does not auto-expand vanilla kinds whose defaults are safe', () => {
    expect(kindNeedsExpansion('endmill')).toBe(false);
    expect(kindNeedsExpansion('ball_nose')).toBe(false);
  });
});

describe('fieldDisabledReason', () => {
  it('returns kind-specific copy for known disabled combinations', () => {
    expect(fieldDisabledReason('speed', 'drag_knife')).toMatch(/doesn't spin/i);
    expect(fieldDisabledReason('plunge', 'laser_beam')).toMatch(/constant Z/i);
    expect(fieldDisabledReason('defaultStep', 'drill')).toMatch(/peck/i);
  });

  it('falls back to a generic "not used for <kind>" tooltip for flutes', () => {
    // Endmill has flutes — disabling flutes is unusual but the generic
    // branch should still produce a reason.
    expect(fieldDisabledReason('flutes', 'endmill')).toMatch(/not used for endmill/i);
  });

  it('returns the empty string when the field has no per-kind reason', () => {
    expect(fieldDisabledReason('coolant', 'endmill')).toBe('');
  });
});
