/// Tool-tip mesh construction for the 3D preview. Pure builders:
/// given a tool kind / mode / dimensions, return a `THREE.Mesh`
/// whose origin sits at the cutting tip with the tool axis along
/// +Z (matches the Z-up world the rest of the scene uses).
///
/// Extracted from Scene3D for testability and so the component file
/// stays focused on scene management. Mesh + material lifecycle is
/// still the caller's responsibility — call `disposeMesh()` before
/// dropping a mesh you built here.

import * as THREE from 'three';
import type { HolderShape } from '../state/project.svelte';

/// Free a mesh's geometry + material(s). Idempotent on multi-material
/// meshes (Three.js exposes `mesh.material` as `Material | Material[]`).
export function disposeMesh(mesh: THREE.Mesh) {
  mesh.geometry.dispose();
  const m = mesh.material;
  if (Array.isArray(m)) m.forEach((mm) => mm.dispose());
  else m.dispose();
}

/// Manual merge for two-piece tools (cylinder body + hemisphere cap,
/// or flute + shank + holder stack). Avoids the
/// `BufferGeometryUtils` import dance.
export function mergeBufferGeometries(geometries: THREE.BufferGeometry[]): THREE.BufferGeometry {
  const out = new THREE.BufferGeometry();
  let posCount = 0;
  let idxCount = 0;
  for (const g of geometries) {
    posCount += g.attributes.position.count;
    idxCount += g.index ? g.index.count : g.attributes.position.count;
  }
  const positions = new Float32Array(posCount * 3);
  const indices = new Uint32Array(idxCount);
  let posOffset = 0;
  let idxOffset = 0;
  let vertexBase = 0;
  for (const g of geometries) {
    const p = g.attributes.position.array as Float32Array;
    positions.set(p, posOffset * 3);
    const idx = g.index ? (g.index.array as ArrayLike<number>) : null;
    const n = g.attributes.position.count;
    if (idx) {
      for (let i = 0; i < idx.length; i++) {
        indices[idxOffset + i] = idx[i] + vertexBase;
      }
      idxOffset += idx.length;
    } else {
      for (let i = 0; i < n; i++) {
        indices[idxOffset + i] = i + vertexBase;
      }
      idxOffset += n;
    }
    vertexBase += n;
    posOffset += n;
  }
  out.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  out.setIndex(new THREE.BufferAttribute(indices, 1));
  out.computeVertexNormals();
  return out;
}

/// Build a stacked endmill envelope: flutes + (optional) shank +
/// (optional) holder. When `fluteLen` / `shankDia` / `holder` are
/// all undefined, falls back to a single cylinder body (legacy
/// behavior for tools without holder metadata).
export function buildEndmillStack(
  diameter: number,
  mat: THREE.MeshBasicMaterial,
  fluteLen?: number,
  shankDia?: number,
  holder?: HolderShape,
): THREE.Mesh {
  const radius = diameter * 0.5;
  if (fluteLen === undefined && shankDia === undefined && !holder) {
    const bodyLen = Math.max(diameter * 6, 8);
    const body = new THREE.CylinderGeometry(radius, radius, bodyLen, 24);
    body.rotateX(Math.PI / 2);
    body.translate(0, 0, bodyLen / 2);
    return new THREE.Mesh(body, mat);
  }
  const pieces: THREE.BufferGeometry[] = [];
  const shankR = (shankDia ?? diameter) * 0.5;
  let zCursor = 0;
  const fLen = Math.max(0, fluteLen ?? Math.max(diameter * 4, 6));
  if (fLen > 0) {
    const flutes = new THREE.CylinderGeometry(radius, radius, fLen, 24);
    flutes.rotateX(Math.PI / 2);
    flutes.translate(0, 0, zCursor + fLen / 2);
    pieces.push(flutes);
    zCursor += fLen;
  }
  // Shank: between top of flutes and bottom of holder. When the
  // holder is undefined, give the shank a sensible default length so
  // the user can still see "this is the non-cutting part" sticking
  // out.
  const shankLen = holder ? Math.max(diameter * 2, 4) : Math.max(diameter * 4, 6);
  if (shankR > 0 && shankLen > 0) {
    const shank = new THREE.CylinderGeometry(shankR, shankR, shankLen, 18);
    shank.rotateX(Math.PI / 2);
    shank.translate(0, 0, zCursor + shankLen / 2);
    pieces.push(shank);
    zCursor += shankLen;
  }
  if (holder) {
    if (holder.kind === 'cylinder') {
      const r = holder.diameter_mm * 0.5;
      const len = holder.length_mm;
      if (r > 0 && len > 0) {
        const g = new THREE.CylinderGeometry(r, r, len, 18);
        g.rotateX(Math.PI / 2);
        g.translate(0, 0, zCursor + len / 2);
        pieces.push(g);
      }
    } else if (holder.kind === 'cone') {
      const rb = holder.bottom_diameter_mm * 0.5;
      const rt = holder.top_diameter_mm * 0.5;
      const len = holder.length_mm;
      if (Math.max(rb, rt) > 0 && len > 0) {
        // CylinderGeometry: first arg is +Y (upper) radius, second
        // arg is -Y (lower). After rotateX(π/2): +Y → -Z, -Y → +Z.
        // Pass (top, bottom) swapped so the bottom lands at zCursor.
        const g = new THREE.CylinderGeometry(rt, rb, len, 18);
        g.rotateX(Math.PI / 2);
        g.translate(0, 0, zCursor + len / 2);
        pieces.push(g);
      }
    } else if (holder.kind === 'stepped') {
      const cylR = holder.cylinder_diameter_mm * 0.5;
      const cylLen = holder.cylinder_length_mm;
      const coneTopR = holder.cone_top_diameter_mm * 0.5;
      const coneLen = holder.cone_length_mm;
      if (cylR > 0 && cylLen > 0) {
        const g = new THREE.CylinderGeometry(cylR, cylR, cylLen, 18);
        g.rotateX(Math.PI / 2);
        g.translate(0, 0, zCursor + cylLen / 2);
        pieces.push(g);
        zCursor += cylLen;
      }
      if (Math.max(cylR, coneTopR) > 0 && coneLen > 0) {
        const g = new THREE.CylinderGeometry(coneTopR, cylR, coneLen, 18);
        g.rotateX(Math.PI / 2);
        g.translate(0, 0, zCursor + coneLen / 2);
        pieces.push(g);
      }
    }
  }
  const merged = pieces.length === 1 ? pieces[0] : mergeBufferGeometries(pieces);
  return new THREE.Mesh(merged, mat);
}

/// Build the tool-tip mesh for the given spec. The cutting tip sits
/// at the mesh's local origin with the tool axis along +Z, so
/// `mesh.position.set(px, py, pz)` lands the tip on the toolpath
/// point. Caller chooses material color; caller disposes the mesh.
export function buildToolMesh(
  kind: string,
  mode: string,
  diameter: number,
  tipDiameter: number | undefined,
  dragoff: number | undefined,
  colorHex: number,
  fluteLen?: number,
  shankDia?: number,
  holder?: HolderShape,
  tipAngleDeg?: number,
): THREE.Mesh {
  const radius = diameter * 0.5;
  const mat = new THREE.MeshBasicMaterial({
    color: colorHex,
    transparent: true,
    opacity: 0.85,
  });
  if (mode === 'drag' || kind === 'drag_knife') {
    const off = dragoff ?? 0;
    const bladeLen = Math.max(diameter * 4, 4);
    const bladeT = Math.max(0.4, diameter * 0.4);
    const geom = new THREE.BoxGeometry(bladeT, off > 0 ? off * 2 : bladeT, bladeLen);
    geom.translate(0, 0, bladeLen / 2);
    return new THREE.Mesh(geom, mat);
  }
  if (mode === 'laser' || kind === 'laser_beam') {
    const beamLen = Math.max(8, diameter * 6);
    const geom = new THREE.CylinderGeometry(0.3, 0.3, beamLen, 12);
    geom.rotateX(Math.PI / 2); // CylinderGeometry's axis is +Y → put on +Z
    geom.translate(0, 0, beamLen / 2);
    return new THREE.Mesh(geom, mat);
  }
  if (kind === 'v_bit' || kind === 'engraver') {
    // Tapered cutter: cone whose flank angle matches the bit's FULL apex
    // angle (tipAngleDeg). The cone rises from the tip (radius tipR, 0
    // for a true V-bit) to the full radius over a height of
    // (radius − tipR) / tan(apex / 2) — so a 60° bit looks like a 60°
    // bit. Previously the height was a fixed diameter×4, which drew the
    // wrong angle for every V-bit. Honors the same convention as the
    // V-Carve depth math (z = −R / tan(tipAngle / 2)).
    const tipR = Math.max((tipDiameter ?? 0) * 0.5, 0);
    const apexDeg = tipAngleDeg ?? 60;
    const halfTan = Math.tan(((apexDeg * Math.PI) / 180) * 0.5);
    const len = halfTan > 1e-6 ? (radius - tipR) / halfTan : Math.max(diameter * 4, 8);
    const geom = new THREE.CylinderGeometry(radius, Math.max(tipR, 0.001), Math.max(len, 0.1), 24);
    geom.rotateX(Math.PI / 2);
    geom.translate(0, 0, Math.max(len, 0.1) / 2);
    return new THREE.Mesh(geom, mat);
  }
  if (kind === 'drill') {
    // Twist drill: cylindrical body with a short conical tip whose
    // FULL apex angle = tipAngleDeg (118° for general-purpose HSS, 90°
    // for thin material, 135° for stainless). Tip cone length =
    // R / tan(apex / 2) so the cone tapers from full diameter down to
    // a point. Body length honours `fluteLen` when set (the cutting
    // length of the drill) — falls back to `6 × diameter` for tools
    // without that field. The body extends UP from the top of the tip
    // cone (`zCursor = tipLen`) so the cutting edge starts at the
    // right Z and the user can see real stickout when they edit
    // fluteLengthMm in the library.
    const apexDeg = tipAngleDeg ?? 118;
    const apexRad = (apexDeg * Math.PI) / 180;
    const halfTan = Math.tan(apexRad * 0.5);
    const tipLen = halfTan > 1e-6 ? radius / halfTan : radius * 0.01;
    const bodyLen = Math.max(0, fluteLen ?? Math.max(diameter * 6, 8));
    const tip = new THREE.CylinderGeometry(radius, 0.001, tipLen, 24);
    tip.rotateX(Math.PI / 2);
    tip.translate(0, 0, tipLen / 2);
    const pieces: THREE.BufferGeometry[] = [tip];
    if (bodyLen > 0) {
      const body = new THREE.CylinderGeometry(radius, radius, bodyLen, 24);
      body.rotateX(Math.PI / 2);
      body.translate(0, 0, tipLen + bodyLen / 2);
      pieces.push(body);
    }
    const merged = pieces.length === 1 ? pieces[0] : mergeBufferGeometries(pieces);
    return new THREE.Mesh(merged, mat);
  }
  if (kind === 'ball_nose') {
    // Cylinder body with a hemisphere at the cutting end.
    const bodyLen = Math.max(diameter * 5, 8);
    const body = new THREE.CylinderGeometry(radius, radius, bodyLen, 24);
    body.rotateX(Math.PI / 2);
    body.translate(0, 0, radius + bodyLen / 2);
    const ball = new THREE.SphereGeometry(radius, 24, 12, 0, Math.PI * 2, 0, Math.PI / 2);
    // Default sphere: top half is +Y. rotateX(-π/2) puts the dome
    // face at -Z. Translate so the pole sits at z=0.
    ball.rotateX(-Math.PI / 2);
    ball.translate(0, 0, radius);
    const merged = mergeBufferGeometries([body, ball]);
    return new THREE.Mesh(merged, mat);
  }
  // Endmill / generic: stacked envelope (flutes + shank + holder).
  return buildEndmillStack(diameter, mat, fluteLen, shankDia, holder);
}
