/// Machine work-area wireframe — the always-visible envelope the cutter
/// can't leave. A dashed box from (0,0,0) to (workArea.x/y/z) so the user
/// sees the machinable limits; dim so it sits behind the toolpath.
///
/// Extracted from Scene3D.svelte (4w2f). Owns its THREE.Group; rebuilt
/// whenever the user edits the work area in MachineDialog.

import * as THREE from 'three';
import type { AxisLimits } from '../state/project.svelte';
import { disposeGroup } from './dispose';
import type { Builder, BuilderContext, CssColor } from './builder';

export interface WorkAreaInput {
  workArea: AxisLimits | undefined;
}

export class WorkAreaBuilder implements Builder {
  readonly group = new THREE.Group();

  constructor(
    private ctx: BuilderContext,
    private cssColor: CssColor,
  ) {
    this.group.name = 'work-area';
    ctx.scene.add(this.group);
  }

  build(input: WorkAreaInput) {
    disposeGroup(this.group);
    const wa = input.workArea;
    if (!wa || wa.x <= 0 || wa.y <= 0 || wa.z <= 0) return;
    // Center the box on (wa.x/2, wa.y/2, wa.z/2) since BoxGeometry is
    // centered on its local origin. The work-area corner sits at (0,0,0).
    const cx = wa.x * 0.5;
    const cy = wa.y * 0.5;
    const cz = wa.z * 0.5;
    const box = new THREE.BoxGeometry(wa.x, wa.y, wa.z);
    const edges = new THREE.EdgesGeometry(box);
    const lineMat = new THREE.LineDashedMaterial({
      color: this.cssColor('--text-muted', 0x888888),
      dashSize: 3,
      gapSize: 2,
      transparent: true,
      opacity: 0.45,
    });
    const wire = new THREE.LineSegments(edges, lineMat);
    wire.computeLineDistances();
    wire.position.set(cx, cy, cz);
    this.group.add(wire);
    box.dispose();
  }

  dispose() {
    disposeGroup(this.group);
    this.ctx.scene.remove(this.group);
  }
}
