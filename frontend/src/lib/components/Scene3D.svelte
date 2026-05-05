<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as THREE from 'three';
  import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
  import { project } from '../state/project.svelte';

  let host: HTMLDivElement;
  let renderer: THREE.WebGLRenderer | undefined;
  let scene: THREE.Scene | undefined;
  let camera: THREE.PerspectiveCamera | undefined;
  let controls: OrbitControls | undefined;
  let geometryGroup: THREE.Group | undefined;
  let raf = 0;
  let observer: ResizeObserver | undefined;

  onMount(() => {
    scene = new THREE.Scene();
    scene.background = new THREE.Color(0x0d0d0d);

    camera = new THREE.PerspectiveCamera(45, 1, 0.1, 5000);
    camera.position.set(150, -150, 150);
    camera.up.set(0, 0, 1);

    renderer = new THREE.WebGLRenderer({ antialias: true });
    renderer.setPixelRatio(window.devicePixelRatio);
    host.appendChild(renderer.domElement);

    controls = new OrbitControls(camera, renderer.domElement);
    controls.enableDamping = true;

    // Z-up grid on the XY plane.
    const grid = new THREE.GridHelper(400, 40, 0x444444, 0x222222);
    grid.rotation.x = Math.PI / 2;
    scene.add(grid);

    const axes = new THREE.AxesHelper(50);
    scene.add(axes);

    scene.add(new THREE.AmbientLight(0xffffff, 0.7));
    const dir = new THREE.DirectionalLight(0xffffff, 0.8);
    dir.position.set(100, -100, 200);
    scene.add(dir);

    geometryGroup = new THREE.Group();
    scene.add(geometryGroup);

    observer = new ResizeObserver(() => fit());
    observer.observe(host);
    fit();

    const tick = () => {
      controls?.update();
      if (renderer && scene && camera) renderer.render(scene, camera);
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
  });

  onDestroy(() => {
    cancelAnimationFrame(raf);
    observer?.disconnect();
    controls?.dispose();
    renderer?.dispose();
    if (renderer && host?.contains(renderer.domElement)) {
      host.removeChild(renderer.domElement);
    }
  });

  function fit() {
    if (!renderer || !camera || !host) return;
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    renderer.setSize(w, h);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
  }

  // Mirror imported geometry into the 3D scene as flat polylines on Z=0.
  // When a /generate response is also available, draw the 3D toolpath on
  // top with depth + color coded by move kind (rapid/cut/plunge/retract).
  $effect(() => {
    if (!geometryGroup || !scene) return;
    geometryGroup.clear();
    const data = project.imported;
    const gen = project.generated;
    if (!data && !gen) return;

    const positions: number[] = [];
    const colors: number[] = [];
    const c = new THREE.Color();

    // Imported entities at Z=0 (faded if a toolpath is also visible).
    if (data) {
      const flat = !!gen;
      for (const seg of data.segments) {
        if (!project.visibleLayers.has(seg.layer)) continue;
        const points = tessellate(seg);
        c.set(flat ? 0x444444 : aciColor(seg.color));
        for (let i = 0; i < points.length - 1; i++) {
          const [ax, ay] = points[i];
          const [bx, by] = points[i + 1];
          positions.push(ax, ay, 0, bx, by, 0);
          colors.push(c.r, c.g, c.b, c.r, c.g, c.b);
        }
      }
    }

    // Toolpath: rapid=cyan, cut=red, plunge=yellow, retract=green.
    const toolpathColor: Record<string, number> = {
      rapid: 0x35a2ff,
      cut: 0xff5555,
      plunge: 0xffd23a,
      retract: 0x5fd06e,
      arc: 0xff8a3a,
    };
    if (gen) {
      for (const seg of gen.toolpath) {
        c.set(toolpathColor[seg.kind] ?? 0xffffff);
        positions.push(seg.from.x, seg.from.y, seg.from.z, seg.to.x, seg.to.y, seg.to.z);
        colors.push(c.r, c.g, c.b, c.r, c.g, c.b);
      }
    }
    if (positions.length === 0) return;
    const geom = new THREE.BufferGeometry();
    geom.setAttribute('position', new THREE.Float32BufferAttribute(positions, 3));
    geom.setAttribute('color', new THREE.Float32BufferAttribute(colors, 3));
    const mat = new THREE.LineBasicMaterial({ vertexColors: true });
    const lines = new THREE.LineSegments(geom, mat);
    geometryGroup.add(lines);

    // Frame the geometry's bounding sphere with margin, then place the
    // camera along the +x/-y/+z diagonal looking at the centroid.
    if (camera && controls) {
      const sphere = new THREE.Sphere();
      lines.geometry.computeBoundingSphere();
      if (lines.geometry.boundingSphere) sphere.copy(lines.geometry.boundingSphere);
      const radius = Math.max(sphere.radius, 1);
      const fov = (camera.fov * Math.PI) / 180;
      const distance = (radius * 1.4) / Math.sin(fov / 2);
      const dir = new THREE.Vector3(0.6, -0.9, 0.9).normalize();
      controls.target.copy(sphere.center);
      camera.position.copy(sphere.center).addScaledVector(dir, distance);
      camera.near = Math.max(distance / 1000, 0.01);
      camera.far = distance * 10;
      camera.updateProjectionMatrix();
      controls.update();
    }
  });

  function tessellate(seg: { start: { x: number; y: number }; end: { x: number; y: number }; bulge: number; type: string }): [number, number][] {
    if (seg.type === 'POINT') return [[seg.start.x, seg.start.y]];
    if (Math.abs(seg.bulge) < 1e-9) {
      return [
        [seg.start.x, seg.start.y],
        [seg.end.x, seg.end.y],
      ];
    }
    // Recompute arc center from start/end/bulge (canonical formula).
    const dx = seg.end.x - seg.start.x;
    const dy = seg.end.y - seg.start.y;
    const chord = Math.hypot(dx, dy);
    if (chord < 1e-9) return [[seg.start.x, seg.start.y]];
    const sagitta = (seg.bulge * chord) / 2;
    const r = (chord / 2) ** 2 / (2 * Math.abs(sagitta)) + Math.abs(sagitta) / 2;
    const mx = (seg.start.x + seg.end.x) / 2;
    const my = (seg.start.y + seg.end.y) / 2;
    const ux = -dy / chord;
    const uy = dx / chord;
    const h = r - Math.abs(sagitta);
    const sign = seg.bulge > 0 ? 1 : -1;
    const cx = mx + ux * h * sign;
    const cy = my + uy * h * sign;
    const a0 = Math.atan2(seg.start.y - cy, seg.start.x - cx);
    const a1 = Math.atan2(seg.end.y - cy, seg.end.x - cx);
    let sweep = a1 - a0;
    if (seg.bulge > 0 && sweep < 0) sweep += Math.PI * 2;
    if (seg.bulge < 0 && sweep > 0) sweep -= Math.PI * 2;
    const steps = Math.max(8, Math.ceil(Math.abs(sweep) / (Math.PI / 18))); // ≤10° per step
    const pts: [number, number][] = [];
    for (let i = 0; i <= steps; i++) {
      const t = a0 + (sweep * i) / steps;
      pts.push([cx + r * Math.cos(t), cy + r * Math.sin(t)]);
    }
    return pts;
  }

  function aciColor(c: number): number {
    const map: Record<number, number> = {
      1: 0xff0000,
      2: 0xffff00,
      3: 0x00ff00,
      4: 0x00ffff,
      5: 0x0000ff,
      6: 0xff00ff,
      7: 0xe6e6e6,
    };
    return map[c] ?? 0xbbbbbb;
  }
</script>

<div class="scene" bind:this={host}></div>

<style>
  .scene {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    background: #0d0d0d;
  }
</style>
