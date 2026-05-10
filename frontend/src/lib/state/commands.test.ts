/// Command-builder coverage. Each builder is exercised in isolation with
/// a plain CommandTarget mock so the tests don't pull in the Svelte
/// rune runtime. Apply → revert should restore byte-for-byte; coalesce
/// keys are checked for stability across repeated builds.

import { describe, expect, it } from 'vitest';
import {
  addFixtureCommand,
  addOperationCommand,
  addTabCommand,
  addToolCommand,
  appendImportedCommand,
  assignToolCommand,
  autoFixToCommand,
  changeProfileOffsetCommand,
  clearTabsCommand,
  deleteOperationCommand,
  deleteToolCommand,
  disableOpCommand,
  lowerSimResolutionCommand,
  removeFixtureCommand,
  removeTabCommand,
  reorderOperationCommand,
  replaceToolsCommand,
  setMachineCommand,
  setOpFieldCommand,
  setStockCommand,
  updateFixtureCommand,
  updateOperationCommand,
  type CommandTarget,
} from './commands';
import type { Fixture, MachineSettings, OpEntry, StockConfig, ToolEntry } from './project.svelte';

function blankTarget(): CommandTarget {
  return {
    imported: null,
    operations: [],
    tools: [],
    fixtures: [],
    tabs: {},
    machine: {
      unit: 'mm',
      mode: 'mill',
      comments: true,
      arcs: true,
      supportsToolchange: false,
      fastMoveZ: 5,
    } as MachineSettings,
    stock: {
      visible: true,
      mode: 'auto',
      margin: 5,
      thickness: 5,
      customX: 100,
      customY: 100,
    } as StockConfig,
    settings: {} as CommandTarget['settings'],
    dirty: false,
  };
}

function sampleOp(id: number, name = 'Op'): OpEntry {
  return {
    id,
    name,
    enabled: true,
    kind: 'profile',
    toolId: 1,
    sourceLayers: null,
    depth: -2,
    startDepth: 0,
    step: -1,
    offset: 'outside',
    pocketStrategy: null,
    sourceCombine: 'auto',
  };
}

function sampleTool(id: number): ToolEntry {
  return {
    id,
    name: `Tool ${id}`,
    kind: 'endmill',
    diameter: 3,
    flutes: 2,
    speed: 18000,
    plungeRate: 100,
    feedRate: 800,
    coolant: 'off',
  };
}

function sampleFixture(id: number): Fixture {
  return {
    id,
    name: `Fixture ${id}`,
    kind: { shape: 'box', width: 30, depth: 50 },
    origin: [0, 0],
    z_bottom: 0,
    z_top: 10,
    color: 0xff_a0_50_c0,
  };
}

describe('addOperationCommand', () => {
  it('apply appends; revert removes', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1)];
    const cmd = addOperationCommand(sampleOp(2));
    cmd.apply(t);
    expect(t.operations.map((o) => o.id)).toEqual([1, 2]);
    cmd.revert(t);
    expect(t.operations.map((o) => o.id)).toEqual([1]);
  });
});

describe('deleteOperationCommand', () => {
  it('undo restores at original index', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1, 'A'), sampleOp(2, 'B'), sampleOp(3, 'C')];
    const cmd = deleteOperationCommand(2);
    cmd.apply(t);
    expect(t.operations.map((o) => o.id)).toEqual([1, 3]);
    cmd.revert(t);
    expect(t.operations.map((o) => o.id)).toEqual([1, 2, 3]);
    expect(t.operations[1].name).toBe('B');
  });

  it('no-op when id missing', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1)];
    const cmd = deleteOperationCommand(99);
    cmd.apply(t);
    expect(t.operations.map((o) => o.id)).toEqual([1]);
    cmd.revert(t);
    expect(t.operations.map((o) => o.id)).toEqual([1]);
  });
});

describe('updateOperationCommand', () => {
  it('apply sets, revert restores prior fields', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1)];
    const cmd = updateOperationCommand(1, { depth: -5, name: 'Updated' });
    cmd.apply(t);
    expect(t.operations[0].depth).toBe(-5);
    expect(t.operations[0].name).toBe('Updated');
    cmd.revert(t);
    expect(t.operations[0].depth).toBe(-2);
    expect(t.operations[0].name).toBe('Op');
  });

  it('coalesce_key set for single-field patches', () => {
    const cmd = updateOperationCommand(7, { depth: -3 });
    expect(cmd.coalesce_key).toBe('setOpField:7:depth');
    const cmd2 = updateOperationCommand(7, { depth: -3, name: 'x' });
    expect(cmd2.coalesce_key).toBeUndefined();
  });
});

describe('setOpFieldCommand', () => {
  it('undo restores prior value', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1)];
    const cmd = setOpFieldCommand(1, 'depth', -7);
    cmd.apply(t);
    expect(t.operations[0].depth).toBe(-7);
    cmd.revert(t);
    expect(t.operations[0].depth).toBe(-2);
  });

  it('coalesce_key is consistent', () => {
    const a = setOpFieldCommand(3, 'depth', -1);
    const b = setOpFieldCommand(3, 'depth', -2);
    expect(a.coalesce_key).toBe(b.coalesce_key);
    expect(a.coalesce_key).toBe('setOpField:3:depth');
  });
});

describe('reorderOperationCommand', () => {
  it('apply moves; revert restores', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1), sampleOp(2), sampleOp(3)];
    const cmd = reorderOperationCommand(1, 2);
    cmd.apply(t);
    expect(t.operations.map((o) => o.id)).toEqual([2, 3, 1]);
    cmd.revert(t);
    expect(t.operations.map((o) => o.id)).toEqual([1, 2, 3]);
  });
});

describe('tools', () => {
  it('addToolCommand round-trip', () => {
    const t = blankTarget();
    t.tools = [sampleTool(1)];
    const cmd = addToolCommand(sampleTool(2));
    cmd.apply(t);
    expect(t.tools.map((x) => x.id)).toEqual([1, 2]);
    cmd.revert(t);
    expect(t.tools.map((x) => x.id)).toEqual([1]);
  });

  it('deleteToolCommand restores at index', () => {
    const t = blankTarget();
    t.tools = [sampleTool(1), sampleTool(2), sampleTool(3)];
    const cmd = deleteToolCommand(2);
    cmd.apply(t);
    expect(t.tools.map((x) => x.id)).toEqual([1, 3]);
    cmd.revert(t);
    expect(t.tools.map((x) => x.id)).toEqual([1, 2, 3]);
  });

  it('replaceToolsCommand swaps the whole array', () => {
    const t = blankTarget();
    t.tools = [sampleTool(1)];
    const next = [sampleTool(2), sampleTool(3)];
    const cmd = replaceToolsCommand(next);
    cmd.apply(t);
    expect(t.tools.map((x) => x.id)).toEqual([2, 3]);
    cmd.revert(t);
    expect(t.tools.map((x) => x.id)).toEqual([1]);
  });
});

describe('fixtures', () => {
  it('addFixture round-trip', () => {
    const t = blankTarget();
    const cmd = addFixtureCommand(sampleFixture(1));
    cmd.apply(t);
    expect(t.fixtures.map((x) => x.id)).toEqual([1]);
    cmd.revert(t);
    expect(t.fixtures).toEqual([]);
  });

  it('removeFixture preserves index', () => {
    const t = blankTarget();
    t.fixtures = [sampleFixture(1), sampleFixture(2), sampleFixture(3)];
    const cmd = removeFixtureCommand(2);
    cmd.apply(t);
    expect(t.fixtures.map((x) => x.id)).toEqual([1, 3]);
    cmd.revert(t);
    expect(t.fixtures.map((x) => x.id)).toEqual([1, 2, 3]);
  });

  it('updateFixture restores prior fields', () => {
    const t = blankTarget();
    t.fixtures = [sampleFixture(1)];
    const cmd = updateFixtureCommand(1, { name: 'Renamed', z_top: 99 });
    cmd.apply(t);
    expect(t.fixtures[0].name).toBe('Renamed');
    expect(t.fixtures[0].z_top).toBe(99);
    cmd.revert(t);
    expect(t.fixtures[0].name).toBe('Fixture 1');
    expect(t.fixtures[0].z_top).toBe(10);
  });
});

describe('tabs', () => {
  it('addTabCommand pushes to per-segment list', () => {
    const t = blankTarget();
    const cmd = addTabCommand(3, { x: 1, y: 2 });
    cmd.apply(t);
    expect(t.tabs[3]).toEqual([{ x: 1, y: 2 }]);
    cmd.revert(t);
    expect(t.tabs[3]).toBeUndefined();
  });

  it('removeTabCommand restores at position', () => {
    const t = blankTarget();
    t.tabs = {
      5: [
        { x: 1, y: 1 },
        { x: 2, y: 2 },
        { x: 3, y: 3 },
      ],
    };
    const cmd = removeTabCommand(5, 1);
    cmd.apply(t);
    expect(t.tabs[5]).toEqual([
      { x: 1, y: 1 },
      { x: 3, y: 3 },
    ]);
    cmd.revert(t);
    expect(t.tabs[5]).toEqual([
      { x: 1, y: 1 },
      { x: 2, y: 2 },
      { x: 3, y: 3 },
    ]);
  });

  it('clearTabsCommand restores all', () => {
    const t = blankTarget();
    t.tabs = { 5: [{ x: 1, y: 1 }], 7: [{ x: 9, y: 9 }] };
    const cmd = clearTabsCommand();
    cmd.apply(t);
    expect(t.tabs).toEqual({});
    cmd.revert(t);
    expect(t.tabs[5]).toEqual([{ x: 1, y: 1 }]);
    expect(t.tabs[7]).toEqual([{ x: 9, y: 9 }]);
  });
});

describe('machine / stock', () => {
  it('setMachineCommand swaps and restores', () => {
    const t = blankTarget();
    const next: MachineSettings = { ...t.machine, fastMoveZ: 99, mode: 'laser' };
    const cmd = setMachineCommand(next);
    cmd.apply(t);
    expect(t.machine.fastMoveZ).toBe(99);
    expect(t.machine.mode).toBe('laser');
    cmd.revert(t);
    expect(t.machine.fastMoveZ).toBe(5);
    expect(t.machine.mode).toBe('mill');
  });

  it('setStockCommand patches and restores', () => {
    const t = blankTarget();
    const cmd = setStockCommand({ margin: 12 });
    cmd.apply(t);
    expect(t.stock.margin).toBe(12);
    cmd.revert(t);
    expect(t.stock.margin).toBe(5);
    expect(cmd.coalesce_key).toBe('setStock:margin');
  });
});

describe('appendImportedCommand', () => {
  it('round-trips before / after snapshots', () => {
    const t = blankTarget();
    const after = {
      filename: 'text',
      format: 'text',
      bbox: { min_x: 0, min_y: 0, max_x: 10, max_y: 10 },
      layers: [],
      segments: [],
      unit_scale: 1,
      warnings: [],
      objects: [],
      object_meta: [],
    };
    const cmd = appendImportedCommand({ before: null, after });
    cmd.apply(t);
    expect(t.imported).not.toBeNull();
    expect(t.imported!.bbox.max_x).toBe(10);
    cmd.revert(t);
    expect(t.imported).toBeNull();
  });
});

describe('assignToolCommand', () => {
  it('apply sets toolId, revert restores it', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1)];
    const cmd = assignToolCommand(1, 42);
    cmd.apply(t);
    expect(t.operations[0].toolId).toBe(42);
    cmd.revert(t);
    expect(t.operations[0].toolId).toBe(1);
  });
});

describe('disableOpCommand', () => {
  it('toggles enabled and round-trips', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1)];
    const cmd = disableOpCommand(1);
    cmd.apply(t);
    expect(t.operations[0].enabled).toBe(false);
    cmd.revert(t);
    expect(t.operations[0].enabled).toBe(true);
  });
});

describe('changeProfileOffsetCommand', () => {
  it('swaps offset and round-trips', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1)];
    const cmd = changeProfileOffsetCommand(1, 'inside');
    cmd.apply(t);
    expect(t.operations[0].offset).toBe('inside');
    cmd.revert(t);
    expect(t.operations[0].offset).toBe('outside');
  });
});

describe('lowerSimResolutionCommand', () => {
  it('switches to manual mode + sets cellMm; revert restores', () => {
    const t = blankTarget();
    t.settings = {
      ...(t.settings as CommandTarget['settings']),
      cellResolutionMode: 'auto',
      cellResolutionMm: 0.2,
    };
    const cmd = lowerSimResolutionCommand(0.5);
    cmd.apply(t);
    expect(t.settings.cellResolutionMode).toBe('manual');
    expect(t.settings.cellResolutionMm).toBe(0.5);
    cmd.revert(t);
    expect(t.settings.cellResolutionMode).toBe('auto');
    expect(t.settings.cellResolutionMm).toBe(0.2);
  });
});

describe('autoFixToCommand', () => {
  it('AssignTool dispatches assignToolCommand', () => {
    const t = blankTarget();
    t.operations = [sampleOp(2)];
    const cmd = autoFixToCommand({
      kind: 'assign_tool',
      op_id: 2,
      suggested_tool_id: 9,
    });
    cmd.apply(t);
    expect(t.operations[0].toolId).toBe(9);
    cmd.revert(t);
    expect(t.operations[0].toolId).toBe(1);
  });

  it('DisableOp dispatches disableOpCommand', () => {
    const t = blankTarget();
    t.operations = [sampleOp(3)];
    const cmd = autoFixToCommand({ kind: 'disable_op', op_id: 3 });
    cmd.apply(t);
    expect(t.operations[0].enabled).toBe(false);
    cmd.revert(t);
    expect(t.operations[0].enabled).toBe(true);
  });

  it('ChangeProfileOffset dispatches changeProfileOffsetCommand', () => {
    const t = blankTarget();
    t.operations = [sampleOp(4)];
    const cmd = autoFixToCommand({
      kind: 'change_profile_offset',
      op_id: 4,
      suggested: 'inside',
    });
    cmd.apply(t);
    expect(t.operations[0].offset).toBe('inside');
    cmd.revert(t);
    expect(t.operations[0].offset).toBe('outside');
  });

  it('LowerSimResolution dispatches lowerSimResolutionCommand', () => {
    const t = blankTarget();
    t.settings = {
      ...(t.settings as CommandTarget['settings']),
      cellResolutionMode: 'auto',
      cellResolutionMm: 0.1,
    };
    const cmd = autoFixToCommand({
      kind: 'lower_sim_resolution',
      suggested_cell_mm: 0.3,
    });
    cmd.apply(t);
    expect(t.settings.cellResolutionMm).toBe(0.3);
    expect(t.settings.cellResolutionMode).toBe('manual');
  });
});
