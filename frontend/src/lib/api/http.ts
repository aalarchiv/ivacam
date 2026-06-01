// HTTP implementation of WiacClient. Talks to wiac-server (axum) over the
// JSON contract in schema/openapi.yaml.

import { CancelledError, type PipelineEvent, type ProgressEvent, type WiacClient } from './client';
import type {
  GenerateRequest,
  GenerateResponse,
  HelixRadiusRequest,
  HelixRadiusResponse,
  ImportResponse,
  RenderTextRequest,
  RenderTextResponse,
  RenderTextLayerResponse,
  WireTextLayer,
  VersionResponse,
} from './types';

/**
 * Read a failed Response body and throw an Error whose `.message` carries
 * the structured `wiac_core::Error` shape verbatim when the server sent one
 * (luf1). Frontend `tryParseStructuredError` recognises the JSON string and
 * extracts kind / recovery_hint / auto_fix / span; otherwise callers see
 * the legacy `<label> returned <status>: <detail>` form.
 */
async function throwHttpError(label: string, res: Response): Promise<never> {
  let detail: unknown;
  try {
    detail = await res.json();
  } catch {
    detail = await res.text();
  }
  if (looksLikeStructuredError(detail)) {
    // Stringify so tryParseStructuredError(e.message) — which expects a
    // string starting with '{' — parses it back into a WiacError. This
    // keeps a single error-detection codepath across HTTP/Tauri/WASM.
    throw new Error(JSON.stringify(detail));
  }
  throw new Error(`${label} returned ${res.status}: ${JSON.stringify(detail)}`);
}

function looksLikeStructuredError(detail: unknown): boolean {
  if (detail == null || typeof detail !== 'object') return false;
  const d = detail as Record<string, unknown>;
  return typeof d.kind === 'string' && typeof d.message === 'string';
}

export class HttpWiacClient implements WiacClient {
  constructor(private readonly base: string) {}

  async health(): Promise<boolean> {
    const res = await fetch(`${this.base}/healthz`);
    if (!res.ok) return false;
    const body = (await res.json()) as { ok?: boolean };
    return body.ok === true;
  }

  async version(): Promise<VersionResponse> {
    const res = await fetch(`${this.base}/version`);
    if (!res.ok) throw new Error(`/version returned ${res.status}`);
    return (await res.json()) as VersionResponse;
  }

  async importFile(file: File, format?: string): Promise<ImportResponse> {
    const form = new FormData();
    form.append('file', file);
    if (format) form.append('format', format);
    const res = await fetch(`${this.base}/import`, { method: 'POST', body: form });
    if (!res.ok) await throwHttpError('/import', res);
    return (await res.json()) as ImportResponse;
  }

  async generate(request: GenerateRequest): Promise<GenerateResponse> {
    const res = await fetch(`${this.base}/generate`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(request),
    });
    if (!res.ok) await throwHttpError('/generate', res);
    return (await res.json()) as GenerateResponse;
  }

  async renderText(request: RenderTextRequest): Promise<RenderTextResponse> {
    const res = await fetch(`${this.base}/text`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(request),
    });
    if (!res.ok) await throwHttpError('/text', res);
    return (await res.json()) as RenderTextResponse;
  }

  async renderTextLayer(layer: WireTextLayer): Promise<RenderTextLayerResponse> {
    const res = await fetch(`${this.base}/text/layer`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(layer),
    });
    if (!res.ok) await throwHttpError('/text/layer', res);
    return (await res.json()) as RenderTextLayerResponse;
  }

  async computeHelixRadius(request: HelixRadiusRequest): Promise<HelixRadiusResponse> {
    const res = await fetch(`${this.base}/helix-radius`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(request),
    });
    if (!res.ok) await throwHttpError('/helix-radius', res);
    return (await res.json()) as HelixRadiusResponse;
  }

  /**
   * Stream variant — POST + parse text/event-stream by hand. We avoid the
   * built-in EventSource because it's GET-only; the request body is JSON.
   * Emits one `progress` event per phase boundary, then a `result` event
   * carrying the final response (or an `error` event with status+message).
   */
  async generateStream(
    request: GenerateRequest,
    onProgress: (e: ProgressEvent) => void,
  ): Promise<GenerateResponse> {
    const res = await fetch(`${this.base}/generate/stream`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', accept: 'text/event-stream' },
      body: JSON.stringify(request),
    });
    if (!res.ok || !res.body) await throwHttpError('/generate/stream', res);

    const reader = res.body!.getReader();
    const decoder = new TextDecoder('utf-8');
    let buffer = '';
    let result: GenerateResponse | undefined;
    // Raw event-data string from the SSE `error` event. Server emits the
    // full structured `wiac_core::Error` as the payload (luf1); we rethrow
    // it as a JSON string so tryParseStructuredError() works downstream.
    let errorPayload: string | undefined;

    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      // SSE framing: events separated by a blank line; fields by single
      // newlines. We only care about `event:` and `data:`.
      let i: number;
      while ((i = buffer.indexOf('\n\n')) >= 0) {
        const frame = buffer.slice(0, i);
        buffer = buffer.slice(i + 2);
        let eventName = 'message';
        const dataLines: string[] = [];
        for (const line of frame.split('\n')) {
          if (line.startsWith('event:')) eventName = line.slice(6).trim();
          else if (line.startsWith('data:')) dataLines.push(line.slice(5).trimStart());
        }
        if (dataLines.length === 0) continue;
        const data = dataLines.join('\n');
        try {
          if (eventName === 'progress') {
            onProgress(JSON.parse(data) as ProgressEvent);
          } else if (eventName === 'result') {
            result = JSON.parse(data) as GenerateResponse;
          } else if (eventName === 'error') {
            errorPayload = data;
          }
        } catch {
          // Malformed frame — drop and keep reading.
        }
      }
    }

    if (errorPayload !== undefined) {
      throw new Error(errorPayload);
    }
    if (!result) {
      throw new Error('/generate/stream closed before emitting a result');
    }
    return result;
  }

  /**
   * Per-op streaming with cancellation. The /generate/stream SSE stream
   * carries a `token` event up front, then `pipeline` events for each
   * PipelineEvent, and finally `result` (or `cancelled` / `error`).
   * Aborting `cancelToken` POSTs to `/generate/cancel/<token>` so the
   * server flips the shared cancel flag.
   */
  async generateStreaming(
    request: GenerateRequest,
    onEvent: (event: PipelineEvent) => void,
    cancelToken?: AbortSignal,
  ): Promise<GenerateResponse> {
    const res = await fetch(`${this.base}/generate/stream`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', accept: 'text/event-stream' },
      body: JSON.stringify(request),
    });
    if (!res.ok || !res.body) await throwHttpError('/generate/stream', res);

    const reader = res.body!.getReader();
    const decoder = new TextDecoder('utf-8');
    let buffer = '';
    let result: GenerateResponse | undefined;
    let cancelled = false;
    // Raw event-data from the SSE `error` event — the full structured
    // `wiac_core::Error` JSON. Rethrown verbatim so the call site can
    // parse it via tryParseStructuredError().
    let errorPayload: string | undefined;
    let tokenId: number | undefined;
    let abortHandler: (() => void) | undefined;

    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let i: number;
        while ((i = buffer.indexOf('\n\n')) >= 0) {
          const frame = buffer.slice(0, i);
          buffer = buffer.slice(i + 2);
          let eventName = 'message';
          const dataLines: string[] = [];
          for (const line of frame.split('\n')) {
            if (line.startsWith('event:')) eventName = line.slice(6).trim();
            else if (line.startsWith('data:')) dataLines.push(line.slice(5).trimStart());
          }
          if (dataLines.length === 0) continue;
          const data = dataLines.join('\n');
          try {
            if (eventName === 'token') {
              const t = JSON.parse(data) as { token_id: number };
              tokenId = t.token_id;
              if (cancelToken) {
                if (cancelToken.aborted) {
                  void this.cancelGenerate(tokenId);
                } else {
                  abortHandler = () => {
                    if (tokenId !== undefined) void this.cancelGenerate(tokenId);
                  };
                  cancelToken.addEventListener('abort', abortHandler);
                }
              }
            } else if (eventName === 'pipeline') {
              onEvent(JSON.parse(data) as PipelineEvent);
            } else if (eventName === 'result') {
              result = JSON.parse(data) as GenerateResponse;
            } else if (eventName === 'cancelled') {
              cancelled = true;
              onEvent({ kind: 'cancelled' });
            } else if (eventName === 'error') {
              errorPayload = data;
            }
          } catch {
            // Malformed frame — drop and keep reading.
          }
        }
      }
    } finally {
      if (cancelToken && abortHandler) {
        cancelToken.removeEventListener('abort', abortHandler);
      }
    }

    if (cancelled) throw new CancelledError();
    if (errorPayload !== undefined) {
      throw new Error(errorPayload);
    }
    if (!result) {
      throw new Error('/generate/stream closed before emitting a result');
    }
    return result;
  }

  private async cancelGenerate(tokenId: number): Promise<void> {
    try {
      await fetch(`${this.base}/generate/cancel/${tokenId}`, { method: 'POST' });
    } catch {
      // Cancellation is best-effort — ignore network failures.
    }
  }
}

/// Resolved transport choice — the pure decision behind `defaultClient`.
export type ApiChoice = { kind: 'tauri' } | { kind: 'wasm' } | { kind: 'http'; url: string };

/// Pure transport selection (testable without window / wasm / a real
/// build mode). Resolution order:
///   0. Tauri shell                          → in-process invoke()
///   1. VITE_WIAC_API at build time          → that URL (or wasm if =='wasm')
///   2. ?api=… query param at runtime        → that URL (or wasm if =='wasm')
///   3. default — no transport configured:
///        • production build → the in-browser wasm engine, so a static
///          deploy works from a bare URL with no backend (5ue0/xeio
///          browser-trial). Point at a server instead via VITE_WIAC_API.
///        • dev → the local wiac-server on :8766 (the documented dev
///          workflow runs `cargo run -p wiac-server`).
export function resolveApiChoice(opts: {
  hasTauri: boolean;
  envApi: string | undefined;
  queryApi: string | null;
  defaultWasm: boolean;
  serverUrl: string;
}): ApiChoice {
  if (opts.hasTauri) return { kind: 'tauri' };
  if (opts.envApi) {
    return opts.envApi === 'wasm' ? { kind: 'wasm' } : { kind: 'http', url: opts.envApi };
  }
  if (opts.queryApi === 'wasm') return { kind: 'wasm' };
  if (opts.queryApi) return { kind: 'http', url: opts.queryApi };
  return opts.defaultWasm ? { kind: 'wasm' } : { kind: 'http', url: opts.serverUrl };
}

export function defaultClient(): WiacClient {
  const hasWindow = typeof window !== 'undefined';
  const w = hasWindow ? (window as unknown as Record<string, unknown>) : undefined;
  const hasTauri = !!w && typeof w.__TAURI_INTERNALS__ !== 'undefined';
  const queryApi = hasWindow ? new URLSearchParams(window.location.search).get('api') : null;
  const serverUrl = hasWindow
    ? `${window.location.protocol}//${window.location.hostname}:8766`
    : 'http://127.0.0.1:8766';
  const choice = resolveApiChoice({
    hasTauri,
    envApi: import.meta.env.VITE_WIAC_API as string | undefined,
    queryApi,
    // Default to wasm only in a real production browser build — wasm
    // can't run during SSR / tests (no window), which keep the :8766
    // server default so existing test/dev expectations hold.
    defaultWasm: hasWindow && import.meta.env.PROD === true,
    serverUrl,
  });
  switch (choice.kind) {
    case 'tauri': {
      const mod = (w!.__WIAC_TAURI_CLIENT__ ??= new TauriClientLazy()) as TauriClientLazy;
      return mod.proxy;
    }
    case 'wasm':
      // Lazy import so the wasm chunk is only fetched when wasm is chosen.
      return new WasmClientLazy().proxy;
    case 'http':
      return new HttpWiacClient(choice.url);
  }
}

/**
 * 5ue0: build the in-browser wasm client. Prefer the Web Worker variant
 * (non-blocking UI + real cancel); fall back to the main-thread client
 * where module workers aren't available or the worker fails to construct.
 */
async function createWasmClient(): Promise<WiacClient> {
  if (typeof Worker !== 'undefined') {
    try {
      const mod = await import('./wasm-worker-client');
      // Constructs the worker eagerly — a module-worker-unsupported
      // environment throws here and we fall through to the main thread.
      return new mod.WasmWorkerClient();
    } catch {
      /* fall back to the synchronous main-thread client */
    }
  }
  const wm = await import('./wasm');
  return new wm.WasmWiacClient();
}

/**
 * WASM client wrapper — same lazy pattern. The wiac-wasm chunk is only
 * loaded when ?api=wasm is set, otherwise it stays out of the bundle.
 */
class WasmClientLazy {
  private impl: WiacClient | null = null;
  proxy: WiacClient;

  constructor() {
    const ensure = async (): Promise<WiacClient> => {
      if (!this.impl) {
        this.impl = await createWasmClient();
      }
      return this.impl;
    };
    this.proxy = {
      health: () => ensure().then((c) => c.health()),
      version: () => ensure().then((c) => c.version()),
      importFile: (file, format) => ensure().then((c) => c.importFile(file, format)),
      generate: (req) => ensure().then((c) => c.generate(req)),
      generateStream: (req, cb) =>
        ensure().then((c) => (c.generateStream ? c.generateStream(req, cb) : c.generate(req))),
      generateStreaming: (req, onEvent, signal) =>
        ensure().then((c) =>
          c.generateStreaming ? c.generateStreaming(req, onEvent, signal) : c.generate(req),
        ),
      renderText: (req) => ensure().then((c) => c.renderText(req)),
      renderTextLayer: (layer) => ensure().then((c) => c.renderTextLayer(layer)),
      computeHelixRadius: (req) => ensure().then((c) => c.computeHelixRadius(req)),
    };
  }
}

/**
 * Tauri client wrapper — defers loading the implementation module until
 * it's first used so plain web builds don't need to resolve the
 * @tauri-apps/* import graph eagerly.
 */
class TauriClientLazy {
  private impl: WiacClient | null = null;
  proxy: WiacClient;

  constructor() {
    const ensure = async (): Promise<WiacClient> => {
      if (!this.impl) {
        const mod = await import('./tauri');
        this.impl = new mod.TauriWiacClient();
      }
      return this.impl;
    };
    this.proxy = {
      health: () => ensure().then((c) => c.health()),
      version: () => ensure().then((c) => c.version()),
      importFile: (file, format) => ensure().then((c) => c.importFile(file, format)),
      generate: (req) => ensure().then((c) => c.generate(req)),
      generateStream: (req, cb) =>
        ensure().then((c) => (c.generateStream ? c.generateStream(req, cb) : c.generate(req))),
      generateStreaming: (req, onEvent, signal) =>
        ensure().then((c) =>
          c.generateStreaming ? c.generateStreaming(req, onEvent, signal) : c.generate(req),
        ),
      renderText: (req) => ensure().then((c) => c.renderText(req)),
      renderTextLayer: (layer) => ensure().then((c) => c.renderTextLayer(layer)),
      computeHelixRadius: (req) => ensure().then((c) => c.computeHelixRadius(req)),
    };
  }
}
