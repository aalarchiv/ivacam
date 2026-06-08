/// Common shape for the scene3d mesh builders.
///
/// Each builder owns a `THREE.Group` it adds to the shared scene, rebuilds
/// that group's contents from plain typed data via `build(input)`, and
/// frees everything via `dispose()`. Builders never read the Svelte rune
/// store — the renderer host (`Scene3D.svelte`) reads `project` fields in
/// its `$effect`s and hands them in as explicit data args, so each builder
/// is unit-testable without the rune runtime (mirrors `HeightfieldDriver`
/// in ../sim/driver.ts).

import type * as THREE from 'three';

/// Wiring every builder needs: the scene to attach its group to, and a
/// callback to mark the next animation frame dirty after a mutation.
export interface BuilderContext {
  scene: THREE.Scene;
  requestRender: () => void;
}

/// Minimal builder: owns a group, frees it on teardown.
export interface Builder {
  dispose(): void;
}

/// A builder that renders fat lines (Line2 / LineSegments2). The host
/// iterates these for the cross-cutting material effects — preview line
/// width (68ab), the live canvas `resolution` uniform (must track canvas
/// size or the lines render wrong), and wireframe visibility (preview
/// mode).
export interface LineBuilder extends Builder {
  setLineWidth(lw: number): void;
  setResolution(w: number, h: number): void;
  setWireVisible(visible: boolean): void;
}
