import { describe, expect, it } from 'vitest';
import {
  duplicateProfile,
  newProfileId,
  profileFromCurrent,
  profileNameFor,
  profilePayload,
} from './machine_profiles';
import type { MachineProfile } from './workspace';
import type { MachineSettings, ToolEntry } from './project-types';

const machine = (name = ''): MachineSettings =>
  ({
    unit: 'mm',
    mode: 'plasma',
    comments: true,
    arcs: true,
    toolchangeStrategy: 'manual_m0_pause',
    fastMoveZ: 5,
    name,
  }) as MachineSettings;

const torch: ToolEntry = {
  id: 1,
  name: 'Plasma torch',
  kind: 'plasma_torch',
  diameter: 1.5,
  flutes: 0,
  speed: 0,
  plungeRate: 0,
  feedRate: 2000,
  coolant: 'off',
};

const profile = (id: string, name: string): MachineProfile => ({
  id,
  name,
  machine: machine(name),
  tools: [torch],
});

describe('machine_profiles helpers', () => {
  it('ids are unique-ish and prefixed', () => {
    expect(newProfileId()).toMatch(/^mp-/);
    expect(newProfileId()).not.toBe(newProfileId());
  });

  it('profileNameFor uses machine.name, else a non-colliding fallback', () => {
    expect(profileNameFor(machine('Plasma table'), [])).toBe('Plasma table');
    const existing = [profile('a', 'Machine 1'), profile('b', 'Machine 3')];
    // length+1 = 3 collides with 'Machine 3' → bumps to 4.
    expect(profileNameFor(machine(''), existing)).toBe('Machine 4');
  });

  it('profileFromCurrent deep-clones (no aliasing of the live project)', () => {
    const m = machine('Shop');
    const tools = [{ ...torch }];
    const p = profileFromCurrent(m, tools, []);
    tools[0].diameter = 99;
    m.fastMoveZ = 99;
    expect(p.tools[0].diameter).toBe(1.5);
    expect(p.machine.fastMoveZ).toBe(5);
    expect(p.name).toBe('Shop');
  });

  it('duplicateProfile picks a fresh "(copy)" name and id, and renames the machine to match', () => {
    const src = profile('mp-1', 'Plasma table');
    const existing = [src, profile('mp-2', 'Plasma table (copy)')];
    const dup = duplicateProfile(src, existing);
    expect(dup.id).not.toBe('mp-1');
    expect(dup.name).toBe('Plasma table (copy 2)');
    expect(dup.machine.name).toBe('Plasma table (copy 2)');
    expect(dup.tools).toHaveLength(1);
  });

  it('profilePayload deep-clones and survives migration passes', () => {
    const src = profile('mp-1', 'Plasma table');
    const { machine: m, tools } = profilePayload(src);
    expect(m.mode).toBe('plasma');
    expect(tools[0].kind).toBe('plasma_torch');
    // Clone, not alias.
    tools[0].diameter = 99;
    expect(src.tools[0].diameter).toBe(1.5);
  });
});
