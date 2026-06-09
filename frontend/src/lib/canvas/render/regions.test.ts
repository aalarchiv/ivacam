import { describe, it, expect, beforeEach, vi } from 'vitest';
import type { RegionPreview } from '../../api/types';
import { RegionPathCache, drawRegions } from './regions';
import { stubCtx } from './stub-ctx';

// vitest runs in the node environment — provide a minimal Path2D so the
// cache can build its data-space paths.
class FakePath2D {
  segments: string[] = [];
  moveTo(x: number, y: number) {
    this.segments.push(`M${x},${y}`);
  }
  lineTo(x: number, y: number) {
    this.segments.push(`L${x},${y}`);
  }
  closePath() {
    this.segments.push('Z');
  }
}

beforeEach(() => {
  vi.stubGlobal('Path2D', FakePath2D);
});

const TRI: RegionPreview = {
  op_id: 7,
  outer: [
    { x: 0, y: 0 },
    { x: 10, y: 0 },
    { x: 0, y: 10 },
  ],
};

describe('RegionPathCache', () => {
  it('rebuilds only when the regions array reference changes', () => {
    const cache = new RegionPathCache();
    const regions = [TRI];
    const first = cache.paths(regions);
    expect(cache.paths(regions)).toBe(first); // same ref → cache hit
    expect(cache.paths([TRI])).not.toBe(first); // new ref → rebuild
  });

  it('traces outer + holes and skips degenerate (<3 pts) polygons', () => {
    const cache = new RegionPathCache();
    const region: RegionPreview = {
      ...TRI,
      holes: [
        [
          { x: 2, y: 2 },
          { x: 4, y: 2 },
          { x: 2, y: 4 },
        ],
        [{ x: 9, y: 9 }], // degenerate — ignored
      ],
    };
    const [rp] = cache.paths([region]);
    const path = rp.path as unknown as FakePath2D;
    // outer (M + 2L + Z) + one hole (M + 2L + Z); the 1-pt hole adds nothing.
    expect(path.segments.filter((s) => s.startsWith('M'))).toHaveLength(2);
    expect(path.segments.filter((s) => s === 'Z')).toHaveLength(2);
  });
});

describe('drawRegions', () => {
  it('fills the selected op brighter and uses even-odd to punch holes', () => {
    const s = stubCtx();
    const cache = new RegionPathCache();
    const other: RegionPreview = { ...TRI, op_id: 8 };
    drawRegions(s.ctx, cache, [TRI, other], 2, 5, 100, 7, '#2d6cdf');
    const fills = s.ops('fill');
    expect(fills).toHaveLength(2);
    expect(fills[0].fillStyle).toBe('#2d6cdf66'); // selected op 7
    expect(fills[1].fillStyle).toBe('#2d6cdf33');
    expect(fills[0].args[1]).toBe('evenodd');
    // Data → canvas transform with the y-flip composed in one call.
    expect(s.ops('transform')[0].args).toEqual([2, 0, 0, -2, 5, 100]);
  });
});
