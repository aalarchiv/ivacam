import { describe, it, expect } from 'vitest';
import type { ToolKind } from './op_types';
import { TOOL_FAMILY, attrApplies, kindsInFamily, toolFamily } from './tool_family';

const ALL_KINDS: ToolKind[] = [
  'endmill',
  'ball_nose',
  'v_bit',
  'engraver',
  'drag_knife',
  'drill',
  'laser_beam',
  'bull_nose',
  'compression',
  'form_profile',
  'kegel',
  'thread_mill',
];

describe('tool_family capability table', () => {
  it('classifies every ToolKind', () => {
    for (const k of ALL_KINDS) {
      expect(TOOL_FAMILY[k], `missing family for ${k}`).toBeDefined();
    }
  });

  it('reproduces the legacy fieldApplies matrix exactly', () => {
    // Snapshot of the pre-refactor fieldApplies() behavior — these must
    // not change for the 11 existing kinds (no behavior change in Phase 1).
    const noSpin: ToolKind[] = ['drag_knife', 'laser_beam'];
    for (const k of ALL_KINDS) {
      expect(attrApplies('flutes', k)).toBe(!noSpin.includes(k));
      expect(attrApplies('speed', k)).toBe(!noSpin.includes(k));
      expect(attrApplies('plunge', k)).toBe(!['drag_knife', 'laser_beam', 'drill'].includes(k));
      // No generic Z step for drag-knife / laser / drill / thread-mill.
      expect(attrApplies('defaultStep', k)).toBe(
        !['drag_knife', 'laser_beam', 'drill', 'thread_mill'].includes(k),
      );
      // Conical family (v_bit, engraver, kegel) carries tip geometry; a
      // thread mill carries no tip ⌀ but uses tipAngleDeg as the flank
      // angle.
      expect(attrApplies('tipDiameter', k)).toBe(['v_bit', 'engraver', 'kegel'].includes(k));
      expect(attrApplies('tipAngleDeg', k)).toBe(
        ['v_bit', 'engraver', 'kegel', 'drill', 'thread_mill'].includes(k),
      );
    }
  });

  it('gates kind-specific sections to the right kind', () => {
    expect(attrApplies('dragoff', 'drag_knife')).toBe(true);
    expect(attrApplies('dragoff', 'endmill')).toBe(false);
    expect(attrApplies('cornerRadius', 'bull_nose')).toBe(true);
    expect(attrApplies('cornerRadius', 'ball_nose')).toBe(false);
    expect(attrApplies('compressionTransition', 'compression')).toBe(true);
    expect(attrApplies('compressionTransition', 'endmill')).toBe(false);
    expect(attrApplies('threadPitch', 'thread_mill')).toBe(true);
    expect(attrApplies('threadPitch', 'endmill')).toBe(false);
    expect(attrApplies('formProfile', 'form_profile')).toBe(true);
    expect(attrApplies('laser', 'laser_beam')).toBe(true);
    expect(attrApplies('laser', 'endmill')).toBe(false);
  });

  it('kindsInFamily preserves declaration order across families', () => {
    expect(kindsInFamily('cylindrical', 'radiused')).toEqual([
      'endmill',
      'ball_nose',
      'bull_nose',
      'compression',
    ]);
    expect(kindsInFamily('conical')).toEqual(['v_bit', 'engraver', 'kegel']);
  });

  it('toolFamily round-trips', () => {
    expect(toolFamily('v_bit')).toBe('conical');
    expect(toolFamily('form_profile')).toBe('profile');
  });
});
