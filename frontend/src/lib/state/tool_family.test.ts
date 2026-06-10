import { describe, it, expect } from 'vitest';
import type { ToolKind } from './op_types';
import {
  TOOL_COMPATIBLE_MODES,
  TOOL_FAMILY,
  attrApplies,
  effectiveModes,
  kindsForMode,
  kindsInFamily,
  machineModesLabel,
  toolCompatibleWithAnyMode,
  toolCompatibleWithMode,
  toolFamily,
} from './tool_family';

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
  'cone',
  'thread_mill',
  'plasma_torch',
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
    const noSpin: ToolKind[] = ['drag_knife', 'laser_beam', 'plasma_torch'];
    for (const k of ALL_KINDS) {
      expect(attrApplies('flutes', k)).toBe(!noSpin.includes(k));
      expect(attrApplies('speed', k)).toBe(!noSpin.includes(k));
      expect(attrApplies('plunge', k)).toBe(
        !['drag_knife', 'laser_beam', 'drill', 'plasma_torch'].includes(k),
      );
      // No generic Z step for drag-knife / laser / plasma / drill /
      // thread-mill.
      expect(attrApplies('defaultStep', k)).toBe(
        !['drag_knife', 'laser_beam', 'drill', 'thread_mill', 'plasma_torch'].includes(k),
      );
      // Conical family (v_bit, engraver, cone) carries tip geometry; a
      // thread mill carries no tip ⌀ but uses tipAngleDeg as the flank
      // angle.
      expect(attrApplies('tipDiameter', k)).toBe(['v_bit', 'engraver', 'cone'].includes(k));
      expect(attrApplies('tipAngleDeg', k)).toBe(
        ['v_bit', 'engraver', 'cone', 'drill', 'thread_mill'].includes(k),
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
    // Plasma section is gated on the torch KIND, not the machine mode.
    expect(attrApplies('plasma', 'plasma_torch')).toBe(true);
    expect(attrApplies('plasma', 'laser_beam')).toBe(false);
    expect(attrApplies('plasma', 'endmill')).toBe(false);
    expect(attrApplies('laser', 'plasma_torch')).toBe(false);
  });

  it('mirrors the Rust ToolKind::compatible_modes() table', () => {
    // Mirror of crates/ivac-core/src/project/tool.rs — if this changes,
    // update the Rust table (and vice versa).
    expect(TOOL_COMPATIBLE_MODES.engraver).toEqual(['mill', 'drag']);
    expect(TOOL_COMPATIBLE_MODES.plasma_torch).toEqual(['plasma']);
    expect(TOOL_COMPATIBLE_MODES.laser_beam).toEqual(['laser']);
    expect(TOOL_COMPATIBLE_MODES.drag_knife).toEqual(['drag']);
    for (const k of ALL_KINDS) {
      expect(TOOL_COMPATIBLE_MODES[k].length, `no modes for ${k}`).toBeGreaterThan(0);
    }
    expect(toolCompatibleWithMode('endmill', 'mill')).toBe(true);
    expect(toolCompatibleWithMode('endmill', 'plasma')).toBe(false);
    expect(toolCompatibleWithMode('engraver', 'drag')).toBe(true);
    expect(kindsForMode('plasma')).toEqual(['plasma_torch']);
    expect(kindsForMode('laser')).toEqual(['laser_beam']);
    expect(kindsForMode('drag')).toEqual(['engraver', 'drag_knife']);
    expect(kindsForMode('mill')).not.toContain('plasma_torch');
    expect(kindsForMode('mill')).not.toContain('laser_beam');
    expect(kindsForMode('mill')).not.toContain('drag_knife');
  });

  it('kindsInFamily preserves declaration order across families', () => {
    expect(kindsInFamily('cylindrical', 'radiused')).toEqual([
      'endmill',
      'ball_nose',
      'bull_nose',
      'compression',
    ]);
    expect(kindsInFamily('conical')).toEqual(['v_bit', 'engraver', 'cone']);
  });

  it('toolFamily round-trips', () => {
    expect(toolFamily('v_bit')).toBe('conical');
    expect(toolFamily('form_profile')).toBe('profile');
  });

  it('effectiveModes mirrors the Rust capability resolution', () => {
    // Empty / absent capabilities ⇒ just the primary mode.
    expect(effectiveModes({ mode: 'plasma' })).toEqual(['plasma']);
    expect(effectiveModes({ mode: 'mill', capabilities: [] })).toEqual(['mill']);
    // Non-empty capabilities ARE the effective set (deduped).
    expect(effectiveModes({ mode: 'plasma', capabilities: ['plasma', 'mill'] })).toEqual([
      'plasma',
      'mill',
    ]);
    expect(effectiveModes({ mode: 'mill', capabilities: ['mill', 'mill'] })).toEqual(['mill']);
  });

  it('toolCompatibleWithAnyMode covers combo machines', () => {
    expect(toolCompatibleWithAnyMode('endmill', ['mill', 'plasma'])).toBe(true);
    expect(toolCompatibleWithAnyMode('plasma_torch', ['mill', 'plasma'])).toBe(true);
    expect(toolCompatibleWithAnyMode('plasma_torch', ['mill'])).toBe(false);
    expect(toolCompatibleWithAnyMode('laser_beam', ['mill', 'plasma'])).toBe(false);
  });

  it('machineModesLabel joins mode nouns for combo machines', () => {
    expect(machineModesLabel(['plasma'])).toBe('plasma');
    expect(machineModesLabel(['mill', 'plasma'])).toBe('mill + plasma');
    expect(machineModesLabel(['drag'])).toBe('drag-knife');
  });
});
