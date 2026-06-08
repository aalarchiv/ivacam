import { describe, it, expect, vi } from 'vitest';
import * as THREE from 'three';
import { disposeGroup } from './dispose';

describe('disposeGroup', () => {
  it('detaches and disposes geometry + material of every child', () => {
    const g = new THREE.Group();
    const meshGeom = new THREE.BoxGeometry(1, 1, 1);
    const meshMat = new THREE.MeshBasicMaterial();
    const lineGeom = new THREE.BufferGeometry();
    const lineMat = new THREE.LineBasicMaterial();
    g.add(new THREE.Mesh(meshGeom, meshMat));
    g.add(new THREE.LineSegments(lineGeom, lineMat));

    const geomSpies = [meshGeom, lineGeom].map((x) => vi.spyOn(x, 'dispose'));
    const matSpies = [meshMat, lineMat].map((x) => vi.spyOn(x, 'dispose'));

    disposeGroup(g);

    expect(g.children).toHaveLength(0);
    for (const s of geomSpies) expect(s).toHaveBeenCalledOnce();
    for (const s of matSpies) expect(s).toHaveBeenCalledOnce();
  });

  it('disposes every material of a multi-material mesh', () => {
    const g = new THREE.Group();
    const geom = new THREE.BoxGeometry(1, 1, 1);
    const mats = [new THREE.MeshBasicMaterial(), new THREE.MeshBasicMaterial()];
    g.add(new THREE.Mesh(geom, mats));
    const spies = mats.map((m) => vi.spyOn(m, 'dispose'));

    disposeGroup(g);

    for (const s of spies) expect(s).toHaveBeenCalledOnce();
  });

  it('is a no-op on an empty group', () => {
    const g = new THREE.Group();
    expect(() => disposeGroup(g)).not.toThrow();
    expect(g.children).toHaveLength(0);
  });
});
