/// Pure-logic tests for the heightfield-mesh helpers that don't need
/// a WebGL context. The HeightfieldMesh / HeightfieldMeshPyramid
/// classes themselves are exercised by the e2e + integration runs;
/// here we cover the budget-driven LOD selection knob so the
/// state-machine math is locked down independently of Three.js.

import { describe, expect, it } from 'vitest';
import { pickMinLodLevelForBudget } from './heightfield_mesh';

describe('pickMinLodLevelForBudget', () => {
  it('returns 0 when the source grid already fits the budget', () => {
    // 200 × 200 × 6 = 240_000 triangles ≤ 2M → L0 is affordable.
    expect(pickMinLodLevelForBudget(200, 200, 2_000_000)).toBe(0);
  });

  it('returns 1 when L0 exceeds but L1 fits', () => {
    // L0 = 1000 * 1000 * 6 = 6M tri > 2M.
    // L1 = 500 * 500 * 6 = 1.5M tri ≤ 2M → minLevel = 1.
    expect(pickMinLodLevelForBudget(1000, 1000, 2_000_000)).toBe(1);
  });

  it('returns 2 when L0 + L1 both exceed', () => {
    // L0 = 2000 * 2000 * 6 = 24M > 2M.
    // L1 = 1000 * 1000 * 6 = 6M > 2M.
    // L2 = 500 * 500 * 6 = 1.5M ≤ 2M → minLevel = 2.
    expect(pickMinLodLevelForBudget(2000, 2000, 2_000_000)).toBe(2);
  });

  it('returns maxLevel when even the coarsest level exceeds the budget', () => {
    // Tiny budget forces all the way to L3 (the default cap).
    expect(pickMinLodLevelForBudget(2000, 2000, 1000)).toBe(3);
  });

  it('respects a custom maxLevel cap', () => {
    expect(pickMinLodLevelForBudget(2000, 2000, 1000, 5)).toBe(5);
  });

  it('defaults to 0 when the budget is zero or negative', () => {
    expect(pickMinLodLevelForBudget(1000, 1000, 0)).toBe(0);
    expect(pickMinLodLevelForBudget(1000, 1000, -1)).toBe(0);
  });

  it('handles rectangular grids with non-power-of-two dimensions', () => {
    // 1023 × 513 ≈ 525k cells × 6 = 3.15M tri > 2M.
    // L1 ≈ 512 × 257 × 6 ≈ 790k tri ≤ 2M.
    expect(pickMinLodLevelForBudget(1023, 513, 2_000_000)).toBe(1);
  });

  it('clamps to minLevel 0 for trivially small grids', () => {
    expect(pickMinLodLevelForBudget(1, 1, 2_000_000)).toBe(0);
    expect(pickMinLodLevelForBudget(10, 10, 2_000_000)).toBe(0);
  });
});
