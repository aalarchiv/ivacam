/// Pure-logic tests for the simulator driver helpers. Today this only
/// covers `computeFootprint`, the small function shared with Scene3D's
/// stock-box visualisation. The Simulator/HeightfieldMesh side requires
/// a browser + the wasm-pack-built pkg and is exercised by the e2e
/// build instead.

import { describe, expect, it } from 'vitest';
import {
  computeFootprint,
  toWireTool,
  effectiveSimCellCap,
  WASM_TRIAL_SIM_CELL_CAP,
} from './driver';
import { planAdvance } from './playhead';
import type { ImportResponse } from '../api/types';
import type { ToolEntry } from '../state/project-types';

function importedWithBbox(
  min_x: number,
  min_y: number,
  max_x: number,
  max_y: number,
): ImportResponse {
  return {
    bbox: { min_x, min_y, max_x, max_y },
  } as unknown as ImportResponse;
}

describe('computeFootprint', () => {
  it('returns a 100x100 default when no import AND no work area is present', () => {
    const fp = computeFootprint(null, { mode: 'auto', margin: 5, customX: 50, customY: 50 });
    expect(fp).toEqual({ minX: 0, minY: 0, maxX: 100, maxY: 100 });
  });

  it('falls back to the machine work area when no drawing is imported', () => {
    const fp = computeFootprint(
      null,
      { mode: 'auto', margin: 5, customX: 0, customY: 0 },
      { x: 200, y: 300 },
    );
    expect(fp).toEqual({ minX: 0, minY: 0, maxX: 200, maxY: 300 });
  });

  it('honors manual customX/Y centered on the origin when no drawing is present', () => {
    const fp = computeFootprint(
      null,
      { mode: 'manual', margin: 5, customX: 40, customY: 30 },
      { x: 200, y: 300 },
    );
    expect(fp).toEqual({ minX: -20, minY: -15, maxX: 20, maxY: 15 });
  });

  it('expands the bbox by the configured margin in auto mode', () => {
    const imp = importedWithBbox(10, 20, 30, 50);
    const fp = computeFootprint(imp, { mode: 'auto', margin: 5, customX: 0, customY: 0 });
    expect(fp).toEqual({ minX: 5, minY: 15, maxX: 35, maxY: 55 });
  });

  it('clamps negative margins to zero in auto mode', () => {
    const imp = importedWithBbox(0, 0, 10, 10);
    const fp = computeFootprint(imp, { mode: 'auto', margin: -3, customX: 0, customY: 0 });
    expect(fp).toEqual({ minX: 0, minY: 0, maxX: 10, maxY: 10 });
  });

  it('centers a manual footprint on the bbox center', () => {
    const imp = importedWithBbox(0, 0, 20, 10); // center (10, 5)
    const fp = computeFootprint(imp, { mode: 'manual', margin: 0, customX: 40, customY: 30 });
    expect(fp).toEqual({ minX: -10, minY: -10, maxX: 30, maxY: 20 });
  });

  it('ignores margin in manual mode', () => {
    const imp = importedWithBbox(0, 0, 10, 10);
    const a = computeFootprint(imp, { mode: 'manual', margin: 0, customX: 20, customY: 20 });
    const b = computeFootprint(imp, { mode: 'manual', margin: 99, customX: 20, customY: 20 });
    expect(a).toEqual(b);
  });
});

describe('planAdvance', () => {
  it('returns null for an empty toolpath', () => {
    expect(planAdvance(0, 0, 0, 0, 0)).toBeNull();
  });

  it('returns null when target equals current state', () => {
    expect(planAdvance(5, 0.3, 5, 0.3, 10)).toBeNull();
  });

  it('starts a partial in the first segment from a fresh sim', () => {
    const p = planAdvance(0, 0, 0, 0.3, 10);
    expect(p).not.toBeNull();
    expect(p!.reset).toBe(false);
    expect(p!.finalizePartial).toBeNull();
    expect(p!.bulkAdvance).toBeNull();
    expect(p!.startPartial).toEqual({ segIdx: 0, startT: 0, endT: 0.3 });
    expect(p!.newAppliedSeg).toBe(0);
    expect(p!.newPartialT).toBe(0.3);
  });

  it('extends the current partial within the same segment', () => {
    const p = planAdvance(3, 0.2, 3, 0.7, 10);
    expect(p!.reset).toBe(false);
    expect(p!.bulkAdvance).toBeNull();
    expect(p!.finalizePartial).toBeNull();
    expect(p!.startPartial).toEqual({ segIdx: 3, startT: 0.2, endT: 0.7 });
    expect(p!.newPartialT).toBe(0.7);
  });

  it('finalizes the in-flight partial, bulk-advances, and starts a new partial on a multi-segment jump', () => {
    const p = planAdvance(2, 0.4, 5, 0.6, 10);
    expect(p!.reset).toBe(false);
    expect(p!.finalizePartial).toEqual({ segIdx: 2, fromT: 0.4 });
    expect(p!.bulkAdvance).toEqual({ from: 3, to: 5 });
    expect(p!.startPartial).toEqual({ segIdx: 5, startT: 0, endT: 0.6 });
    expect(p!.newAppliedSeg).toBe(5);
    expect(p!.newPartialT).toBe(0.6);
  });

  it('skips the finalize step when partialT is 0', () => {
    const p = planAdvance(2, 0, 5, 0.6, 10);
    expect(p!.finalizePartial).toBeNull();
    expect(p!.bulkAdvance).toEqual({ from: 2, to: 5 });
    expect(p!.startPartial).toEqual({ segIdx: 5, startT: 0, endT: 0.6 });
  });

  it('snaps `partialT == 1` to the next segment for non-terminal boundaries', () => {
    const p = planAdvance(3, 0.5, 3, 1, 10);
    expect(p!.startPartial).toEqual({ segIdx: 3, startT: 0.5, endT: 1 });
    expect(p!.newAppliedSeg).toBe(4);
    expect(p!.newPartialT).toBe(0);
  });

  it('keeps `(last, 1)` as the canonical terminal state', () => {
    const p = planAdvance(9, 0.5, 9, 1, 10);
    expect(p!.newAppliedSeg).toBe(9);
    expect(p!.newPartialT).toBe(1);
  });

  it('flags backward scrub when target precedes current state in (seg, t) order', () => {
    const p = planAdvance(5, 0.5, 3, 0.8, 10);
    expect(p!.reset).toBe(true);
    // After reset, replay from (0, 0) up to (3, 0.8): no finalize, bulk
    // advance segments [0..3), then partial in segment 3.
    expect(p!.finalizePartial).toBeNull();
    expect(p!.bulkAdvance).toEqual({ from: 0, to: 3 });
    expect(p!.startPartial).toEqual({ segIdx: 3, startT: 0, endT: 0.8 });
    expect(p!.newAppliedSeg).toBe(3);
    expect(p!.newPartialT).toBe(0.8);
  });

  it('flags backward scrub on intra-segment rewind', () => {
    const p = planAdvance(5, 0.8, 5, 0.3, 10);
    expect(p!.reset).toBe(true);
    expect(p!.bulkAdvance).toEqual({ from: 0, to: 5 });
    expect(p!.startPartial).toEqual({ segIdx: 5, startT: 0, endT: 0.3 });
  });

  it('produces no bulk-advance when crossing only one boundary cleanly', () => {
    // (4, 1) → snapped state (5, 0). Now advancing to (5, 0.3).
    const p = planAdvance(5, 0, 5, 0.3, 10);
    expect(p!.bulkAdvance).toBeNull();
    expect(p!.startPartial).toEqual({ segIdx: 5, startT: 0, endT: 0.3 });
  });

  it('backward scrub replays only the tail from a heightmap checkpoint', () => {
    // From (8, 0.5) back to (6, 0.4) with a checkpoint at segment 4: the
    // replay starts at 4, not 0 — bulk-advance [4..6) then partial in 6.
    const p = planAdvance(8, 0.5, 6, 0.4, 10, 4);
    expect(p!.reset).toBe(true);
    expect(p!.finalizePartial).toBeNull();
    expect(p!.bulkAdvance).toEqual({ from: 4, to: 6 });
    expect(p!.startPartial).toEqual({ segIdx: 6, startT: 0, endT: 0.4 });
    expect(p!.newAppliedSeg).toBe(6);
    expect(p!.newPartialT).toBe(0.4);
  });

  it('clamps a checkpoint past the target back to the target segment', () => {
    // A checkpoint at 9 but scrubbing back to 3: the replay base can't
    // exceed the target, so it falls to 3 (pure restore, no forward ops).
    const p = planAdvance(8, 0, 3, 0, 10, 9);
    expect(p!.reset).toBe(true);
    expect(p!.bulkAdvance).toBeNull();
    expect(p!.startPartial).toBeNull();
    expect(p!.newAppliedSeg).toBe(3);
    expect(p!.newPartialT).toBe(0);
  });

  it('replayFrom is ignored on a forward scrub', () => {
    // Forward moves never restore a checkpoint; the base stays at the
    // current appliedSeg regardless of the replayFrom hint.
    const p = planAdvance(2, 0, 5, 0.6, 10, 4);
    expect(p!.reset).toBe(false);
    expect(p!.bulkAdvance).toEqual({ from: 2, to: 5 });
    expect(p!.startPartial).toEqual({ segIdx: 5, startT: 0, endT: 0.6 });
  });
});

// Regression: the sim driver also
// ships a tool spec to the WASM `Simulator`, which deserializes through
// the SAME Rust `ToolKind` enum that expects the German `kegel` for the
// cone variant. Without this mapping, picking a cone tool for any op
// fails Generate with "unknown variant `cone`".
describe('toWireTool — German wire contract (8njb regression)', () => {
  function tool(over: Partial<ToolEntry> = {}): ToolEntry {
    const raw: Record<string, unknown> = {
      id: 1,
      name: 'T1',
      kind: 'endmill',
      diameter: 6,
      flutes: 2,
      speed: 18000,
      plungeRate: 200,
      feedRate: 1200,
      coolant: 'off',
      ...over,
    };
    return raw as unknown as ToolEntry;
  }

  it('maps the cone tool kind to the German wire value kegel', () => {
    expect(toWireTool(tool({ kind: 'cone' })).kind).toBe('kegel');
  });

  it('passes non-cone kinds through unchanged', () => {
    expect(toWireTool(tool({ kind: 'v_bit' })).kind).toBe('v_bit');
    expect(toWireTool(tool({ kind: 'endmill' })).kind).toBe('endmill');
    expect(toWireTool(tool({ kind: 'drag_knife' })).kind).toBe('drag_knife');
  });
});

describe('effectiveSimCellCap (5v1b trial-mode fidelity)', () => {
  it('returns the user setting verbatim off the wasm trial', () => {
    expect(effectiveSimCellCap(1_000_000, false)).toBe(1_000_000);
    expect(effectiveSimCellCap(4_000_000, false)).toBe(4_000_000);
  });

  it('clamps to the trial cap in wasm mode when the user setting is higher', () => {
    expect(effectiveSimCellCap(1_000_000, true)).toBe(WASM_TRIAL_SIM_CELL_CAP);
    expect(effectiveSimCellCap(4_000_000, true)).toBe(WASM_TRIAL_SIM_CELL_CAP);
  });

  it('keeps a lower-than-cap user setting even in wasm mode', () => {
    const low = WASM_TRIAL_SIM_CELL_CAP - 50_000;
    expect(effectiveSimCellCap(low, true)).toBe(low);
  });

  it('never returns less than 1 cell', () => {
    expect(effectiveSimCellCap(0, false)).toBe(1);
    expect(effectiveSimCellCap(-10, true)).toBe(1);
  });
});
