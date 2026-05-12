/// TS mirror of `wiac_core::cam::tabs` (rt1.10). The 2D canvas
/// (ghost-tab projection + click toggle) and the 3D scene (tab marker
/// rendering) both need to resolve `(objectId, t)` to world XY without
/// a round-trip to the backend. Keep this file numerically equivalent
/// to the Rust helpers so the ghost cursor lines up with what the
/// backend computes.

import type { ImportResponse, Point2, Segment } from '../api/types';
import type { TabPlacement } from '../state/project.svelte';

/// One closed/open chained object: a flat polyline (post-bulge
/// densification) and a "closed" flag.
export interface ObjectPolyline {
  /// 1-based id matching `OperationSource::Objects::ids`.
  objectId: number;
  /// Densified XY vertices (sampling matches Rust's
  /// `segments_to_points(_, 6)` density at curved segments).
  pts: Point2[];
  closed: boolean;
}

/// Build a per-object polyline list from the imported geometry. Each
/// object groups every segment that shares its 1-based id via
/// `imported.objects[seg_idx]`. Arc / circle segments get linear
/// densification with 6 interpolation steps (matching the Rust
/// `interpolate` argument the pipeline uses).
export function buildObjectPolylines(imp: ImportResponse): ObjectPolyline[] {
  const groups = new Map<number, Segment[]>();
  for (let i = 0; i < imp.segments.length; i++) {
    const obj = imp.objects?.[i] ?? 0;
    if (obj === 0) continue;
    const seg = imp.segments[i];
    const arr = groups.get(obj) ?? [];
    arr.push(seg);
    groups.set(obj, arr);
  }
  const out: ObjectPolyline[] = [];
  for (const [objectId, segs] of groups) {
    const pts: Point2[] = [];
    for (let j = 0; j < segs.length; j++) {
      const s = segs[j];
      pts.push(...densifySegment(s, 6, j === 0));
    }
    // Heuristic: closed when the chain's first and last vertex
    // coincide (within 1e-6). Matches what the backend chaining pass
    // marks as closed.
    const first = pts[0];
    const last = pts[pts.length - 1];
    const closed =
      pts.length >= 3 &&
      first != null &&
      last != null &&
      Math.hypot(first.x - last.x, first.y - last.y) < 1e-3;
    if (closed && pts.length > 1) {
      pts.pop(); // drop the duplicate seam — polyline_at_t treats the chain as a loop
    }
    if (pts.length >= 2) out.push({ objectId, pts, closed });
  }
  return out;
}

/// Densify a single segment to N+1 points. For Line / Point: 2 points
/// (start, end). For Arc / Circle (bulge !== 0): 1 + steps points along
/// the arc.
function densifySegment(s: Segment, steps: number, includeStart: boolean): Point2[] {
  if (s.type === 'POINT' || (s.bulge ?? 0) === 0) {
    return includeStart ? [s.start, s.end] : [s.end];
  }
  const out: Point2[] = includeStart ? [s.start] : [];
  const bulge = s.bulge!;
  const dx = s.end.x - s.start.x;
  const dy = s.end.y - s.start.y;
  const chord = Math.hypot(dx, dy);
  if (chord < 1e-9) return out;
  const sagitta = (bulge * chord) / 2;
  const radius = (chord / 2) ** 2 / (2 * Math.abs(sagitta)) + Math.abs(sagitta) / 2;
  const mx = (s.start.x + s.end.x) / 2;
  const my = (s.start.y + s.end.y) / 2;
  const ux = -dy / chord;
  const uy = dx / chord;
  const h = radius - Math.abs(sagitta);
  const sign = bulge > 0 ? 1 : -1;
  const cx = mx + ux * h * sign;
  const cy = my + uy * h * sign;
  const startAng = Math.atan2(s.start.y - cy, s.start.x - cx);
  const endAng = Math.atan2(s.end.y - cy, s.end.x - cx);
  let delta = endAng - startAng;
  // bulge > 0 → CCW (positive delta)
  if (bulge > 0 && delta < 0) delta += 2 * Math.PI;
  if (bulge < 0 && delta > 0) delta -= 2 * Math.PI;
  for (let i = 1; i <= steps; i++) {
    const t = i / steps;
    const a = startAng + delta * t;
    out.push({ x: cx + radius * Math.cos(a), y: cy + radius * Math.sin(a) });
  }
  return out;
}

/// Cumulative arc lengths per vertex; `total_open` is the polyline
/// length not including the closing edge.
function arcLengths(pts: Point2[]): { cum: number[]; totalOpen: number } {
  const cum = [0];
  let total = 0;
  for (let i = 0; i + 1 < pts.length; i++) {
    total += Math.hypot(pts[i + 1].x - pts[i].x, pts[i + 1].y - pts[i].y);
    cum.push(total);
  }
  return { cum, totalOpen: total };
}

/// Project a world point onto a polyline. Returns `(t, snap, d²)` where
/// `t ∈ [0, 1)` is the arc-length parameter (closed loops wrap; open
/// polylines clamp to [0, 1 − ε]). `snap` is the projected point.
export function polylineProject(
  pts: Point2[],
  q: Point2,
  closed: boolean,
): { t: number; snap: Point2; d2: number } {
  if (pts.length < 2) return { t: 0, snap: pts[0] ?? { x: 0, y: 0 }, d2: Infinity };
  const { totalOpen } = arcLengths(pts);
  const totalClose = closed ? Math.hypot(pts[0].x - pts.at(-1)!.x, pts[0].y - pts.at(-1)!.y) : 0;
  const total = totalOpen + totalClose;
  if (total < 1e-12) return { t: 0, snap: pts[0], d2: 0 };
  const nSegs = closed ? pts.length : pts.length - 1;
  let bestD2 = Infinity;
  let bestT = 0;
  let bestSnap: Point2 = pts[0];
  let acc = 0;
  for (let i = 0; i < nSegs; i++) {
    const a = pts[i];
    const b = pts[(i + 1) % pts.length];
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const segLen = Math.hypot(dx, dy);
    if (segLen < 1e-12) continue;
    let u = ((q.x - a.x) * dx + (q.y - a.y) * dy) / (segLen * segLen);
    u = Math.max(0, Math.min(1, u));
    const sx = a.x + u * dx;
    const sy = a.y + u * dy;
    const d2 = (q.x - sx) * (q.x - sx) + (q.y - sy) * (q.y - sy);
    if (d2 < bestD2) {
      bestD2 = d2;
      bestT = (acc + u * segLen) / total;
      bestSnap = { x: sx, y: sy };
    }
    acc += segLen;
  }
  const t = closed ? ((bestT % 1) + 1) % 1 : Math.max(0, Math.min(1 - 1e-12, bestT));
  return { t, snap: bestSnap, d2: bestD2 };
}

/// Inverse: walk the polyline to arc-length parameter `t` and return
/// the world point + unit tangent vector. Outgoing tangent at vertices.
export function polylineAtT(
  pts: Point2[],
  t: number,
  closed: boolean,
): { point: Point2; tangent: Point2 } {
  if (pts.length < 2) return { point: pts[0] ?? { x: 0, y: 0 }, tangent: { x: 1, y: 0 } };
  const { totalOpen } = arcLengths(pts);
  const totalClose = closed ? Math.hypot(pts[0].x - pts.at(-1)!.x, pts[0].y - pts.at(-1)!.y) : 0;
  const total = totalOpen + totalClose;
  if (total < 1e-12) return { point: pts[0], tangent: { x: 1, y: 0 } };
  const tw = closed ? ((t % 1) + 1) % 1 : Math.max(0, Math.min(1 - 1e-12, t));
  const target = tw * total;
  const nSegs = closed ? pts.length : pts.length - 1;
  let acc = 0;
  for (let i = 0; i < nSegs; i++) {
    const a = pts[i];
    const b = pts[(i + 1) % pts.length];
    const segLen = Math.hypot(b.x - a.x, b.y - a.y);
    if (segLen < 1e-12) continue;
    if (target <= acc + segLen) {
      const u = Math.max(0, Math.min(1, (target - acc) / segLen));
      return {
        point: { x: a.x + u * (b.x - a.x), y: a.y + u * (b.y - a.y) },
        tangent: { x: (b.x - a.x) / segLen, y: (b.y - a.y) / segLen },
      };
    }
    acc += segLen;
  }
  // Fall-through (numerical drift): clamp to last segment endpoint.
  const i = nSegs - 1;
  const a = pts[i];
  const b = pts[(i + 1) % pts.length];
  const segLen = Math.max(1e-12, Math.hypot(b.x - a.x, b.y - a.y));
  return { point: b, tangent: { x: (b.x - a.x) / segLen, y: (b.y - a.y) / segLen } };
}

/// Walk a polyline and collect t parameters for every vertex and
/// every segment midpoint (1q3). Useful as snap candidates next to
/// the contour projection.
export function vertexAndMidpointTs(
  pts: Point2[],
  closed: boolean,
): { t: number; point: Point2; kind: 'vertex' | 'midpoint' }[] {
  if (pts.length < 2) return [];
  const { totalOpen } = arcLengths(pts);
  const totalClose = closed ? Math.hypot(pts[0].x - pts.at(-1)!.x, pts[0].y - pts.at(-1)!.y) : 0;
  const total = totalOpen + totalClose;
  if (total < 1e-12) return [];
  const out: { t: number; point: Point2; kind: 'vertex' | 'midpoint' }[] = [];
  const nSegs = closed ? pts.length : pts.length - 1;
  let acc = 0;
  for (let i = 0; i < nSegs; i++) {
    const a = pts[i];
    const b = pts[(i + 1) % pts.length];
    out.push({ t: acc / total, point: { x: a.x, y: a.y }, kind: 'vertex' });
    const segLen = Math.hypot(b.x - a.x, b.y - a.y);
    if (segLen > 1e-12) {
      out.push({
        t: (acc + segLen * 0.5) / total,
        point: { x: a.x + (b.x - a.x) * 0.5, y: a.y + (b.y - a.y) * 0.5 },
        kind: 'midpoint',
      });
    }
    acc += segLen;
  }
  if (!closed && pts.length > 0) {
    out.push({ t: 1 - 1e-12, point: pts[pts.length - 1], kind: 'vertex' });
  }
  return out;
}

/// N evenly spaced tab parameters. Closed: [0, 1/N, 2/N, ...]. Open:
/// inset by 0.5/N so the first/last don't land on the endpoints.
export function autoTabTs(count: number, closed: boolean): number[] {
  if (count <= 0) return [];
  if (closed) return Array.from({ length: count }, (_, i) => i / count);
  return Array.from({ length: count }, (_, i) => (i + 0.5) / count);
}

/// Resolve a single `TabPlacement` to a world (x, y), or null when its
/// object is no longer in the project. Used by Scene3D and the 2D
/// canvas's tab-marker drawer.
export function resolveTabPlacementToWorld(
  imp: ImportResponse,
  placement: TabPlacement,
): [number, number] | null {
  const objects = buildObjectPolylines(imp);
  const obj = objects.find((o) => o.objectId === placement.objectId);
  if (!obj) return null;
  const { point } = polylineAtT(obj.pts, placement.t, obj.closed);
  return [point.x, point.y];
}
