import { describe, expect, it } from 'vitest';
import { assessModeSwitch } from './mode_switch';
import type { ToolEntry } from './project-types';
import type { OpEntry, ToolKind } from './op_types';

const tool = (id: number, kind: ToolKind): ToolEntry =>
  ({
    id,
    name: `${kind} ${id}`,
    kind,
    diameter: 3,
    flutes: 2,
    speed: 18000,
    plungeRate: 100,
    feedRate: 800,
    coolant: 'off',
  }) as unknown as ToolEntry;

const op = (id: number, toolId: number, kind = 'profile'): OpEntry =>
  ({
    id,
    name: `op ${id}`,
    kind,
    enabled: true,
    toolId,
    sourceLayers: null,
    depth: -2,
    startDepth: 0,
    step: -1,
  }) as unknown as OpEntry;

describe('assessModeSwitch', () => {
  it('returns null when everything fits the new mode', () => {
    const tools = [tool(1, 'endmill')];
    const ops = [op(1, 1)];
    expect(assessModeSwitch({ mode: 'mill' }, ops, tools)).toBeNull();
  });

  it('collects ops referencing now-incompatible tools (the mill→plasma footgun)', () => {
    const tools = [tool(1, 'endmill'), tool(2, 'plasma_torch')];
    const ops = [op(1, 1), op(2, 2), op(3, 1)];
    const a = assessModeSwitch({ mode: 'plasma' }, ops, tools);
    expect(a).not.toBeNull();
    expect(a!.affectedOpIds).toEqual([1, 3]);
    expect(a!.compatibleToolId).toBe(2);
    expect(a!.seedOffer).toBe(false);
  });

  it('offers seeding when a singleton mode has zero compatible tools', () => {
    const tools = [tool(1, 'endmill')];
    const a = assessModeSwitch({ mode: 'plasma' }, [op(1, 1)], tools);
    expect(a).not.toBeNull();
    expect(a!.compatibleToolId).toBeNull();
    expect(a!.seedOffer).toBe(true);
  });

  it('offers seeding with no affected ops at all (empty-compatible library)', () => {
    const tools = [tool(1, 'endmill')];
    const a = assessModeSwitch({ mode: 'laser' }, [], tools);
    expect(a).not.toBeNull();
    expect(a!.affectedOpIds).toEqual([]);
    expect(a!.seedOffer).toBe(true);
  });

  it('never offers seeding for mill (not a singleton mode)', () => {
    const tools = [tool(1, 'laser_beam')];
    const a = assessModeSwitch({ mode: 'mill' }, [op(1, 1)], tools);
    expect(a).not.toBeNull();
    expect(a!.seedOffer).toBe(false);
    expect(a!.compatibleToolId).toBeNull();
  });

  it('skips program-only ops and dangling tool references', () => {
    const tools = [tool(1, 'endmill')];
    const pause = { ...op(1, 0), kind: 'pause' } as unknown as OpEntry;
    const dangling = op(2, 99);
    const a = assessModeSwitch({ mode: 'plasma' }, [pause, dangling], tools);
    // No affected ops; notice exists only because of the seed offer.
    expect(a!.affectedOpIds).toEqual([]);
    expect(a!.seedOffer).toBe(true);
  });

  it('the engraver dual-compatibility keeps drag machines quiet', () => {
    const tools = [tool(1, 'engraver')];
    const ops = [op(1, 1, 'engrave')];
    expect(assessModeSwitch({ mode: 'drag' }, ops, tools)).toBeNull();
  });

  it('judges against the effective capability set, not just the primary mode', () => {
    const tools = [tool(1, 'endmill'), tool(2, 'plasma_torch')];
    const ops = [op(1, 1), op(2, 2)];
    // Combo machine: primary plasma, but mill capability retained →
    // the endmill op stays valid, nothing to report.
    expect(
      assessModeSwitch({ mode: 'plasma', capabilities: ['plasma', 'mill'] }, ops, tools),
    ).toBeNull();
    // Dropping the mill capability (plasma-only) trips the notice for
    // the endmill op even though the primary mode didn't change.
    const a = assessModeSwitch({ mode: 'plasma', capabilities: ['plasma'] }, ops, tools);
    expect(a).not.toBeNull();
    expect(a!.affectedOpIds).toEqual([1]);
    expect(a!.modes).toEqual(['plasma']);
  });
});
