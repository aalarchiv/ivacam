// HTTP implementation of WiacClient. Talks to whichever server is wired
// up — Stage-1 Python FastAPI now, Rust axum later. Same shape.

import type { DefaultsResponse, ProgressEvent, WiacClient } from './client';
import type {
  GenerateRequest,
  GenerateResponse,
  ImportResponse,
  VersionResponse,
} from './types';

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
    if (!res.ok) {
      let detail: unknown;
      try {
        detail = await res.json();
      } catch {
        detail = await res.text();
      }
      throw new Error(`/import returned ${res.status}: ${JSON.stringify(detail)}`);
    }
    return (await res.json()) as ImportResponse;
  }

  async defaults(): Promise<DefaultsResponse> {
    const res = await fetch(`${this.base}/defaults`);
    if (!res.ok) throw new Error(`/defaults returned ${res.status}`);
    return (await res.json()) as DefaultsResponse;
  }

  async generate(request: GenerateRequest): Promise<GenerateResponse> {
    const res = await fetch(`${this.base}/generate`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(request),
    });
    if (!res.ok) {
      let detail: unknown;
      try {
        detail = await res.json();
      } catch {
        detail = await res.text();
      }
      throw new Error(`/generate returned ${res.status}: ${JSON.stringify(detail)}`);
    }
    return (await res.json()) as GenerateResponse;
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
    if (!res.ok || !res.body) {
      let detail: unknown;
      try {
        detail = await res.json();
      } catch {
        detail = await res.text();
      }
      throw new Error(`/generate/stream returned ${res.status}: ${JSON.stringify(detail)}`);
    }

    const reader = res.body.getReader();
    const decoder = new TextDecoder('utf-8');
    let buffer = '';
    let result: GenerateResponse | undefined;
    let errorPayload: { status: number; message: string } | undefined;

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
            errorPayload = JSON.parse(data) as { status: number; message: string };
          }
        } catch {
          // Malformed frame — drop and keep reading.
        }
      }
    }

    if (errorPayload) {
      throw new Error(`/generate/stream errored ${errorPayload.status}: ${errorPayload.message}`);
    }
    if (!result) {
      throw new Error('/generate/stream closed before emitting a result');
    }
    return result;
  }
}

export function defaultClient(): WiacClient {
  // Resolution order:
  //   0. Running inside the Tauri shell → in-process invoke()
  //   1. VITE_WIAC_API at build time
  //   2. ?api=… query param at runtime (handy for demos)
  //   3. http://<host>:8766 — Rust server
  if (typeof window !== 'undefined') {
    const w = window as unknown as Record<string, unknown>;
    if (typeof w.__TAURI_INTERNALS__ !== 'undefined') {
      const mod = (w.__WIAC_TAURI_CLIENT__ ??= new TauriClientLazy()) as TauriClientLazy;
      return mod.proxy;
    }
  }

  const fromEnv = import.meta.env.VITE_WIAC_API as string | undefined;
  if (fromEnv) return new HttpWiacClient(fromEnv);

  if (typeof window !== 'undefined') {
    const params = new URLSearchParams(window.location.search);
    const fromQuery = params.get('api');
    if (fromQuery) return new HttpWiacClient(fromQuery);

    const { protocol, hostname } = window.location;
    return new HttpWiacClient(`${protocol}//${hostname}:8766`);
  }

  return new HttpWiacClient('http://127.0.0.1:8765');
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
      defaults: () => ensure().then((c) => c.defaults()),
    };
  }
}
