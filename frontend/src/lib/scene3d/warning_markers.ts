/// Sim-warning markers — one small tetrahedron per critical / holder
/// warning, colored by severity and anchored at the warning's world
/// position. Lazily rebuilt whenever sim diagnostics change.
///
/// Extracted from Scene3D.svelte. Owns its THREE.Group. The marker
/// radius scales off `sceneRadius` (derived from the line buffers, still
/// owned by the host) so it reads at any zoom.

import * as THREE from 'three';
import type { SimWarning, ToolpathSegment } from '../api/types';
import { simWarningSeverity } from '../state/project.svelte';
import { warningPosition } from './warning_position';
import { disposeGroup } from './dispose';
import type { Builder, BuilderContext, CssColor } from './builder';

export interface WarningMarkersInput {
  warnings: SimWarning[];
  toolpath: ToolpathSegment[] | undefined;
  sceneRadius: number;
}

export class WarningMarkersBuilder implements Builder {
  readonly group = new THREE.Group();

  constructor(
    private ctx: BuilderContext,
    private cssColor: CssColor,
  ) {
    ctx.scene.add(this.group);
  }

  private markerColor(w: SimWarning): THREE.Color {
    return simWarningSeverity(w) === 'critical'
      ? this.cssColor('--error', 0xe54848)
      : this.cssColor('--warn', 0xf0c020);
  }

  build(input: WarningMarkersInput) {
    disposeGroup(this.group);
    const warnings = input.warnings;
    if (warnings.length === 0) return;
    const radius = Math.max(0.5, input.sceneRadius * 0.012);
    const geom = new THREE.TetrahedronGeometry(radius, 0);
    for (const w of warnings) {
      const pos = warningPosition(w, input.toolpath);
      if (!pos) continue;
      const mat = new THREE.MeshBasicMaterial({
        color: this.markerColor(w),
        transparent: true,
        opacity: 0.9,
      });
      const mesh = new THREE.Mesh(geom, mat);
      mesh.position.set(pos.x, pos.y, pos.z + radius);
      this.group.add(mesh);
    }
  }

  dispose() {
    disposeGroup(this.group);
    this.ctx.scene.remove(this.group);
  }
}
