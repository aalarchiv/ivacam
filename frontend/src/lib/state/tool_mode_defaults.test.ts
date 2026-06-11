import { describe, expect, it } from 'vitest';
import type { MachineMode } from './op_types';
import { defaultKindForMode, defaultToolForMode } from './tool_mode_defaults';
import { toolCompatibleWithMode } from './tool_family';
import { rowInvalid } from './tool_validation';

const MODES: MachineMode[] = ['mill', 'laser', 'drag', 'plasma'];

describe('tool_mode_defaults', () => {
  it("seeds each mode's signature kind", () => {
    expect(defaultKindForMode('mill')).toBe('endmill');
    expect(defaultKindForMode('laser')).toBe('laser_beam');
    expect(defaultKindForMode('drag')).toBe('drag_knife');
    expect(defaultKindForMode('plasma')).toBe('plasma_torch');
  });

  it('every seeded default is compatible with its mode and passes row validation', () => {
    for (const mode of MODES) {
      const t = defaultToolForMode(mode, 7);
      expect(t.id).toBe(7);
      expect(t.kind).toBe(defaultKindForMode(mode));
      expect(toolCompatibleWithMode(t.kind, mode), `${mode} default incompatible`).toBe(true);
      expect(rowInvalid(t), `${mode} default fails row validation`).toBe(false);
    }
  });

  it('the torch carries the stock pierce entry sequence + kerf', () => {
    const torch = defaultToolForMode('plasma', 1);
    expect(torch.pierceHeightMm).toBe(3.8);
    expect(torch.cutHeightMm).toBe(1.5);
    expect(torch.pierceDelaySec).toBe(0.5);
    expect(torch.kerfMm).toBe(1.5);
  });

  it('the knife carries a trailing offset; the beam an explicit kerf', () => {
    expect(defaultToolForMode('drag', 1).dragoff).toBe(0.25);
    expect(defaultToolForMode('laser', 1).kerfMm).toBe(0.15);
  });

  it('the mill default matches the historical "+ Add tool" entry', () => {
    const t = defaultToolForMode('mill', 3);
    expect(t).toEqual({
      id: 3,
      name: '3mm endmill',
      kind: 'endmill',
      diameter: 3,
      flutes: 2,
      speed: 18000,
      plungeRate: 100,
      feedRate: 800,
      coolant: 'off',
    });
  });
});
