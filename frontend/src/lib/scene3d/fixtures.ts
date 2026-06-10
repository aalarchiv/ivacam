/// 3D fixture group. Each fixture extrudes between `z_bottom..z_top` in
/// its declared color; the selected fixture gets an accented outline. The
/// builder also owns the per-fixture material map + base colors so the
/// playhead-collision "flash" can flip a fixture red in place (one
/// `.color.set` per material) without a rebuild.
///
/// Extracted from Scene3D.svelte. Owns its THREE.Group.

import * as THREE from 'three';
import type { Fixture } from '../state/project.svelte';
import { unpackFixtureColor, DEFAULT_FIXTURE_COLOR } from '../canvas/fixture-color';
import { disposeGroup } from './dispose';
import type { Builder, BuilderContext, CssColor } from './builder';

export interface FixturesInput {
  fixtures: Fixture[];
  selectedFixtureId: number | null;
}

export class FixturesBuilder implements Builder {
  readonly group = new THREE.Group();
  /// Per-fixture-id → materials whose color we flip when the playhead
  /// crosses a `fixture_collision` warning's segment.
  private materials = new Map<number, THREE.MeshBasicMaterial[]>();
  /// Recorded base colors so we can restore on un-flash.
  private baseColors = new Map<number, number>();
  /// Fixture ids currently flashing red (driven by the host's collision
  /// effect via `flash()`); retained across rebuilds.
  private flashing = new Set<number>();

  constructor(
    private ctx: BuilderContext,
    private cssColor: CssColor,
  ) {
    ctx.scene.add(this.group);
  }

  build(input: FixturesInput) {
    disposeGroup(this.group);
    this.materials = new Map();
    this.baseColors = new Map();
    const accent = this.cssColor('--accent', 0x4a8df0);
    for (const f of input.fixtures) {
      // Shared unpack with the 2D canvas. Default alpha ~0.5 when
      // the wire color omits it; the 3D opacity treatment stays here.
      const { a, hex } = unpackFixtureColor(f.color);
      const opacity = Math.max(0.2, Math.min(1.0, a > 0 ? a / 255 : 0.5));
      this.baseColors.set(f.id, hex);

      const mat = new THREE.MeshBasicMaterial({
        color: hex,
        transparent: true,
        opacity,
        depthWrite: false,
        side: THREE.DoubleSide,
      });
      const matsForFix: THREE.MeshBasicMaterial[] = [mat];
      const sizeZ = Math.max(0.05, f.z_top - f.z_bottom);
      const cz = (f.z_top + f.z_bottom) * 0.5;

      let geom: THREE.BufferGeometry | undefined;
      if (f.kind.shape === 'box') {
        geom = new THREE.BoxGeometry(
          Math.max(0.01, f.kind.width),
          Math.max(0.01, f.kind.depth),
          sizeZ,
        );
      } else if (f.kind.shape === 'cylinder') {
        geom = new THREE.CylinderGeometry(
          Math.max(0.01, f.kind.radius),
          Math.max(0.01, f.kind.radius),
          sizeZ,
          24,
        );
        // CylinderGeometry's axis is +Y; rotate so it stands on +Z.
        geom.rotateX(Math.PI / 2);
      } else if (f.kind.shape === 'polygon') {
        const shape = new THREE.Shape(f.kind.vertices.map(([x, y]) => new THREE.Vector2(x, y)));
        geom = new THREE.ExtrudeGeometry(shape, { depth: sizeZ, bevelEnabled: false });
      }
      if (!geom) continue;
      const mesh = new THREE.Mesh(geom, mat);
      if (f.kind.shape === 'polygon') {
        // ExtrudeGeometry extrudes along +Z from the shape plane (Z=0).
        // Translate so the extrusion sits in [z_bottom, z_top].
        mesh.position.set(f.origin[0], f.origin[1], f.z_bottom);
      } else {
        mesh.position.set(f.origin[0], f.origin[1], cz);
      }
      this.group.add(mesh);

      const isSelected = input.selectedFixtureId === f.id;
      const edgeColor = isSelected ? accent : new THREE.Color(hex);
      const edgesGeom = new THREE.EdgesGeometry(geom);
      const edgeMat = new THREE.LineBasicMaterial({
        color: edgeColor,
        transparent: true,
        opacity: isSelected ? 0.95 : 0.7,
      });
      const wire = new THREE.LineSegments(edgesGeom, edgeMat);
      wire.position.copy(mesh.position);
      this.group.add(wire);
      this.materials.set(f.id, matsForFix);
    }
    this.applyFlash();
  }

  /// Replace the flashing set (the host computes it from sim warnings +
  /// playhead). Returns true when it actually changed, so the host can
  /// request a render only then — matching the prior in-component behavior.
  flash(next: Set<number>): boolean {
    let changed = next.size !== this.flashing.size;
    if (!changed) {
      for (const id of next)
        if (!this.flashing.has(id)) {
          changed = true;
          break;
        }
    }
    if (changed) {
      this.flashing = next;
      this.applyFlash();
    }
    return changed;
  }

  private applyFlash() {
    const flashColor = this.cssColor('--error', 0xe54848);
    for (const [id, mats] of this.materials) {
      const flash = this.flashing.has(id);
      const base = this.baseColors.get(id) ?? DEFAULT_FIXTURE_COLOR;
      for (const m of mats) {
        if (flash) m.color.copy(flashColor);
        else m.color.set(base);
      }
    }
  }

  dispose() {
    disposeGroup(this.group);
    this.ctx.scene.remove(this.group);
  }
}
