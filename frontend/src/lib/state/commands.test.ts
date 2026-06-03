/// Command-builder coverage. Each builder is exercised in isolation with
/// a plain CommandTarget mock so the tests don't pull in the Svelte
/// rune runtime. Apply → revert should restore byte-for-byte; coalesce
/// keys are checked for stability across repeated builds.

import { describe, expect, it } from 'vitest';
import {
  addFixtureCommand,
  addOperationCommand,
  addToolCommand,
  assignToolCommand,
  autoFixToCommand,
  changeProfileOffsetCommand,
  deleteOperationCommand,
  deleteToolCommand,
  disableOpCommand,
  duplicateOperationCommand,
  lowerSimResolutionCommand,
  removeFixtureCommand,
  reorderOperationCommand,
  replaceToolsCommand,
  setGroupOpsByToolCommand,
  setMachineCommand,
  setOpFieldCommand,
  setStockCommand,
  setWorkOffsetCommand,
  toggleTabPlacementCommand,
  updateFixtureCommand,
  updateOperationCommand,
  addTextLayerCommand,
  deleteTextLayerCommand,
  updateTextLayerCommand,
  updateReliefSourceCommand,
  type CommandTarget,
} from './commands';
import type {
  Fixture,
  MachineSettings,
  OpEntry,
  StockConfig,
  TextLayer,
  ToolEntry,
} from './project.svelte';
import type { ProfileOp } from './op_types';

function sampleTextLayer(id: number, text = 'Hello'): TextLayer {
  return {
    id,
    kind: 'TEXT',
    name: `TEXT — "${text}"`,
    text,
    fontSource: { kind: 'bundled', path: '/fonts/DejaVuSans.ttf', bytes_b64: '' },
    sizeMm: 12,
    origin: { x: 0, y: 0 },
    rotationDeg: 0,
    letterSpacingMm: 0,
    lineSpacingMm: 0,
    alignment: 'left',
    widthScale: 1.0,
    singleLine: false,
  };
}

function blankTarget(): CommandTarget {
  return {
    operations: [],
    tools: [],
    fixtures: [],
    machine: {
      unit: 'mm',
      mode: 'mill',
      comments: true,
      arcs: true,
      toolchangeStrategy: 'manual_m0_pause',
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
    textLayers: [],
    reliefSources: [],
    imports: [],
    workOffset: { x_mm: 0, y_mm: 0, z_mm: 0, wcs: 'G54' },
    groupOpsByTool: false,
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

describe('duplicateOperationCommand', () => {
  it('inserts the clone immediately after the source row', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1, 'A'), sampleOp(2, 'B'), sampleOp(3, 'C')];
    const copy: OpEntry = { ...sampleOp(99, 'B (copy)') };
    const cmd = duplicateOperationCommand(2, copy, 2);
    cmd.apply(t);
    expect(t.operations.map((o) => o.id)).toEqual([1, 2, 99, 3]);
    expect(t.operations[2].name).toBe('B (copy)');
    expect(t.dirty).toBe(true);
  });

  it('revert removes the inserted op', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1, 'A'), sampleOp(2, 'B')];
    const copy: OpEntry = { ...sampleOp(7, 'A (copy)') };
    const cmd = duplicateOperationCommand(1, copy, 1);
    cmd.apply(t);
    expect(t.operations.map((o) => o.id)).toEqual([1, 7, 2]);
    cmd.revert(t);
    expect(t.operations.map((o) => o.id)).toEqual([1, 2]);
  });

  it('appends when insertAfter id is unknown', () => {
    const t = blankTarget();
    t.operations = [sampleOp(1), sampleOp(2)];
    const copy: OpEntry = { ...sampleOp(5, 'rogue') };
    const cmd = duplicateOperationCommand(1, copy, 999);
    cmd.apply(t);
    expect(t.operations.map((o) => o.id)).toEqual([1, 2, 5]);
    cmd.revert(t);
    expect(t.operations.map((o) => o.id)).toEqual([1, 2]);
  });

  it('round-trip is independent of source-state mutation', () => {
    const t = blankTarget();
    const src = sampleOp(1, 'orig');
    t.operations = [src];
    const copy: OpEntry = { ...sampleOp(2, 'orig (copy)'), depth: -9 };
    const cmd = duplicateOperationCommand(1, copy, 1);
    cmd.apply(t);
    t.operations = t.operations.map((o) => (o.id === 1 ? { ...o, depth: -42 } : o));
    const inserted = t.operations.find((o) => o.id === 2)!;
    expect(inserted.depth).toBe(-9);
    cmd.revert(t);
    expect(t.operations.find((o) => o.id === 2)).toBeUndefined();
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

describe('tabs (rt1.10)', () => {
  function withOp(t: CommandTarget, opId: number, placements: { objectId: number; t: number }[]) {
    t.operations = [
      {
        id: opId,
        name: 'Profile',
        enabled: true,
        kind: 'profile',
        toolId: 1,
        offset: 'outside',
        depth: -2,
        startDepth: 0,
        step: -1,
        sourceLayers: null,
        tabPlacements: placements.map((p) => ({ objectId: p.objectId, t: p.t })),
      } as OpEntry,
    ];
  }

  /// Narrow accessor for the profile op `withOp` just put at index 0.
  /// Tests in this describe block specifically operate on a ProfileOp
  /// with tab placements; pull the narrow type out so the test reads
  /// `.tabPlacements` without per-call casts.
  function opTabs(t: CommandTarget) {
    return (t.operations[0] as ProfileOp).tabPlacements;
  }

  it('toggleTabPlacementCommand adds a tab on first click', () => {
    const t = blankTarget();
    withOp(t, 1, []);
    const cmd = toggleTabPlacementCommand(1, { objectId: 2, t: 0.4 }, 0.01);
    cmd.apply(t);
    expect(opTabs(t)).toEqual([{ objectId: 2, t: 0.4 }]);
    cmd.revert(t);
    expect(opTabs(t)).toEqual([]);
  });

  it('toggleTabPlacementCommand removes a tab on second click within tolerance', () => {
    const t = blankTarget();
    withOp(t, 1, [{ objectId: 2, t: 0.405 }]);
    const cmd = toggleTabPlacementCommand(1, { objectId: 2, t: 0.41 }, 0.01);
    cmd.apply(t);
    expect(opTabs(t)).toEqual([]);
    cmd.revert(t);
    expect(opTabs(t)).toEqual([{ objectId: 2, t: 0.405 }]);
  });

  it('toggleTabPlacementCommand respects per-op isolation (different op untouched)', () => {
    const t = blankTarget();
    withOp(t, 1, [{ objectId: 2, t: 0.5 }]);
    const cmd = toggleTabPlacementCommand(99, { objectId: 2, t: 0.5 }, 0.01);
    cmd.apply(t);
    expect(opTabs(t)).toEqual([{ objectId: 2, t: 0.5 }]);
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

  it('setWorkOffsetCommand patches X / WCS and restores; flips dirty (audit abdk)', () => {
    const t = blankTarget();
    t.dirty = false;
    const cmd = setWorkOffsetCommand({ x_mm: 5.76, y_mm: 5.79, wcs: 'G55' });
    cmd.apply(t);
    expect(t.workOffset.x_mm).toBeCloseTo(5.76);
    expect(t.workOffset.y_mm).toBeCloseTo(5.79);
    expect(t.workOffset.wcs).toBe('G55');
    expect(t.workOffset.z_mm).toBe(0);
    expect(t.dirty).toBe(true);
    cmd.revert(t);
    expect(t.workOffset.x_mm).toBe(0);
    expect(t.workOffset.y_mm).toBe(0);
    expect(t.workOffset.wcs).toBe('G54');
  });

  it('setGroupOpsByToolCommand toggles, restores, and flips dirty (7iej.8)', () => {
    const t = blankTarget();
    t.dirty = false;
    const cmd = setGroupOpsByToolCommand(true);
    cmd.apply(t);
    expect(t.groupOpsByTool).toBe(true);
    expect(t.dirty).toBe(true);
    cmd.revert(t);
    expect(t.groupOpsByTool).toBe(false);
    expect(t.dirty).toBe(true);
    // Discrete toggle — not a drag, so no coalescing.
    expect(cmd.coalesce_key).toBeUndefined();
  });

  it('setWorkOffsetCommand coalesces per-field (audit abdk)', () => {
    expect(setWorkOffsetCommand({ x_mm: 1 }).coalesce_key).toBe('setWorkOffset:x_mm');
    expect(setWorkOffsetCommand({ wcs: 'G56' }).coalesce_key).toBe('setWorkOffset:wcs');
    // Multi-field patches sort their keys so the coalesce_key is stable.
    expect(setWorkOffsetCommand({ y_mm: 1, x_mm: 2 }).coalesce_key).toBe('setWorkOffset:x_mm,y_mm');
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
    expect((t.operations[0] as ProfileOp).offset).toBe('inside');
    cmd.revert(t);
    expect((t.operations[0] as ProfileOp).offset).toBe('outside');
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

  it('does NOT flip project dirty — settings is per-install, not part of the project snapshot (audit zxee)', () => {
    const t = blankTarget();
    t.settings = {
      ...(t.settings as CommandTarget['settings']),
      cellResolutionMode: 'auto',
      cellResolutionMm: 0.2,
    };
    t.dirty = false;
    const cmd = lowerSimResolutionCommand(0.5);
    cmd.apply(t);
    expect(t.dirty).toBe(false);
    cmd.revert(t);
    expect(t.dirty).toBe(false);
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
    expect((t.operations[0] as ProfileOp).offset).toBe('inside');
    cmd.revert(t);
    expect((t.operations[0] as ProfileOp).offset).toBe('outside');
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

describe('text-layer commands', () => {
  it('add → revert removes the inserted layer', () => {
    const t = blankTarget();
    const layer = sampleTextLayer(1, 'Hi');
    const cmd = addTextLayerCommand(layer);
    cmd.apply(t);
    expect(t.textLayers).toHaveLength(1);
    expect(t.textLayers[0]).toEqual(layer);
    expect(t.dirty).toBe(true);
    cmd.revert(t);
    expect(t.textLayers).toEqual([]);
  });

  it('delete → revert restores the layer at its original index', () => {
    const t = blankTarget();
    t.textLayers = [sampleTextLayer(1, 'a'), sampleTextLayer(2, 'b'), sampleTextLayer(3, 'c')];
    const cmd = deleteTextLayerCommand(2);
    cmd.apply(t);
    expect(t.textLayers.map((tl) => tl.id)).toEqual([1, 3]);
    cmd.revert(t);
    expect(t.textLayers.map((tl) => tl.id)).toEqual([1, 2, 3]);
    expect(t.textLayers[1].text).toBe('b');
  });

  it('update merges patch and revert restores the prior value', () => {
    const t = blankTarget();
    t.textLayers = [sampleTextLayer(7, 'before')];
    const cmd = updateTextLayerCommand(7, { text: 'after', sizeMm: 24 });
    cmd.apply(t);
    expect(t.textLayers[0].text).toBe('after');
    expect(t.textLayers[0].sizeMm).toBe(24);
    cmd.revert(t);
    expect(t.textLayers[0].text).toBe('before');
    expect(t.textLayers[0].sizeMm).toBe(12);
  });

  it('update coalesces drags of the same field into one undo step', () => {
    // Two single-field updates of the same (id, key) share a coalesce
    // key — the History engine collapses them. Cross-field edits get
    // independent keys.
    const a = updateTextLayerCommand(1, { sizeMm: 10 });
    const b = updateTextLayerCommand(1, { sizeMm: 14 });
    const c = updateTextLayerCommand(1, { rotationDeg: 45 });
    expect(a.coalesce_key).toBe(b.coalesce_key);
    expect(a.coalesce_key).not.toBe(c.coalesce_key);
  });

  it('multi-field updates skip coalescing', () => {
    const cmd = updateTextLayerCommand(1, { sizeMm: 10, rotationDeg: 45 });
    expect(cmd.coalesce_key).toBeUndefined();
  });
});

describe('selectObjectsCommand (80gv)', () => {
  // Selection target shape matches what `SelectionState` exposes,
  // but we use a plain object here to avoid pulling in the Svelte
  // rune runtime (selection.svelte.ts) from the test.
  function blankSel() {
    return {
      selectedObjects: new Set<number>(),
      selectionAnchorObjectId: null as number | null,
    };
  }

  it('apply → revert restores the prior selection set and anchor', async () => {
    const { selectObjectsCommand } = await import('./commands');
    const sel = blankSel();
    sel.selectedObjects = new Set([3, 7]);
    sel.selectionAnchorObjectId = 7;
    const cmd = selectObjectsCommand(
      sel,
      { selected: new Set([3, 7]), anchor: 7 },
      { selected: new Set([4]), anchor: 4 },
    );
    cmd.apply(undefined);
    expect([...sel.selectedObjects]).toEqual([4]);
    expect(sel.selectionAnchorObjectId).toBe(4);
    cmd.revert(undefined);
    expect([...sel.selectedObjects].sort()).toEqual([3, 7]);
    expect(sel.selectionAnchorObjectId).toBe(7);
  });

  it('is marked as view-only (marksDirty=false) so undo does not flag the project as edited', async () => {
    const { selectObjectsCommand } = await import('./commands');
    const sel = blankSel();
    const cmd = selectObjectsCommand(
      sel,
      { selected: new Set(), anchor: null },
      { selected: new Set([1]), anchor: 1 },
    );
    expect(cmd.marksDirty).toBe(false);
    expect(cmd.coalesce_key).toBe('selection');
  });
});

describe('updateReliefSourceCommand (rt1.12 j7b4)', () => {
  function targetWithSource(): CommandTarget {
    const t = blankTarget();
    t.reliefSources = [
      {
        id: 1,
        name: 'pic',
        origin: { x: 0, y: 0 },
        cell: 0.5,
        cols: 4,
        rows: 4,
        brightness: new Array(16).fill(0.5),
      },
    ];
    return t;
  }

  it('apply patches origin, revert restores it', () => {
    const t = targetWithSource();
    const cmd = updateReliefSourceCommand(1, { origin: { x: 12, y: 7 } });
    cmd.apply(t);
    expect(t.reliefSources[0].origin).toEqual({ x: 12, y: 7 });
    cmd.revert(t);
    expect(t.reliefSources[0].origin).toEqual({ x: 0, y: 0 });
  });

  it('single-field patches coalesce (one undo entry per drag); multi-field do not', () => {
    expect(updateReliefSourceCommand(1, { origin: { x: 1, y: 1 } }).coalesce_key).toBe(
      'reliefSource:1:origin',
    );
    expect(
      updateReliefSourceCommand(1, { origin: { x: 1, y: 1 }, cell: 0.3 }).coalesce_key,
    ).toBeUndefined();
  });
});
