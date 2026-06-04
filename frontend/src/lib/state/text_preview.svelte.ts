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
}

const cache: Map<number, CacheEntry> = new Map();
const inflight: Map<number, { key: string; timer: ReturnType<typeof setTimeout> }> = new Map();

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
  return [
    layer.text,
    layer.kind,
    layer.sizeMm,
    layer.origin.x,
    layer.origin.y,
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
    // Build the wire payload (which atob-decodes the WHOLE font into a
    // number[]) only when the debounce actually fires — never per
    // scheduling call. A text-origin drag re-schedules this on every
    // pointermove with a fresh key (origin changes), so decoding here
    // instead of up-front keeps the drag from re-decoding the font on
    // every move (k9cz). `layer` is the last-scheduled snapshot, which
    // matches `key`.
    const wire = toWire(layer);
    const client = defaultClient();
    void client
      .renderTextLayer(wire as never)
      .then((resp) => {
        cache.set(layer.id, { key, segments: resp.segments });
        bumpVersion();
      })
      .catch(() => {
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
export function previewSegmentsFor(layerId: number): Segment[] | null {
  return cache.get(layerId)?.segments ?? null;
}

/// Drop the cache + cancel pending timers for `layerId`. Call when the
/// layer is deleted so no stale segments linger.
export function invalidatePreview(layerId: number): void {
  const pending = inflight.get(layerId);
  if (pending) {
    clearTimeout(pending.timer);
    inflight.delete(layerId);
  }
  if (cache.delete(layerId)) bumpVersion();
}

/// Drop EVERY cache entry — call when the project is replaced (load a
/// different project file).
export function resetPreviewCache(): void {
  if (cache.size === 0 && inflight.size === 0) return;
  for (const { timer } of inflight.values()) clearTimeout(timer);
  inflight.clear();
  cache.clear();
  bumpVersion();
}
