/// Translucent stock box + its wireframe. Always visible (not only in sim
/// mode) whenever an import is loaded and both `stock.visible` and
/// `settings.showStockBox` are on. The XY footprint comes from the shared
/// `computeFootprint` (auto = bbox + margin; manual = customX/Y centered on
/// the bbox); Z extents are `[offsetZ − thickness, offsetZ]`.
///
/// Extracted from Scene3D.svelte. Owns its THREE.Group.

import * as THREE from 'three';
import type { ImportResponse } from '../api/types';
import type { AxisLimits, StockConfig } from '../state/project.svelte';
import { computeFootprint } from '../sim/driver';
import { disposeGroup } from './dispose';
import type { Builder, BuilderContext, CssColor } from './builder';

export interface StockBoxInput {
  stock: StockConfig;
  showStockBox: boolean;
  imported: ImportResponse | null;
  workArea: AxisLimits | undefined;
}

export class StockBoxBuilder implements Builder {
  readonly group = new THREE.Group();

  constructor(
    private ctx: BuilderContext,
    private cssColor: CssColor,
  ) {
    ctx.scene.add(this.group);
  }

  build(input: StockBoxInput) {
    disposeGroup(this.group);
    const cfg = input.stock;
    if (!cfg.visible || !input.showStockBox) return;
    // Stock-first: render the stock even without a drawing (falls back to
    // the machine work-area inside computeFootprint).
    const fp = computeFootprint(input.imported, cfg, input.workArea);
    const sizeX = fp.maxX - fp.minX;
    const sizeY = fp.maxY - fp.minY;
    const thickness = Math.max(0.1, cfg.thickness);
    if (sizeX <= 0.1 || sizeY <= 0.1) return;

    const cx = (fp.minX + fp.maxX) * 0.5;
    const cy = (fp.minY + fp.maxY) * 0.5;
    // Stock top sits at offsetZ (default 0); box centered half a
    // thickness below it, so it spans [offsetZ − thickness, offsetZ].
    const cz = (cfg.offsetZ ?? 0) - thickness * 0.5;
    const box = new THREE.BoxGeometry(sizeX, sizeY, thickness);
    const fillMat = new THREE.MeshBasicMaterial({
      transparent: true,
      opacity: 0.05,
      // Theme-tracking neutral so the stock fill is visible against both
      // the dark and light backdrops. `--stock-edge` is the matching
      // outline token (used a few lines below).
      color: this.cssColor('--stock-edge', 0xcccccc),
      side: THREE.DoubleSide,
      depthWrite: false,
    });
    const fill = new THREE.Mesh(box, fillMat);
    fill.position.set(cx, cy, cz);
    this.group.add(fill);

    const edges = new THREE.EdgesGeometry(box);
    const lineMat = new THREE.LineBasicMaterial({
      color: this.cssColor('--stock-edge', 0x888888),
      transparent: true,
      opacity: 0.4,
    });
    const wire = new THREE.LineSegments(edges, lineMat);
    wire.position.set(cx, cy, cz);
    this.group.add(wire);
  }

  dispose() {
    disposeGroup(this.group);
    this.ctx.scene.remove(this.group);
  }
}
