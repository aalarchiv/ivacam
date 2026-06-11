import { describe, expect, it } from 'vitest';
import { isAutoToolName, suggestMachineName, suggestToolName } from './tool_naming';

describe('suggestToolName', () => {
  it('units follow the number without a space', () => {
    expect(suggestToolName({ kind: 'endmill', diameter: 3 })).toBe('3mm endmill');
    expect(suggestToolName({ kind: 'ball_nose', diameter: 6 })).toBe('6mm ball-nose');
    expect(suggestToolName({ kind: 'drill', diameter: 4.2 })).toBe('4.2mm drill');
  });
  it('conical kinds name by angle; engraver prefers its tip diameter', () => {
    expect(suggestToolName({ kind: 'v_bit', diameter: 6, tipAngleDeg: 90 })).toBe('90° v-bit');
    expect(suggestToolName({ kind: 'cone', diameter: 6 })).toBe('30° cone');
    expect(suggestToolName({ kind: 'engraver', diameter: 6, tipDiameter: 0.2 })).toBe(
      '0.2mm engraver',
    );
    expect(suggestToolName({ kind: 'engraver', diameter: 6, tipAngleDeg: 45 })).toBe(
      '45° engraver',
    );
  });
  it('beam / torch name by kerf when set', () => {
    expect(suggestToolName({ kind: 'laser_beam', diameter: 0.15, kerfMm: 0.15 })).toBe(
      '0.15mm laser',
    );
    expect(suggestToolName({ kind: 'laser_beam', diameter: 0.15 })).toBe('laser beam');
    expect(suggestToolName({ kind: 'plasma_torch', diameter: 1.5, kerfMm: 1.5 })).toBe(
      '1.5mm plasma torch',
    );
    expect(suggestToolName({ kind: 'drag_knife', diameter: 0.9 })).toBe('drag knife');
  });
  it('rounds to two decimals without trailing zeros', () => {
    expect(suggestToolName({ kind: 'endmill', diameter: 3.175 })).toBe('3.18mm endmill');
    expect(suggestToolName({ kind: 'endmill', diameter: 6.0 })).toBe('6mm endmill');
  });
});

describe('isAutoToolName', () => {
  it('empty or own-suggestion names are auto; custom names are not', () => {
    expect(isAutoToolName({ kind: 'endmill', diameter: 3, name: '' })).toBe(true);
    expect(isAutoToolName({ kind: 'endmill', diameter: 3, name: '3mm endmill' })).toBe(true);
    expect(isAutoToolName({ kind: 'endmill', diameter: 3, name: 'my favourite bit' })).toBe(false);
    // A stale suggestion (settings changed since) counts as custom —
    // the rewrite happens at edit time, never retroactively.
    expect(isAutoToolName({ kind: 'endmill', diameter: 6, name: '3mm endmill' })).toBe(false);
  });
});

describe('suggestMachineName', () => {
  it('mode + work area', () => {
    expect(suggestMachineName({ mode: 'mill', workArea: { x: 200, y: 300 } })).toBe('Mill 200×300');
    expect(suggestMachineName({ mode: 'plasma', workArea: { x: 1500, y: 3000 } })).toBe(
      'Plasma 1500×3000',
    );
    expect(suggestMachineName({ mode: 'drag' })).toBe('Drag-knife');
  });
});
