/// Per-op tool-kind constraint helper (k94n). Pure data — vitest
/// drives it without booting the Svelte rune runtime.

import { describe, expect, it } from 'vitest';
import {
  expectedToolKinds,
  formatExpectedToolKinds,
  isToolKindAcceptable,
} from './op_tool_constraint';

describe('expectedToolKinds', () => {
  it('returns the v_bit-only set for V-Carve', () => {
    expect(expectedToolKinds('vcarve')).toEqual(['v_bit']);
  });

  it('returns v_bit + engraver for Chamfer', () => {
    expect([...expectedToolKinds('chamfer')]).toEqual(['v_bit', 'engraver']);
  });

  it('returns drill + endmill for Drill', () => {
    expect([...expectedToolKinds('drill')]).toEqual(['drill', 'endmill']);
  });

  it('returns drag_knife-only for Drag knife', () => {
    expect([...expectedToolKinds('drag_knife')]).toEqual(['drag_knife']);
  });

  it('returns t_slot-only for T-Slot', () => {
    expect([...expectedToolKinds('t_slot')]).toEqual(['t_slot']);
  });

  it('returns form_profile-only for Dovetail (b7qz)', () => {
    expect([...expectedToolKinds('dovetail')]).toEqual(['form_profile']);
  });

  it('returns flat-bottom cutters for Pocket', () => {
    expect([...expectedToolKinds('pocket')]).toEqual([
      'endmill',
      'ball_nose',
      'bull_nose',
      'compression',
    ]);
  });

  it('returns the empty set for Pause (no tool needed)', () => {
    expect([...expectedToolKinds('pause')]).toEqual([]);
  });
});

describe('isToolKindAcceptable', () => {
  it('accepts a drill bit on a drill op', () => {
    expect(isToolKindAcceptable('drill', 'drill')).toBe(true);
  });

  it('accepts an endmill on a drill op (poor chip evacuation but works)', () => {
    expect(isToolKindAcceptable('drill', 'endmill')).toBe(true);
  });

  it('rejects a v-bit on a drill op', () => {
    expect(isToolKindAcceptable('drill', 'v_bit')).toBe(false);
  });

  it('rejects a drill on a V-Carve op', () => {
    expect(isToolKindAcceptable('vcarve', 'drill')).toBe(false);
  });

  it('rejects an endmill on a drag-knife op', () => {
    expect(isToolKindAcceptable('drag_knife', 'endmill')).toBe(false);
  });

  it('rejects a v-bit on a pocket op (tapers, not flat-bottom)', () => {
    expect(isToolKindAcceptable('pocket', 'v_bit')).toBe(false);
  });

  it('accepts anything on Pause', () => {
    expect(isToolKindAcceptable('pause', 'v_bit')).toBe(true);
    expect(isToolKindAcceptable('pause', 'drill')).toBe(true);
  });

  it('accepts undefined tool kind (no tool loaded yet)', () => {
    expect(isToolKindAcceptable('drill', undefined)).toBe(true);
  });
});

describe('formatExpectedToolKinds', () => {
  it('formats a single-item list as the label', () => {
    expect(formatExpectedToolKinds('vcarve')).toBe('V-bit');
  });

  it('formats two items with "or"', () => {
    expect(formatExpectedToolKinds('chamfer')).toBe('V-bit or engraver');
  });

  it('formats three+ items with Oxford-comma "or"', () => {
    expect(formatExpectedToolKinds('pocket')).toBe('endmill, ball-nose, bull-nose, or compression');
  });

  it('returns the empty string for op kinds with no constraint', () => {
    expect(formatExpectedToolKinds('pause')).toBe('');
  });
});
