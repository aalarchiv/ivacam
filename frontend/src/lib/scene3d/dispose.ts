/// Shared THREE disposal helpers for the scene3d builders.
///
/// `renderer.dispose()` frees the GL context but does NOT walk the scene
/// graph, so every builder that owns a `THREE.Group` must free its
/// children's geometry + materials explicitly or they leak on every
/// rebuild / pane swap. Extracted from Scene3D.svelte so
/// each builder class can reuse one implementation.

import * as THREE from 'three';

export { disposeMesh } from './tool_mesh';

/// Free geometry + material(s) for every drawable child of `g`, then
/// detach it. `THREE.Line` covers plain lines AND `LineSegments` (which
/// extends it); `THREE.Mesh` covers meshes AND the fat-line
/// `LineSegments2` (which extends Mesh). Both carry `.geometry` +
/// `.material`. Disposing a geometry/material shared across several
/// children (e.g. a reused SphereGeometry) more than once is a safe
/// no-op in three.js.
export function disposeGroup(g: THREE.Group) {
  while (g.children.length > 0) {
    const child = g.children[0];
    g.remove(child);
    if (child instanceof THREE.Mesh || child instanceof THREE.Line) {
      child.geometry.dispose();
      const m = (child as THREE.Mesh | THREE.Line).material as THREE.Material | THREE.Material[];
      if (Array.isArray(m)) m.forEach((mm) => mm.dispose());
      else m.dispose();
    }
  }
}
