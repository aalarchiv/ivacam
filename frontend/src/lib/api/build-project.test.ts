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
import type { MachineSettings, ToolEntry, WorkOffset } from '../state/project-types';

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
    toolchangeStrategy: 'manual_m0_pause',
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

describe('buildTool — German wire contract (8njb)', () => {
  // The frontend uses English identifiers (cone, whirl*) but the backend
  // wire schema keeps the original German names. These lock that mapping
  // so an accidental rename of the wire keys would fail the build, not
  // silently desync the frontend from the Rust ToolKind / ToolEntry.
  it('maps the cone tool kind to the German wire value kegel', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool({ kind: 'cone', tipAngleDeg: 30 })],
      operations: [profileOp()],
    });
    expect(project!.tools[0]).toMatchObject({ kind: 'kegel' });
  });

  it('passes non-cone kinds through unchanged', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool({ kind: 'v_bit', tipAngleDeg: 60 })],
      operations: [profileOp()],
    });
    expect(project!.tools[0]).toMatchObject({ kind: 'v_bit' });
  });

  it('emits whirl* fields under their German wirbeln_* wire keys', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [
        baseTool({
          whirl: true,
          whirlStepoverMm: 0.75,
          whirlExtraWidthMm: 3,
          whirlOscMm: 0.2,
        }),
      ],
      operations: [profileOp()],
    });
    expect(project!.tools[0]).toMatchObject({
      wirbeln: true,
      wirbeln_stepover_mm: 0.75,
      wirbeln_extra_width_mm: 3,
      wirbeln_osc_mm: 0.2,
    });
    const tool = project!.tools[0] as unknown as Record<string, unknown>;
    expect(tool).not.toHaveProperty('whirl');
    expect(tool).not.toHaveProperty('whirlStepoverMm');
  });

  it('omits the whirl wire fields when disabled / at default', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
    });
    const tool = project!.tools[0] as unknown as Record<string, unknown>;
    expect(tool).not.toHaveProperty('wirbeln');
    expect(tool).not.toHaveProperty('wirbeln_stepover_mm');
  });
});

describe('buildTool — 1wit form-profile samples', () => {
  it('maps formProfileMm to snake_case wire samples when ≥2 rows', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [
        baseTool({
          kind: 'form_profile',
          formProfileMm: [
            { zMm: 0, rMm: 6.35 },
            { zMm: 9.5, rMm: 4 },
          ],
        }),
      ],
      operations: [profileOp()],
    });
    expect(project!.tools[0]).toMatchObject({
      form_profile_mm: [
        { z_mm: 0, r_mm: 6.35 },
        { z_mm: 9.5, r_mm: 4 },
      ],
    });
  });

  it('omits form_profile_mm with fewer than 2 samples (sim falls back to taper)', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool({ kind: 'form_profile', formProfileMm: [{ zMm: 0, rMm: 3 }] })],
      operations: [profileOp()],
    });
    const tool = project!.tools[0] as unknown as Record<string, unknown>;
    expect(tool).not.toHaveProperty('form_profile_mm');
  });

  it('omits form_profile_mm for non-form kinds even if samples linger', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [
        baseTool({
          kind: 'endmill',
          formProfileMm: [
            { zMm: 0, rMm: 3 },
            { zMm: 5, rMm: 3 },
          ],
        }),
      ],
      operations: [profileOp()],
    });
    const tool = project!.tools[0] as unknown as Record<string, unknown>;
    expect(tool).not.toHaveProperty('form_profile_mm');
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
    expect(m).not.toHaveProperty('optional_stop');
  });

  it('emits optional_stop when set (4lq5)', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: { ...baseMachine(), optionalStop: true },
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.machine).toMatchObject({ optional_stop: true });
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

describe('geometryView preference (8jce)', () => {
  function importWith(seg: number): ImportResponse {
    return {
      ...fakeImport(),
      segments: Array.from({ length: seg }, (_, i) => ({
        type: 'LINE' as const,
        start: { x: i, y: 0 },
        end: { x: i + 1, y: 0 },
        bulge: 0,
        layer: '0',
        color: 7,
      })),
    };
  }

  it('sends geometryView segments (with the stock outline) when present', () => {
    const project = buildProject({
      transformedImport: importWith(2),
      geometryView: importWith(6), // 2 import + 4 outline
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.segments).toHaveLength(6);
  });

  it('falls back to transformedImport when geometryView is absent', () => {
    const project = buildProject({
      transformedImport: importWith(2),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.segments).toHaveLength(2);
  });
});

describe('stock box (vrrr)', () => {
  it('resolves the auto-mode stock box from the transformedImport bbox + margin', () => {
    // fakeImport bbox is 0..10 on each axis; auto + margin 5 → -5..15.
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
      stock: {
        visible: true,
        mode: 'auto',
        margin: 5,
        thickness: 3,
        customX: 0,
        customY: 0,
      },
    });
    expect(project!.stock).toEqual({
      origin: [-5, -5],
      width_mm: 20,
      height_mm: 20,
      thickness_mm: 3,
    });
  });

  it('sizes the stock from transformedImport, NOT the stock-augmented geometryView, and floors thickness', () => {
    // geometryView's bbox is huge (it would include the stock outline).
    // Stock must resolve against transformedImport (0..10) so the auto
    // bbox does not balloon around the stock outline itself.
    const hugeView: ImportResponse = {
      ...fakeImport(),
      bbox: { min_x: -500, min_y: -500, max_x: 500, max_y: 500 },
    };
    const project = buildProject({
      transformedImport: fakeImport(), // bbox 0..10
      geometryView: hugeView,
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
      stock: { visible: true, mode: 'auto', margin: 0, thickness: 0, customX: 0, customY: 0 },
    });
    // auto + margin 0 against the 0..10 bbox → exactly 10×10; thickness 0
    // is floored to 0.01 mm (matching the old frontend boundsScan).
    expect(project!.stock).toEqual({
      origin: [0, 0],
      width_mm: 10,
      height_mm: 10,
      thickness_mm: 0.01,
    });
  });

  it('omits the stock key when no stock is modeled', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.stock).toBeUndefined();
  });
});

describe('relief mill (f60x-D)', () => {
  it('maps a relief_mill op + relief source to the wire shape', () => {
    const reliefOp = {
      id: 1,
      name: 'Relief',
      enabled: true,
      kind: 'relief_mill',
      toolId: 1,
      sourceLayers: null,
      depth: -3,
      startDepth: 0,
      step: -1,
      sourceId: 5,
      zMinMm: -3,
      zMaxMm: 0,
      invert: true,
      scallopHeightMm: 0.1,
      stepoverMm: null,
      scanDirection: 'along_y',
      alongStepMm: 0.4,
    } as unknown as OpEntry;
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool({ kind: 'ball_nose' })],
      operations: [reliefOp],
      reliefSources: [
        {
          id: 5,
          name: 'pic.png',
          origin: { x: 2, y: 3 },
          cell: 0.5,
          cols: 4,
          rows: 4,
          brightness: new Array(16).fill(0.5),
        },
      ],
    });
    const op = project!.operations[0];
    expect(op.kind).toEqual({
      type: 'relief_mill',
      source_id: 5,
      z_min_mm: -3,
      z_max_mm: 0,
      invert: true,
      scallop_height_mm: 0.1,
      // stepover_mm omitted when null (auto)
      scan_direction: 'along_y',
      along_step_mm: 0.4,
    });
    expect(project!.relief_sources).toEqual([
      {
        id: 5,
        name: 'pic.png',
        origin: [2, 3], // object → tuple
        cell: 0.5,
        cols: 4,
        rows: 4,
        brightness: new Array(16).fill(0.5),
      },
    ]);
  });

  it('omits relief_sources when none are present', () => {
    const project = buildProject({
      transformedImport: fakeImport(),
      machine: baseMachine(),
      tools: [baseTool()],
      operations: [profileOp()],
    });
    expect(project!.relief_sources).toBeUndefined();
  });
});
