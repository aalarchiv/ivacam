import { describe, expect, it } from 'vitest';
import {
  bboxOfSegments,
  clamp,
  distanceToSegment,
  lineCrossesBBox,
  pointInPolygon,
  projectOntoSegment,
} from './selection-geometry';
import type { Segment } from '../api/types';

describe('selection-geometry', () => {
  describe('clamp', () => {
    it('passes through values in range', () => {
      expect(clamp(5, 0, 10)).toBe(5);
    });
    it('clamps below the lower bound', () => {
      expect(clamp(-3, 0, 10)).toBe(0);
    });
    it('clamps above the upper bound', () => {
      expect(clamp(42, 0, 10)).toBe(10);
    });
  });

  describe('distanceToSegment', () => {
    it('zero when the query equals an endpoint', () => {
      const d = distanceToSegment({ x: 0, y: 0 }, { x: 10, y: 0 }, 0, 0);
      expect(d).toBeCloseTo(0);
    });
    it('measures perpendicular distance for points whose projection lies inside', () => {
      const d = distanceToSegment({ x: 0, y: 0 }, { x: 10, y: 0 }, 5, 4);
      expect(d).toBeCloseTo(4);
    });
    it('measures endpoint distance when the projection falls past an endpoint', () => {
      const d = distanceToSegment({ x: 0, y: 0 }, { x: 10, y: 0 }, 13, 0);
      expect(d).toBeCloseTo(3);
    });
    it('collapses to point-to-point for a zero-length segment', () => {
      const d = distanceToSegment({ x: 4, y: 7 }, { x: 4, y: 7 }, 7, 11);
      expect(d).toBeCloseTo(5);
    });
  });

  describe('projectOntoSegment', () => {
    it('returns the midpoint when the query is the segment midpoint', () => {
      const p = projectOntoSegment({ x: 0, y: 0 }, { x: 10, y: 0 }, 5, 1);
      expect(p.x).toBeCloseTo(5);
      expect(p.y).toBeCloseTo(0);
    });
    it('clamps to the near endpoint when the projection falls past it', () => {
      const p = projectOntoSegment({ x: 0, y: 0 }, { x: 10, y: 0 }, -3, 4);
      expect(p.x).toBe(0);
      expect(p.y).toBe(0);
    });
  });

  describe('pointInPolygon', () => {
    const square: [number, number][] = [
      [0, 0],
      [10, 0],
      [10, 10],
      [0, 10],
    ];
    it('returns true for the center', () => {
      expect(pointInPolygon(square, 5, 5)).toBe(true);
    });
    it('returns false for a point outside', () => {
      expect(pointInPolygon(square, 15, 5)).toBe(false);
    });
    it('returns false for degenerate polygons (< 3 vertices)', () => {
      expect(pointInPolygon([[0, 0], [1, 1]], 0, 0)).toBe(false);
    });
  });

  describe('lineCrossesBBox', () => {
    const bbox = { min_x: 0, min_y: 0, max_x: 10, max_y: 10 };
    it('detects a segment fully inside the bbox', () => {
      expect(lineCrossesBBox({ x: 2, y: 2 }, { x: 8, y: 8 }, bbox)).toBe(true);
    });
    it('detects a segment crossing into the bbox', () => {
      expect(lineCrossesBBox({ x: -5, y: 5 }, { x: 5, y: 5 }, bbox)).toBe(true);
    });
    it('rejects a segment fully outside the bbox along x', () => {
      expect(lineCrossesBBox({ x: 11, y: 5 }, { x: 15, y: 5 }, bbox)).toBe(false);
    });
    it('handles vertical segments grazing the bbox edge', () => {
      expect(lineCrossesBBox({ x: 5, y: -5 }, { x: 5, y: 5 }, bbox)).toBe(true);
    });
  });

  describe('bboxOfSegments', () => {
    it('returns a zero-extent bbox for an empty list', () => {
      const b = bboxOfSegments([]);
      expect(b).toEqual({ min_x: 0, min_y: 0, max_x: 0, max_y: 0 });
    });
    it('expands to cover every endpoint', () => {
      const segs: Segment[] = [
        {
          type: 'LINE',
          start: { x: 0, y: 0 },
          end: { x: 10, y: 5 },
          bulge: 0,
          layer: '0',
          color: 7,
        },
        {
          type: 'LINE',
          start: { x: -2, y: 3 },
          end: { x: 4, y: 8 },
          bulge: 0,
          layer: '0',
          color: 7,
        },
      ];
      const b = bboxOfSegments(segs);
      expect(b).toEqual({ min_x: -2, min_y: 0, max_x: 10, max_y: 8 });
    });
  });
});
