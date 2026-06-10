/// Approach-point needle for the currently selected op. A vertical
/// line from z=0 up to `fastMoveZ` at the op's `approachPoint`, plus a tiny
/// base dot so the marker reads even when the camera is top-down. Shown
/// only when the selected op carries one — same data the 2D canvas paints.
///
/// Extracted from Scene3D.svelte. Owns its THREE.Group.

import * as THREE from 'three';
import type { OpEntry } from '../state/project.svelte';
import { disposeGroup } from './dispose';
import type { Builder, BuilderContext, CssColor } from './builder';

export interface ApproachInput {
  selectedOpId: number | null;
  operations: OpEntry[];
  fastMoveZ: number;
}

export class ApproachBuilder implements Builder {
  readonly group = new THREE.Group();

  constructor(
    private ctx: BuilderContext,
    private cssColor: CssColor,
  ) {
    ctx.scene.add(this.group);
  }

  build(input: ApproachInput) {
    // Dispose the previous needle + dot geom/material — `.clear()`
    // alone leaks them on every approach-point drag / op selection.
    disposeGroup(this.group);
    const opId = input.selectedOpId;
    if (opId == null) return;
    const op = input.operations.find((o) => o.id === opId);
    if (!op) return;
    if (op.kind !== 'profile' && op.kind !== 'pocket') return;
    const ap = op.approachPoint;
    if (!ap) return;
    const [x, y] = ap;
    const topZ = Math.max(1, input.fastMoveZ);
    const color = this.cssColor('--accent', 0x44aaaa);
    // Vertical needle from (x, y, 0) up to (x, y, topZ).
    const geom = new THREE.BufferGeometry().setFromPoints([
      new THREE.Vector3(x, y, 0),
      new THREE.Vector3(x, y, topZ),
    ]);
    const mat = new THREE.LineBasicMaterial({ color, linewidth: 2 });
    this.group.add(new THREE.Line(geom, mat));
    // Base dot — tiny sphere at z=0 to anchor the needle visually when the
    // camera is overhead.
    const dotR = Math.max(0.4, topZ * 0.04);
    const dotGeom = new THREE.SphereGeometry(dotR, 12, 8);
    const dotMat = new THREE.MeshBasicMaterial({ color });
    const dot = new THREE.Mesh(dotGeom, dotMat);
    dot.position.set(x, y, 0);
    this.group.add(dot);
  }

  dispose() {
    disposeGroup(this.group);
    this.ctx.scene.remove(this.group);
  }
}
