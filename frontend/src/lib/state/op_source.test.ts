import { describe, it, expect } from 'vitest';
import { opIncludesObject } from './op_source';
import type { ImportResponse } from '../api/types';

// Minimal import response with two objects on different layers.
// objects[i] gives the object id of segment i; segments[i].layer gives
// the source layer. Object 1 lives on 'top', object 2 on 'bottom'.
const IMP: ImportResponse = {
  segments: [
    { type: 'LINE', layer: 'top', start: { x: 0, y: 0 }, end: { x: 1, y: 0 } },
    { type: 'LINE', layer: 'bottom', start: { x: 0, y: 0 }, end: { x: 1, y: 0 } },
  ],
  objects: [1, 2],
  object_meta: [],
  bbox: { min_x: 0, min_y: 0, max_x: 1, max_y: 0 },
} as unknown as ImportResponse;

describe('opIncludesObject', () => {
  it('explicit sourceObjects: only listed ids match', () => {
    const op = { sourceLayers: null, sourceObjects: [1] };
    expect(opIncludesObject(op, 1, IMP)).toBe(true);
    expect(opIncludesObject(op, 2, IMP)).toBe(false);
  });

  it('sourceLayers: object matches when its layer is listed', () => {
    const op = { sourceLayers: ['top'] };
    expect(opIncludesObject(op, 1, IMP)).toBe(true);
    expect(opIncludesObject(op, 2, IMP)).toBe(false);
  });

  it('an unknown object id with a layer-source op is excluded', () => {
    const op = { sourceLayers: ['top'] };
    expect(opIncludesObject(op, 99, IMP)).toBe(false);
  });

  it('no sourceObjects and no sourceLayers ⇒ "all chained objects" match', () => {
    const op = { sourceLayers: null };
    expect(opIncludesObject(op, 1, IMP)).toBe(true);
    expect(opIncludesObject(op, 2, IMP)).toBe(true);
    expect(opIncludesObject(op, 99, IMP)).toBe(true);
  });

  it('empty sourceObjects falls through to layers / all-objects', () => {
    expect(opIncludesObject({ sourceLayers: ['top'], sourceObjects: [] }, 1, IMP)).toBe(true);
    expect(opIncludesObject({ sourceLayers: ['top'], sourceObjects: [] }, 2, IMP)).toBe(false);
  });
});
