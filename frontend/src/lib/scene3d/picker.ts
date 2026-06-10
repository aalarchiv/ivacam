/// Cursor → toolpath/imported-segment picking. Casts a ray through the
/// cursor against every pickable line builder and resolves the closest hit
/// to the owning segment, so the host can act on it (select an object /
/// scrub the playhead). Owns its raycaster + NDC scratch vector.
///
/// Extracted from Scene3D.svelte. THREE-only — no rune state — so
/// the side-effecting actions (selection, playhead) stay in the host.

import * as THREE from 'three';
import type { LineSegments2 } from 'three/addons/lines/LineSegments2.js';
import type { LineOwner, PickableLineBuilder } from './builder';

export interface PickRequest {
  clientX: number;
  clientY: number;
  /// The canvas' client rect (renderer.domElement.getBoundingClientRect()).
  rect: { left: number; top: number; width: number; height: number };
  camera: THREE.Camera;
  builders: PickableLineBuilder[];
}

/// Outcome of a click:
///   owner  — a segment was hit; act on it.
///   clear  — geometry exists but the ray missed it (caller clears the
///            selection unless the click was additive).
///   ignore — nothing pickable, or the hit didn't resolve to an owner;
///            the caller leaves all state untouched.
export type PickResult =
  | { kind: 'owner'; owner: LineOwner }
  | { kind: 'clear' }
  | { kind: 'ignore' };

export class Picker {
  private raycaster = new THREE.Raycaster();
  private ndc = new THREE.Vector2();

  pick(req: PickRequest): PickResult {
    // Pair each pickable buffer with its owner array up front so we can map
    // the hit object back to the right owners (closest hit wins — Three's
    // intersectObjects sorts by distance).
    const targets: LineSegments2[] = [];
    const ownersByObject = new Map<LineSegments2, LineOwner[]>();
    for (const b of req.builders) {
      const p = b.pickable;
      if (!p) continue;
      targets.push(p);
      ownersByObject.set(p, b.lineOwners);
    }
    if (targets.length === 0) return { kind: 'ignore' };

    this.ndc.x = ((req.clientX - req.rect.left) / req.rect.width) * 2 - 1;
    this.ndc.y = -((req.clientY - req.rect.top) / req.rect.height) * 2 + 1;
    this.raycaster.setFromCamera(this.ndc, req.camera);
    // LineSegments2 raycasts in screen space against the line width; the
    // threshold (px) widens the pick corridor so thin lines stay clickable.
    this.raycaster.params.Line2 = { threshold: 8 };
    const hits = this.raycaster.intersectObjects(targets, false);
    if (hits.length === 0) return { kind: 'clear' };

    const hit = hits[0];
    // LineSegments2 reports the picked segment as `faceIndex`; the owner
    // arrays hold one entry per segment, so it maps directly.
    const segIndex = hit.faceIndex ?? (hit.index != null ? Math.floor(hit.index / 2) : null);
    if (segIndex == null) return { kind: 'ignore' };
    const owners = ownersByObject.get(hit.object as LineSegments2);
    const owner = owners?.[segIndex];
    if (!owner) return { kind: 'ignore' };
    return { kind: 'owner', owner };
  }
}
