/// Imported-drawing wireframe + text-layer previews — the flat polylines
/// the 3D scene draws on (or just above) the Z=0 plane. Owns the pickable
/// fat-line buffer, the multi-op dashed assignment overlays, and the
/// per-object color ranges that let selection toggle recolor in place.
///
/// Independent of the toolpath / op-enable state — rebuilds only on
/// transformedImport / visibleLayers / textLayers / previewVersion changes
/// (plus `generated` to switch the wireframe to faded color when a
/// toolpath exists). Selection clicks take the applySelection() fast path
/// instead of a full rebuild.
///
/// Extracted from Scene3D.svelte. Owns its THREE.Group.

import * as THREE from 'three';
import { LineSegments2 } from 'three/addons/lines/LineSegments2.js';
import { LineSegmentsGeometry } from 'three/addons/lines/LineSegmentsGeometry.js';
import { LineMaterial } from 'three/addons/lines/LineMaterial.js';
import type { ImportResponse } from '../api/types';
import type { OpEntry, TextLayer } from '../state/project.svelte';
import { opSourceHsl } from '../state/op-color';
import { previewSegmentsFor } from '../state/text_preview.svelte';
import { resolveAci } from '../canvas/aci-color';
import { tessellate } from './tessellate';
import { buildFatLines } from './fat_lines';
import type { BuilderContext, CssColor, LineOwner, PickableLineBuilder } from './builder';

export interface ImportedGeometryInput {
  data: ImportResponse | null;
  visibleLayers: Set<string>;
  operations: OpEntry[];
  selectedOpId: number | null;
  selectedObjects: Set<number>;
  textLayers: TextLayer[];
  /// !!project.gen.generated — switches the base wireframe to faded color.
  hasGenerated: boolean;
  previewMode: string;
  edgeColor: string;
  lineWidth: number;
  wireVisible: boolean;
  /// Live canvas size for the fat-line resolution uniform.
  width: number;
  height: number;
  /// Host's current combined scene radius, used only to size the dashed
  /// overlay tiling.
  sceneRadius: number;
}

/// Per-object color ranges into the color attribute. `start` is the vertex
/// index where this object's first vertex lives, `count` how many vertices
/// belong to it, `base` the original (non-selected) color to revert to.
type ColorRange = { start: number; count: number; base: [number, number, number] };

export class ImportedGeometryBuilder implements PickableLineBuilder {
  readonly group = new THREE.Group();
  private lines: LineSegments2 | undefined;
  private overlays: LineSegments2[] = [];
  private owners: LineOwner[] = [];
  private objectColorRanges = new Map<number, ColorRange[]>();
  /// Selection set the color attribute currently reflects.
  private appliedSelection = new Set<number>();

  constructor(
    private ctx: BuilderContext,
    private cssColor: CssColor,
  ) {
    ctx.scene.add(this.group);
  }

  /// The pickable fat-line object (raycast target) + its per-segment owner
  /// array, consumed by the host's picker. Undefined before the first
  /// non-empty build.
  get pickable(): LineSegments2 | undefined {
    return this.lines;
  }
  get lineOwners(): LineOwner[] {
    return this.owners;
  }

  private aciColor(c: number): THREE.Color {
    // Shared palette + classification with the 2D canvas.
    const r = resolveAci(c);
    return r.kind === 'fixed' ? new THREE.Color(r.hex) : this.cssColor(r.token, r.fallback);
  }

  private teardown() {
    if (this.lines) {
      this.group.remove(this.lines);
      this.lines.geometry.dispose();
      (this.lines.material as THREE.Material).dispose();
      this.lines = undefined;
    }
    for (const o of this.overlays) {
      this.group.remove(o);
      o.geometry.dispose();
      (o.material as THREE.Material).dispose();
    }
    this.overlays = [];
    this.owners = [];
    this.objectColorRanges = new Map();
  }

  build(input: ImportedGeometryInput) {
    this.teardown();
    const data = input.data;
    if (!data) return;

    const positions: number[] = [];
    const colors: number[] = [];
    const c = new THREE.Color();
    const fadedColor = this.cssColor('--imported-faded', 0x444444);
    const selectedColor = this.cssColor('--accent', 0x4a8df0);
    // When the stock heightfield is visible as a solid surface, the tan /
    // configured stock color drowns out the ACI / faded wireframe colors.
    // Use the user-configured EDGE color (already chosen for contrast
    // against the stock material) as the line tint. Falls back to ACI when
    // no solid is showing.
    const solidVisible = input.previewMode === 'solid' || input.previewMode === 'both';
    const contrastOverStock = solidVisible ? new THREE.Color(input.edgeColor) : null;
    // Lift the wireframe slightly above the stock top surface so it doesn't
    // Z-fight with the heightfield mesh (top_z = 0 in stock coords). 0.1 mm
    // is below the smallest carve step but enough to win the depth test.
    const lineZ = solidVisible ? 0.1 : 0;
    const flat = input.hasGenerated;
    // Source-assignment tint: objectId → op ids that reference it (mirror
    // of EntityCanvas2D.objectToOps). An assigned object is drawn in its
    // op's color — overriding the ACI / faded base so the assignment is
    // visible even after Generate. The base wireframe carries the PRIMARY
    // op's solid color (selected op if assigned, else the first); objects
    // in several ops additionally get phase-staggered DASHED overlays
    // (built below) so every assigned op's color shows as interleaved
    // bands — a single thin/thick line can only carry one color at a time.
    const objectToOps3d = new Map<number, number[]>();
    for (const op of input.operations) {
      const refs = op.sourceObjects;
      if (!refs) continue;
      for (const id of refs) {
        if (id <= 0) continue;
        const list = objectToOps3d.get(id);
        if (list) list.push(op.id);
        else objectToOps3d.set(id, [op.id]);
      }
    }
    const selOpId = input.selectedOpId;
    // Per-object path points for the multi-op dashed overlays. Only
    // populated for objects in ≥2 ops; each object's pairs are pushed in
    // buffer (path) order so LineSegments2.computeLineDistances gives a
    // cumulative distance → the dashes tile continuously along the path.
    const overlayPosByObj = new Map<number, number[]>();
    let segIdx = 0;
    for (const seg of data.segments) {
      if (!input.visibleLayers.has(seg.layer)) {
        segIdx++;
        continue;
      }
      const objectId = data.objects?.[segIdx] ?? 0;
      const isSelected = objectId > 0 && input.selectedObjects.has(objectId);
      const points = tessellate(seg);
      const assignedOps = objectId > 0 ? objectToOps3d.get(objectId) : undefined;
      let baseR: number;
      let baseG: number;
      let baseB: number;
      if (assignedOps && assignedOps.length > 0) {
        // Primary op: the selected one if this object is among its sources,
        // otherwise the first-assigned op.
        const primaryOp =
          selOpId != null && assignedOps.includes(selOpId) ? selOpId : assignedOps[0];
        const [hh, ss, ll] = opSourceHsl(primaryOp, primaryOp === selOpId);
        c.setHSL(hh, ss, ll);
        baseR = c.r;
        baseG = c.g;
        baseB = c.b;
      } else if (contrastOverStock) {
        baseR = contrastOverStock.r;
        baseG = contrastOverStock.g;
        baseB = contrastOverStock.b;
      } else if (flat) {
        baseR = fadedColor.r;
        baseG = fadedColor.g;
        baseB = fadedColor.b;
      } else {
        c.copy(this.aciColor(seg.color));
        baseR = c.r;
        baseG = c.g;
        baseB = c.b;
      }
      const r = isSelected ? selectedColor.r : baseR;
      const g = isSelected ? selectedColor.g : baseG;
      const b = isSelected ? selectedColor.b : baseB;
      const startVertex = positions.length / 3;
      let pairCount = 0;
      let overlayBuf: number[] | null = null;
      if (assignedOps && assignedOps.length >= 2) {
        overlayBuf = overlayPosByObj.get(objectId) ?? null;
        if (!overlayBuf) {
          overlayBuf = [];
          overlayPosByObj.set(objectId, overlayBuf);
        }
      }
      for (let i = 0; i < points.length - 1; i++) {
        const [ax, ay] = points[i];
        const [bx, by] = points[i + 1];
        positions.push(ax, ay, lineZ, bx, by, lineZ);
        colors.push(r, g, b, r, g, b);
        this.owners.push({ kind: 'object', objectId });
        if (overlayBuf) overlayBuf.push(ax, ay, lineZ, bx, by, lineZ);
        pairCount++;
      }
      if (objectId > 0 && pairCount > 0) {
        const range: ColorRange = {
          start: startVertex,
          count: pairCount * 2,
          base: [baseR, baseG, baseB],
        };
        const list = this.objectColorRanges.get(objectId);
        if (list) list.push(range);
        else this.objectColorRanges.set(objectId, [range]);
      }
      segIdx++;
    }

    // Text-layer previews. Each TextLayer renders client-side into a
    // segment list cached by `text_preview`; the 2D canvas reads the same
    // cache. Drawn in the accent color so they read as "live preview, not
    // yet baked into the toolpath".
    if (input.textLayers.length > 0) {
      const previewC = this.cssColor('--accent', 0x4a8df0);
      for (const layer of input.textLayers) {
        // Segments come back translated to the layer's current origin, so
        // the 3D position is correct without a re-render.
        const segs = previewSegmentsFor(layer.id, layer.origin);
        if (!segs || segs.length === 0) continue;
        for (const seg of segs) {
          const points = tessellate(seg);
          for (let i = 0; i < points.length - 1; i++) {
            const [ax, ay] = points[i];
            const [bx, by] = points[i + 1];
            positions.push(ax, ay, lineZ, bx, by, lineZ);
            colors.push(previewC.r, previewC.g, previewC.b, previewC.r, previewC.g, previewC.b);
            this.owners.push({ kind: 'object', objectId: 0 });
          }
        }
      }
    }

    if (positions.length > 0) {
      this.lines = buildFatLines(positions, colors, input.lineWidth, input.width, input.height);
      this.lines.visible = input.wireVisible;
      this.group.add(this.lines);
    }
    // Selection set is now baked into the color attribute.
    this.appliedSelection = new Set(input.selectedObjects);

    // Multi-op dashed overlays. For an object in N ops we lay N dashed
    // copies of its path, each in one op's color, with dashSize = L and
    // gapSize = (N-1)·L so op i's dashes occupy slot i of an N·L period
    // (dashOffset = -i·L). The slots tile the whole path → it reads as
    // consecutive colored bands A B C A B C, every assigned op visible.
    const lw = Math.max(0.5, input.lineWidth);
    const dash = Math.max(0.3, input.sceneRadius * 0.04);
    const w0 = input.width || 1;
    const h0 = input.height || 1;
    for (const [objectId, pos] of overlayPosByObj) {
      if (pos.length === 0) continue;
      const ops = (objectToOps3d.get(objectId) ?? []).slice().sort((a, b) => a - b);
      const n = ops.length;
      if (n < 2) continue;
      for (let i = 0; i < n; i++) {
        const opId = ops[i];
        const [hh, ss, ll] = opSourceHsl(opId, opId === selOpId);
        const mat = new LineMaterial({
          color: new THREE.Color().setHSL(hh, ss, ll).getHex(),
          worldUnits: false,
          linewidth: lw + 1,
          dashed: true,
          dashSize: dash,
          gapSize: dash * (n - 1),
        });
        mat.dashOffset = -i * dash;
        mat.resolution.set(w0, h0);
        const geom = new LineSegmentsGeometry();
        geom.setPositions(new Float32Array(pos));
        const obj = new LineSegments2(geom, mat);
        obj.computeLineDistances();
        obj.renderOrder = 2; // sit on top of the base wireframe
        obj.visible = input.wireVisible;
        this.group.add(obj);
        this.overlays.push(obj);
      }
    }
  }

  /// Selection-only fast path: mutate the color attribute in place instead
  /// of rebuilding the whole mesh on every click. No-op when geometry isn't
  /// built yet (the next build() picks up the current selection naturally).
  applySelection(next: Set<number>) {
    if (!this.lines) return;
    // Interleaved instance color buffer (6 floats / segment); the
    // ColorRange offsets (start = first vertex index) index it identically
    // to the old flat color attribute.
    const colorAttr = this.lines.geometry.getAttribute(
      'instanceColorStart',
    ) as THREE.InterleavedBufferAttribute;
    const arr = colorAttr.array as Float32Array;
    const selectedColor = this.cssColor('--accent', 0x4a8df0);
    let touched = false;
    // Newly-selected objects: paint accent over their ranges.
    for (const id of next) {
      if (this.appliedSelection.has(id)) continue;
      const ranges = this.objectColorRanges.get(id);
      if (!ranges) continue;
      for (const r of ranges) {
        for (let v = 0; v < r.count; v++) {
          const off = (r.start + v) * 3;
          arr[off] = selectedColor.r;
          arr[off + 1] = selectedColor.g;
          arr[off + 2] = selectedColor.b;
        }
      }
      touched = true;
    }
    // Newly-deselected objects: restore base color from the recorded ranges
    // so the wireframe goes back to ACI / faded.
    for (const id of this.appliedSelection) {
      if (next.has(id)) continue;
      const ranges = this.objectColorRanges.get(id);
      if (!ranges) continue;
      for (const r of ranges) {
        const [br, bg, bb] = r.base;
        for (let v = 0; v < r.count; v++) {
          const off = (r.start + v) * 3;
          arr[off] = br;
          arr[off + 1] = bg;
          arr[off + 2] = bb;
        }
      }
      touched = true;
    }
    if (touched) colorAttr.data.needsUpdate = true;
    this.appliedSelection = new Set(next);
  }

  /// Bounding sphere of the base wireframe (text previews included), or
  /// null before the first non-empty build. Fed to the host's combined
  /// fit-to-view / scene-radius math.
  boundingSphere(): THREE.Sphere | null {
    if (!this.lines) return null;
    this.lines.geometry.computeBoundingSphere();
    return this.lines.geometry.boundingSphere?.clone() ?? null;
  }

  setLineWidth(lw: number) {
    const m = this.lines?.material as LineMaterial | undefined;
    if (m) m.linewidth = lw;
    // Overlays render a touch wider so the colored dashes sit proud of the
    // base wireframe.
    for (const o of this.overlays) (o.material as LineMaterial).linewidth = lw + 1;
  }

  setResolution(w: number, h: number) {
    (this.lines?.material as LineMaterial | undefined)?.resolution.set(w, h);
    for (const o of this.overlays) (o.material as LineMaterial).resolution.set(w, h);
  }

  setWireVisible(visible: boolean) {
    if (this.lines) this.lines.visible = visible;
    for (const o of this.overlays) o.visible = visible;
  }

  dispose() {
    this.teardown();
    this.ctx.scene.remove(this.group);
  }
}
