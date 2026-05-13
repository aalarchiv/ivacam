/// Pure geometry: tessellate an imported segment (Line / Arc /
/// Point) into a flat 2D polyline for wireframe drawing. Moved out
/// of Scene3D for testability and so the file boundary documents
/// "what's pure vs what touches Three.js / DOM state."
///
/// The arc math (chord + sagitta → radius + center → angular sweep)
/// mirrors `cam::math::bulge_to_arc` on the Rust side; this is just
/// the TS equivalent we need for previewing imported geometry.

export interface TessellateSeg {
  start: { x: number; y: number };
  end: { x: number; y: number };
  bulge: number;
  type: string;
}

export function tessellate(seg: TessellateSeg): [number, number][] {
  if (seg.type === 'POINT') return [[seg.start.x, seg.start.y]];
  if (Math.abs(seg.bulge) < 1e-9) {
    return [
      [seg.start.x, seg.start.y],
      [seg.end.x, seg.end.y],
    ];
  }
  // Recompute arc center from start / end / bulge (canonical formula).
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
  // ≤10° per step, minimum 8 steps so a near-straight arc still
  // gets a few subdivisions.
  const steps = Math.max(8, Math.ceil(Math.abs(sweep) / (Math.PI / 18)));
  const pts: [number, number][] = [];
  for (let i = 0; i <= steps; i++) {
    const t = a0 + (sweep * i) / steps;
    pts.push([cx + r * Math.cos(t), cy + r * Math.sin(t)]);
  }
  return pts;
}
