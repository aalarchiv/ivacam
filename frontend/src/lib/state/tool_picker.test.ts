/// Pure-logic tests for the best-fit tool picker.

import { describe, expect, it } from 'vitest';
import { pickBestToolForOp, pickBestDrillTool, inferDrillDiameterMm } from './tool_picker';
import type { components } from '../api/generated';
import type { ToolEntry } from './project-types';

type ImportedObject = components['schemas']['ImportedObject'];

const drill = (id: number, diameter: number, name = `drill ${diameter}`): ToolEntry =>
  ({
    id,
    name,
    kind: 'drill',
    diameter,
    flutes: 2,
    speed: 6000,
    plungeRate: 100,
    feedRate: 100,
    coolant: 'off',
    pause: 1,
    tipAngleDeg: 60,
  }) as unknown as ToolEntry;

const endmill = (id: number, diameter: number): ToolEntry =>
  ({
    id,
    name: `em ${diameter}`,
    kind: 'endmill',
    diameter,
    flutes: 2,
    speed: 18000,
    plungeRate: 100,
    feedRate: 800,
    coolant: 'off',
    pause: 1,
    tipAngleDeg: 60,
  }) as unknown as ToolEntry;

const vbit = (id: number, diameter: number): ToolEntry =>
  ({
    id,
    name: `vbit ${diameter}`,
    kind: 'v_bit',
    diameter,
    flutes: 2,
    speed: 18000,
    plungeRate: 100,
    feedRate: 800,
    coolant: 'off',
    pause: 1,
    tipAngleDeg: 60,
  }) as unknown as ToolEntry;

const circleObject = (id: number, diameter: number): ImportedObject => ({
  id,
  closed: true,
  color: 7,
  layer: '0',
  bbox: { min_x: 0, min_y: 0, max_x: diameter, max_y: diameter },
});

const slotObject = (id: number, w: number, h: number): ImportedObject => ({
  id,
  closed: true,
  color: 7,
  layer: '0',
  bbox: { min_x: 0, min_y: 0, max_x: w, max_y: h },
});

describe('inferDrillDiameterMm', () => {
  it('returns null on empty selection', () => {
    expect(inferDrillDiameterMm([], [])).toBeNull();
  });

  it('returns null when selected ids do not resolve to meta', () => {
    expect(inferDrillDiameterMm([99], [circleObject(1, 6)])).toBeNull();
  });

  it('returns null for non-square bboxes (slots)', () => {
    expect(inferDrillDiameterMm([1], [slotObject(1, 6, 12)])).toBeNull();
  });

  it('returns the diameter for a single square-ish object', () => {
    expect(inferDrillDiameterMm([1], [circleObject(1, 6)])).toBeCloseTo(6, 6);
  });

  it('returns the MIN diameter across multiple selected objects', () => {
    // 3mm, 6mm, 8mm circles selected → tool should fit the smallest.
    const meta = [circleObject(1, 3), circleObject(2, 6), circleObject(3, 8)];
    expect(inferDrillDiameterMm([1, 2, 3], meta)).toBeCloseTo(3, 6);
  });

  it('treats bboxes within the 10% squareness tolerance as round', () => {
    // 6.0 × 6.5 = 8.3% asymmetry → still round.
    const meta = [slotObject(1, 6.0, 6.5)];
    expect(inferDrillDiameterMm([1], meta)).toBeCloseTo(6.25, 6);
  });
});

describe('pickBestDrillTool', () => {
  it('returns null when no drill or endmill tools exist', () => {
    expect(pickBestDrillTool(6, [vbit(1, 6)])).toBeNull();
  });

  it('prefers exact match within tolerance', () => {
    const tools = [drill(1, 3), drill(2, 6), drill(3, 8)];
    expect(pickBestDrillTool(6, tools)?.id).toBe(2);
  });

  it('treats sub-tolerance differences as exact', () => {
    const tools = [drill(1, 5.98), drill(2, 6.5)];
    expect(pickBestDrillTool(6, tools)?.id).toBe(1);
  });

  it('falls through to next-smaller when no exact', () => {
    const tools = [drill(1, 3), drill(2, 5), drill(3, 8)];
    expect(pickBestDrillTool(6, tools)?.id).toBe(2);
  });

  it('falls through to next-larger when no smaller exists', () => {
    const tools = [drill(1, 8), drill(2, 12)];
    expect(pickBestDrillTool(6, tools)?.id).toBe(1);
  });

  it('considers both drills AND endmills', () => {
    // Endmill closest in size wins even when only drills are smaller.
    const tools = [drill(1, 3), endmill(2, 6.02)];
    expect(pickBestDrillTool(6, tools)?.id).toBe(2);
  });

  it('ignores v-bits and other non-drillable tools', () => {
    // V-bit 6mm should NOT be picked — only drill/endmill.
    const tools = [vbit(1, 6), drill(2, 4)];
    expect(pickBestDrillTool(6, tools)?.id).toBe(2);
  });
});

describe('pickBestToolForOp', () => {
  it('returns null when the tool library is empty', () => {
    expect(pickBestToolForOp('drill', [1], [circleObject(1, 6)], [])).toBeNull();
  });

  it('returns tools[0] for non-drill ops regardless of selection', () => {
    const tools = [endmill(1, 3), endmill(2, 6)];
    const meta = [circleObject(1, 6)];
    expect(pickBestToolForOp('pocket', [1], meta, tools)?.id).toBe(1);
    expect(pickBestToolForOp('profile', [1], meta, tools)?.id).toBe(1);
  });

  it('picks best drill diameter for a Drill op on a square selection', () => {
    const tools = [drill(1, 3), drill(2, 6), drill(3, 8)];
    const meta = [circleObject(1, 6)];
    expect(pickBestToolForOp('drill', [1], meta, tools)?.id).toBe(2);
  });

  it('falls back to tools[0] for Drill when selection is non-square', () => {
    const tools = [endmill(1, 3), endmill(2, 6)];
    const meta = [slotObject(1, 6, 12)];
    // Selection is a slot — no inferred diameter — so default tool 1.
    expect(pickBestToolForOp('drill', [1], meta, tools)?.id).toBe(1);
  });

  it('falls back to tools[0] for Drill with empty selection', () => {
    const tools = [endmill(1, 3), drill(2, 6)];
    expect(pickBestToolForOp('drill', [], [], tools)?.id).toBe(1);
  });
});
