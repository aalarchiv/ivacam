/// Fat-line (Line2 / LineSegments2) construction. WebGL caps
/// LineBasicMaterial.linewidth to 1px, so the preview-line-width setting
/// (68ab) drives LineMaterial instead, which renders width in screen
/// pixels via a `resolution` uniform that must track the canvas size.
///
/// Flat per-segment position + color arrays (6 floats per segment —
/// start-rgb, end-rgb) match exactly how LineSegmentsGeometry stores its
/// interleaved instance buffers, so the playhead-fade / selection-recolor
/// offset math downstream is unchanged.
///
/// Extracted from Scene3D.svelte (4w2f) so both line builders share it.

import { LineSegments2 } from 'three/addons/lines/LineSegments2.js';
import { LineSegmentsGeometry } from 'three/addons/lines/LineSegmentsGeometry.js';
import { LineMaterial } from 'three/addons/lines/LineMaterial.js';

export function buildFatLines(
  positions: number[],
  colors: number[],
  lineWidth: number,
  width: number,
  height: number,
): LineSegments2 {
  const geom = new LineSegmentsGeometry();
  geom.setPositions(new Float32Array(positions));
  geom.setColors(new Float32Array(colors));
  const mat = new LineMaterial({
    vertexColors: true,
    linewidth: Math.max(0.5, lineWidth),
    worldUnits: false,
  });
  mat.resolution.set(width || 1, height || 1);
  return new LineSegments2(geom, mat);
}
