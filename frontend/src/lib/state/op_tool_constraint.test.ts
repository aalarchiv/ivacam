/// Per-op tool-kind constraint helper. Pure data — vitest
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

  it('returns the conical family (v_bit + engraver + cone) for Chamfer', () => {
    expect([...expectedToolKinds('chamfer')]).toEqual(['v_bit', 'engraver', 'cone']);
  });

  it('returns drill + endmill for Drill', () => {
    expect([...expectedToolKinds('drill')]).toEqual(['drill', 'endmill']);
  });

  it('returns drag_knife-only for Drag knife', () => {
    expect([...expectedToolKinds('drag_knife')]).toEqual(['drag_knife']);
  });

  it('returns form_profile for T-Slot (folded into the profile family)', () => {
    expect([...expectedToolKinds('t_slot')]).toEqual(['form_profile']);
  });

  it('prefers a thread mill (with endmill/form_profile fallback) for Thread', () => {
    expect([...expectedToolKinds('thread')]).toEqual(['thread_mill', 'endmill', 'form_profile']);
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

  it('accepts beam + torch kerf cutters for Profile', () => {
    const kinds = expectedToolKinds('profile');
    expect(kinds).toContain('laser_beam');
    expect(kinds).toContain('plasma_torch');
    expect(kinds).not.toContain('drag_knife');
    expect(kinds).not.toContain('drill');
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
    expect(formatExpectedToolKinds('drill')).toBe('drill or endmill');
  });

  it('formats the conical family with Oxford-comma "or"', () => {
    expect(formatExpectedToolKinds('chamfer')).toBe('V-bit, engraver, or cone');
  });

  it('formats three+ items with Oxford-comma "or"', () => {
    expect(formatExpectedToolKinds('pocket')).toBe('endmill, ball-nose, bull-nose, or compression');
  });

  it('returns the empty string for op kinds with no constraint', () => {
    expect(formatExpectedToolKinds('pause')).toBe('');
  });
});
