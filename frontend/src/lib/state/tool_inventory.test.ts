import { describe, expect, it } from 'vitest';
import { seedInventoryFromProject, stockTool, syncStockedFromInventory } from './tool_inventory';
import type { ToolEntry } from './project-types';

const tool = (id: number, name: string, diameter = 3): ToolEntry => ({
  id,
  name,
  kind: 'endmill',
  diameter,
  flutes: 2,
  speed: 18000,
  plungeRate: 100,
  feedRate: 800,
  coolant: 'off',
});

describe('syncStockedFromInventory', () => {
  it('updates same-id stocked copies from the inventory definition', () => {
    const inventory = [tool(1, '6 mm endmill', 6), tool(2, '3 mm endmill', 3)];
    const stocked = [tool(1, '6 mm endmill', 5.9), tool(3, 'project-local', 4)];
    const next = syncStockedFromInventory(inventory, stocked);
    expect(next).not.toBeNull();
    expect(next![0].diameter).toBe(6);
    // Tools without an inventory counterpart stay untouched.
    expect(next![1]).toEqual(stocked[1]);
  });

  it('returns null when nothing changed (callers skip the undoable replace)', () => {
    const inventory = [tool(1, 'a')];
    const stocked = [tool(1, 'a'), tool(2, 'b')];
    expect(syncStockedFromInventory(inventory, stocked)).toBeNull();
  });

  it('returned copies do not alias the inventory objects', () => {
    const inventory = [tool(1, 'a', 6)];
    const stocked = [tool(1, 'a', 3)];
    const next = syncStockedFromInventory(inventory, stocked)!;
    inventory[0].diameter = 99;
    expect(next[0].diameter).toBe(6);
  });
});

describe('stockTool', () => {
  it('copies id-preserving when the id is free', () => {
    const copy = stockTool(tool(7, 'torch'), [tool(1, 'em')]);
    expect(copy).not.toBeNull();
    expect(copy!.id).toBe(7);
  });

  it('is a no-op when the identical tool is already stocked', () => {
    const t = tool(7, 'torch');
    expect(stockTool(t, [t])).toBeNull();
  });

  it('bumps to the next free id on a collision with a DIFFERENT tool', () => {
    const copy = stockTool(tool(1, 'inventory 6mm', 6), [tool(1, 'legacy 3mm', 3), tool(5, 'x')]);
    expect(copy).not.toBeNull();
    expect(copy!.id).toBe(6);
    expect(copy!.name).toBe('inventory 6mm');
  });
});

describe('seedInventoryFromProject', () => {
  it('deep-clones the project tools', () => {
    const src = [tool(1, 'a')];
    const inv = seedInventoryFromProject(src);
    src[0].diameter = 99;
    expect(inv[0].diameter).toBe(3);
    expect(inv).toHaveLength(1);
  });
});
