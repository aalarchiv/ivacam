/// Tab markers — small spheres at each op's holding-tab placements.
/// rt1.10 + hr5: tabs are per-op. Manual placements resolve directly via
/// (objectId, t); Auto / Mixed modes additionally walk every object the op
/// covers and emit auto-spaced t values. Same arc-length math as the 2D
/// canvas + backend.
///
/// Extracted from Scene3D.svelte (4w2f). Owns its THREE.Group.

import * as THREE from 'three';
import type { ImportResponse } from '../api/types';
import type { OpEntry } from '../state/project.svelte';
import { isContourOp } from '../state/project.svelte';
import { autoTabTs, buildObjectPolylines, polylineAtT } from '../cam/tabs';
import { opIncludesObject } from '../state/op_source';
import { disposeGroup } from './dispose';
import type { Builder, BuilderContext, CssColor } from './builder';

export interface TabsInput {
  imported: ImportResponse | null;
  operations: OpEntry[];
}

export class TabsBuilder implements Builder {
  readonly group = new THREE.Group();

  constructor(
    private ctx: BuilderContext,
    private cssColor: CssColor,
  ) {
    ctx.scene.add(this.group);
  }

  build(input: TabsInput) {
    // 7iej.4: dispose the previous run's geometry/material (the shared
    // sphere geom+mat from the last call) — `.clear()` only detaches
    // children and would leak a geom+mat pair on every op/transform edit.
    disposeGroup(this.group);
    const imp = input.imported;
    if (!imp) return;
    const color = this.cssColor('--tab-marker', 0xffd23a);
    const radius = Math.max(0.5, (imp.bbox.max_x - imp.bbox.min_x || 100) * 0.008);
    const geom = new THREE.SphereGeometry(radius, 12, 8);
    const mat = new THREE.MeshBasicMaterial({ color });
    // Performance (90j): build the object-polyline cache ONCE and resolve
    // placements inline against this local cache. The prior code called
    // resolveTabPlacementToWorld(imp, tp) per manual placement, which
    // internally re-ran buildObjectPolylines — O(N_placements × N_segments)
    // on a multi-thousand-segment DXF.
    const objects = buildObjectPolylines(imp);
    const objectById = new Map(objects.map((o) => [o.objectId, o]));
    for (const op of input.operations) {
      if (!isContourOp(op)) continue;
      const mode = op.tabMode;
      if (!mode || mode.kind === 'off') continue;
      // Manual placements (Manual + Mixed).
      if (mode.kind === 'manual' || mode.kind === 'mixed') {
        for (const tp of op.tabPlacements ?? []) {
          const obj = objectById.get(tp.objectId);
          if (!obj) continue;
          const { point } = polylineAtT(obj.pts, tp.t, obj.closed);
          const sphere = new THREE.Mesh(geom, mat);
          sphere.position.set(point.x, point.y, 0);
          this.group.add(sphere);
        }
      }
      // Auto-spaced placements (Auto + Mixed).
      if (mode.kind === 'auto' || mode.kind === 'mixed') {
        const count = mode.kind === 'auto' ? mode.count : mode.auto_count;
        if (count <= 0) continue;
        for (const obj of objects) {
          if (!opIncludesObject(op, obj.objectId, imp)) continue;
          const ts = autoTabTs(count, obj.closed);
          for (const t of ts) {
            const { point } = polylineAtT(obj.pts, t, obj.closed);
            const sphere = new THREE.Mesh(geom, mat);
            sphere.position.set(point.x, point.y, 0);
            this.group.add(sphere);
          }
        }
      }
    }
  }

  dispose() {
    disposeGroup(this.group);
    this.ctx.scene.remove(this.group);
  }
}
