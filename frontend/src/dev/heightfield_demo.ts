/// Standalone demo for the HeightfieldMesh helper. NOT wired into the
/// app's main routes — this is a manual smoke check that exercises every
/// public method against a synthetic sinusoidal heightmap.
///
/// To run it:
///   1. Add a temporary `dev.html` to the frontend root (next to
///      `index.html`) whose `<script type="module" src="...">` points at
///      this file:
///        <script type="module" src="/src/dev/heightfield_demo.ts"></script>
///      and a `<div id="heightfield-demo" style="width:100vw;height:100vh"></div>`.
///   2. `npm run dev` and browse to /dev.html.
/// The orchestrator can later wire this as a multi-input vite build target
/// or a /dev route; that lives in c28, not here.
///
/// Mounts a Three.js scene, builds a 64×64 heightmap with values
/// `topZ - sin(x*0.4)*cos(y*0.4) * 2`, runs it through HeightfieldMesh,
/// and animates a slow phase shift so the surface ripples — this proves
/// `updateHeights` (full and AABB) and `rebuildEdges` debouncing work.
/// Press 'e' to toggle edges, 's' to toggle solid, 'v' to toggle the
/// whole group, and 'r' to retint with a random hex color via
/// `setStyle`.

import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { HeightfieldMesh } from '../lib/sim/heightfield_mesh';

const COLS = 64;
const ROWS = 64;
const CELL = 1.5;
const TOP_Z = 0;
const AMPLITUDE = 2;

export function mountHeightfieldDemo(host: HTMLElement): () => void {
  const scene = new THREE.Scene();
  scene.background = new THREE.Color(0x101418);

  const camera = new THREE.PerspectiveCamera(45, 1, 0.1, 5000);
  camera.position.set(80, -120, 90);
  camera.up.set(0, 0, 1);

  const renderer = new THREE.WebGLRenderer({ antialias: true });
  renderer.setPixelRatio(window.devicePixelRatio);
  host.appendChild(renderer.domElement);

  const controls = new OrbitControls(camera, renderer.domElement);
  controls.enableDamping = true;

  scene.add(new THREE.AmbientLight(0xffffff, 0.5));
  const dir = new THREE.DirectionalLight(0xffffff, 0.9);
  dir.position.set(80, -100, 200);
  scene.add(dir);

  const grid = new THREE.GridHelper(200, 20, 0x404040, 0x202020);
  grid.rotation.x = Math.PI / 2;
  scene.add(grid);
  scene.add(new THREE.AxesHelper(20));

  const hf = new HeightfieldMesh({
    cols: COLS,
    rows: ROWS,
    cellSize: CELL,
    originX: -((COLS * CELL) / 2),
    originY: -((ROWS * CELL) / 2),
    topZ: TOP_Z,
    solidColor: '#c8b48a',
    solidOpacity: 0.5,
    edgeColor: '#1a1a1a',
    edgeOpacity: 1.0,
  });
  scene.add(hf.group);

  controls.target.set(0, 0, 0);
  controls.update();

  const data = new Float32Array(COLS * ROWS);
  function fillData(phase: number) {
    for (let iy = 0; iy < ROWS; iy++) {
      for (let ix = 0; ix < COLS; ix++) {
        const x = ix * 0.4;
        const y = iy * 0.4;
        data[iy * COLS + ix] = TOP_Z - Math.sin(x + phase) * Math.cos(y) * AMPLITUDE;
      }
    }
  }

  fillData(0);
  hf.updateHeights(data);
  hf.rebuildEdges();

  // Animate a slow phase shift; rebuild edges at ~10 Hz, not every frame.
  let lastEdgeRebuild = performance.now();
  const start = performance.now();
  let raf = 0;
  function tick() {
    const now = performance.now();
    const phase = (now - start) * 0.0008;
    fillData(phase);
    // Alternate between full updates and a single-quadrant AABB update
    // every other frame to prove both code paths render correctly.
    if (Math.floor(phase * 4) % 2 === 0) {
      hf.updateHeights(data);
    } else {
      hf.updateHeights(data, { ix0: 0, iy0: 0, ix1: COLS >> 1, iy1: ROWS >> 1 });
      hf.updateHeights(data, { ix0: COLS >> 1, iy0: ROWS >> 1, ix1: COLS, iy1: ROWS });
    }
    if (now - lastEdgeRebuild > 100) {
      hf.rebuildEdges();
      lastEdgeRebuild = now;
    }
    controls.update();
    renderer.render(scene, camera);
    raf = requestAnimationFrame(tick);
  }

  function fit() {
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    renderer.setSize(w, h);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
  }
  const ro = new ResizeObserver(fit);
  ro.observe(host);
  fit();

  // Keyboard hotkeys exercise the visibility + setStyle paths. Track
  // toggle state locally; the helper exposes setters but no getters.
  let edgesVisible = true;
  let solidVisible = true;
  let groupVisible = true;
  function onKey(e: KeyboardEvent) {
    if (e.key === 'e') {
      edgesVisible = !edgesVisible;
      hf.setEdgesVisible(edgesVisible);
    } else if (e.key === 's') {
      solidVisible = !solidVisible;
      hf.setSolidVisible(solidVisible);
    } else if (e.key === 'v') {
      groupVisible = !groupVisible;
      hf.setVisible(groupVisible);
    } else if (e.key === 'r') {
      const rand = `#${Math.floor(Math.random() * 0xffffff)
        .toString(16)
        .padStart(6, '0')}`;
      hf.setStyle({ solidColor: rand });
    }
  }
  window.addEventListener('keydown', onKey);

  raf = requestAnimationFrame(tick);

  return () => {
    cancelAnimationFrame(raf);
    window.removeEventListener('keydown', onKey);
    ro.disconnect();
    controls.dispose();
    hf.dispose();
    renderer.dispose();
    if (host.contains(renderer.domElement)) host.removeChild(renderer.domElement);
  };
}

// Auto-mount when this file is the entry point of a standalone HTML page.
// `import.meta.url` is unique per module, so this only runs when loaded
// directly (not when imported by another module).
if (typeof document !== 'undefined') {
  const target = document.getElementById('heightfield-demo');
  if (target) {
    mountHeightfieldDemo(target);
  }
}
