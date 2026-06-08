import { describe, it, expect } from 'vitest';
import * as THREE from 'three';
import { Picker } from './picker';
import { buildFatLines } from './fat_lines';
import type { LineOwner, PickableLineBuilder } from './builder';

const RECT = { left: 0, top: 0, width: 200, height: 200 };

/// Minimal stand-in for a pickable builder — only the two members the
/// Picker reads. (The real builders carry the full LineBuilder surface.)
function fakeBuilder(
  pickable: ReturnType<typeof buildFatLines> | undefined,
  owners: LineOwner[],
): PickableLineBuilder {
  return {
    pickable,
    lineOwners: owners,
    setLineWidth() {},
    setResolution() {},
    setWireVisible() {},
    dispose() {},
  };
}

/// A camera looking straight down −Z at the origin, far enough to frame a
/// ~±10 mm segment on the Z=0 plane.
function topDownCamera(): THREE.PerspectiveCamera {
  const cam = new THREE.PerspectiveCamera(45, 1, 0.1, 1000);
  cam.position.set(0, 0, 100);
  cam.up.set(0, 1, 0);
  cam.lookAt(0, 0, 0);
  cam.updateMatrixWorld(true);
  return cam;
}

describe('Picker', () => {
  it('ignores a click when there are no pickable builders', () => {
    const r = new Picker().pick({
      clientX: 100,
      clientY: 100,
      rect: RECT,
      camera: topDownCamera(),
      builders: [],
    });
    expect(r.kind).toBe('ignore');
  });

  it('ignores a click when every builder has no built buffer', () => {
    const r = new Picker().pick({
      clientX: 100,
      clientY: 100,
      rect: RECT,
      camera: topDownCamera(),
      builders: [fakeBuilder(undefined, [])],
    });
    expect(r.kind).toBe('ignore');
  });

  it('reports clear when geometry exists but the ray misses it', () => {
    // One short horizontal segment near the origin.
    const lines = buildFatLines([-5, 0, 0, 5, 0, 0], [1, 1, 1, 1, 1, 1], 2, 200, 200);
    lines.updateMatrixWorld(true);
    // Click the top-left corner — far from the centered segment.
    const r = new Picker().pick({
      clientX: 2,
      clientY: 2,
      rect: RECT,
      camera: topDownCamera(),
      builders: [fakeBuilder(lines, [{ kind: 'object', objectId: 7 }])],
    });
    expect(r.kind).toBe('clear');
  });

  it('resolves the owner of the hit segment at the cursor', () => {
    const lines = buildFatLines([-20, 0, 0, 20, 0, 0], [1, 1, 1, 1, 1, 1], 4, 200, 200);
    lines.updateMatrixWorld(true);
    const owner: LineOwner = { kind: 'toolpath', segIdx: 3 };
    // Dead-center: the segment passes through the world origin, which the
    // top-down camera projects to the canvas center (100, 100).
    const r = new Picker().pick({
      clientX: 100,
      clientY: 100,
      rect: RECT,
      camera: topDownCamera(),
      builders: [fakeBuilder(lines, [owner])],
    });
    expect(r).toEqual({ kind: 'owner', owner });
  });
});
