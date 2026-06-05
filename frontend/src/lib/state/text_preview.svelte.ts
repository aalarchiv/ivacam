/// Live 2D preview cache for editable text layers.
///
/// Each `TextLayer` in `project.textLayers` maps to a rendered segment
/// list the canvas overlays on top of the imported geometry. Renders go
/// through the `renderTextLayer` backend API (same code path the pipeline
/// uses at Generate time, so preview and gcode agree).
///
/// Cache key is a content hash of the editable fields — same text + size +
/// origin + rotation + spacing + alignment + font bytes → same segments.
/// Edits invalidate one entry only; switching between layers is free.
///
/// API:
///   `previewSegmentsFor(layer)` — last successful render, or null when
///     the layer hasn't rendered yet (or its render is in flight). Reads
///     are reactive: components subscribe via `void previewVersion`.
///   `requestPreview(layer)` — schedules a debounced render. Multiple
///     calls for the same layer collapse onto the most recent inputs.

import { defaultClient } from '../api/http';
import type { Segment } from '../api/types';
import type { TextLayer } from './project.svelte';

const DEBOUNCE_MS = 250;

interface CacheEntry {
  /// Hash of the inputs that produced these segments.
  key: string;
  segments: Segment[];
  /// The layer origin these segments were rendered at. Origin is NOT part
  /// of the hash (an origin change is a pure translation, not a reshape),
  /// so a drag never re-renders; consumers translate by
  /// `currentOrigin - renderOrigin` instead. See `previewRenderOrigin`.
  renderOrigin: { x: number; y: number };
}

const cache: Map<number, CacheEntry> = new Map();
const inflight: Map<number, { key: string; timer: ReturnType<typeof setTimeout> }> = new Map();

/// Per-layer generation token of the most-recently-dispatched render.
/// `inflight` only tracks the debounce timer and is cleared the moment a
/// render fires, so two renders for the same layer can be in flight at
/// once (edit while one is resolving). Without this guard a slower stale
/// render could resolve last and overwrite fresh segments — and an
/// in-flight render could repopulate a just-deleted layer. A render only
/// mutates the cache if its captured token is still the latest. The
/// counter is global+monotonic so a delete→recreate can't collide.
const generation: Map<number, number> = new Map();
let genCounter = 0;

/// Bumped on every cache mutation so reactive readers re-derive.
/// Components touch `previewVersion.v` inside a `$derived` / `$effect`
/// to subscribe — the cache itself is a plain Map so the version
/// counter is the sole reactivity signal.
export const previewVersion = $state({ v: 0 });
function bumpVersion() {
  previewVersion.v += 1;
}

function hashLayer(layer: TextLayer): string {
  const fontKey =
    layer.fontSource.kind === 'bundled'
      ? `b:${layer.fontSource.path}`
      : `u:${layer.fontSource.filename}:${layer.fontSource.bytes_b64.length}`;
  // NOTE: origin is deliberately excluded — it only translates the result,
  // so dragging the origin must not invalidate the cache / trigger a render
  // (each render re-marshals the whole font; k9cz). Consumers apply the
  // origin delta at draw time.
  return [
    layer.text,
    layer.kind,
    layer.sizeMm,
    layer.rotationDeg,
    layer.letterSpacingMm,
    layer.lineSpacingMm,
    layer.alignment,
    layer.widthScale,
    fontKey,
  ].join('|');
}

function decodeFontBytes(b64: string): number[] {
  const binary = atob(b64);
  const out: number[] = new Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

/// Strict wire shape — mirrors the Rust TextLayer with font_bytes as
/// an integer array. We don't import the generated type here because
/// the OpenAPI codegen models `font_bytes` as `number[]`, which is
/// what the renderer wants.
interface WireLayer {
  id: number;
  kind: 'TEXT' | 'MTEXT';
  name: string;
  text: string;
  font_bytes: number[];
  size_mm: number;
  origin: [number, number];
  rotation_deg: number;
  letter_spacing_mm: number;
  line_spacing_mm: number;
  alignment: 'left' | 'center' | 'right';
  width_scale: number;
}

function toWire(layer: TextLayer): WireLayer {
  return {
    id: layer.id,
    kind: layer.kind,
    name: layer.name,
    text: layer.text,
    font_bytes: decodeFontBytes(layer.fontSource.bytes_b64),
    size_mm: layer.sizeMm,
    origin: [layer.origin.x, layer.origin.y],
    rotation_deg: layer.rotationDeg,
    letter_spacing_mm: layer.letterSpacingMm,
    line_spacing_mm: layer.lineSpacingMm,
    alignment: layer.alignment,
    width_scale: layer.widthScale,
  };
}

/// Schedule a debounced render for `layer`. Successive calls collapse
/// onto the latest inputs; cached entries return immediately on the
/// next `previewSegmentsFor` read.
export function requestPreview(layer: TextLayer): void {
  const key = hashLayer(layer);
  const cached = cache.get(layer.id);
  if (cached?.key === key) return; // already current
  const pending = inflight.get(layer.id);
  if (pending) {
    if (pending.key === key) return;
    clearTimeout(pending.timer);
  }
  const timer = setTimeout(() => {
    inflight.delete(layer.id);
    // Token for THIS render. Any later dispatch (or an invalidate/reset)
    // advances the layer's generation, so a stale resolution below bails.
    const gen = ++genCounter;
    generation.set(layer.id, gen);
    // Build the wire payload (which atob-decodes the WHOLE 700 KB+ font
    // into a number[] and marshals it across the worker / IPC boundary)
    // only when the debounce actually fires. Origin is out of the hash, so
    // a drag no longer reaches here at all — this runs only on a real
    // text / size / font change (k9cz).
    const wire = toWire(layer);
    // The origin baked into these segments. Consumers translate by
    // (currentOrigin - renderOrigin), so an origin drag needs no re-render.
    const renderOrigin = { x: layer.origin.x, y: layer.origin.y };
    const client = defaultClient();
    void client
      .renderTextLayer(wire as never)
      .then((resp) => {
        if (generation.get(layer.id) !== gen) return; // superseded
        cache.set(layer.id, { key, segments: resp.segments, renderOrigin });
        bumpVersion();
      })
      .catch(() => {
        if (generation.get(layer.id) !== gen) return; // superseded
        // Drop the cache entry so the next edit retries. Failures
        // typically mean an empty / parseable-but-empty render; the UI
        // shows nothing rather than a stale outdated preview.
        cache.delete(layer.id);
        bumpVersion();
      });
  }, DEBOUNCE_MS);
  inflight.set(layer.id, { key, timer });
}

/// Latest cached segments for `layer`, or null if no render has
/// resolved yet. Callers should call `requestPreview(layer)` separately
/// to keep the cache warm.
/// Cached segments for `layerId`, translated to `origin`, or null if no
/// render has resolved yet. The cache holds glyphs at the origin they were
/// rendered at; origin is excluded from the render hash (it's a pure
/// translation), so a drag never re-renders — every consumer (draw, 3D,
/// hit-test, bbox) passes the layer's CURRENT origin and gets correctly
/// positioned segments. Translating start/end suffices: draw + tessellate
/// recompute arc centers from start/end, and the hit-test uses chords. (k9cz)
export function previewSegmentsFor(
  layerId: number,
  origin: { x: number; y: number },
): Segment[] | null {
  const entry = cache.get(layerId);
  if (!entry) return null;
  const dx = origin.x - entry.renderOrigin.x;
  const dy = origin.y - entry.renderOrigin.y;
  if (dx === 0 && dy === 0) return entry.segments;
  return entry.segments.map((s) => ({
    ...s,
    start: { ...s.start, x: s.start.x + dx, y: s.start.y + dy },
    end: { ...s.end, x: s.end.x + dx, y: s.end.y + dy },
  }));
}

/// Force reactive readers (3D scene) to re-derive without a re-render —
/// e.g. at the end of an origin drag, so views that don't track origin
/// per-frame pick up the final position via the translation above.
export function forceTextPreviewRefresh(): void {
  bumpVersion();
}

/// Drop the cache + cancel pending timers for `layerId`. Call when the
/// layer is deleted so no stale segments linger.
export function invalidatePreview(layerId: number): void {
  const pending = inflight.get(layerId);
  if (pending) {
    clearTimeout(pending.timer);
    inflight.delete(layerId);
  }
  // Supersede any already-dispatched render so its late resolution can't
  // repopulate the cache for a layer we just deleted.
  generation.delete(layerId);
  if (cache.delete(layerId)) bumpVersion();
}

/// Drop EVERY cache entry — call when the project is replaced (load a
/// different project file).
export function resetPreviewCache(): void {
  if (cache.size === 0 && inflight.size === 0) return;
  for (const { timer } of inflight.values()) clearTimeout(timer);
  inflight.clear();
  // Supersede every in-flight render so none repopulates the fresh cache.
  generation.clear();
  cache.clear();
  bumpVersion();
}
