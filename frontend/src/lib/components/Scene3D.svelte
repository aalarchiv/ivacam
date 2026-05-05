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
  let toolGroup: THREE.Group | undefined;
  let raf = 0;
  let observer: ResizeObserver | undefined;
  let themeMql: MediaQueryList | undefined;

  function cssVar(name: string, fallback: string): string {
    if (!host) return fallback;
    const v = getComputedStyle(host).getPropertyValue(name).trim();
    return v || fallback;
  }
  function cssColor(name: string, fallback: number): THREE.Color {
    return new THREE.Color(cssVar(name, '') || fallback);
  }

  onMount(() => {
    scene = new THREE.Scene();
    scene.background = cssColor('--bg-app', 0x0d0d0d);

    camera = new THREE.PerspectiveCamera(45, 1, 0.1, 5000);
    camera.position.set(150, -150, 150);
    camera.up.set(0, 0, 1);

    renderer = new THREE.WebGLRenderer({ antialias: true });
    renderer.setPixelRatio(window.devicePixelRatio);
    host.appendChild(renderer.domElement);

    controls = new OrbitControls(camera, renderer.domElement);
    controls.enableDamping = true;

    // Z-up grid on the XY plane. Colors track the active theme.
    const gridMajor = cssColor('--grid-major', 0x262626);
    const gridMinor = cssColor('--grid-minor', 0x1a1a1a);
    const grid = new THREE.GridHelper(400, 40, gridMajor, gridMinor);
    grid.rotation.x = Math.PI / 2;
    grid.name = 'theme-grid';
    scene.add(grid);

    const axes = new THREE.AxesHelper(50);
    scene.add(axes);

    scene.add(new THREE.AmbientLight(0xffffff, 0.7));
    const dir = new THREE.DirectionalLight(0xffffff, 0.8);
    dir.position.set(100, -100, 200);
    scene.add(dir);

    geometryGroup = new THREE.Group();
    scene.add(geometryGroup);

    toolGroup = new THREE.Group();
    scene.add(toolGroup);

    observer = new ResizeObserver(() => fit());
    observer.observe(host);
    fit();

    const tick = () => {
      controls?.update();
      if (renderer && scene && camera) renderer.render(scene, camera);
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);

    // Re-skin background + grid + (re-trigger) toolpath colors when the
    // OS theme changes. The toolpath group rebuilds via the $effect below
    // since we touch project.imported as a Svelte dep.
    themeMql = window.matchMedia('(prefers-color-scheme: light)');
    const onTheme = () => applyTheme();
    themeMql.addEventListener('change', onTheme);
  });

  function applyTheme() {
    if (!scene) return;
    scene.background = cssColor('--bg-app', 0x0d0d0d);
    const grid = scene.getObjectByName('theme-grid') as THREE.GridHelper | undefined;
    if (grid) {
      const newGrid = new THREE.GridHelper(
        400,
        40,
        cssColor('--grid-major', 0x262626),
        cssColor('--grid-minor', 0x1a1a1a),
      );
      newGrid.rotation.x = Math.PI / 2;
      newGrid.name = 'theme-grid';
      scene.remove(grid);
      grid.geometry.dispose();
      (grid.material as THREE.Material).dispose();
      scene.add(newGrid);
    }
    rebuildGeometry();
  }

  onDestroy(() => {
    cancelAnimationFrame(raf);
    observer?.disconnect();
    controls?.dispose();
    renderer?.dispose();
    if (themeMql) {
      const handler = () => applyTheme();
      themeMql.removeEventListener('change', handler);
    }
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
    void project.imported;
    void project.visibleLayers;
    void project.generated;
    void project.playhead;
    void project.tabs;
    rebuildGeometry();
    updateTool();
    updateTabs();
  });

  let tabsGroup: THREE.Group | undefined;

  function updateTabs() {
    if (!scene) return;
    if (!tabsGroup) {
      tabsGroup = new THREE.Group();
      scene.add(tabsGroup);
    }
    tabsGroup.clear();
    const color = cssColor('--tab-marker', 0xffd23a);
    const radius = Math.max(
      0.5,
      ((project.imported?.bbox.max_x ?? 100) - (project.imported?.bbox.min_x ?? 0)) * 0.008,
    );
    const geom = new THREE.SphereGeometry(radius, 12, 8);
    const mat = new THREE.MeshBasicMaterial({ color });
    for (const list of Object.values(project.tabs)) {
      for (const t of list) {
        const sphere = new THREE.Mesh(geom, mat);
        sphere.position.set(t.x, t.y, 0);
        tabsGroup.add(sphere);
      }
    }
  }

  /// Tool-tip cone: a small inverted cone whose apex sits at the current
  /// toolpath position. Color is the active move kind (cut/plunge/etc.) so
  /// the user can see at a glance what the tool is doing right now.
  function updateTool() {
    if (!toolGroup) return;
    toolGroup.clear();
    const gen = project.generated;
    if (!gen || gen.toolpath.length === 0) return;
    const total = gen.toolpath.length;
    const headIdx = Math.max(0, Math.min(total - 1, Math.round(project.playhead * total) - 1));
    const seg = gen.toolpath[headIdx];
    if (!seg) return;
    // Interpolate within the active segment for smooth motion at low speeds.
    const subT = project.playhead * total - headIdx;
    const t = Math.max(0, Math.min(1, subT));
    const px = seg.from.x + (seg.to.x - seg.from.x) * t;
    const py = seg.from.y + (seg.to.y - seg.from.y) * t;
    const pz = seg.from.z + (seg.to.z - seg.from.z) * t;
    const tipColor: Record<string, number> = {
      rapid: 0x35a2ff,
      cut: 0xff5555,
      plunge: 0xffd23a,
      retract: 0x5fd06e,
      arc: 0xff8a3a,
    };
    const colorHex = tipColor[seg.kind] ?? 0xff5555;
    const radius = Math.max(2, ((project.imported?.bbox.max_x ?? 100) - (project.imported?.bbox.min_x ?? 0)) * 0.015);
    const height = radius * 4;
    const geom = new THREE.ConeGeometry(radius, height, 16);
    geom.rotateX(Math.PI); // apex points down (-Z)
    geom.translate(0, 0, height / 2);
    const mat = new THREE.MeshBasicMaterial({ color: colorHex, transparent: true, opacity: 0.85 });
    const mesh = new THREE.Mesh(geom, mat);
    mesh.position.set(px, py, pz);
    toolGroup.add(mesh);
  }

  function rebuildGeometry() {
    if (!geometryGroup || !scene) return;
    geometryGroup.clear();
    const data = project.imported;
    const gen = project.generated;
    if (!data && !gen) return;

    const positions: number[] = [];
    const colors: number[] = [];
    const c = new THREE.Color();

    const fadedColor = cssColor('--imported-faded', 0x444444);
    if (data) {
      const flat = !!gen;
      for (const seg of data.segments) {
        if (!project.visibleLayers.has(seg.layer)) continue;
        const points = tessellate(seg);
        if (flat) {
          c.copy(fadedColor);
        } else {
          c.copy(aciColor(seg.color));
        }
        for (let i = 0; i < points.length - 1; i++) {
          const [ax, ay] = points[i];
          const [bx, by] = points[i + 1];
          positions.push(ax, ay, 0, bx, by, 0);
          colors.push(c.r, c.g, c.b, c.r, c.g, c.b);
        }
      }
    }

    if (gen) {
      const toolpath: Record<string, THREE.Color> = {
        rapid: cssColor('--toolpath-rapid', 0x35a2ff),
        cut: cssColor('--toolpath-cut', 0xff5555),
        plunge: cssColor('--toolpath-plunge', 0xffd23a),
        retract: cssColor('--toolpath-retract', 0x5fd06e),
        arc: cssColor('--toolpath-arc', 0xff8a3a),
      };
      const total = gen.toolpath.length;
      const head = Math.max(0, Math.min(total, Math.round(project.playhead * total)));
      for (let i = 0; i < total; i++) {
        const seg = gen.toolpath[i];
        const tp = toolpath[seg.kind] ?? toolpath.cut;
        let r = tp.r;
        let g = tp.g;
        let b = tp.b;
        // Future moves (after the head) faded so the user can see what's
        // come and what's coming next.
        if (i >= head) {
          const f = 0.25;
          r = tp.r * f + 0.05;
          g = tp.g * f + 0.05;
          b = tp.b * f + 0.05;
        }
        positions.push(seg.from.x, seg.from.y, seg.from.z, seg.to.x, seg.to.y, seg.to.z);
        colors.push(r, g, b, r, g, b);
      }
    }
    if (positions.length === 0) return;
    const geom = new THREE.BufferGeometry();
    geom.setAttribute('position', new THREE.Float32BufferAttribute(positions, 3));
    geom.setAttribute('color', new THREE.Float32BufferAttribute(colors, 3));
    const mat = new THREE.LineBasicMaterial({ vertexColors: true });
    const lines = new THREE.LineSegments(geom, mat);
    geometryGroup.add(lines);

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
  }

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

  function aciColor(c: number): THREE.Color {
    const fixed: Record<number, number> = {
      1: 0xff0000,
      2: 0xffff00,
      3: 0x00ff00,
      4: 0x00ffff,
      5: 0x0000ff,
      6: 0xff00ff,
    };
    if (c === 7 || c === 256) return cssColor('--text-strong', 0xe6e6e6);
    if (c === 8) return cssColor('--text-muted', 0x888888);
    if (fixed[c] !== undefined) return new THREE.Color(fixed[c]);
    return cssColor('--text-faint', 0xbbbbbb);
  }
</script>

<div class="scene" bind:this={host}></div>

<style>
  .scene {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    background: var(--bg-app);
  }
</style>
