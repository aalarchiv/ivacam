import { describe, it, expect } from 'vitest';
import { distanceToSegments, nearestTextLayer, type TextHitLayer } from './text-hit';
import type { Segment } from '../api/types';

/// Minimal LINE segment from (x0,y0)→(x1,y1).
function seg(x0: number, y0: number, x1: number, y1: number): Segment {
  return {
    start: { x: x0, y: y0 },
    end: { x: x1, y: y1 },
    bulge: 0,
    color: 7,
    layer: '__text_1',
    type: 'LINE',
  };
}

describe('distanceToSegments', () => {
  it('returns Infinity for an empty list', () => {
    expect(distanceToSegments([], 0, 0)).toBe(Infinity);
  });

  it('measures distance to the nearest stroke', () => {
    // A horizontal stroke along y=0 from x=0..10. Point at (5, 2) ⇒ 2.
    const segs = [seg(0, 0, 10, 0)];
    expect(distanceToSegments(segs, 5, 2)).toBeCloseTo(2, 6);
    // On the stroke ⇒ 0.
    expect(distanceToSegments(segs, 5, 0)).toBeCloseTo(0, 6);
  });

  it('reports the true distance for far points (no pruning here)', () => {
    const segs = [seg(0, 0, 10, 0)];
    expect(distanceToSegments(segs, 5, 100)).toBeCloseTo(100, 6);
  });

  it('is far from the whitespace between two distant strokes', () => {
    // Two vertical strokes 100 apart; a point centered between them is
    // far from both even though it's inside the bounding box — this is
    // exactly why we hit by stroke distance, not bbox.
    const segs = [seg(0, 0, 0, 10), seg(100, 0, 100, 10)];
    expect(distanceToSegments(segs, 50, 5)).toBeCloseTo(50, 6);
  });
});

describe('nearestTextLayer', () => {
  const layers: TextHitLayer[] = [
    { id: 1, segments: [seg(0, 0, 10, 0)] },
    { id: 2, segments: [seg(0, 50, 10, 50)] },
  ];

  it('returns the layer whose stroke is within tolerance', () => {
    expect(nearestTextLayer(layers, 5, 1, 3)).toEqual({ id: 1, dist: 1 });
    expect(nearestTextLayer(layers, 5, 49, 3)?.id).toBe(2);
  });

  it('returns null when no stroke is within tolerance', () => {
    // Point at (5, 25) is 25 from layer 1 and 25 from layer 2; tol 5.
    expect(nearestTextLayer(layers, 5, 25, 5)).toBeNull();
  });

  it('picks the nearest layer when several are in range', () => {
    // Closer to layer 1 (dist 2) than layer 2 (dist 48).
    expect(nearestTextLayer(layers, 5, 2, 60)?.id).toBe(1);
  });

  it('resolves an exact tie to the last-listed (topmost) layer', () => {
    const stacked: TextHitLayer[] = [
      { id: 10, segments: [seg(0, 0, 10, 0)] },
      { id: 20, segments: [seg(0, 0, 10, 0)] }, // identical, drawn on top
    ];
    expect(nearestTextLayer(stacked, 5, 0, 3)?.id).toBe(20);
  });
});
