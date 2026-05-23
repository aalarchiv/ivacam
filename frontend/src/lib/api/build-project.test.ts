/// mqap / j4tv: round-trip tests for the wire-side coverage of the
/// audit-added Project / ToolEntry fields. Prior to mqap + j4tv the
/// frontend silently dropped `spindle_direction`, `stickout_length_mm`,
/// `kerf_mm` (ToolEntry) and `work_offset` (Project) on the FE→Rust
/// boundary — these tests fail loudly if anyone re-introduces that
/// regression.

import { describe, expect, it } from 'vitest';
import { buildProject } from './build-project';
import type { ImportResponse } from './types';
import type { OpEntry } from '../state/op_types';
import type {
  MachineSettings,
  ToolEntry,
  WorkOffset,
} from '../state/project-types';

function fakeImport(): ImportResponse {
  return {
    bbox: { min_x: 0, min_y: 0, max_x: 10, max_y: 10 },
    filename: 'test.dxf',
    format: 'dxf',
    layers: [],
    object_meta: [],
    objects: [],
    segments: [],
    unit_scale: 1,
    warnings: [],
  };
}

function baseTool(over: Partial<ToolEntry> = {}): ToolEntry {
  return {
    id: 1,
    name: 'test',
    kind: 'endmill',
    diameter: 3,
    flutes: 2,
    speed: 18000,
    plungeRate: 100,
    feedRate: 800,
    coolant: 'off',
    ...over,
  };
}

function baseMachine(): MachineSettings {
  return {
    unit: 'mm',
    mode: 'mill',
    comments: true,
    arcs: true,
    supportsToolchange: false,
    fastMoveZ: 5,
  };
}

function profileOp(): OpEntry {
  // Minimal profile op — buildProject demands at least one op or it
  // returns null.
  return {
    id: 1,
    name: 'cut',
    enabled: true,
    kind: 'profile',
    toolId: 1,
    sourceLayers: null,
    depth: -3,
    startDepth: 0,
    step: null,
    offset: 'outside',
  };
}

describe('buildTool — mqap audit fields', () => {
  it('omits all three optional fields when at default', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project).not.toBeNull();
    const tool = project!.tools[0] as unknown as Record<string, unknown>;
    expect(tool).not.toHaveProperty('spindle_direction');
    expect(tool).not.toHaveProperty('stickout_length_mm');
    expect(tool).not.toHaveProperty('kerf_mm');
  });

  it('emits spindle_direction when ccw', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool({ spindleDirection: 'ccw' })],
      operations: [profileOp()],
    });
    expect(project!.tools[0]).toMatchObject({ spindle_direction: 'ccw' });
  });

  it('skips spindle_direction when explicitly cw (default)', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool({ spindleDirection: 'cw' })],
      operations: [profileOp()],
    });
    const tool = project!.tools[0] as unknown as Record<string, unknown>;
    expect(tool).not.toHaveProperty('spindle_direction');
  });

  it('emits stickout_length_mm when > 0', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool({ stickoutLengthMm: 12.5 })],
      operations: [profileOp()],
    });
    expect(project!.tools[0]).toMatchObject({ stickout_length_mm: 12.5 });
  });

  it('skips stickout_length_mm when 0', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool({ stickoutLengthMm: 0 })],
      operations: [profileOp()],
    });
    const tool = project!.tools[0] as unknown as Record<string, unknown>;
    expect(tool).not.toHaveProperty('stickout_length_mm');
  });

  it('emits kerf_mm when > 0', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool({ kind: 'laser_beam', kerfMm: 0.05 })],
      operations: [profileOp()],
    });
    expect(project!.tools[0]).toMatchObject({ kerf_mm: 0.05 });
  });

  it('skips kerf_mm when undefined', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool({ kind: 'laser_beam' })],
      operations: [profileOp()],
    });
    const tool = project!.tools[0] as unknown as Record<string, unknown>;
    expect(tool).not.toHaveProperty('kerf_mm');
  });
});

describe('buildProject — j4tv work_offset wiring', () => {
  it('omits work_offset entirely when at default (all-zero @ G54)', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
      workOffset: { x_mm: 0, y_mm: 0, z_mm: 0, wcs: 'G54' },
    });
    expect(project).not.toHaveProperty('work_offset');
  });

  it('omits work_offset when state has no workOffset (legacy projects)', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project).not.toHaveProperty('work_offset');
  });

  it('emits non-zero x_mm only', () => {
    const wo: WorkOffset = { x_mm: 12.5, y_mm: 0, z_mm: 0, wcs: 'G54' };
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
      workOffset: wo,
    });
    expect(project!.work_offset).toEqual({ x_mm: 12.5 });
  });

  it('emits wcs when non-default G55', () => {
    const wo: WorkOffset = { x_mm: 0, y_mm: 0, z_mm: 0, wcs: 'G55' };
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
      workOffset: wo,
    });
    expect(project!.work_offset).toEqual({ wcs: 'G55' });
  });

  it('emits full offset (x + y + z + wcs)', () => {
    const wo: WorkOffset = { x_mm: 5, y_mm: -3, z_mm: 1.5, wcs: 'G56' };
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
      workOffset: wo,
    });
    expect(project!.work_offset).toEqual({
      x_mm: 5,
      y_mm: -3,
      z_mm: 1.5,
      wcs: 'G56',
    });
  });
});

// Round-3 P2: MachineDialog now exposes spindle_rpm_min/max,
// spindle_start_dwell_sec, spindle_stop_dwell_sec, park_at_home,
// park_xy. The wire layer skips each on default so legacy projects
// round-trip unchanged; with all six set the WireMachine carries the
// canonical snake_case names the Rust serde derive expects.
describe('buildMachine — Round-3 spindle clamps & parking', () => {
  it('omits every spindle / park field when at default', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
    });
    const m = project!.machine as unknown as Record<string, unknown>;
    expect(m).not.toHaveProperty('spindle_rpm_min');
    expect(m).not.toHaveProperty('spindle_rpm_max');
    expect(m).not.toHaveProperty('spindle_start_dwell_sec');
    expect(m).not.toHaveProperty('spindle_stop_dwell_sec');
    expect(m).not.toHaveProperty('park_at_home');
    expect(m).not.toHaveProperty('park_xy');
  });

  it('emits spindle_rpm_min / max when set', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: { ...baseMachine(), spindleRpmMin: 6000, spindleRpmMax: 24000 },
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.machine).toMatchObject({
      spindle_rpm_min: 6000,
      spindle_rpm_max: 24000,
    });
  });

  it('emits spindle_start_dwell_sec and spindle_stop_dwell_sec when set', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: {
        ...baseMachine(),
        spindleStartDwellSec: 1.2,
        spindleStopDwellSec: 2.5,
      },
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.machine).toMatchObject({
      spindle_start_dwell_sec: 1.2,
      spindle_stop_dwell_sec: 2.5,
    });
  });

  it('emits park_at_home when true', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: { ...baseMachine(), parkAtHome: true },
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.machine).toMatchObject({ park_at_home: true });
  });

  it('emits park_xy when park_at_home is false', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: { ...baseMachine(), parkAtHome: false, parkXy: [150, 75] },
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.machine).toMatchObject({ park_xy: [150, 75] });
    const m = project!.machine as unknown as Record<string, unknown>;
    expect(m).not.toHaveProperty('park_at_home');
  });

  it('drops park_xy when park_at_home is true (ambiguous combo)', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: { ...baseMachine(), parkAtHome: true, parkXy: [150, 75] },
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.machine).toMatchObject({ park_at_home: true });
    const m = project!.machine as unknown as Record<string, unknown>;
    expect(m).not.toHaveProperty('park_xy');
  });

  it('round-trips all six fields together', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: {
        ...baseMachine(),
        spindleRpmMin: 8000,
        spindleRpmMax: 30000,
        spindleStartDwellSec: 0.75,
        spindleStopDwellSec: 1.5,
        parkAtHome: false,
        parkXy: [200, 100],
      },
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.machine).toMatchObject({
      spindle_rpm_min: 8000,
      spindle_rpm_max: 30000,
      spindle_start_dwell_sec: 0.75,
      spindle_stop_dwell_sec: 1.5,
      park_xy: [200, 100],
    });
    const m = project!.machine as unknown as Record<string, unknown>;
    expect(m).not.toHaveProperty('park_at_home');
  });
});
