import { describe, expect, it } from 'vitest';
import { buildOpEntry, type OpDefaultsCtx } from './op_defaults';
import type { OpKind } from './op_types';
import type { ToolEntry } from './project-types';

// buildOpEntry only reads `id` / `kind` off each tool; a minimal cast keeps
// the fixtures readable without spelling out every ToolEntry field.
const tool = (id: number, kind: string): ToolEntry => ({ id, kind }) as unknown as ToolEntry;

function ctx(overrides: Partial<OpDefaultsCtx> = {}): OpDefaultsCtx {
  return {
    nextId: 7,
    tools: [tool(1, 'end_mill'), tool(2, 'ball_nose'), tool(3, 'laser_beam')],
    reliefSources: [{ id: 42 } as OpDefaultsCtx['reliefSources'][number]],
    selectionIds: [],
    objectMeta: [],
    ...overrides,
  };
}

describe('buildOpEntry — shared skeleton', () => {
  it('stamps id / name / enabled / sourceCombine / sourceLayers on every kind', () => {
    const op = buildOpEntry('profile', ctx());
    expect(op.id).toBe(7);
    expect(op.enabled).toBe(true);
    expect(op.sourceCombine).toBe('auto');
    expect(op.sourceLayers).toBeNull();
    expect(op.name.length).toBeGreaterThan(0);
  });
});

describe('buildOpEntry — program-only kinds (no tool, no geometry)', () => {
  it('pause: only a message, toolId 0', () => {
    const op = buildOpEntry('pause', ctx());
    expect(op.kind).toBe('pause');
    expect(op.toolId).toBe(0);
    expect(op).toMatchObject({ message: '' });
  });

  it('homing: retractToSafeZ default on', () => {
    expect(buildOpEntry('homing', ctx())).toMatchObject({
      kind: 'homing',
      toolId: 0,
      retractToSafeZ: true,
    });
  });

  it('probe: z axis, -10mm, 100 mm/min', () => {
    expect(buildOpEntry('probe', ctx())).toMatchObject({
      kind: 'probe',
      toolId: 0,
      axis: 'z',
      distanceMm: -10,
      feedMmMin: 100,
    });
  });

  it('cycle_marker: empty label', () => {
    expect(buildOpEntry('cycle_marker', ctx())).toMatchObject({
      kind: 'cycle_marker',
      toolId: 0,
      label: '',
    });
  });

  it('gcode_include: empty path/content, verbose off', () => {
    expect(buildOpEntry('gcode_include', ctx())).toMatchObject({
      kind: 'gcode_include',
      toolId: 0,
      path: '',
      content: '',
      verboseUnsimWarnings: false,
    });
  });
});

describe('buildOpEntry — surface kinds', () => {
  it('relief_mill: prefers a ball/bull tool, binds first relief source', () => {
    expect(buildOpEntry('relief_mill', ctx())).toMatchObject({
      kind: 'relief_mill',
      toolId: 2, // ball_nose
      depth: -2,
      startDepth: 0,
      step: -1,
      sourceId: 42,
      zMinMm: -2,
      zMaxMm: 0,
      invert: false,
      scallopHeightMm: 0.05,
      stepoverMm: null,
      scanDirection: 'along_x',
      alongStepMm: 0.5,
    });
  });

  it('relief_mill: falls back to first tool / id 1 when no ball/bull', () => {
    expect(buildOpEntry('relief_mill', ctx({ tools: [tool(5, 'end_mill')] })).toolId).toBe(5);
    expect(buildOpEntry('relief_mill', ctx({ tools: [] })).toolId).toBe(1);
  });

  it('raster_engrave: prefers a laser tool, linear S0..S1000', () => {
    expect(buildOpEntry('raster_engrave', ctx())).toMatchObject({
      kind: 'raster_engrave',
      toolId: 3, // laser_beam
      depth: 0,
      startDepth: 0,
      step: null,
      sourceId: 42,
      resolutionMm: 0.1,
      powerCurve: { kind: 'linear', min: 0, max: 1000 },
      scanDirection: 'along_x',
      link: 'lift_between',
      overscanFactor: 0,
    });
  });

  it('raster_engrave: sourceId 0 when no relief sources', () => {
    expect(buildOpEntry('raster_engrave', ctx({ reliefSources: [] }))).toMatchObject({
      sourceId: 0,
    });
  });
});

describe('buildOpEntry — geometry kinds', () => {
  it('profile: outside offset, no pocket strategy, conventional, first tool', () => {
    expect(buildOpEntry('profile', ctx())).toMatchObject({
      kind: 'profile',
      toolId: 1,
      depth: -2,
      startDepth: 0,
      step: -1,
      offset: 'outside',
      pocketStrategy: null,
      cutDirection: 'conventional',
      finishCutDirection: 'conventional',
      plunge: { kind: 'direct' },
      xyOverlap: 0.5,
    });
  });

  it('pocket: cascade strategy', () => {
    expect(buildOpEntry('pocket', ctx())).toMatchObject({
      offset: 'outside',
      pocketStrategy: 'cascade',
    });
  });

  it.each(['engrave', 'drag_knife', 't_slot', 'dovetail'] as OpKind[])(
    '%s: on-the-line offset',
    (kind) => {
      expect(buildOpEntry(kind, ctx())).toMatchObject({ offset: 'on' });
    },
  );

  it('drill: simple cycle default', () => {
    expect(buildOpEntry('drill', ctx())).toMatchObject({
      offset: 'outside',
      drillCycle: { kind: 'simple', dwell_sec: 0 },
    });
  });

  it('vcarve: multiPassRefine off by default', () => {
    expect(buildOpEntry('vcarve', ctx())).toMatchObject({ multiPassRefine: false });
  });

  it('pins to the canvas selection when one exists, omits sourceObjects otherwise', () => {
    expect(buildOpEntry('profile', ctx({ selectionIds: [3, 4] }))).toMatchObject({
      sourceObjects: [3, 4],
    });
    expect(buildOpEntry('profile', ctx({ selectionIds: [] }))).not.toHaveProperty('sourceObjects');
  });
});
