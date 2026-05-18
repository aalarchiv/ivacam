/// OSnap engine (64p / Estlcam `Transform.cs::Snaps[]`). One place
/// to ask "where would my click land if I were within snap distance
/// of a CAD feature?" — every click-tool routes through here.
///
/// Snap targets, in priority order (the engine returns whichever
/// kind is closest to the cursor inside the per-kind tolerance):
///
/// - `endpoint`     — segment start or end vertex (■ square)
/// - `center`       — arc / circle geometric center (◯ ring)
/// - `intersection` — line ↔ line crossing (× cross)
/// - `midpoint`     — segment XY midpoint (▲ triangle)
/// - `grid`         — nearest grid intersection (+ plus)
///
/// The data layer is precomputed once per imported-geometry change
/// (caller wraps in `$derived` against `project.imported`) so mouse-
/// move only does cursor → nearest-target distance scans.
///
/// Intersection coverage today: line-line only (O(n²) at precompute,
/// linear at query). Arc-arc and line-arc intersections are a
/// follow-up — the segment count where they'd matter most (laser-cut
/// raster fields) is also where users tend to disable snap entirely.

import type { ImportResponse, Segment } from '../api/types';

/// Snap-target classification. The engine returns one of these
/// alongside the world coordinates so the canvas can paint the
/// matching glyph.
export type OSnapKind = 'endpoint' | 'midpoint' | 'intersection' | 'center' | 'grid';

export interface OSnapCandidate {
  kind: OSnapKind;
  x: number;
  y: number;
}

/// Per-kind on/off plus grid spacing. Plumbed from app settings so the
/// user can dial it to their workflow.
export interface OSnapSettings {
  endpoint: boolean;
  midpoint: boolean;
  intersection: boolean;
  center: boolean;
  grid: boolean;
  /// Grid step in data units (mm). Cursor snaps to integer multiples
  /// of this offset from the origin.
  gridStepMm: number;
}

/// Sensible defaults: all CAD-feature kinds on, grid off (most users
/// snap to existing geometry rather than abstract grid spots).
export const DEFAULT_OSNAP_SETTINGS: OSnapSettings = {
  endpoint: true,
  midpoint: true,
  intersection: true,
  center: true,
  grid: false,
  gridStepMm: 5,
};

/// Precomputed target collection — built once per import. All arrays
/// are in data coordinates (mm).
export interface OSnapTargets {
  endpoints: { x: number; y: number }[];
  midpoints: { x: number; y: number }[];
  intersections: { x: number; y: number }[];
  centers: { x: number; y: number }[];
}

/// Build the full target set for an imported drawing. Cheap for the
/// common case (~hundreds of segments); the only quadratic step is
/// line-line intersection detection, capped by `MAX_INTERSECTION_SCAN`
/// segments to keep huge drawings responsive.
const MAX_INTERSECTION_SCAN = 2_000;

export function precomputeOSnapTargets(
  imported: ImportResponse | null | undefined,
): OSnapTargets {
  const empty: OSnapTargets = {
    endpoints: [],
    midpoints: [],
    intersections: [],
    centers: [],
  };
  if (!imported || imported.segments.length === 0) return empty;

  const endpoints: { x: number; y: number }[] = [];
  const midpoints: { x: number; y: number }[] = [];
  const centers: { x: number; y: number }[] = [];
  const seenEnd = new Set<string>();
  const seenMid = new Set<string>();
  const seenCenter = new Set<string>();
  const keyOf = (x: number, y: number): string =>
    `${Math.round(x * 10000)},${Math.round(y * 10000)}`;
  const pushUnique = (
    arr: { x: number; y: number }[],
    seen: Set<string>,
    x: number,
    y: number,
  ): void => {
    const k = keyOf(x, y);
    if (seen.has(k)) return;
    seen.add(k);
    arr.push({ x, y });
  };

  for (const s of imported.segments) {
    pushUnique(endpoints, seenEnd, s.start.x, s.start.y);
    pushUnique(endpoints, seenEnd, s.end.x, s.end.y);
    pushUnique(
      midpoints,
      seenMid,
      (s.start.x + s.end.x) / 2,
      (s.start.y + s.end.y) / 2,
    );
    if (s.center && (s.type === 'ARC' || s.type === 'CIRCLE')) {
      pushUnique(centers, seenCenter, s.center.x, s.center.y);
    }
  }

  const intersections = computeLineLineIntersections(imported.segments);

  return { endpoints, midpoints, intersections, centers };
}

/// Line ↔ line intersection scan, O(n²) over segments. Skipped when
/// segment count exceeds `MAX_INTERSECTION_SCAN`. Only LINE segments
/// participate — arcs would need analytic intersections and aren't
/// in scope today.
function computeLineLineIntersections(
  segments: readonly Segment[],
): { x: number; y: number }[] {
  if (segments.length > MAX_INTERSECTION_SCAN) return [];
  const lines: Segment[] = segments.filter(
    (s) => s.type === 'LINE' && (s.bulge ?? 0) === 0,
  );
  const out: { x: number; y: number }[] = [];
  const seen = new Set<string>();
  const key = (x: number, y: number): string =>
    `${Math.round(x * 10000)},${Math.round(y * 10000)}`;
  for (let i = 0; i < lines.length; i++) {
    for (let j = i + 1; j < lines.length; j++) {
      const ip = segmentSegmentIntersection(lines[i], lines[j]);
      if (!ip) continue;
      const k = key(ip.x, ip.y);
      if (seen.has(k)) continue;
      seen.add(k);
      out.push(ip);
    }
  }
  return out;
}

/// Strict line-segment intersection: returns a point only when both
/// segments cross within their endpoints (not extrapolated lines).
/// Collinear / parallel segments report no intersection.
function segmentSegmentIntersection(
  a: Segment,
  b: Segment,
): { x: number; y: number } | null {
  const x1 = a.start.x;
  const y1 = a.start.y;
  const x2 = a.end.x;
  const y2 = a.end.y;
  const x3 = b.start.x;
  const y3 = b.start.y;
  const x4 = b.end.x;
  const y4 = b.end.y;
  const denom = (x1 - x2) * (y3 - y4) - (y1 - y2) * (x3 - x4);
  if (Math.abs(denom) < 1e-12) return null;
  const t = ((x1 - x3) * (y3 - y4) - (y1 - y3) * (x3 - x4)) / denom;
  const u = ((x1 - x3) * (y1 - y2) - (y1 - y3) * (x1 - x2)) / denom;
  // Strict within-segment with a tiny epsilon for FP edge cases at the
  // exact endpoint. Endpoint coincidences are already covered by the
  // `endpoint` kind, so this skips T-joints where t ≈ 0/1.
  const EPS = 1e-9;
  if (t < EPS || t > 1 - EPS || u < EPS || u > 1 - EPS) return null;
  const x = x1 + t * (x2 - x1);
  const y = y1 + t * (y2 - y1);
  return { x, y };
}

/// Find the nearest snap candidate to `(x, y)` within `toleranceData`
/// (data units). Honors `settings.<kind>` toggles. Ties broken by
/// priority (endpoint > center > intersection > midpoint > grid).
///
/// Returns `null` when no enabled kind has a candidate in range.
export function findOSnap(
  targets: OSnapTargets,
  x: number,
  y: number,
  toleranceData: number,
  settings: OSnapSettings,
): OSnapCandidate | null {
  if (toleranceData <= 0) return null;
  const t2 = toleranceData * toleranceData;

  let best: OSnapCandidate | null = null;
  let bestD2 = Infinity;
  let bestPrio = Infinity;

  const tryKind = (
    kind: OSnapKind,
    enabled: boolean,
    candidates: readonly { x: number; y: number }[],
    priority: number,
  ): void => {
    if (!enabled) return;
    for (const c of candidates) {
      const dx = c.x - x;
      const dy = c.y - y;
      const d2 = dx * dx + dy * dy;
      if (d2 >= t2) continue;
      if (
        d2 < bestD2 ||
        // Same-distance tie → prefer the higher-priority kind
        // (lower priority number).
        (Math.abs(d2 - bestD2) < 1e-12 && priority < bestPrio)
      ) {
        best = { kind, x: c.x, y: c.y };
        bestD2 = d2;
        bestPrio = priority;
      }
    }
  };

  // Priority order: endpoint > center > intersection > midpoint > grid.
  tryKind('endpoint', settings.endpoint, targets.endpoints, 0);
  tryKind('center', settings.center, targets.centers, 1);
  tryKind('intersection', settings.intersection, targets.intersections, 2);
  tryKind('midpoint', settings.midpoint, targets.midpoints, 3);

  // Grid snap is computed on the fly — nearest multiple of `gridStepMm`
  // to the cursor. Only consider when grid is enabled AND the grid
  // vertex sits inside tolerance (otherwise the cursor is far enough
  // from a grid line that snapping there would be unexpected).
  if (settings.grid && settings.gridStepMm > 0) {
    const gx = Math.round(x / settings.gridStepMm) * settings.gridStepMm;
    const gy = Math.round(y / settings.gridStepMm) * settings.gridStepMm;
    const dx = gx - x;
    const dy = gy - y;
    const d2 = dx * dx + dy * dy;
    if (d2 < t2 && d2 < bestD2) {
      best = { kind: 'grid', x: gx, y: gy };
      bestD2 = d2;
      bestPrio = 4;
    }
  }

  return best;
}
