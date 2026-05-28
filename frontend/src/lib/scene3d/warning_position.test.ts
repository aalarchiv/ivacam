import { describe, it, expect } from 'vitest';
import { warningPosition } from './warning_position';
import type { SimWarning, ToolpathSegment } from '../api/types';

function seg(x: number, y: number, z: number): ToolpathSegment {
  return {
    kind: 'cut',
    from: { x, y, z },
    to: { x, y, z },
  } as unknown as ToolpathSegment;
}

describe('warningPosition', () => {
  it('rapid_through_material: returns the recorded worst point', () => {
    const w = {
      kind: 'rapid_through_material',
      worst_x: 10,
      worst_y: 20,
      worst_cell_z: -5,
    } as unknown as SimWarning;
    expect(warningPosition(w, [])).toEqual({ x: 10, y: 20, z: -5 });
  });

  it('fixture_collision: nearest point on the fixture, z = 0', () => {
    const w = { kind: 'fixture_collision', nearest_x: 3, nearest_y: 4 } as unknown as SimWarning;
    expect(warningPosition(w, [])).toEqual({ x: 3, y: 4, z: 0 });
  });

  it('holder_collision: worst-encroachment XY with the wall-Z', () => {
    const w = {
      kind: 'holder_collision',
      worst_x: 1,
      worst_y: 2,
      wall_z: -3,
    } as unknown as SimWarning;
    expect(warningPosition(w, [])).toEqual({ x: 1, y: 2, z: -3 });
  });

  it('an unknown warning kind falls back to the segment endpoint', () => {
    // The 4 current SimWarning kinds are all handled by the named
    // branches above (cell_size_coarsened short-circuits via
    // simWarningSegmentIdx returning -1, so it hits null). Forge a
    // hypothetical future kind to exercise the fallback path.
    const w = { kind: 'future_span_kind', segment_idx: 1 } as unknown as SimWarning;
    const tp: ToolpathSegment[] = [seg(0, 0, 0), seg(7, 8, -2)];
    expect(warningPosition(w, tp)).toEqual({ x: 7, y: 8, z: -2 });
  });

  it('cell_size_coarsened has no segment — returns null', () => {
    const w = {
      kind: 'cell_size_coarsened',
      original_cell_size_mm: 0.2,
      coarsened_cell_size_mm: 0.4,
      reason: 'budget',
    } as unknown as SimWarning;
    const tp: ToolpathSegment[] = [seg(0, 0, 0)];
    expect(warningPosition(w, tp)).toBeNull();
  });
});
